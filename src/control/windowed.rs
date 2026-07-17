// SPDX-License-Identifier: GPL-3.0-or-later

//! Socket plumbing for the windowed control server (`--control-gui`):
//! an accept thread plus, per connection, a reader thread (framing and
//! auth, so unauthenticated requests never reach the frame loop) and a
//! writer thread. Parsed requests cross to the winit frame loop over an
//! mpsc channel and are drained at the top of `about_to_wait`; replies
//! travel back over a per-connection channel. This module holds no
//! winit types -- the frame loop passes a wake callback so a command
//! arriving while the machine is paused (`ControlFlow::Wait`) still
//! gets serviced promptly.
//!
//! One client at a time, like the GDB stub: extra connections receive
//! one JSON error line and are closed.

use super::exec::{self, Request};
use super::observe::OUTBOUND_NOTIFICATION_CAPACITY;
use super::proto::{self, AuthGate, CtlError, Gate};
use super::Config;
use anyhow::{Context, Result};
use serde_json::Value;
use std::io::BufReader;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

/// A message from the socket threads to the frame loop.
pub enum CtlMsg {
    /// A client authenticated; the frame loop resets session state and
    /// arms time travel.
    Connected,
    /// The client went away (EOF or error); the frame loop tears down
    /// session-owned breakpoints and drops any pending resume.
    Disconnected,
    /// `shutdown`: reply and exit the application.
    Shutdown { id: Value },
    /// A parsed, authenticated request.
    Request { id: Value, req: Request },
}

/// The frame loop's handle on the control server: the command receiver,
/// the current connection's reply channel, and the connection flag for
/// the status bar.
pub struct ControlHandle {
    listener: Option<TcpListener>,
    token: String,
    cmd_tx: Sender<CtlMsg>,
    cmd_rx: Receiver<CtlMsg>,
    connection: Arc<Mutex<Option<Arc<OutboundConnection>>>>,
    connected: Arc<AtomicBool>,
}

struct OutboundConnection {
    reply_tx: SyncSender<String>,
    disconnect_stream: Option<TcpStream>,
}

impl ControlHandle {
    /// Bind the listener, resolve the token, and announce the endpoint
    /// (stderr line + optional info file). Called from `main` before
    /// the window exists so scripts can attach as soon as it opens;
    /// threads are spawned later by [`ControlHandle::start`].
    pub fn bind(config: &Config) -> Result<Self> {
        let bind = crate::gdbstub::normalize_listen_addr(&config.listen)?;
        let listener =
            TcpListener::bind(&bind).with_context(|| format!("binding control server {bind}"))?;
        let local = listener.local_addr().context("resolving control address")?;
        let token = config.resolve_token();
        super::announce(&local, &token, config.info_file.as_ref())?;
        log::info!("control: listening on {local}");
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        Ok(Self {
            listener: Some(listener),
            token,
            cmd_tx,
            cmd_rx,
            connection: Arc::new(Mutex::new(None)),
            connected: Arc::new(AtomicBool::new(false)),
        })
    }

    /// A loopback handle with no sockets, for driving the frame-loop
    /// drain directly in tests: commands are pushed through the
    /// returned sender, replies arrive on the returned receiver.
    pub fn test_pair() -> (Self, Sender<CtlMsg>, Receiver<String>) {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(OUTBOUND_NOTIFICATION_CAPACITY);
        let connection = OutboundConnection {
            reply_tx,
            disconnect_stream: None,
        };
        let handle = Self {
            listener: None,
            token: String::new(),
            cmd_tx: cmd_tx.clone(),
            cmd_rx,
            connection: Arc::new(Mutex::new(Some(Arc::new(connection)))),
            connected: Arc::new(AtomicBool::new(true)),
        };
        (handle, cmd_tx, reply_rx)
    }

