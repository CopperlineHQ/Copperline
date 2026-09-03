// SPDX-License-Identifier: GPL-3.0-or-later

//! The Debug Adapter Protocol server: `copperline-ctl --dap`.
//!
//! A DAP client (VS Code through the `tools/vscode-copperline` extension,
//! nvim-dap, any editor speaking the protocol) spawns the adapter on its
//! stdio, or connects to `--dap-listen`. The adapter is a control-protocol
//! client like the MCP mode: it launches an emulator with `--run PROG`
//! (windowed by default, so the Amiga's screen is live beside the IDE)
//! or attaches to one started with `--control-info`, and translates each
//! DAP request into control-protocol calls over one `Bridge`. Source
//! lines, symbols, variables and call frames come from the program's
//! own debug information (`crate::debuginfo`), relocated by the hunk
//! addresses the guest reports at the program's load.
//!
//! Threads: a reader for the client's messages, one thread waiting on
//! the outstanding resume verb's reply, one draining the bridge's
//! notifications, and a ticker; all feed one channel that the main loop
//! owns the session state on. Responses and events leave through one
//! locked writer. Nothing here is async.

pub mod proto;

mod breaks;
mod eval;
mod session;
mod vars;

use super::bridge::{Bridge, Reply};
use proto::{Outgoing, Request};
use serde_json::{json, Value};
use session::Session;
use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The main loop's inbox.
pub enum Msg {
    Request(Request),
    /// The client's stream ended.
    ClientClosed,
    /// The outstanding resume verb's stop reply. `generation` names the
    /// session whose bridge it came from: a restart replaces the session
    /// while the old bridge's threads may still have messages queued.
    Resume {
        generation: u64,
        id: u64,
        reply: Reply,
    },
    /// A control-protocol notification (`event.*`).
    Event {
        generation: u64,
        event: Value,
    },
    /// The bridge connection ended.
    Lost {
        generation: u64,
        why: String,
    },
    /// Periodic housekeeping.
    Tick,
}

/// How often the ticker fires; the windowed status poll runs on every
/// second tick.
const TICK: Duration = Duration::from_millis(500);

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// The event side handed to session code: numbered events written
/// straight to the client.
pub struct Emit<'a> {
    out: &'a mut Outgoing,
    writer: &'a SharedWriter,
}

impl Emit<'_> {
    pub fn event(&mut self, name: &str, body: Value) {
        let msg = self.out.event(name, body);
        write_locked(self.writer, &msg);
    }

    /// An `output` event; `category` is `console`, `stdout`, `stderr`
    /// or `important`.
    pub fn output(&mut self, category: &str, text: &str) {
        self.event("output", json!({"category": category, "output": text}));
    }

    /// A console line for the adapter's own notes.
    pub fn note(&mut self, text: &str) {
        self.output("console", &format!("{text}\n"));
    }

    pub fn stopped(&mut self, reason: &str, description: Option<&str>, hit: &[i64]) {
        let mut body = json!({
            "reason": reason,
            "threadId": session::THREAD_ID,
            "allThreadsStopped": true,
        });
        if let Some(text) = description {
            body["description"] = Value::from(text);
        }
        if !hit.is_empty() {
            body["hitBreakpointIds"] = json!(hit);
        }
        self.event("stopped", body);
    }

    pub fn continued(&mut self) {
        self.event(
            "continued",
            json!({"threadId": session::THREAD_ID, "allThreadsContinued": true}),
        );
    }

    pub fn terminated(&mut self) {
        self.event("terminated", Value::Null);
    }
}

/// The current session, when a bridge-thread message is from it.
fn session_for(session: &mut Option<Session>, generation: u64) -> Option<&mut Session> {
    session.as_mut().filter(|s| s.generation() == generation)
}

fn write_locked(writer: &SharedWriter, msg: &Value) {
    let mut w = writer.lock().unwrap_or_else(|e| e.into_inner());
    if let Err(e) = proto::write_message(&mut *w, msg) {
        eprintln!("copperline-dap: writing to the client: {e}");
    }
}

/// The adapter: one client, at most one debug session.
pub struct Adapter {
    out: Outgoing,
    writer: SharedWriter,
    tx: Sender<Msg>,
    session: Option<Session>,
    /// A bridge handed in on the command line (`--info`), used by the
    /// first `attach` (or `launch`, which then attaches instead).
    preattached: Option<Bridge>,
    /// The `launch` / `attach` arguments, replayed by `restart`.
    start_args: Option<(String, Value)>,
    lines_start_at_1: bool,
    columns_start_at_1: bool,
    /// The client asked to leave; the loop ends once the session is
    /// down.
    finished: bool,
}

impl Adapter {
    fn new(writer: SharedWriter, tx: Sender<Msg>, preattached: Option<Bridge>) -> Self {
        Self {
            out: Outgoing::default(),
            writer,
            tx,
            session: None,
            preattached,
            start_args: None,
            lines_start_at_1: true,
            columns_start_at_1: true,
            finished: false,
        }
    }

