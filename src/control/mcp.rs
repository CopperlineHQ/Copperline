// SPDX-License-Identifier: GPL-3.0-or-later

//! An MCP (Model Context Protocol) server over stdio that exposes the
//! control protocol as tools, so an agent in Claude Code, Cursor, or any
//! other MCP client can drive a live machine without a REPL:
//! `copperline-ctl --mcp`.
//!
//! The server is a bridge. Every control-protocol method is a tool named
//! after it with dots replaced by underscores (see `catalogue`), called
//! over one authenticated connection (`bridge`); the session tools
//! (`session_launch`, `session_attach`, `session_status`,
//! `session_close`) manage that connection and the emulator process
//! behind it, and `events_next` / `events_drain` read the notifications
//! the bridge queues.
//!
//! Protocol subset: newline-delimited JSON-RPC 2.0 on stdin/stdout,
//! `initialize`, `notifications/initialized`, `ping`, `tools/list` and
//! `tools/call`, per MCP 2025-06-18 (earlier revisions are accepted on
//! `initialize`, the wire subset is identical). Stdout carries protocol
//! messages only; diagnostics go to stderr. MCP requests are handled one
//! at a time, so a blocking resume verb blocks the loop, which is the
//! documented behaviour: give it `wait_ms`.

use super::bridge::{self, Bridge, LaunchSpec, Launched, Reply};
use super::catalogue::{self, ToolDef};
use super::proto::{self, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR};
use super::toaster_link;
use serde_json::{json, Map, Value};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

/// The protocol revision this server speaks.
pub const LATEST_PROTOCOL_VERSION: &str = "2025-06-18";

/// Revisions accepted on `initialize`; the subset served here is the
/// same on each, so the client's choice is echoed.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// After a `wait_ms` expiry the bridge sends `pause` and waits this long
/// for the machine to land on a quantum boundary and reply.
const PAUSE_GRACE: Duration = Duration::from_secs(30);

/// Once the stop has landed, how long the bridge waits for the pause's
/// own reply (the server writes it right behind the stop) before
/// forgetting the id, so a later arrival is dropped rather than kept.
const PAUSE_REPLY_GRACE: Duration = Duration::from_millis(50);

/// Default `events_next` wait.
const DEFAULT_EVENT_WAIT: Duration = Duration::from_secs(1);

/// Longest `events_next` wait, so a typo cannot park the loop for hours.
const MAX_EVENT_WAIT: Duration = Duration::from_secs(600);

/// How long `session_close` and `shutdown` give the emulator to exit on
/// its own before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(3);

/// The workflow summary handed to the client on `initialize`.
pub const INSTRUCTIONS: &str =
    "Copperline is a cycle-driven Amiga emulator; these tools drive one \
live machine through its control protocol. Start with session_launch (spawns a headless emulator, \
paused at power-on, with the arguments you give: a config file, --model, --run PROG, --whdload \
PKG...) or session_attach (an emulator started with --control/--control-gui and --control-info). \
Then: continue, run_until, step, step_frame and friends run the machine and return a stop event; \
status, regs_get, mem_read, disasm, custom_dump, beam_get and display_get inspect it; break_add \
installs breakpoints, watches and traps; input_key, input_mouse, input_mouse_to and input_joy \
inject input; media_floppy_insert and media_cd_insert swap media; capture_screenshot returns the \
screen as an image. Tool names are the control-protocol method names with dots replaced by \
underscores (warp.get -> warp_get). Times are emulated seconds; addresses take integers or hex \
strings (\"0xDFF096\", \"$C00000\"). A resume with no stop condition blocks until something stops \
the machine, so give continue, run_until and step_frame a wait_ms: the bridge pauses the machine \
for you when it expires. Subscribe to notifications with events_subscribe and read them with \
events_next or events_drain. session_close disconnects and stops an emulator this server \
launched. Video Toaster (a machine with [video_slot] card = \"toaster\"): toaster_status reads \
the card (what is on Program and Preview, the fader, the inputs); toaster_capture returns the \
Program/Preview pictures as images; toaster_record records Program to the Output folder; \
toaster_input points an input at a host file; toaster_switcher runs a switcher command \
(M001-M004/MDV1/MDV2/MBKG pick Program, P001.../PBKG pick Preview, TAKE, AUTO, TBAR n 0-511, \
SLOW/MEDM/FAST, STAT MAIN...); toaster_project runs a project-editor function (CROUTONLOAD \
path, CROUTONPICK n then toaster_switcher DOEN to make it the transition, PROJECTLOAD/ \
PROJECTSAVE path, CROUTONSINPROJECT, CROUTONNAME...); toaster_program reaches CG, Paint, \
LightWave and Modeler through their own ports; flyer_import/flyer_export convert video files \
to and from Video Flyer clips; \
toaster_rexx runs any ARexx script inside the Amiga and returns what it says. Those three go \
through S:Copperline-Link.rexx in the Amiga and the ToasterFiles: volume ([video_slot] volume), \
and need the bridge on the same computer as the emulator.";

/// The server: at most one attached session, plus the counters the
/// session tools report.
pub struct McpServer {
    session: Option<Session>,
    screenshot_seq: u64,
    /// The next Toaster link job id.
    link_job: u64,
    /// A `--control-info` file to attach to when a tool first needs a
    /// session (`copperline-ctl --mcp --attach-on-use FILE`).
    attach_on_use: Option<PathBuf>,
}

struct Session {
    bridge: Bridge,
    launched: Option<Launched>,
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}

/// A tool call's outcome, rendered as the MCP `tools/call` result.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub content: Vec<Value>,
    pub is_error: bool,
}

impl ToolResult {
    fn text_block(value: &Value) -> Value {
        let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
        json!({"type": "text", "text": text})
    }

    /// A successful result carrying one JSON document.
    pub fn ok(value: Value) -> Self {
        Self {
            content: vec![Self::text_block(&value)],
            is_error: false,
        }
    }

    /// A bridge-side failure.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![Self::text_block(&json!({"error": message.into()}))],
            is_error: true,
        }
    }

    /// A control-protocol error reply, kept as a tool result so the
    /// model sees the code and message rather than a transport failure.
    pub fn ccp_error(code: i64, message: String) -> Self {
        Self {
            content: vec![Self::text_block(
                &json!({"error": {"code": code, "message": message}}),
            )],
            is_error: true,
        }
    }

    fn into_value(self) -> Value {
        json!({"content": self.content, "isError": self.is_error})
    }
}

type RpcResult = Result<Value, (i64, String)>;

impl McpServer {
    pub fn new() -> Self {
        Self {
            session: None,
            screenshot_seq: 0,
            link_job: toaster_link::first_id(),
            attach_on_use: None,
        }
    }

    /// Attach an already-connected bridge (the `--info` / `--connect`
    /// command-line forms).
    pub fn attach(&mut self, bridge: Bridge) {
        self.session = Some(Session {
            bridge,
            launched: None,
        });
    }

    pub fn attached(&self) -> bool {
        self.session.is_some()
    }

    /// Attach to the session in `info_file` when a tool first needs one
    /// and none is attached: the emulator may start after this server.
    pub fn set_attach_on_use(&mut self, info_file: Option<PathBuf>) {
        self.attach_on_use = info_file;
    }

    /// The `--attach-on-use` attach, when it applies.
    fn auto_attach(&mut self) -> Option<String> {
        let path = self.attach_on_use.clone()?;
        // An emulator that was closed and started again: its old
        // connection is gone, so attach to the new one.
        let lost = self
            .session
            .as_ref()
            .is_some_and(|s| s.launched.is_none() && s.bridge.closed().is_some());
        if lost {
            self.session = None;
        }
        if self.session.is_some() {
            return None;
        }
        match Bridge::connect_info_file(&path) {
            Ok(bridge) => {
                eprintln!("copperline-mcp: attached to {}", bridge.listen());
                self.session = Some(Session {
                    bridge,
                    launched: None,
                });
                None
            }
            Err(e) => Some(format!(
                "no session attached, and attaching to the emulator in {} failed ({e}); is it \
                 running with --control-gui and --control-info?",
                path.display()
            )),
        }
    }

    /// Serve newline-delimited JSON-RPC from `reader` to `writer` until
    /// EOF. Diagnostics go to stderr; nothing else is written to
    /// `writer`.
    pub fn serve<R: BufRead, W: Write>(&mut self, mut reader: R, mut writer: W) -> io::Result<()> {
        loop {
            let line = match proto::read_msg_line(&mut reader)? {
                Some(line) => line,
                None => return Ok(()),
            };
            if let Some(reply) = self.handle_message(&line) {
                proto::write_line(&mut writer, &reply)?;
            }
        }
    }