    /// Spawn the accept thread. `wake` is invoked after every enqueued
    /// message so the event loop leaves `ControlFlow::Wait`.
    pub fn start(&mut self, wake: Box<dyn Fn() + Send + Sync>) {
        let Some(listener) = self.listener.take() else {
            return; // test handle, or started twice
        };
        let token = self.token.clone();
        let cmd_tx = self.cmd_tx.clone();
        let connection_slot = Arc::clone(&self.connection);
        let connected = Arc::clone(&self.connected);
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::from(wake);
        std::thread::Builder::new()
            .name("control-accept".into())
            .spawn(move || {
                accept_loop(listener, token, cmd_tx, connection_slot, connected, wake);
            })
            .expect("spawning control accept thread");
    }

    /// Whether a client is currently attached (status-bar indicator).
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Drain one queued message, non-blocking.
    pub fn try_recv(&self) -> Option<CtlMsg> {
        self.cmd_rx.try_recv().ok()
    }

    /// Enqueue one required reply/notification without blocking the emulator
    /// frame loop. If the bounded queue is full, disconnect the slow client:
    /// silently dropping a JSON-RPC reply would leave its request unresolved.
    pub fn send(&self, line: String) {
        let Some(connection) = self.connection.lock().expect("connection lock").clone() else {
            return;
        };
        match connection.reply_tx.try_send(line) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.disconnect_current(&connection, "outbound reply queue is full");
            }
            Err(TrySendError::Disconnected(_)) => {
                self.disconnect_current(&connection, "control writer stopped");
            }
        }
    }

    /// Attempt to enqueue a streaming notification without blocking the
    /// emulator frame loop. A full queue means the client is not draining its
    /// stream quickly enough; the caller records the drop in session stats.
    pub fn try_send_event(&self, line: String) -> bool {
        let Some(connection) = self.connection.lock().expect("connection lock").clone() else {
            return false;
        };
        match connection.reply_tx.try_send(line) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => {
                self.disconnect_current(&connection, "control writer stopped");
                false
            }
        }
    }

    /// Tear down the socket side of the current connection. `shutdown`
    /// wakes the blocking reader so the accept thread can perform normal
    /// session cleanup and begin accepting a replacement client.
    fn disconnect_current(&self, connection: &Arc<OutboundConnection>, reason: &str) {
        let removed = {
            let mut active = self.connection.lock().expect("connection lock");
            if active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, connection))
            {
                active.take();
                true
            } else {
                false
            }
        };
        if !removed {
            return;
        }
        log::warn!("control: {reason}; detaching client");
        self.connected.store(false, Ordering::Relaxed);
        if let Some(stream) = &connection.disconnect_stream {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

fn accept_loop(
    listener: TcpListener,
    token: String,
    cmd_tx: Sender<CtlMsg>,
    connection_slot: Arc<Mutex<Option<Arc<OutboundConnection>>>>,
    connected: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    loop {
        let (stream, peer) = match listener.accept() {
            Ok(conn) => conn,
            Err(e) => {
                log::warn!("control: accept failed: {e}");
                return;
            }
        };
        if connected.load(Ordering::Relaxed) {
            // One client at a time; tell the extra one why.
            let mut refused = stream;
            let _ = proto::write_line(
                &mut refused,
                &proto::err_line(
                    &Value::Null,
                    &CtlError::new(proto::INVALID_STATE, "another control client is attached"),
                ),
            );
            continue;
        }
        log::info!("control: connection from {peer}");
        stream.set_nodelay(true).ok();

        // Writer thread: owns the write half, drains the reply channel.
        let (reply_tx, reply_rx) =
            std::sync::mpsc::sync_channel::<String>(OUTBOUND_NOTIFICATION_CAPACITY);
        let disconnect_stream = match stream.try_clone() {
            Ok(clone) => clone,
            Err(e) => {
                log::warn!("control: cloning disconnect stream failed: {e}");
                continue;
            }
        };
        let write_stream = match stream.try_clone() {
            Ok(clone) => clone,
            Err(e) => {
                log::warn!("control: cloning stream failed: {e}");
                continue;
            }
        };
        write_stream
            .set_write_timeout(Some(std::time::Duration::from_millis(250)))
            .ok();
        let writer = std::thread::Builder::new()
            .name("control-write".into())
            .spawn(move || {
                let mut stream = write_stream;
                for line in reply_rx {
                    if proto::write_line(&mut stream, &line).is_err() {
                        break;
                    }
                }
            })
            .expect("spawning control writer thread");

        let connection = Arc::new(OutboundConnection {
            reply_tx,
            disconnect_stream: Some(disconnect_stream),
        });
        *connection_slot.lock().expect("connection lock") = Some(Arc::clone(&connection));
        connected.store(true, Ordering::Relaxed);
        let _ = cmd_tx.send(CtlMsg::Connected);
        wake();

        // Read this connection to completion on the accept thread; the
        // next client is only accepted afterwards (one at a time).
        read_connection(stream, &token, &cmd_tx, &connection.reply_tx, &wake);

        let mut active = connection_slot.lock().expect("connection lock");
        if active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &connection))
        {
            active.take();
        }
        drop(active);
        connected.store(false, Ordering::Relaxed);
        let _ = cmd_tx.send(CtlMsg::Disconnected);
        wake();
        drop(connection); // writer thread drains and exits
        let _ = writer.join();
        log::info!("control: client detached; listening again");
    }
}