    fn emit(&mut self) -> Emit<'_> {
        Emit {
            out: &mut self.out,
            writer: &self.writer,
        }
    }

    fn reply(&mut self, req: &Request, body: Value) {
        let msg = self.out.response(req, body);
        write_locked(&self.writer, &msg);
    }

    fn fail(&mut self, req: &Request, message: impl Into<String>) {
        let msg = self.out.error(req, message);
        write_locked(&self.writer, &msg);
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Request(req) => self.request(req),
            Msg::ClientClosed => {
                self.close_session(true);
                self.finished = true;
            }
            Msg::Resume {
                generation,
                id,
                reply,
            } => {
                let Some(session) = session_for(&mut self.session, generation) else {
                    return;
                };
                let (out, writer) = (&mut self.out, &self.writer);
                let mut emit = Emit { out, writer };
                session.resume_replied(&mut emit, id, reply);
            }
            Msg::Event { generation, event } => {
                let Some(session) = session_for(&mut self.session, generation) else {
                    return;
                };
                let (out, writer) = (&mut self.out, &self.writer);
                let mut emit = Emit { out, writer };
                session.notification(&mut emit, &event);
            }
            Msg::Lost { generation, why } => {
                if session_for(&mut self.session, generation).is_some() {
                    self.emit().note(&format!("control connection lost: {why}"));
                    self.close_session(false);
                    self.emit().terminated();
                }
            }
            Msg::Tick => {
                let Some(session) = self.session.as_mut() else {
                    return;
                };
                let (out, writer) = (&mut self.out, &self.writer);
                let mut emit = Emit { out, writer };
                session.tick(&mut emit);
            }
        }
    }

    /// Drop the session: stop a launched emulator when `terminate`.
    fn close_session(&mut self, terminate: bool) {
        if let Some(session) = self.session.take() {
            session.close(terminate);
        }
    }

    fn request(&mut self, req: Request) {
        let command = req.command.clone();
        let result = match command.as_str() {
            "initialize" => self.initialize(&req),
            "launch" => self.launch(&req),
            "attach" => self.attach(&req),
            "restart" => self.restart(&req),
            "disconnect" => {
                let terminate = req.arguments["terminateDebuggee"]
                    .as_bool()
                    .unwrap_or_else(|| self.session.as_ref().is_some_and(|s| s.launched()));
                self.close_session(terminate);
                self.finished = true;
                Ok(Value::Null)
            }
            "terminate" => {
                self.close_session(true);
                self.emit().terminated();
                Ok(Value::Null)
            }
            "cancel" => Ok(Value::Null),
            _ => self.with_session(&req),
        };
        match result {
            Ok(body) => self.reply(&req, body),
            Err(message) => self.fail(&req, message),
        }
        // Events that belong after the response (a step's `stopped`).
        if let Some(session) = self.session.as_mut() {
            let (out, writer) = (&mut self.out, &self.writer);
            let mut emit = Emit { out, writer };
            session.flush_deferred(&mut emit);
        }
    }

    fn initialize(&mut self, req: &Request) -> Result<Value, String> {
        let args = &req.arguments;
        self.lines_start_at_1 = args["linesStartAt1"].as_bool().unwrap_or(true);
        self.columns_start_at_1 = args["columnsStartAt1"].as_bool().unwrap_or(true);
        Ok(capabilities())
    }

    fn launch(&mut self, req: &Request) -> Result<Value, String> {
        if self.session.is_some() {
            return Err("a debug session is already running".into());
        }
        let args = req.arguments.clone();
        let session = match self.preattached.take() {
            // `copperline-ctl --dap --info FILE`: the emulator is already
            // up; a launch becomes an attach to it.
            Some(bridge) => Session::attach(bridge, None, &args, self.tx.clone())?,
            None => Session::launch(&args, self.tx.clone())?,
        };
        self.start_args = Some(("launch".into(), args));
        self.install(session)
    }

    fn attach(&mut self, req: &Request) -> Result<Value, String> {
        if self.session.is_some() {
            return Err("a debug session is already running".into());
        }
        let args = req.arguments.clone();
        let session = match self.preattached.take() {
            Some(bridge) => Session::attach(bridge, None, &args, self.tx.clone())?,
            None => {
                let bridge = session::connect_from_args(&args)?;
                Session::attach(bridge, None, &args, self.tx.clone())?
            }
        };
        self.start_args = Some(("attach".into(), args));
        self.install(session)
    }

    fn install(&mut self, mut session: Session) -> Result<Value, String> {
        {
            let (out, writer) = (&mut self.out, &self.writer);
            let mut emit = Emit { out, writer };
            session.started(&mut emit);
        }
        // `initialized` goes out after the launch/attach response;
        // breakpoints and configurationDone follow it.
        session.defer_event("initialized", Value::Null);
        self.session = Some(session);
        Ok(Value::Null)
    }

    fn restart(&mut self, _req: &Request) -> Result<Value, String> {
        let Some((command, args)) = self.start_args.clone() else {
            return Err("nothing to restart".into());
        };
        self.close_session(true);
        let started = match command.as_str() {
            "launch" => Session::launch(&args, self.tx.clone()),
            _ => session::connect_from_args(&args)
                .and_then(|bridge| Session::attach(bridge, None, &args, self.tx.clone())),
        };
        match started {
            Ok(session) => self.install(session),
            Err(e) => {
                // The old session is gone; say so, or the client keeps
                // a dead session open.
                self.emit().terminated();
                Err(e)
            }
        }
    }

    fn with_session(&mut self, req: &Request) -> Result<Value, String> {
        let lines_at_1 = self.lines_start_at_1;
        let columns_at_1 = self.columns_start_at_1;
        let Some(session) = self.session.as_mut() else {
            return Err(format!(
                "{}: no debug session (launch or attach first)",
                req.command
            ));
        };
        let (out, writer) = (&mut self.out, &self.writer);
        let mut emit = Emit { out, writer };
        session.request(&mut emit, req, lines_at_1, columns_at_1)
    }
}