    /// Handle one client message; the reply line to send, if any
    /// (notifications get none).
    pub fn handle_message(&mut self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(msg) => msg,
            Err(e) => {
                return Some(error_line(
                    &Value::Null,
                    PARSE_ERROR,
                    format!("parse error: {e}"),
                ))
            }
        };
        let Some(obj) = msg.as_object() else {
            return Some(error_line(
                &Value::Null,
                INVALID_REQUEST,
                "request must be a JSON object (batches are not supported)",
            ));
        };
        if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(error_line(
                &Value::Null,
                INVALID_REQUEST,
                "jsonrpc must be \"2.0\"",
            ));
        }
        // MCP narrows JSON-RPC's id to a string or an integer: no null,
        // and nothing else. A reply to a malformed id carries null.
        let id = match obj.get("id") {
            None => None,
            Some(Value::String(s)) => Some(Value::String(s.clone())),
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => Some(Value::Number(n.clone())),
            Some(_) => {
                return Some(error_line(
                    &Value::Null,
                    INVALID_REQUEST,
                    "id must be a string or an integer",
                ))
            }
        };
        let method = match obj.get("method") {
            None => None,
            Some(Value::String(method)) => Some(method.as_str()),
            Some(_) => {
                return Some(error_line(
                    &id.unwrap_or(Value::Null),
                    INVALID_REQUEST,
                    "method must be a string",
                ))
            }
        };
        let Some(method) = method else {
            // A reply to a server-initiated request (this server sends
            // none) is ignored; anything else without a method is
            // malformed, id or no id.
            if obj.contains_key("result") || obj.contains_key("error") {
                return None;
            }
            return Some(error_line(
                &id.unwrap_or(Value::Null),
                INVALID_REQUEST,
                "request has no method",
            ));
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else {
            self.handle_notification(method, &params);
            return None;
        };
        Some(match self.handle_request(method, &params) {
            Ok(result) => proto::ok_line(&id, result),
            Err((code, message)) => error_line(&id, code, message),
        })
    }

    fn handle_notification(&mut self, method: &str, _params: &Value) {
        // notifications/initialized, notifications/cancelled, and
        // anything a newer client adds: nothing to do for a server
        // that answers each request before reading the next.
        let _ = method;
    }

    fn handle_request(&mut self, method: &str, params: &Value) -> RpcResult {
        match method {
            "initialize" => Ok(initialize_result(params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": list_tools()})),
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| (INVALID_PARAMS, "tools/call needs a tool name".to_string()))?;
                let arguments = match params.get("arguments") {
                    None | Some(Value::Null) => Value::Object(Map::new()),
                    Some(args @ Value::Object(_)) => args.clone(),
                    Some(_) => {
                        return Err((INVALID_PARAMS, "arguments must be an object".to_string()))
                    }
                };
                Ok(self.call_tool(name, arguments).into_value())
            }
            other => Err((METHOD_NOT_FOUND, format!("unknown method: {other}"))),
        }
    }

    /// Run one tool. Every failure is a tool result with `isError`, so
    /// the model can read it and adapt; protocol errors are for
    /// malformed requests only.
    pub fn call_tool(&mut self, name: &str, args: Value) -> ToolResult {
        if !name.starts_with("session_") && !name.starts_with("flyer_") {
            if let Some(error) = self.auto_attach() {
                return ToolResult::error(error);
            }
        }
        match name {
            "session_launch" => self.session_launch(&args),
            "session_attach" => self.session_attach(&args),
            "session_status" => self.session_status(),
            "session_close" => self.session_close(),
            "events_next" => self.events_next(&args),
            "events_drain" => self.events_drain(),
            "toaster_rexx" => self.toaster_rexx(&args),
            "toaster_switcher" => self.toaster_function(&args, "command", Some("Switcher")),
            "toaster_project" => self.toaster_function(&args, "function", None),
            "toaster_program" => self.toaster_program(&args),
            "flyer_import" | "flyer_export" | "flyer_list" => self.flyer_tool(name, &args),
            _ => match catalogue::find(name) {
                Some(def) => self.call_ccp(def, args),
                None => ToolResult::error(format!("unknown tool: {name}")),
            },
        }
    }

    // -----------------------------------------------------------------
    // Session tools

    fn session_launch(&mut self, args: &Value) -> ToolResult {
        if self.session.is_some() {
            return ToolResult::error(
                "a session is already attached; session_close it first (one session at a time)",
            );
        }
        let mut spec = LaunchSpec {
            binary: args
                .get("binary")
                .and_then(Value::as_str)
                .map(PathBuf::from),
            cwd: args.get("cwd").and_then(Value::as_str).map(PathBuf::from),
            timeout: Duration::from_millis(
                args.get("timeout_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(bridge::DEFAULT_LAUNCH_TIMEOUT.as_millis() as u64),
            ),
            args: Vec::new(),
            windowed: false,
            noaudio: true,
        };
        if args.get("factory").and_then(Value::as_bool) == Some(true) {
            spec.args.push("--factory".into());
        }
        for (key, flag) in [
            ("config", "--config"),
            ("model", "--model"),
            ("run", "--run"),
            ("whdload", "--whdload"),
        ] {
            match args.get(key) {
                None | Some(Value::Null) => {}
                Some(Value::String(value)) => {
                    spec.args.push(flag.into());
                    spec.args.push(value.clone());
                }
                Some(_) => return ToolResult::error(format!("{key} must be a string")),
            }
        }
        match args.get("args") {
            None | Some(Value::Null) => {}
            Some(Value::Array(extra)) => {
                for item in extra {
                    match item.as_str() {
                        Some(s) => spec.args.push(s.to_string()),
                        None => return ToolResult::error("args must be an array of strings"),
                    }
                }
            }
            Some(_) => return ToolResult::error("args must be an array of strings"),
        }
        let (bridge, launched) = match bridge::launch(&spec) {
            Ok(pair) => pair,
            Err(e) => return ToolResult::error(e),
        };
        eprintln!(
            "copperline-mcp: launched pid {} on {} (log {})",
            launched.pid(),
            bridge.listen(),
            launched.log_path.display()
        );
        let mut session = Session {
            bridge,
            launched: Some(launched),
        };
        let status = session.status_value();
        let launched = session.launched.as_ref().expect("just set");
        let result = json!({
            "launched": true,
            "pid": launched.pid(),
            "listen": session.bridge.listen(),
            "proto": session.bridge.hello()["proto"],
            "emulator": session.bridge.hello()["emulator"],
            "log": launched.log_path.display().to_string(),
            "command": launched.command,
            "status": status,
        });
        self.session = Some(session);
        ToolResult::ok(result)
    }

    fn session_attach(&mut self, args: &Value) -> ToolResult {
        if self.session.is_some() {
            return ToolResult::error(
                "a session is already attached; session_close it first (one session at a time)",
            );
        }
        let bridge = if let Some(path) = args.get("info_file").and_then(Value::as_str) {
            Bridge::connect_info_file(std::path::Path::new(path))
        } else {
            match (
                args.get("listen").and_then(Value::as_str),
                args.get("token").and_then(Value::as_str),
            ) {
                (Some(listen), Some(token)) => Bridge::connect(listen, token),
                _ => {
                    return ToolResult::error("session_attach needs info_file, or listen and token")
                }
            }
        };
        let bridge = match bridge {
            Ok(bridge) => bridge,
            Err(e) => return ToolResult::error(e),
        };
        eprintln!("copperline-mcp: attached to {}", bridge.listen());
        let mut session = Session {
            bridge,
            launched: None,
        };
        let status = session.status_value();
        let result = json!({
            "attached": true,
            "listen": session.bridge.listen(),
            "proto": session.bridge.hello()["proto"],
            "emulator": session.bridge.hello()["emulator"],
            "status": status,
        });
        self.session = Some(session);
        ToolResult::ok(result)
    }

    fn session_status(&mut self) -> ToolResult {
        let Some(session) = &self.session else {
            return ToolResult::ok(json!({"attached": false}));
        };
        let (queued, dropped) = session.bridge.event_counts();
        let mut result = json!({
            "attached": true,
            "listen": session.bridge.listen(),
            "proto": session.bridge.hello()["proto"],
            "emulator": session.bridge.hello()["emulator"],
            "connection": match session.bridge.closed() {
                None => "open".to_string(),
                Some(reason) => format!("lost: {reason}"),
            },
            "events_queued": queued,
            "events_dropped": dropped,
        });
        if let Some(launched) = &session.launched {
            result["launched_pid"] = json!(launched.pid());
            result["log"] = json!(launched.log_path.display().to_string());
        }
        ToolResult::ok(result)
    }

    fn session_close(&mut self) -> ToolResult {
        let Some(session) = self.session.take() else {
            return ToolResult::ok(json!({"closed": false, "note": "no session was attached"}));
        };
        let report = close_session(session, true);
        ToolResult::ok(report)
    }

    // -----------------------------------------------------------------
    // Event tools

    fn events_next(&mut self, args: &Value) -> ToolResult {
        let Some(session) = &self.session else {
            return ToolResult::error(NO_SESSION);
        };
        let timeout = match args.get("timeout_ms") {
            None | Some(Value::Null) => DEFAULT_EVENT_WAIT,
            Some(v) => match v.as_u64() {
                Some(ms) => Duration::from_millis(ms).min(MAX_EVENT_WAIT),
                None => return ToolResult::error("timeout_ms must be a non-negative integer"),
            },
        };
        let event = session.bridge.next_event(timeout);
        let (queued, dropped) = session.bridge.event_counts();
        let mut result = json!({
            "event": event.as_ref().map(event_value),
            "timed_out": event.is_none(),
            "queued": queued,
            "dropped": dropped,
        });
        if let Some(reason) = session.bridge.closed() {
            result["connection"] = json!(format!("lost: {reason}"));
        }
        ToolResult::ok(result)
    }

    fn events_drain(&mut self) -> ToolResult {
        let Some(session) = &self.session else {
            return ToolResult::error(NO_SESSION);
        };
        let events: Vec<Value> = session
            .bridge
            .drain_events()
            .iter()
            .map(event_value)
            .collect();
        let (_, dropped) = session.bridge.event_counts();
        ToolResult::ok(json!({
            "count": events.len(),
            "events": events,
            "dropped": dropped,
        }))
    }

    // -----------------------------------------------------------------
    // Control-protocol tools

    fn call_ccp(&mut self, def: &ToolDef, mut args: Value) -> ToolResult {
        let Some(session) = self.session.as_mut() else {
            return ToolResult::error(NO_SESSION);
        };
        if let Some(reason) = session.bridge.closed() {
            return ToolResult::error(format!(
                "the session connection was lost ({reason}); session_close it and launch or \
                 attach again"
            ));
        }
        if def.method == "capture.screenshot" {
            self.screenshot_seq += 1;
            return screenshot(session, &args, self.screenshot_seq);
        }
        if def.method == "toaster.capture" {
            self.screenshot_seq += 1;
            return toaster_capture(session, &args, self.screenshot_seq);
        }
        let wait = if def.wait_ms {
            match args.as_object_mut().and_then(|o| o.remove("wait_ms")) {
                None | Some(Value::Null) => None,
                Some(v) => match v.as_u64() {
                    Some(ms) if ms > 0 => Some(Duration::from_millis(ms)),
                    _ => return ToolResult::error("wait_ms must be a positive integer"),
                },
            }
        } else {
            None
        };
        let reply = match wait {
            None => session.bridge.call(def.method, args),
            Some(wait) => resume_with_wait(&mut session.bridge, def.method, args, wait),
        };
        match reply {
            Ok(Reply::Ok(mut value)) => {
                if def.method == "shutdown" {
                    // The emulator is on its way out; reap it and let the
                    // model know the session is gone.
                    let session = self.session.take().expect("checked above");
                    value["session"] = close_session(session, false);
                }
                ToolResult::ok(value)
            }
            Ok(Reply::Err { code, message }) => ToolResult::ccp_error(code, message),
            Ok(Reply::TimedOut) => ToolResult::error(format!(
                "the machine did not stop within {} ms of the bridge's pause; the run is \
                 still outstanding",
                PAUSE_GRACE.as_millis()
            )),
            Err(e) => ToolResult::error(e),
        }
    }

    // -----------------------------------------------------------------
    // Toaster link tools

    fn toaster_rexx(&mut self, args: &Value) -> ToolResult {
        let Some(script) = args.get("script").and_then(Value::as_str) else {
            return ToolResult::error("toaster_rexx needs a script");
        };
        let timeout = link_timeout(args);
        self.run_link_job(script, timeout)
    }

    /// `toaster_switcher` / `toaster_project`: one function call.
    fn toaster_function(&mut self, args: &Value, key: &str, function: Option<&str>) -> ToolResult {
        let Some(name) = args.get(key).and_then(Value::as_str) else {
            return ToolResult::error(format!("missing {key}"));
        };
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return ToolResult::error(format!("{key} must be a plain name, got {name:?}"));
        }
        let mut call_args: Vec<Value> = Vec::new();
        let function = match function {
            // Switcher(command, args...)
            Some(function) => {
                call_args.push(Value::String(name.to_ascii_uppercase()));
                function.to_string()
            }
            None => name.to_ascii_uppercase(),
        };
        match args.get("args") {
            None | Some(Value::Null) => {}
            Some(Value::Array(extra)) => call_args.extend(extra.iter().cloned()),
            Some(one) => call_args.push(one.clone()),
        }
        let body = toaster_link::call_body(&function, &call_args);
        let mut result = self.run_link_job(&body, link_timeout(args));
        if !result.is_error {
            if let Some(Value::String(text)) = result.content.first().and_then(|b| b.get("text")) {
                if let Ok(mut v) = serde_json::from_str::<Value>(text) {
                    v["call"] = json!(format!(
                        "{function}({})",
                        call_args
                            .iter()
                            .map(toaster_link::arg)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    result = ToolResult::ok(v);
                }
            }
        }
        result
    }

    /// `toaster_program`: a command for one of the Toaster suite's
    /// programs, through its own ARexx port.
    fn toaster_program(&mut self, args: &Value) -> ToolResult {
        let Some(program) = args.get("program").and_then(Value::as_str) else {
            return ToolResult::error("missing program");
        };
        let Some(app) = toaster_link::app(program) else {
            return ToolResult::error(format!(
                "unknown program {program:?}: {}",
                toaster_link::APP_NAMES
            ));
        };
        let Some(command) = args.get("command").and_then(Value::as_str) else {
            return ToolResult::error("missing command");
        };
        let call_args: Vec<Value> = match args.get("args") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a.clone(),
            Some(one) => vec![one.clone()],
        };
        let body = match toaster_link::app_body(app, command, &call_args) {
            Ok(body) => body,
            Err(e) => return ToolResult::error(e),
        };
        self.run_link_job(&body, link_timeout(args))
    }

    /// `flyer_import` / `flyer_export` / `flyer_list`: the clip converter,
    /// on this computer.
    fn flyer_tool(&mut self, name: &str, args: &Value) -> ToolResult {
        // The Flyer's drives: the running Toaster's folder, else the
        // default. Drive FA0 is `Flyer/FA0`, FB0 `Flyer/FB0` and so on.
        let media_root = self
            .session
            .as_mut()
            .and_then(|s| match s.bridge.call("toaster.status", json!({})) {
                Ok(Reply::Ok(v)) => v
                    .pointer("/state/media_root")
                    .and_then(Value::as_str)
                    .map(PathBuf::from),
                _ => None,
            })
            .unwrap_or_else(crate::paths::toaster_files_dir);
        let flyer_dir = media_root.join("Flyer");
        let fa0 = flyer_dir.join("FA0");
        let path_arg = |key: &str| args.get(key).and_then(Value::as_str).map(PathBuf::from);
        if name == "flyer_list" {
            let mut clips = Vec::new();
            let mut drives: Vec<PathBuf> = std::fs::read_dir(&flyer_dir)
                .map(|e| e.filter_map(Result::ok).map(|e| e.path()).collect())
                .unwrap_or_default();
            drives.retain(|d| d.is_dir());
            drives.sort();
            for drive in drives {
                let mut paths: Vec<PathBuf> = std::fs::read_dir(&drive)
                    .map(|e| e.filter_map(Result::ok).map(|e| e.path()).collect())
                    .unwrap_or_default();
                paths.sort();
                for p in paths.into_iter().filter(|p| p.is_file()) {
                    if let Ok(text) = crate::flyer_convert::describe(&p) {
                        clips.push(json!({"path": p.display().to_string(), "info": text}));
                    }
                }
            }
            return ToolResult::ok(
                json!({"folder": flyer_dir.display().to_string(), "clips": clips}),
            );
        }
        let Some(ffmpeg) = crate::flyer_convert::find_ffmpeg(Some(&media_root)) else {
            return ToolResult::error(
                "ffmpeg was not found (COPPERLINE_FFMPEG, the Toaster files folder, beside \
                 copperline-ctl, or the PATH)",
            );
        };
        if name == "flyer_import" {
            let Some(source) = path_arg("source") else {
                return ToolResult::error("missing source");
            };
            let clip = path_arg("clip").unwrap_or_else(|| {
                let stem: String = source
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Clip")
                    .chars()
                    .take(31)
                    .collect();
                crate::flyer_convert::unused_path(&fa0.join(format!("{stem}.clip")))
            });
            let mut options = crate::flyer_convert::ImportOptions::default();
            match args.get("grade").and_then(Value::as_str) {
                None | Some("std") => {}
                Some("hq5") => options.grade = flyerclip::Grade::Hq5,
                Some("hq6") => options.grade = flyerclip::Grade::Hq6,
                Some(other) => {
                    return ToolResult::error(format!("grade must be std|hq5|hq6, got {other}"))
                }
            }
            if args.get("audio").and_then(Value::as_bool) == Some(false) {
                options.audio = false;
            }
            options.max_seconds = args.get("seconds").and_then(Value::as_f64);
            match crate::flyer_convert::import(&ffmpeg, &source, &clip, &options, |_| {}) {
                Ok(r) => ToolResult::ok(json!({
                    "clip": clip.display().to_string(),
                    "fields": r.fields,
                    "seconds": r.seconds,
                    "audio_channels": r.audio_channels,
                    "bytes": r.bytes,
                    "degraded_fields": r.degraded_fields,
                    "truncated_fields": r.truncated_fields,
                })),
                Err(e) => ToolResult::error(format!("{e:#}")),
            }
        } else {
            let Some(clip) = path_arg("clip") else {
                return ToolResult::error("missing clip");
            };
            let output = path_arg("output")
                .unwrap_or_else(|| crate::flyer_convert::unused_path(&clip.with_extension("mp4")));
            match crate::flyer_convert::export(&ffmpeg, &clip, &output, |_| {}) {
                Ok(r) => ToolResult::ok(json!({
                    "output": output.display().to_string(),
                    "frames": r.frames,
                    "seconds": r.seconds,
                    "audio_channels": r.audio_channels,
                })),
                Err(e) => ToolResult::error(format!("{e:#}")),
            }
        }
    }

    /// Hand `body` to the Amiga through the link folder and wait for its
    /// output, running a paused machine forward as needed.
    fn run_link_job(&mut self, body: &str, timeout: Duration) -> ToolResult {
        let Some(session) = self.session.as_mut() else {
            return ToolResult::error(NO_SESSION);
        };
        let status = match session.bridge.call("toaster.status", json!({})) {
            Ok(Reply::Ok(status)) => status,
            Ok(Reply::Err { code, message }) => return ToolResult::ccp_error(code, message),
            Ok(Reply::TimedOut) => return ToolResult::error("toaster.status timed out"),
            Err(e) => return ToolResult::error(e),
        };
        let Some(dir) = toaster_link::folder_from_status(&status) else {
            return ToolResult::error(
                "the Toaster has no host folder ([video_slot] media), so there is no link",
            );
        };
        self.link_job += 1;
        let id = self.link_job;
        if let Err(e) = toaster_link::submit(&dir, id, &toaster_link::wrap(body)) {
            return ToolResult::error(format!(
                "writing the job into {}: {e} (the bridge must run on the emulator's computer)",
                dir.display()
            ));
        }
        let started = std::time::Instant::now();
        loop {
            if let Some(done) = toaster_link::collect(&dir, id) {
                return ToolResult::ok(json!({
                    "rc": done.rc,
                    "output": done.output,
                    "host_ms": started.elapsed().as_millis() as u64,
                }));
            }
            if started.elapsed() >= timeout {
                let note = if toaster_link::withdraw(&dir, id) {
                    "the Amiga never picked the job up: is S:Copperline-Link.rexx running \
                     (S:User-Startup starts it) and the card's folder mounted as ToasterFiles: \
                     ([video_slot] volume = \"ToasterFiles\")?"
                } else {
                    "the Amiga took the job but it has not finished: a requester, a long \
                     transition, or a program that is loaded but not open does not answer \
                     its port (the link carries on with other jobs)"
                };
                return ToolResult::error(format!(
                    "no answer within {} ms: {note}",
                    timeout.as_millis()
                ));
            }
            // A paused machine has to run for the guest to do the job.
            let paused = match session.bridge.call("status", Value::Null) {
                Ok(Reply::Ok(st)) => st.get("state").and_then(Value::as_str) == Some("paused"),
                Ok(Reply::Err { code, message }) => return ToolResult::ccp_error(code, message),
                Ok(Reply::TimedOut) => false,
                Err(e) => return ToolResult::error(e),
            };
            if paused {
                let now = match session.bridge.call("status", Value::Null) {
                    Ok(Reply::Ok(st)) => st.get("seconds").and_then(Value::as_f64).unwrap_or(0.0),
                    _ => 0.0,
                };
                match session
                    .bridge
                    .call("run_until", json!({"seconds": now + LINK_RUN_STEP}))
                {
                    Ok(Reply::Ok(_)) => {}
                    Ok(Reply::Err { code, message }) => {
                        return ToolResult::ccp_error(code, message)
                    }
                    Ok(Reply::TimedOut) => {}
                    Err(e) => return ToolResult::error(e),
                }
            } else {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    /// Close the session, stopping an emulator this server launched.
    /// Called on stdin EOF so no emulator outlives its agent.
    pub fn shutdown(&mut self) {
        if let Some(session) = self.session.take() {
            close_session(session, true);
        }
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Session {
    /// The machine's `status`, or the error, for the attach reports.
    fn status_value(&mut self) -> Value {
        match self.bridge.call("status", Value::Null) {
            Ok(Reply::Ok(status)) => status,
            Ok(Reply::Err { code, message }) => {
                json!({"error": {"code": code, "message": message}})
            }
            Ok(Reply::TimedOut) => Value::Null,
            Err(e) => json!({"error": e}),
        }
    }
}

const NO_SESSION: &str = "no session attached; call session_launch or session_attach first";

/// Send a resume verb and wait `wait` for its stop; on expiry, pause the
/// machine and return the stop that produces.
///
/// Every id this opens is answered or forgotten before it returns: the
/// server answers the pause with the same stop position right behind the
/// stop, so the bridge takes that reply when it is prompt and forgets the
/// id otherwise (the reader then drops a late arrival), and a resume
/// still unanswered after `PAUSE_GRACE` is forgotten the same way. The
/// pending map therefore cannot grow across timed resumes.
fn resume_with_wait(
    bridge: &mut Bridge,
    method: &str,
    params: Value,
    wait: Duration,
) -> Result<Reply, String> {
    let id = bridge.send(method, params)?;
    match bridge.wait(id, Some(wait))? {
        Reply::TimedOut => {
            let pause_id = match bridge.send("pause", Value::Null) {
                Ok(pause_id) => pause_id,
                Err(e) => {
                    bridge.forget(id);
                    return Err(e);
                }
            };
            let reply = bridge.wait(id, Some(PAUSE_GRACE));
            let _ = bridge.wait(pause_id, Some(PAUSE_REPLY_GRACE));
            bridge.forget(pause_id);
            let reply = reply?;
            if reply == Reply::TimedOut {
                bridge.forget(id);
            }
            Ok(match reply {
                Reply::Ok(mut stop) => {
                    stop["bridge"] = json!({"paused_after_ms": wait.as_millis() as u64});
                    Reply::Ok(stop)
                }
                other => other,
            })
        }
        reply => Ok(reply),
    }
}

/// Default wait for a Toaster link job, in host time.
const LINK_DEFAULT_WAIT: Duration = Duration::from_secs(60);

/// How far a paused machine is run between looks at the link folder.
const LINK_RUN_STEP: f64 = 0.5;

fn link_timeout(args: &Value) -> Duration {
    args.get("timeout_ms")
        .and_then(Value::as_u64)
        .map(Duration::from_millis)
        .unwrap_or(LINK_DEFAULT_WAIT)
        .min(MAX_EVENT_WAIT)
}

/// `toaster.capture` with the PNGs attached as image content blocks.
fn toaster_capture(session: &mut Session, args: &Value, seq: u64) -> ToolResult {
    let (param, temporary) = match args.get("path") {
        None | Some(Value::Null) => {
            let path = std::env::temp_dir().join(format!(
                "copperline-mcp-toaster-{}-{seq}.png",
                std::process::id()
            ));
            (json!(path.display().to_string()), true)
        }
        Some(Value::String(given)) => {
            let path = std::path::absolute(given).unwrap_or_else(|_| PathBuf::from(given));
            (json!(path.display().to_string()), false)
        }
        Some(other) => (other.clone(), false),
    };
    let mut params = args.as_object().cloned().unwrap_or_default();
    params.insert("path".to_string(), param);
    let mut result = match session
        .bridge
        .call("toaster.capture", Value::Object(params))
    {
        Ok(Reply::Ok(result)) => result,
        Ok(Reply::Err { code, message }) => return ToolResult::ccp_error(code, message),
        Ok(Reply::TimedOut) => unreachable!("waited without a timeout"),
        Err(e) => return ToolResult::error(e),
    };
    let mut images = Vec::new();
    if let Some(captures) = result.get_mut("captures").and_then(Value::as_array_mut) {
        for capture in captures {
            let Some(path) = capture
                .get("path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
            else {
                continue;
            };
            if let Ok(bytes) = std::fs::read(&path) {
                images.push(json!({
                    "type": "image",
                    "data": proto::encode_base64(&bytes),
                    "mimeType": "image/png",
                }));
            }
            if temporary {
                let _ = std::fs::remove_file(&path);
                capture["path"] = Value::Null;
            }
        }
    }
    let mut content = vec![ToolResult::text_block(&result)];
    content.extend(images);
    ToolResult {
        content,
        is_error: false,
    }
}

/// `capture.screenshot` with the PNG attached as an image content block.
///
/// The path the emulator gets is absolute: the emulator writes relative
/// to its own working directory and this process reads relative to ours,
/// which differ for an attached session or a `session_launch` with a
/// `cwd`, so a relative `path` is resolved here first. A `path` that is
/// not a string goes through as it is, so the method's own invalid-params
/// error comes back like any other tool's.
fn screenshot(session: &mut Session, args: &Value, seq: u64) -> ToolResult {
    let (param, path) = match args.get("path") {
        None | Some(Value::Null) => {
            let path = std::env::temp_dir()
                .join(format!("copperline-mcp-{}-{seq}.png", std::process::id()));
            (json!(path.display().to_string()), Some((path, true)))
        }
        Some(Value::String(given)) => {
            let path = std::path::absolute(given).unwrap_or_else(|_| PathBuf::from(given));
            (json!(path.display().to_string()), Some((path, false)))
        }
        Some(other) => (other.clone(), None),
    };
    let mut params = args.as_object().cloned().unwrap_or_default();
    params.insert("path".to_string(), param);
    let reply = session
        .bridge
        .call("capture.screenshot", Value::Object(params));
    let mut result = match reply {
        Ok(Reply::Ok(result)) => result,
        Ok(Reply::Err { code, message }) => return ToolResult::ccp_error(code, message),
        Ok(Reply::TimedOut) => unreachable!("waited without a timeout"),
        Err(e) => return ToolResult::error(e),
    };
    let Some((path, temporary)) = path else {
        return ToolResult::ok(result);
    };
    let png = std::fs::read(&path);
    if temporary {
        let _ = std::fs::remove_file(&path);
    }
    match png {
        Ok(bytes) => {
            if temporary {
                result["path"] = Value::Null;
                result["note"] =
                    json!("temporary file deleted after reading; the image is attached");
            }
            ToolResult {
                content: vec![
                    ToolResult::text_block(&result),
                    json!({
                        "type": "image",
                        "data": proto::encode_base64(&bytes),
                        "mimeType": "image/png",
                    }),
                ],
                is_error: false,
            }
        }
        Err(e) => ToolResult {
            content: vec![ToolResult::text_block(&json!({
                "error": format!(
                    "the screenshot was written but could not be read back from {}: {e}",
                    path.display()
                ),
                "result": result,
            }))],
            is_error: true,
        },
    }
}

/// Disconnect, and stop an emulator the server launched: `shutdown` is
/// offered first when `ask` (the connection is still usable), then the
/// process gets `EXIT_GRACE` to exit before it is killed.
fn close_session(session: Session, ask: bool) -> Value {
    let Session { bridge, launched } = session;
    let mut report = json!({"closed": true, "listen": bridge.listen()});
    match launched {
        None => {
            bridge.close();
        }
        Some(mut launched) => {
            if ask && bridge.closed().is_none() {
                let _ = bridge.call("shutdown", Value::Null);
            }
            bridge.close();
            let killed = launched.finish(EXIT_GRACE);
            eprintln!(
                "copperline-mcp: pid {} {}",
                launched.pid(),
                if killed { "killed" } else { "exited" }
            );
            report["terminated_pid"] = json!(launched.pid());
            report["killed"] = json!(killed);
            report["log"] = json!(launched.log_path.display().to_string());
        }
    }
    report
}

/// A queued notification as the model sees it: method and params.
fn event_value(msg: &Value) -> Value {
    json!({
        "method": msg.get("method").cloned().unwrap_or(Value::Null),
        "params": msg.get("params").cloned().unwrap_or(Value::Null),
    })
}

fn error_line(id: &Value, code: i64, message: impl Into<String>) -> String {
    proto::err_line(id, &proto::CtlError::new(code, message))
}

fn initialize_result(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = match requested {
        Some(v) if SUPPORTED_PROTOCOL_VERSIONS.contains(&v) => v,
        _ => LATEST_PROTOCOL_VERSION,
    };
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": "copperline", "version": env!("CARGO_PKG_VERSION")},
        "instructions": INSTRUCTIONS,
    })
}

/// The bridge-owned tools, listed ahead of the protocol catalogue.
pub fn bridge_tools() -> Vec<Value> {
    let object = |props: Value, required: Vec<&str>| {
        let mut schema = json!({"type": "object", "properties": props});
        if !required.is_empty() {
            schema["required"] = json!(required);
        }
        schema
    };
    vec![
        json!({
            "name": "session_launch",
            "description": "Start a headless Copperline emulator (paused at power-on, no audio, \
                            unpaced) with a control server on an ephemeral loopback port, \
                            connect to it, and make it the current session. Pass a `config` \
                            TOML path, a `model` (A500, A600, A1200, A4000, CD32, CDTV...), a \
                            `run` executable or `whdload` package, `factory: true` to ignore any \
                            saved default configuration, and any other copperline command-line \
                            flags verbatim in `args` (\"--chipset\", \"AGA\", \"--fast\", \"8M\", \
                            \"--insert-disk-after\", \"0\", \"df0\", \"game.adf\"...). The binary \
                            is `binary`, else $COPPERLINE_BIN, else the copperline next to \
                            copperline-ctl. Returns the pid, the listen address, the log file \
                            the emulator's output goes to, and the initial status. One session \
                            at a time: session_close first to launch another.",
            "inputSchema": object(json!({
                "config": {"type": "string", "description": "Configuration TOML path (--config)"},
                "model": {"type": "string", "description": "Machine model (--model)"},
                "run": {"type": "string", "description": "Amiga executable to boot straight into (--run)"},
                "whdload": {"type": "string", "description": "WHDLoad package or slave directory (--whdload)"},
                "factory": {"type": "boolean", "description": "Ignore the saved default configuration (--factory)"},
                "args": {"type": "array", "items": {"type": "string"}, "description": "Further copperline flags, verbatim"},
                "binary": {"type": "string", "description": "Path of the copperline binary (optional)"},
                "cwd": {"type": "string", "description": "Working directory for the emulator (optional)"},
                "timeout_ms": {"type": "integer", "minimum": 1, "description": "How long to wait for the control endpoint (default 30000)"}
            }), vec![]),
        }),
        json!({
            "name": "session_attach",
            "description": "Attach to a running emulator's control server: either `info_file` \
                            (the JSON file `--control-info` wrote) or `listen` and `token` (as \
                            printed on the emulator's stderr). Works with both a headless \
                            `--control` server and a windowed `--control-gui` session. Returns \
                            the protocol version, emulator version and initial status.",
            "inputSchema": object(json!({
                "info_file": {"type": "string", "description": "Path of the --control-info file"},
                "listen": {"type": "string", "description": "Control server address, host:port"},
                "token": {"type": "string", "description": "Session token"}
            }), vec![]),
        }),
        json!({
            "name": "session_status",
            "description": "Report the bridge's own state: whether a session is attached, its \
                            address, the pid and log of an emulator this server launched, \
                            whether the connection is still open, and the event queue's \
                            depth and drop count. Does not touch the machine; use status for \
                            the machine's state.",
            "inputSchema": object(json!({}), vec![]),
        }),
        json!({
            "name": "session_close",
            "description": "Disconnect from the current session (the server drops this \
                            session's breakpoints and subscriptions) and, if session_launch \
                            started the emulator, shut it down (killed after 3 s if it does \
                            not exit). Safe with no session attached.",
            "inputSchema": object(json!({}), vec![]),
        }),
        json!({
            "name": "events_next",
            "description": "Wait up to `timeout_ms` (default 1000, at most 600000) for the \
                            next queued notification from an events_subscribe subscription \
                            (or the unsolicited event.warp) and return it as {method, params}, \
                            or `timed_out: true`. Also reports the queue depth and how many \
                            events the bounded queue (1024) has dropped.",
            "inputSchema": object(json!({
                "timeout_ms": {"type": "integer", "minimum": 0, "maximum": 600000, "description": "Milliseconds to wait for an event (default 1000)"}
            }), vec![]),
        }),
        json!({
            "name": "events_drain",
            "description": "Return every queued notification, oldest first, and empty the \
                            queue; `dropped` counts events the bounded queue lost since the \
                            session started.",
            "inputSchema": object(json!({}), vec![]),
        }),
        json!({
            "name": "toaster_switcher",
            "description": "Run one Video Toaster switcher command inside the Amiga, through \
                            the switcher's ARexx port, and return what it answers (OK, or the \
                            value asked for). `command` is the four-letter code and `args` its \
                            arguments: M001-M004, MDV1, MDV2, MBKG put a source on Program; \
                            P001-P004, PDV1, PDV2, PBKG on Preview; TAKE cuts; AUTO runs the \
                            selected transition; TBAR [0-511] moves the T-bar; SLOW, MEDM, FAST \
                            set its speed; DOEN loads the effect picked in the project grid; BACK [index] picks the matte colour; FMLD [path] loads \
                            a framestore into the bank on Preview; FMSV [path] saves one; STAT \
                            [MAIN|PREV|TBAR|FXNM|...] reads state; LDCG/LDTP load CG and \
                            ToasterPaint. Needs S:Copperline-Link.rexx running in the Amiga.",
            "inputSchema": object(json!({
                "command": {"type": "string", "description": "Switcher command code (TAKE, M001, TBAR...)"},
                "args": {"type": "array", "description": "Arguments: numbers stay numbers, the rest are passed as strings"},
                "timeout_ms": {"type": "integer", "minimum": 1, "description": "Host milliseconds to wait (default 60000)"}
            }), vec!["command"]),
        }),
        json!({
            "name": "toaster_project",
            "description": "Call one of the Toaster project editor's ARexx functions \
                            (PROJECT_REXX_PORT) inside the Amiga and return its result: \
                            CROUTONLOAD [path] adds an effect or framestore to the project grid \
                            (paths like \"Toaster:Effects/4000FX/Wipes/Square B-R\"); \
                            CROUTONPICK [n] selects one (counting from 0; toaster_switcher DOEN \
                            then loads it as the transition TAKE/AUTO/TBAR use); \
                            CROUTONSINPROJECT, CROUTONNAME [row, col], CROUTONTYPE; \
                            CROUTONDELETE; PROJECTLOAD and PROJECTSAVE [path]; PROJECTPLAY; \
                            REQ_ERROR [text]. Needs S:Copperline-Link.rexx running in the Amiga.",
            "inputSchema": object(json!({
                "function": {"type": "string", "description": "Function name (CROUTONLOAD, PROJECTSAVE...)"},
                "args": {"type": "array", "description": "Arguments: numbers stay numbers, the rest are passed as strings"},
                "timeout_ms": {"type": "integer", "minimum": 1, "description": "Host milliseconds to wait (default 60000)"}
            }), vec!["function"]),
        }),
        json!({
            "name": "toaster_program",
            "description": "Send a command to one of the Video Toaster suite's programs through \
                            its own ARexx port, inside the Amiga, and return its answer. \
                            `program`: switcher (ToasterARexx.port: TAKE, AUTO, M001...), \
                            project (the project editor and the Flyer's functions: \
                            CROUTONLOAD, PROJECTSAVE, FLYERDRIVE, RECORDCLIP...), cg (ToasterCG, \
                            CG_AREXX: GET_PAGE, PICKPAGE, LOADFONT, ...), paint (ToasterPaint, \
                            the DigiPaint command host: Load, Save, ...), lightwave (LightWave \
                            3D, LightWaveARexx.port: LoadScene, Render...), modeler \
                            (LWModelerARexx.port). `command` open loads the program (CG, \
                            Paint and LightWave through the switcher; Paint is also entered); \
                            otherwise it is the program's command or function name, with \
                            `args`. The program must be on screen for its port to answer: CG \
                            and LightWave are entered from the Toaster's Programs menu, and a \
                            call to a program that is only loaded waits until the timeout. The \
                            commands each program takes are in Toaster:Programs/Documentation/ \
                            Arexx (CGARexx.Doc, TPaintARexx.Doc, LightWaveARexx.Doc, \
                            ModelerARexx.Doc, SwitcherARexx.Doc, EditorARexx.Doc); \
                            toaster_rexx can read them.",
            "inputSchema": object(json!({
                "program": {"type": "string", "enum": ["switcher", "project", "cg", "paint", "lightwave", "modeler"], "description": "Which program"},
                "command": {"type": "string", "description": "Command or function name, or open"},
                "args": {"type": "array", "description": "Arguments: numbers stay numbers, the rest are passed as strings"},
                "timeout_ms": {"type": "integer", "minimum": 1, "description": "Host milliseconds to wait (default 60000)"}
            }), vec!["program", "command"]),
        }),
        json!({
            "name": "flyer_import",
            "description": "Convert a video file (MP4, MOV, AVI... anything ffmpeg reads) into a \
                            NewTek Video Flyer clip: VTASC-coded NTSC composite fields plus \
                            stereo 16-bit audio, on the Flyer's drive FA0 (the Flyer/FA0 \
                            folder of the Toaster files folder) unless `clip` names the file (a default name never replaces a file). `grade` std (default), hq5 or hq6 \
                            sets the per-field size budget; `audio` false drops the sound; \
                            `seconds` stops early. Runs on this computer (needs ffmpeg), no \
                            emulator needed.",
            "inputSchema": object(json!({
                "source": {"type": "string", "description": "Host path of the video file"},
                "clip": {"type": "string", "description": "Host path of the clip to write (optional)"},
                "grade": {"type": "string", "enum": ["std", "hq5", "hq6"], "description": "Video grade (default std)"},
                "audio": {"type": "boolean", "description": "Keep the sound (default true)"},
                "seconds": {"type": "number", "description": "Convert at most this much"}
            }), vec!["source"]),
        }),
        json!({
            "name": "flyer_export",
            "description": "Convert a NewTek Video Flyer clip into a video file (MP4 with H.264 \
                            and AAC by default; the extension of `output` picks the format), \
                            640 x 480 at 29.97 frames a second. Without `output` the file goes \
                            beside the clip, under a name not yet in use. Runs on this computer (needs ffmpeg).",
            "inputSchema": object(json!({
                "clip": {"type": "string", "description": "Host path of the Flyer clip"},
                "output": {"type": "string", "description": "Host path of the video to write (optional)"}
            }), vec!["clip"]),
        }),
        json!({
            "name": "flyer_list",
            "description": "List the clips on the Flyer's drives (Flyer/FA0, Flyer/FB0... in the \
                            Toaster files folder) with their length, audio and grade.",
            "inputSchema": object(json!({}), vec![]),
        }),
        json!({
            "name": "toaster_rexx",
            "description": "Run an ARexx script inside the emulated Amiga and return its return \
                            code and everything it printed with `say`. The Toaster's function \
                            hosts (ToasterARexx.port: Switcher(...); PROJECT_REXX_PORT: \
                            CROUTONLOAD(...) and friends) and rexxsupport.library are already \
                            added, OPTIONS RESULTS is on, and a syntax error is reported rather \
                            than hanging. `address command '...'` runs AmigaDOS commands. A \
                            paused machine is run until the script finishes. Needs \
                            S:Copperline-Link.rexx running in the Amiga and the bridge on the \
                            emulator's computer.",
            "inputSchema": object(json!({
                "script": {"type": "string", "description": "ARexx statements, e.g. \"say Switcher('STAT', 'MAIN')\""},
                "timeout_ms": {"type": "integer", "minimum": 1, "description": "Host milliseconds to wait (default 60000)"}
            }), vec!["script"]),
        }),
    ]
}

/// The `tools/list` payload: the session and event tools, then every
/// control-protocol method.
pub fn list_tools() -> Vec<Value> {
    let mut tools = bridge_tools();
    tools.extend(catalogue::catalogue().iter().map(|def| {
        json!({
            "name": def.name,
            "description": def.description,
            "inputSchema": def.schema,
        })
    }));
    tools
}

/// Serve stdin/stdout until EOF, then close the session (stopping an
/// emulator this server launched).
pub fn run_stdio(attach: Option<Bridge>) -> io::Result<()> {
    run_stdio_with(attach, None)
}

/// [`run_stdio`], attaching to the session in `attach_on_use` when a tool
/// first needs one.
pub fn run_stdio_with(attach: Option<Bridge>, attach_on_use: Option<PathBuf>) -> io::Result<()> {
    let mut server = McpServer::new();
    server.set_attach_on_use(attach_on_use);
    if let Some(bridge) = attach {
        server.attach(bridge);
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    let result = server.serve(stdin.lock(), stdout.lock());
    server.shutdown();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::bridge::test_support::{hello_reply, scripted_server};
    use crate::control::headless::spawn_test_session;
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::thread::JoinHandle;

    fn request(id: u64, method: &str, params: Value) -> String {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
    }

    fn call(server: &mut McpServer, id: u64, tool: &str, args: Value) -> Value {
        let line = request(id, "tools/call", json!({"name": tool, "arguments": args}));
        let reply = server.handle_message(&line).expect("requests get replies");
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["id"], id);
        assert!(reply.get("error").is_none(), "protocol error: {reply}");
        reply["result"].clone()
    }

    /// The JSON document in a tool result's first text block.
    fn text_json(result: &Value) -> Value {
        let text = result["content"][0]["text"].as_str().expect("text block");
        serde_json::from_str(text).expect("text block is JSON")
    }

    /// A server attached to a fresh headless session on a background
    /// thread; the handle joins once the session is closed.
    fn attached() -> (McpServer, JoinHandle<()>) {
        let (addr, token, handle) = spawn_test_session();
        let mut server = McpServer::new();
        let result = call(
            &mut server,
            1,
            "session_attach",
            json!({"listen": addr.to_string(), "token": token}),
        );
        assert_eq!(result["isError"], false, "{result}");
        let doc = text_json(&result);
        assert_eq!(doc["attached"], true);
        assert_eq!(doc["status"]["state"], "paused");
        (server, handle)
    }

    fn close(mut server: McpServer, handle: JoinHandle<()>) {
        let result = call(&mut server, 999, "session_close", json!({}));
        assert_eq!(text_json(&result)["closed"], true);
        assert!(!server.attached());
        handle.join().expect("session thread");
    }

    #[test]
    fn initialize_echoes_a_supported_version_and_describes_the_server() {
        let mut server = McpServer::new();
        let reply = server
            .handle_message(&request(
                1,
                "initialize",
                json!({"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
            ))
            .unwrap();
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["id"], 1);
        let result = &reply["result"];
        assert_eq!(result["protocolVersion"], "2025-03-26");
        assert_eq!(result["capabilities"]["tools"]["listChanged"], false);
        assert_eq!(result["serverInfo"]["name"], "copperline");
        assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
        assert!(result["instructions"]
            .as_str()
            .unwrap()
            .contains("session_launch"));

        // An unknown revision gets the latest one this server speaks.
        let reply = server
            .handle_message(&request(
                2,
                "initialize",
                json!({"protocolVersion": "1999-01-01"}),
            ))
            .unwrap();
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["result"]["protocolVersion"], LATEST_PROTOCOL_VERSION);

        // The initialized notification and ping.
        assert!(server
            .handle_message(
                &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string()
            )
            .is_none());
        let reply = server
            .handle_message(&request(3, "ping", Value::Null))
            .unwrap();
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["result"], json!({}));
    }

    #[test]
    fn malformed_and_unknown_requests_get_json_rpc_errors() {
        let mut server = McpServer::new();
        let reply: Value =
            serde_json::from_str(&server.handle_message("{not json").unwrap()).unwrap();
        assert_eq!(reply["error"]["code"], PARSE_ERROR);
        assert_eq!(reply["id"], Value::Null);

        let reply: Value = serde_json::from_str(&server.handle_message("[1,2]").unwrap()).unwrap();
        assert_eq!(reply["error"]["code"], INVALID_REQUEST);

        let reply: Value = serde_json::from_str(
            &server
                .handle_message("{\"id\": 4, \"jsonrpc\": \"2.0\"}")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(reply["error"]["code"], INVALID_REQUEST);
        assert_eq!(reply["id"], 4);

        let reply: Value = serde_json::from_str(
            &server
                .handle_message(&request(5, "resources/list", Value::Null))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(reply["error"]["code"], METHOD_NOT_FOUND);

        let reply: Value = serde_json::from_str(
            &server
                .handle_message(&request(6, "tools/call", json!({"arguments": {}})))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(reply["error"]["code"], INVALID_PARAMS);

        // A stray response is ignored, an unknown notification too.
        assert!(server
            .handle_message("{\"jsonrpc\": \"2.0\", \"id\": 9, \"result\": {}}")
            .is_none());
        assert!(server
            .handle_message("{\"jsonrpc\": \"2.0\", \"method\": \"notifications/cancelled\"}")
            .is_none());
    }

    #[test]
    fn envelopes_must_be_json_rpc_2_with_a_string_or_integer_id() {
        let mut server = McpServer::new();
        let invalid = |server: &mut McpServer, line: &str| -> Value {
            let reply: Value = serde_json::from_str(
                &server
                    .handle_message(line)
                    .unwrap_or_else(|| panic!("{line} must be answered")),
            )
            .unwrap();
            assert_eq!(reply["error"]["code"], INVALID_REQUEST, "{line}: {reply}");
            reply
        };
        // The version is required, and must be 2.0.
        let reply = invalid(&mut server, "{\"id\": 1, \"method\": \"ping\"}");
        assert_eq!(reply["id"], Value::Null);
        invalid(
            &mut server,
            "{\"jsonrpc\": \"1.0\", \"id\": 1, \"method\": \"ping\"}",
        );
        invalid(
            &mut server,
            "{\"jsonrpc\": 2, \"id\": 1, \"method\": \"ping\"}",
        );
        // Ids: null, booleans, floats, objects and arrays are not ids,
        // and the error reply for one carries null.
        for id in ["null", "true", "1.5", "{}", "[1]"] {
            let reply = invalid(
                &mut server,
                &format!("{{\"jsonrpc\": \"2.0\", \"id\": {id}, \"method\": \"ping\"}}"),
            );
            assert_eq!(reply["id"], Value::Null, "id {id}");
        }
        // No method and no id is malformed, not a notification.
        let reply = invalid(&mut server, "{\"jsonrpc\": \"2.0\"}");
        assert_eq!(reply["id"], Value::Null);
        let reply = invalid(&mut server, "{\"jsonrpc\": \"2.0\", \"params\": {}}");
        assert_eq!(reply["id"], Value::Null);
        // A method that is not a string is malformed too, with the id.
        let reply = invalid(
            &mut server,
            "{\"jsonrpc\": \"2.0\", \"id\": 7, \"method\": 3}",
        );
        assert_eq!(reply["id"], 7);

        // String and integer ids are answered as themselves.
        for (id, expect) in [
            ("\"abc\"", json!("abc")),
            ("0", json!(0)),
            ("-3", json!(-3)),
        ] {
            let reply: Value = serde_json::from_str(
                &server
                    .handle_message(&format!(
                        "{{\"jsonrpc\": \"2.0\", \"id\": {id}, \"method\": \"ping\"}}"
                    ))
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(reply["id"], expect);
            assert_eq!(reply["result"], json!({}));
        }
        // A valid notification with an unknown method is still ignored,
        // and so is a stray response, whatever its id.
        assert!(server
            .handle_message(
                "{\"jsonrpc\": \"2.0\", \"method\": \"notifications/progress\", \"params\": {}}"
            )
            .is_none());
        assert!(server
            .handle_message("{\"jsonrpc\": \"2.0\", \"id\": \"x\", \"error\": {\"code\": 1, \"message\": \"m\"}}")
            .is_none());
    }

    #[test]
    fn tools_list_carries_session_tools_and_the_catalogue() {
        let mut server = McpServer::new();
        let reply: Value = serde_json::from_str(
            &server
                .handle_message(&request(1, "tools/list", json!({})))
                .unwrap(),
        )
        .unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"status"));
        assert!(names.contains(&"session_launch"));
        assert!(names.contains(&"events_next"));
        assert!(names.contains(&"capture_screenshot"));
        assert!(names.contains(&"warp_get"));
        assert_eq!(
            tools.len(),
            bridge_tools().len() + catalogue::catalogue().len()
        );
        let mut seen = std::collections::HashSet::new();
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            assert!(seen.insert(name), "duplicate {name}");
            assert!(
                name.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    && name.len() <= 64,
                "bad name {name}"
            );
            assert_eq!(tool["inputSchema"]["type"], "object", "{name}");
            assert!(tool["description"].as_str().unwrap().is_ascii(), "{name}");
        }
    }

    #[test]
    fn tools_need_a_session_first() {
        let mut server = McpServer::new();
        let result = call(&mut server, 1, "status", json!({}));
        assert_eq!(result["isError"], true);
        assert!(text_json(&result)["error"]
            .as_str()
            .unwrap()
            .contains("session_launch"));
        let result = call(&mut server, 2, "events_next", json!({"timeout_ms": 10}));
        assert_eq!(result["isError"], true);
        let result = call(&mut server, 3, "session_status", json!({}));
        assert_eq!(text_json(&result)["attached"], false);
        let result = call(&mut server, 4, "session_close", json!({}));
        assert_eq!(text_json(&result)["closed"], false);
        let result = call(&mut server, 5, "session_attach", json!({}));
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn status_through_the_bridge_reports_the_machine() {
        let (mut server, handle) = attached();
        let result = call(&mut server, 2, "status", json!({}));
        assert_eq!(result["isError"], false);
        assert_eq!(result["content"][0]["type"], "text");
        let status = text_json(&result);
        assert_eq!(status["frame"], 0);
        assert_eq!(status["pc"], 0xF80010);
        assert_eq!(status["state"], "paused");

        let result = call(&mut server, 3, "session_status", json!({}));
        let doc = text_json(&result);
        assert_eq!(doc["attached"], true);
        assert_eq!(doc["connection"], "open");
        assert!(doc.get("launched_pid").is_none());
        close(server, handle);
    }

    #[test]
    fn unknown_tools_and_protocol_errors_are_tool_results() {
        let (mut server, handle) = attached();
        let result = call(&mut server, 2, "no_such_tool", json!({}));
        assert_eq!(result["isError"], true);
        assert!(text_json(&result)["error"]
            .as_str()
            .unwrap()
            .contains("no_such_tool"));

        // mem.read without addr: the parser's invalid-params error.
        let result = call(&mut server, 3, "mem_read", json!({}));
        assert_eq!(result["isError"], true);
        let doc = text_json(&result);
        assert_eq!(doc["error"]["code"], INVALID_PARAMS);
        assert_eq!(doc["error"]["message"], "missing addr");

        // The dotted name is not a tool name.
        let result = call(&mut server, 4, "mem.read", json!({"addr": 0}));
        assert_eq!(result["isError"], true);
        close(server, handle);
    }

    #[test]
    fn mem_and_step_round_trip_with_hex_addresses() {
        let (mut server, handle) = attached();
        let result = call(
            &mut server,
            2,
            "mem_write",
            json!({"addr": "0x1000", "data": "cafe"}),
        );
        assert_eq!(result["isError"], false, "{result}");
        let result = call(
            &mut server,
            3,
            "mem_read",
            json!({"addr": "$1000", "len": 2}),
        );
        assert_eq!(text_json(&result)["data"], "cafe");
        let result = call(&mut server, 4, "step", json!({"n": 1}));
        let stop = text_json(&result);
        assert_eq!(stop["reason"], "step");
        assert_eq!(stop["pc"], 0xF80012);
        close(server, handle);
    }

    #[test]
    fn continue_with_wait_ms_pauses_a_free_running_machine() {
        let (mut server, handle) = attached();
        let result = call(&mut server, 2, "continue", json!({"wait_ms": 100}));
        assert_eq!(result["isError"], false, "{result}");
        let stop = text_json(&result);
        assert_eq!(stop["reason"], "pause");
        assert_eq!(stop["bridge"]["paused_after_ms"], 100);
        assert!(stop["retired_instructions"].as_u64().unwrap() > 0);
        // The machine is paused again and answers.
        let status = text_json(&call(&mut server, 3, "status", json!({})));
        assert_eq!(status["state"], "paused");
        // wait_ms is stripped before forwarding: a breakpoint stop still
        // arrives as itself, without the bridge marker.
        call(
            &mut server,
            4,
            "break_add",
            json!({"kind": "pc", "addr": "$F8001A"}),
        );
        let stop = text_json(&call(&mut server, 5, "continue", json!({"wait_ms": 5000})));
        assert_eq!(stop["reason"], "breakpoint");
        assert!(stop.get("bridge").is_none());
        let result = call(&mut server, 6, "continue", json!({"wait_ms": 0}));
        assert_eq!(result["isError"], true);
        close(server, handle);
    }

    #[test]
    fn a_late_pause_reply_leaves_nothing_pending() {
        // A scripted server that never stops on its own: `continue` gets
        // no reply until `pause` arrives, which is answered with the stop
        // for the resume only; the pause's own reply is held back until
        // the next request, well past the bridge's grace for it.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let ids = Mutex::new((None::<Value>, None::<Value>));
        let server = scripted_server(listener, move |req| {
            if let Some(hello) = hello_reply(req) {
                return vec![hello];
            }
            let mut ids = ids.lock().unwrap();
            let stop = json!({"reason": "pause", "frame": 7});
            match req["method"].as_str() {
                Some("continue") => {
                    ids.0 = Some(req["id"].clone());
                    vec![]
                }
                Some("pause") => {
                    ids.1 = Some(req["id"].clone());
                    vec![proto::ok_line(
                        ids.0.as_ref().expect("continue first"),
                        stop,
                    )]
                }
                Some("status") => {
                    let mut lines = Vec::new();
                    if let Some(pause_id) = ids.1.take() {
                        lines.push(proto::ok_line(&pause_id, stop));
                    }
                    lines.push(proto::ok_line(&req["id"], json!({"frame": 7})));
                    lines
                }
                _ => vec![],
            }
        });
        let mut bridge = Bridge::connect(&addr.to_string(), "ok").unwrap();
        for round in 0..3 {
            let reply = resume_with_wait(
                &mut bridge,
                "continue",
                Value::Null,
                Duration::from_millis(20),
            )
            .unwrap();
            let Reply::Ok(stop) = reply else {
                panic!("round {round}: {reply:?}");
            };
            assert_eq!(stop["reason"], "pause");
            assert_eq!(stop["bridge"]["paused_after_ms"], 20);
            assert_eq!(bridge.outstanding(), 0, "round {round}");
            // The pause reply lands now, late, and is dropped unread.
            assert_eq!(
                bridge.call("status", Value::Null).unwrap(),
                Reply::Ok(json!({"frame": 7}))
            );
            assert_eq!(bridge.outstanding(), 0, "round {round}");
        }
        bridge.close();
        server.join().unwrap();
    }

    #[test]
    fn events_flow_through_the_queue() {
        let (mut server, handle) = attached();
        let result = call(&mut server, 2, "events_next", json!({"timeout_ms": 20}));
        let doc = text_json(&result);
        assert_eq!(doc["timed_out"], true);
        assert_eq!(doc["event"], Value::Null);

        call(
            &mut server,
            3,
            "events_subscribe",
            json!({"events": ["frame"], "frame_interval": 1}),
        );
        let stop = text_json(&call(&mut server, 4, "step_frame", json!({"n": 3})));
        assert_eq!(stop["reason"], "step");
        let next = text_json(&call(
            &mut server,
            5,
            "events_next",
            json!({"timeout_ms": 2000}),
        ));
        assert_eq!(next["timed_out"], false);
        assert_eq!(next["event"]["method"], "event.frame");
        let drained = text_json(&call(&mut server, 6, "events_drain", json!({})));
        assert!(drained["count"].as_u64().unwrap() >= 1, "{drained}");
        assert_eq!(drained["dropped"], 0);
        let again = text_json(&call(&mut server, 7, "events_drain", json!({})));
        assert_eq!(again["count"], 0);
        close(server, handle);
    }

    #[test]
    fn screenshots_come_back_as_images() {
        let (mut server, handle) = attached();
        let result = call(&mut server, 2, "capture_screenshot", json!({}));
        assert_eq!(result["isError"], false, "{result}");
        let content = result["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        let doc = text_json(&result);
        assert!(doc["width"].as_u64().unwrap() > 0);
        assert_eq!(doc["path"], Value::Null);
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["mimeType"], "image/png");
        let png = proto::decode_base64(content[1]["data"].as_str().unwrap()).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");

        // An explicit path is kept.
        let dir = std::env::temp_dir().join(format!("ccp-mcp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shot.png");
        let result = call(
            &mut server,
            3,
            "capture_screenshot",
            json!({"path": path.display().to_string()}),
        );
        assert_eq!(text_json(&result)["path"], path.display().to_string());
        assert!(path.is_file());
        std::fs::remove_dir_all(&dir).ok();

        // A relative path is resolved against this process's working
        // directory and forwarded absolute, so an emulator with another
        // cwd writes the file where it is read back from.
        let rel_dir = format!("ccp-mcp-rel-{}", std::process::id());
        std::fs::create_dir_all(&rel_dir).unwrap();
        let result = call(
            &mut server,
            4,
            "capture_screenshot",
            json!({"path": format!("{rel_dir}/shot.png")}),
        );
        assert_eq!(result["isError"], false, "{result}");
        let expected = std::env::current_dir()
            .unwrap()
            .join(&rel_dir)
            .join("shot.png");
        assert_eq!(text_json(&result)["path"], expected.display().to_string());
        assert!(expected.is_file());
        assert_eq!(result["content"][1]["type"], "image");
        std::fs::remove_dir_all(&rel_dir).ok();

        // A path that is not a string is the method's own invalid-params
        // error, not a temporary screenshot.
        let result = call(&mut server, 5, "capture_screenshot", json!({"path": 7}));
        assert_eq!(result["isError"], true, "{result}");
        assert_eq!(text_json(&result)["error"]["code"], INVALID_PARAMS);
        assert_eq!(result["content"].as_array().unwrap().len(), 1);
        close(server, handle);
    }

    #[test]
    fn one_session_at_a_time_and_a_lost_connection_is_reported() {
        let (mut server, handle) = attached();
        let result = call(
            &mut server,
            2,
            "session_attach",
            json!({"listen": "127.0.0.1:1", "token": "x"}),
        );
        assert_eq!(result["isError"], true);
        assert!(text_json(&result)["error"]
            .as_str()
            .unwrap()
            .contains("already attached"));

        // shutdown ends the server thread; the bridge notices.
        let result = call(&mut server, 3, "shutdown", json!({}));
        assert_eq!(result["isError"], false, "{result}");
        assert_eq!(text_json(&result)["session"]["closed"], true);
        assert!(!server.attached());
        handle.join().unwrap();
        let result = call(&mut server, 4, "status", json!({}));
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn serve_runs_a_script_to_eof_and_writes_only_replies() {
        let (addr, token, handle) = spawn_test_session();
        let script = [
            request(1, "initialize", json!({"protocolVersion": LATEST_PROTOCOL_VERSION})),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
            request(
                2,
                "tools/call",
                json!({"name": "session_attach", "arguments": {"listen": addr.to_string(), "token": token}}),
            ),
            request(3, "tools/call", json!({"name": "beam_get"})),
            request(4, "tools/call", json!({"name": "session_close"})),
        ]
        .join("\n");
        let mut out = Vec::new();
        let mut server = McpServer::new();
        server
            .serve(std::io::Cursor::new(script), &mut out)
            .unwrap();
        handle.join().unwrap();
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0]["id"], 1);
        assert_eq!(lines[1]["id"], 2);
        assert_eq!(lines[2]["result"]["isError"], false);
        let beam: Value =
            serde_json::from_str(lines[2]["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert!(beam.get("vpos").is_some(), "{beam}");
        assert_eq!(lines[3]["id"], 4);
    }
}
