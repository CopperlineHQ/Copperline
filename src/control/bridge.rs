// SPDX-License-Identifier: GPL-3.0-or-later

//! A control-protocol client for the MCP server: one authenticated
//! connection, a reader thread that owns the socket's receive side and
//! routes replies by id, and a bounded queue for the `event.*`
//! notifications that arrive between and during requests.
//!
//! The split matters because a resume verb's reply is the eventual stop
//! event: while `continue` is outstanding, frame and serial events keep
//! streaming and must not sit in the socket buffer behind it. The reader
//! thread drains everything as it arrives; the request thread waits on a
//! condition variable for its own id, with or without a timeout.
//!
//! Also the launcher for a headless emulator process, since an agent
//! attaching through MCP usually has no emulator running yet.

use super::proto;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{BufReader, Write as _};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Notifications kept between `events_next` / `events_drain` calls;
/// beyond this the oldest is dropped and counted.
pub const EVENT_QUEUE_CAPACITY: usize = 1024;

/// How long `launch` waits for the emulator to announce its endpoint
/// unless the caller says otherwise.
pub const DEFAULT_LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

/// The outcome of one request.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// The `result` of a successful reply.
    Ok(Value),
    /// The server's JSON-RPC error.
    Err { code: i64, message: String },
    /// No reply within the wait; the request is still outstanding and
    /// `wait` can be called again for it.
    TimedOut,
}

impl Reply {
    fn from_envelope(msg: Value) -> Self {
        if let Some(err) = msg.get("error") {
            return Reply::Err {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_string(),
            };
        }
        Reply::Ok(msg.get("result").cloned().unwrap_or(Value::Null))
    }
}