/// Read one connection until EOF: framing, auth, and parsing happen
/// here so the frame loop only ever sees valid, authenticated requests.
fn read_connection(
    stream: TcpStream,
    token: &str,
    cmd_tx: &Sender<CtlMsg>,
    reply_tx: &SyncSender<String>,
    wake: &Arc<dyn Fn() + Send + Sync>,
) {
    let mut reader = BufReader::new(stream);
    let mut gate = AuthGate::new(token.to_string());
    loop {
        let line = match proto::read_msg_line(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(e) => {
                log::warn!("control: read failed: {e}");
                return;
            }
        };
        let req = match proto::parse_request(&line) {
            Ok(req) => req,
            Err(reply) => {
                if reply_tx.send(reply).is_err() {
                    return;
                }
                continue;
            }
        };
        match gate.handle(&req) {
            Gate::Reply(reply) => {
                if reply_tx.send(reply).is_err() {
                    return;
                }
                continue;
            }
            Gate::ReplyAndClose(reply) => {
                let _ = reply_tx.send(reply);
                return;
            }
            Gate::Pass => {}
        }
        if req.method == "shutdown" {
            let _ = cmd_tx.send(CtlMsg::Shutdown { id: req.id });
            wake();
            continue;
        }
        match exec::parse_method(&req.method, &req.params) {
            Ok(parsed) => {
                let _ = cmd_tx.send(CtlMsg::Request {
                    id: req.id,
                    req: parsed,
                });
                wake();
            }
            Err(err) => {
                if reply_tx.send(proto::err_line(&req.id, &err)).is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_delivery_is_bounded_and_nonblocking() {
        let (handle, _cmd_tx, _reply_rx) = ControlHandle::test_pair();
        for index in 0..OUTBOUND_NOTIFICATION_CAPACITY {
            assert!(handle.try_send_event(index.to_string()));
        }
        assert!(!handle.try_send_event("overflow".to_string()));
        assert!(handle.connected());
    }

    #[test]
    fn required_reply_overflow_disconnects_without_blocking() {
        let (handle, _cmd_tx, reply_rx) = ControlHandle::test_pair();
        for index in 0..OUTBOUND_NOTIFICATION_CAPACITY {
            assert!(handle.try_send_event(index.to_string()));
        }

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            handle.send("required reply".to_string());
            done_tx.send(handle.connected()).unwrap();
        });
        let completed = done_rx.recv_timeout(std::time::Duration::from_secs(2));
        if completed.is_err() {
            drop(reply_rx);
            worker.join().unwrap();
            panic!("required reply blocked behind a full outbound queue");
        }
        assert!(!completed.unwrap());
        drop(reply_rx);
        worker.join().unwrap();
    }
}