/// The adapter's `initialize` reply.
pub fn capabilities() -> Value {
    json!({
        "supportsConfigurationDoneRequest": true,
        "supportsFunctionBreakpoints": true,
        "supportsConditionalBreakpoints": true,
        "supportsHitConditionalBreakpoints": true,
        "supportsEvaluateForHovers": true,
        "supportsStepBack": true,
        "supportsSetVariable": true,
        "supportsGotoTargetsRequest": true,
        "supportsModulesRequest": true,
        "supportsRestartRequest": true,
        "supportsExceptionInfoRequest": true,
        "supportTerminateDebuggee": true,
        "supportsLoadedSourcesRequest": true,
        "supportsTerminateRequest": true,
        "supportsDataBreakpoints": true,
        "supportsDataBreakpointBytes": true,
        "supportsReadMemoryRequest": true,
        "supportsWriteMemoryRequest": true,
        "supportsDisassembleRequest": true,
        "supportsSteppingGranularity": true,
        "supportsInstructionBreakpoints": true,
        "supportsValueFormattingOptions": false,
        "supportsDelayedStackTraceLoading": false,
        "exceptionBreakpointFilters": breaks::EXCEPTION_FILTERS.iter().map(|f| json!({
            "filter": f.filter,
            "label": f.label,
            "description": f.description,
            "default": false,
        })).collect::<Vec<_>>(),
    })
}

/// Serve one client on `reader` / `writer` until it disconnects.
pub fn serve(
    reader: Box<dyn BufRead + Send>,
    writer: Box<dyn Write + Send>,
    preattached: Option<Bridge>,
) -> io::Result<()> {
    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = channel();
    let writer: SharedWriter = Arc::new(Mutex::new(writer));
    let reader_tx = tx.clone();
    std::thread::Builder::new()
        .name("copperline-dap-read".into())
        .spawn(move || read_loop(reader, reader_tx))?;
    let tick_tx = tx.clone();
    std::thread::Builder::new()
        .name("copperline-dap-tick".into())
        .spawn(move || {
            while tick_tx.send(Msg::Tick).is_ok() {
                std::thread::sleep(TICK);
            }
        })?;
    let mut adapter = Adapter::new(writer, tx, preattached);
    while !adapter.finished {
        match rx.recv() {
            Ok(msg) => adapter.handle(msg),
            Err(_) => break,
        }
    }
    adapter.close_session(true);
    Ok(())
}

fn read_loop(mut reader: Box<dyn BufRead + Send>, tx: Sender<Msg>) {
    loop {
        match proto::read_message(&mut reader) {
            Ok(Some(msg)) => {
                if let Some(req) = proto::as_request(&msg) {
                    if tx.send(Msg::Request(req)).is_err() {
                        return;
                    }
                }
            }
            Ok(None) => {
                let _ = tx.send(Msg::ClientClosed);
                return;
            }
            Err(e) => {
                eprintln!("copperline-dap: reading from the client: {e}");
                let _ = tx.send(Msg::ClientClosed);
                return;
            }
        }
    }
}

/// `copperline-ctl --dap`: serve stdin/stdout.
pub fn run_stdio(preattached: Option<Bridge>) -> io::Result<()> {
    serve(
        Box::new(BufReader::new(io::stdin())),
        Box::new(io::stdout()),
        preattached,
    )
}

/// `copperline-ctl --dap-listen ADDR`: accept DAP clients over TCP, one
/// at a time (the `debugServer` style of connection).
pub fn run_listen(addr: &str, mut preattached: Option<Bridge>) -> io::Result<()> {
    let bind = crate::debugger::normalize_listen_addr(addr)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    let listener = TcpListener::bind(&bind)?;
    eprintln!(
        "copperline-dap: listening on {}",
        listener
            .local_addr()
            .map_or(bind.clone(), |a| a.to_string())
    );
    loop {
        let (stream, peer) = listener.accept()?;
        eprintln!("copperline-dap: client {peer} connected");
        stream.set_nodelay(true).ok();
        let reader = BufReader::new(stream.try_clone()?);
        serve(Box::new(reader), Box::new(stream), preattached.take())?;
        eprintln!("copperline-dap: client {peer} left");
    }
}