#[derive(Default)]
struct State {
    /// Outstanding request ids, and the reply once it has arrived.
    pending: HashMap<u64, Option<Value>>,
    events: VecDeque<Value>,
    dropped: u64,
    /// Why the connection ended, once it has.
    closed: Option<String>,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One authenticated control-protocol connection.
///
/// Sending takes `&self`: the DAP adapter keeps one thread blocked in
/// [`Bridge::wait`] for an outstanding resume while another sends
/// `pause` and breakpoint changes, so the send side is its own lock.
pub struct Bridge {
    writer: Mutex<TcpStream>,
    next_id: AtomicU64,
    shared: Arc<Shared>,
    reader: Option<JoinHandle<()>>,
    listen: String,
    hello: Value,
}

impl Bridge {
    /// Connect to `addr` and authenticate with `token`.
    pub fn connect(addr: &str, token: &str) -> Result<Self, String> {
        let stream = TcpStream::connect(addr).map_err(|e| format!("connecting to {addr}: {e}"))?;
        stream.set_nodelay(true).ok();
        let read_side = stream
            .try_clone()
            .map_err(|e| format!("cloning the connection: {e}"))?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
        });
        let reader_shared = Arc::clone(&shared);
        let reader = std::thread::Builder::new()
            .name("copperline-mcp-read".into())
            .spawn(move || read_loop(BufReader::new(read_side), reader_shared))
            .map_err(|e| format!("starting the reader thread: {e}"))?;
        let mut bridge = Self {
            writer: Mutex::new(stream),
            next_id: AtomicU64::new(1),
            shared,
            reader: Some(reader),
            listen: addr.to_string(),
            hello: Value::Null,
        };
        match bridge.call("hello", json!({"token": token}))? {
            Reply::Ok(hello) if hello["authed"] == Value::Bool(true) => bridge.hello = hello,
            Reply::Ok(hello) => return Err(format!("auth failed: {hello}")),
            Reply::Err { message, .. } => return Err(format!("auth failed: {message}")),
            Reply::TimedOut => unreachable!("hello is waited without a timeout"),
        }
        Ok(bridge)
    }

    /// Connect using the `{"listen", "token"}` file `--control-info` wrote.
    pub fn connect_info_file(path: &Path) -> Result<Self, String> {
        let (listen, token) = read_info_file(path)?;
        Self::connect(&listen, &token)
    }

    /// The address this bridge connected to.
    pub fn listen(&self) -> &str {
        &self.listen
    }

    /// The server's `hello` reply (`proto`, `emulator`).
    pub fn hello(&self) -> &Value {
        &self.hello
    }

    /// Why the connection ended, if it has.
    pub fn closed(&self) -> Option<String> {
        self.shared.lock().closed.clone()
    }

    /// Send a request and return its id without waiting.
    pub fn send(&self, method: &str, params: Value) -> Result<u64, String> {
        if let Some(reason) = self.closed() {
            return Err(format!("session lost: {reason}"));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.shared.lock().pending.insert(id, None);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let line = format!("{msg}\n");
        let written = {
            let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
            writer
                .write_all(line.as_bytes())
                .and_then(|_| writer.flush())
        };
        if let Err(e) = written {
            self.shared.lock().pending.remove(&id);
            return Err(format!("sending {method}: {e}"));
        }
        Ok(id)
    }

    /// Wait for the reply to `id`, up to `timeout` (forever when `None`).
    pub fn wait(&self, id: u64, timeout: Option<Duration>) -> Result<Reply, String> {
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut state = self.shared.lock();
        loop {
            if let Some(Some(_)) = state.pending.get(&id) {
                let msg = state.pending.remove(&id).flatten().expect("checked above");
                return Ok(Reply::from_envelope(msg));
            }
            if let Some(reason) = state.closed.clone() {
                state.pending.remove(&id);
                return Err(format!("session lost: {reason}"));
            }
            state = match deadline {
                None => self
                    .shared
                    .cv
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner()),
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(Reply::TimedOut);
                    }
                    self.shared
                        .cv
                        .wait_timeout(state, deadline - now)
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
            };
        }
    }

    /// One request-reply round trip with no timeout.
    pub fn call(&self, method: &str, params: Value) -> Result<Reply, String> {
        let id = self.send(method, params)?;
        self.wait(id, None)
    }

    /// Stop waiting for `id`: its pending entry is dropped, so a reply
    /// that arrives later is discarded by the reader rather than kept for
    /// a waiter that never comes. A no-op once the reply has been taken.
    pub fn forget(&self, id: u64) {
        self.shared.lock().pending.remove(&id);
    }

    /// Requests sent and neither answered nor forgotten.
    pub fn outstanding(&self) -> usize {
        self.shared.lock().pending.len()
    }

    /// The next queued notification, waiting up to `timeout` for one.
    pub fn next_event(&self, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.lock();
        loop {
            if let Some(event) = state.events.pop_front() {
                return Some(event);
            }
            if state.closed.is_some() {
                return None;
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            state = self
                .shared
                .cv
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Every queued notification, oldest first.
    pub fn drain_events(&self) -> Vec<Value> {
        self.shared.lock().events.drain(..).collect()
    }

    /// (queued, dropped so far).
    pub fn event_counts(&self) -> (usize, u64) {
        let state = self.shared.lock();
        (state.events.len(), state.dropped)
    }

    /// Shut the socket without waiting for the reader: every thread
    /// blocked in `wait` or `next_event` wakes up with the connection
    /// reported lost, which is how a bridge shared between threads
    /// (the DAP adapter's) is taken down.
    pub fn disconnect(&self) {
        self.shutdown_socket();
    }

    /// Shut the connection and join the reader. The server tears down
    /// session-owned breakpoints and subscriptions on disconnect.
    pub fn close(mut self) {
        self.shutdown_socket();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }

    fn shutdown_socket(&self) {
        let writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writer.shutdown(Shutdown::Both);
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.shutdown_socket();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// The reader thread: every reply goes to its waiter, every
/// notification to the event queue, and the end of the stream closes
/// the bridge for everyone waiting.
fn read_loop(mut reader: BufReader<TcpStream>, shared: Arc<Shared>) {
    let reason = loop {
        let line = match proto::read_msg_line(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => break "server closed the connection".to_string(),
            Err(e) => break format!("reading from the server: {e}"),
        };
        let msg: Value = match serde_json::from_str(&line) {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("copperline-mcp: ignoring a non-JSON server line: {e}");
                continue;
            }
        };
        let mut state = shared.lock();
        if msg.get("method").is_some() {
            if state.events.len() >= EVENT_QUEUE_CAPACITY {
                state.events.pop_front();
                state.dropped += 1;
            }
            state.events.push_back(msg);
        } else if let Some(id) = msg.get("id").and_then(Value::as_u64) {
            // A reply to a forgotten id (a wait that timed out) has no
            // taker and is dropped here rather than parked in the map.
            if let Some(slot) = state.pending.get_mut(&id) {
                *slot = Some(msg);
            }
        }
        drop(state);
        shared.cv.notify_all();
    };
    shared.lock().closed = Some(reason);
    shared.cv.notify_all();
}

/// Parse a `--control-info` file into (listen address, token).
pub fn read_info_file(path: &Path) -> Result<(String, String), String> {
    let body =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let info: Value = serde_json::from_str(body.trim())
        .map_err(|e| format!("parsing {}: {e}", path.display()))?;
    let listen = info
        .get("listen")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{} has no listen address", path.display()))?;
    let token = info
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{} has no token", path.display()))?;
    Ok((listen.to_string(), token.to_string()))
}

// ---------------------------------------------------------------------
// Launching an emulator

/// What to launch: the binary, extra arguments, and where.
#[derive(Debug, Clone, Default)]
pub struct LaunchSpec {
    /// The emulator binary; `None` resolves `COPPERLINE_BIN`, then a
    /// `copperline` next to the running executable, then the PATH.
    pub binary: Option<PathBuf>,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub timeout: Duration,
    /// An interactive window with the control server attached
    /// (`--control-gui`) instead of a headless server (`--control`).
    pub windowed: bool,
    /// Pass `--noaudio`. Headless servers want it (nothing to hear, and
    /// no audio device to open); a windowed debug session usually wants
    /// its sound.
    pub noaudio: bool,
}

/// An emulator this bridge started, with its log.
pub struct Launched {
    pub child: Child,
    pub command: Vec<String>,
    pub log_path: PathBuf,
}

impl Launched {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Wait up to `timeout` for the process to exit, then kill it. Returns
    /// whether it had to be killed.
    pub fn finish(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return false,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        true
    }

    /// The last lines of the process log, for an error report.
    pub fn log_tail(&self, lines: usize) -> String {
        let body = std::fs::read_to_string(&self.log_path).unwrap_or_default();
        let all: Vec<&str> = body.lines().collect();
        let start = all.len().saturating_sub(lines);
        all[start..].join("\n")
    }
}

/// Resolve the emulator binary: the explicit path, `COPPERLINE_BIN`, a
/// sibling of the running executable, or the bare name for the PATH.
pub fn resolve_binary(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    if let Some(env) = std::env::var_os("COPPERLINE_BIN") {
        if !env.is_empty() {
            return PathBuf::from(env);
        }
    }
    let name = if cfg!(windows) {
        "copperline.exe"
    } else {
        "copperline"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(name);
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    PathBuf::from(name)
}

/// Per-process launch counter, so launches issued in the same
/// millisecond (parallel tests, agents opening several sessions at once)
/// never share a temp-file name.
static LAUNCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// Pick the `--control-info` path and create the log file for one launch.
///
/// The emulator announces its token on stderr, so the log is readable by
/// the owner only from the moment it exists, like the info file, and is
/// never an existing file: a symlink planted under a predictable name in a
/// shared temp dir would carry the token away. A name that already exists
/// is simply skipped for the next one in the sequence.
fn create_launch_files() -> Result<(PathBuf, PathBuf, std::fs::File), String> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut last_err = None;
    for _ in 0..64 {
        let seq = LAUNCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let stamp = format!("copperline-mcp-{}-{millis}-{seq}", std::process::id());
        let info_path = std::env::temp_dir().join(format!("{stamp}.json"));
        let log_path = std::env::temp_dir().join(format!("{stamp}.log"));
        match super::owner_only_create().create_new(true).open(&log_path) {
            Ok(log) => {
                let _ = std::fs::remove_file(&info_path);
                return Ok((info_path, log_path, log));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                last_err = Some(format!("creating {}: {e}", log_path.display()));
            }
            Err(e) => return Err(format!("creating {}: {e}", log_path.display())),
        }
    }
    Err(last_err.unwrap_or_else(|| "creating the launch log: no free name".into()))
}

/// Start an emulator with a control server on an ephemeral loopback
/// port (headless, or windowed with `spec.windowed`), wait for its
/// endpoint, connect and authenticate.
pub fn launch(spec: &LaunchSpec) -> Result<(Bridge, Launched), String> {
    let binary = resolve_binary(spec.binary.as_deref());
    let (info_path, log_path, log) = create_launch_files()?;
    let log_err = log
        .try_clone()
        .map_err(|e| format!("cloning the log handle: {e}"))?;

    let mut command = Command::new(&binary);
    command
        .arg(if spec.windowed {
            "--control-gui"
        } else {
            "--control"
        })
        .arg(":0")
        .arg("--control-info")
        .arg(&info_path);
    if spec.noaudio {
        command.arg("--noaudio");
    }
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    let command_line: Vec<String> = std::iter::once(binary.display().to_string())
        .chain(command.get_args().map(|a| a.to_string_lossy().into_owned()))
        .collect();
    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            // Nothing ran, so nothing was logged: leave no empty file behind.
            let _ = std::fs::remove_file(&log_path);
            return Err(format!("starting {}: {e}", binary.display()));
        }
    };
    let mut launched = Launched {
        child,
        command: command_line,
        log_path,
    };

    let timeout = if spec.timeout.is_zero() {
        DEFAULT_LAUNCH_TIMEOUT
    } else {
        spec.timeout
    };
    let deadline = Instant::now() + timeout;
    let endpoint = loop {
        if let Ok(Some(status)) = launched.child.try_wait() {
            let tail = launched.log_tail(40);
            let _ = std::fs::remove_file(&info_path);
            return Err(format!(
                "the emulator exited with {status} before announcing its control endpoint \
                 (command: {}; log {}):\n{tail}",
                launched.command.join(" "),
                launched.log_path.display()
            ));
        }
        if info_path.is_file() {
            if let Ok(endpoint) = read_info_file(&info_path) {
                break endpoint;
            }
        }
        if Instant::now() >= deadline {
            launched.finish(Duration::ZERO);
            let tail = launched.log_tail(40);
            let _ = std::fs::remove_file(&info_path);
            return Err(format!(
                "the emulator did not announce its control endpoint within {} ms (command: \
                 {}; log {}):\n{tail}",
                timeout.as_millis(),
                launched.command.join(" "),
                launched.log_path.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = std::fs::remove_file(&info_path);
    match Bridge::connect(&endpoint.0, &endpoint.1) {
        Ok(bridge) => Ok((bridge, launched)),
        Err(e) => {
            launched.finish(Duration::ZERO);
            Err(e)
        }
    }
}

/// Scripted control servers for the bridge's and the MCP server's tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::io::{BufRead, Write};
    use std::net::TcpListener;

    /// A scripted server on `listener`: `respond` maps each request to
    /// the lines to send back (replies and notifications, in order, or
    /// nothing) until the client closes. Token `ok` authenticates; see
    /// [`hello_reply`].
    pub(crate) fn scripted_server(
        listener: TcpListener,
        respond: impl Fn(&Value) -> Vec<String> + Send + 'static,
    ) -> JoinHandle<()> {
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 0 {
                let req: Value = serde_json::from_str(line.trim()).unwrap();
                for out in respond(&req) {
                    writer.write_all(out.as_bytes()).unwrap();
                    writer.write_all(b"\n").unwrap();
                }
                writer.flush().unwrap();
                line.clear();
            }
        })
    }

    /// The `hello` reply for a scripted server, `None` for any other
    /// request.
    pub(crate) fn hello_reply(req: &Value) -> Option<String> {
        (req["method"] == "hello").then(|| {
            proto::ok_line(
                &req["id"],
                json!({"proto": 1, "emulator": "test", "authed": req["params"]["token"] == "ok"}),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{hello_reply, scripted_server};
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn connects_authenticates_and_routes_replies_and_events() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = scripted_server(listener, |req| {
            if let Some(hello) = hello_reply(req) {
                return vec![hello];
            }
            match req["method"].as_str() {
                Some("status") => vec![
                    proto::event_line("event.frame", json!({"frame": 1})),
                    proto::event_line("event.frame", json!({"frame": 2})),
                    proto::ok_line(&req["id"], json!({"frame": 2})),
                ],
                Some("bad") => vec![proto::err_line(
                    &req["id"],
                    &proto::CtlError::invalid_params("missing addr"),
                )],
                _ => vec![],
            }
        });
        let bridge = Bridge::connect(&addr.to_string(), "ok").unwrap();
        assert_eq!(bridge.hello()["emulator"], "test");
        assert_eq!(
            bridge.call("status", Value::Null).unwrap(),
            Reply::Ok(json!({"frame": 2}))
        );
        assert_eq!(
            bridge.call("bad", Value::Null).unwrap(),
            Reply::Err {
                code: proto::INVALID_PARAMS,
                message: "missing addr".into()
            }
        );
        let first = bridge.next_event(Duration::from_secs(1)).unwrap();
        assert_eq!(first["method"], "event.frame");
        assert_eq!(first["params"]["frame"], 1);
        assert_eq!(bridge.drain_events().len(), 1);
        assert_eq!(bridge.event_counts(), (0, 0));
        // A request nobody answers times out and stays outstanding.
        let id = bridge.send("silent", Value::Null).unwrap();
        assert_eq!(
            bridge.wait(id, Some(Duration::from_millis(20))).unwrap(),
            Reply::TimedOut
        );
        assert!(bridge.next_event(Duration::from_millis(10)).is_none());
        bridge.close();
        server.join().unwrap();
    }

    #[test]
    fn a_forgotten_request_s_late_reply_is_dropped() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // `slow` is never answered on its own: its reply goes out late,
        // ahead of the next request's, once the waiter has given up.
        let held = Mutex::new(None::<Value>);
        let server = scripted_server(listener, move |req| {
            if let Some(hello) = hello_reply(req) {
                return vec![hello];
            }
            let mut held = held.lock().unwrap();
            match req["method"].as_str() {
                Some("slow") => {
                    *held = Some(req["id"].clone());
                    vec![]
                }
                Some("status") => {
                    let mut lines = Vec::new();
                    if let Some(id) = held.take() {
                        lines.push(proto::ok_line(&id, json!({"late": true})));
                    }
                    lines.push(proto::ok_line(&req["id"], json!({"frame": 3})));
                    lines
                }
                _ => vec![],
            }
        });
        let bridge = Bridge::connect(&addr.to_string(), "ok").unwrap();
        for round in 0..3 {
            let id = bridge.send("slow", Value::Null).unwrap();
            assert_eq!(
                bridge.wait(id, Some(Duration::from_millis(10))).unwrap(),
                Reply::TimedOut
            );
            assert_eq!(bridge.outstanding(), 1, "round {round}");
            bridge.forget(id);
            assert_eq!(bridge.outstanding(), 0, "round {round}");
            assert_eq!(
                bridge.call("status", Value::Null).unwrap(),
                Reply::Ok(json!({"frame": 3}))
            );
            assert_eq!(
                bridge.outstanding(),
                0,
                "round {round}: the late reply must not be kept"
            );
        }
        // Forgetting an answered or unknown id is harmless.
        bridge.forget(9999);
        assert_eq!(bridge.outstanding(), 0);
        bridge.close();
        server.join().unwrap();
    }

    #[test]
    fn wrong_token_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = scripted_server(listener, |req| hello_reply(req).into_iter().collect());
        let Err(err) = Bridge::connect(&addr.to_string(), "wrong") else {
            panic!("a wrong token must not authenticate");
        };
        assert!(err.contains("auth failed"), "{err}");
        drop(server);
    }

    #[test]
    fn a_closed_connection_fails_waiters_and_bounds_the_queue() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = scripted_server(listener, |req| {
            if let Some(hello) = hello_reply(req) {
                return vec![hello];
            }
            // Flood, then drop the connection without replying.
            let mut lines: Vec<String> = (0..EVENT_QUEUE_CAPACITY + 5)
                .map(|n| proto::event_line("event.serial", json!({"n": n})))
                .collect();
            lines.push(String::new());
            lines
        });
        let bridge = Bridge::connect(&addr.to_string(), "ok").unwrap();
        let id = bridge.send("status", Value::Null).unwrap();
        // The scripted server returns from its loop only when we close,
        // so wait for the flood to land, then close our side to end it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while bridge.event_counts().1 < 5 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(bridge.event_counts(), (EVENT_QUEUE_CAPACITY, 5));
        bridge.shutdown_socket();
        let err = bridge.wait(id, None).unwrap_err();
        assert!(err.contains("session lost"), "{err}");
        assert!(bridge.closed().is_some());
        bridge.close();
        server.join().unwrap();
    }

    #[test]
    fn info_file_parses_and_reports_missing_fields() {
        let dir = std::env::temp_dir().join(format!("ccp-bridge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("info.json");
        std::fs::write(
            &path,
            "{\"listen\":\"127.0.0.1:1\",\"token\":\"t\",\"proto\":1}\n",
        )
        .unwrap();
        assert_eq!(
            read_info_file(&path).unwrap(),
            ("127.0.0.1:1".to_string(), "t".to_string())
        );
        std::fs::write(&path, "{\"listen\":\"127.0.0.1:1\"}").unwrap();
        assert!(read_info_file(&path).unwrap_err().contains("no token"));
        assert!(read_info_file(&dir.join("absent.json"))
            .unwrap_err()
            .contains("reading"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn launches_in_the_same_millisecond_get_distinct_private_files() {
        // The unit suite runs launch tests in parallel threads of one
        // process; the temp-file names carry a per-process sequence so
        // two launches in the same millisecond never race on one path.
        let (info_a, log_a, _file_a) = create_launch_files().unwrap();
        let (info_b, log_b, _file_b) = create_launch_files().unwrap();
        assert_ne!(log_a, log_b);
        assert_ne!(info_a, info_b);
        assert!(log_a.is_file() && log_b.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for log in [&log_a, &log_b] {
                let mode = std::fs::metadata(log).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{}", log.display());
            }
        }
        std::fs::remove_file(&log_a).ok();
        std::fs::remove_file(&log_b).ok();
    }

    #[test]
    fn launch_reports_a_binary_that_cannot_start() {
        let spec = LaunchSpec {
            binary: Some(PathBuf::from("/nonexistent/copperline-mcp-test-binary")),
            timeout: Duration::from_millis(500),
            ..Default::default()
        };
        let Err(err) = launch(&spec) else {
            panic!("a missing binary must not launch");
        };
        assert!(err.contains("starting"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn launch_reports_a_process_that_exits_without_announcing() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("ccp-launch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-copperline");
        std::fs::write(&script, "#!/bin/sh\necho boot failure >&2\nexit 3\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let spec = LaunchSpec {
            binary: Some(script),
            timeout: Duration::from_secs(5),
            ..Default::default()
        };
        let Err(err) = launch(&spec) else {
            panic!("a failing process must not launch");
        };
        assert!(err.contains("exited"), "{err}");
        assert!(err.contains("boot failure"), "{err}");
        assert!(err.contains("--control :0"), "{err}");
        // The log the emulator's stderr went to (where a real emulator
        // announces its token) is readable by the owner only.
        let log = err
            .split("; log ")
            .nth(1)
            .and_then(|rest| rest.split("):").next())
            .expect("the report names the log");
        let mode = std::fs::metadata(log).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{log}");
        std::fs::remove_file(log).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_binary_prefers_the_explicit_path() {
        assert_eq!(
            resolve_binary(Some(Path::new("/x/copperline"))),
            PathBuf::from("/x/copperline")
        );
        let resolved = resolve_binary(None);
        assert!(resolved.file_name().is_some());
    }
}
