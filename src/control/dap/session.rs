// SPDX-License-Identifier: GPL-3.0-or-later

//! One debug session: the emulator behind a bridge, the program's debug
//! information, and the state machine from launch to the program's
//! entry point and on through stops, steps and reverse steps. Every DAP
//! request past `initialize` lands here.

use super::breaks::{self, BreakTable};
use super::console::{self, Command, DumpMode, MemoryFormat, NumberFormat};
use super::proto::Request;
use super::vars::{self, GuestMem, Node, VarStore};
use super::{eval, Emit, Msg};
use crate::amigaos::symbols::SymbolSnapshot;
use crate::control::bridge::{self, Bridge, LaunchSpec, Launched, Reply};
use crate::control::proto;
use crate::debugger::CaughtException;
use crate::debuginfo::unwind::{self, ExceptionFrame, Frame, FrameStyle, FrameVia, Registers};
use crate::debuginfo::DebugInfo;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;

/// The one thread the adapter reports: the 68k.
pub const THREAD_ID: i64 = 1;
const FRAME_ID_BASE: i64 = 1000;
const MAX_FRAMES: usize = 16;
/// Single steps taken per source-line step before falling back to
/// breakpoints on the other lines of the function.
const LINE_STEP_BUDGET: usize = 64;
/// Reverse single steps per line step back; each costs a snapshot
/// restore and replay.
const REVERSE_LINE_STEP_BUDGET: usize = 32;
/// How long a launched emulator gets to exit on `shutdown` before it
/// is killed.
const EXIT_GRACE: Duration = Duration::from_secs(3);
/// Instructions a `disassemble` request may span backwards from an
/// anchor before giving up on exact alignment.
const DISASSEMBLE_BACK_LIMIT: usize = 512;
/// Largest `instructionCount` / `|instructionOffset|` served per request.
const DISASSEMBLE_CAP: i64 = 4096;
/// Largest `readMemory` served per request.
const READ_MEMORY_CAP: u64 = 16 * 1024 * 1024;
/// The most instructions a whole-hunk `seg_N.dbgasm` document lists.
const SEGMENT_DOCUMENT_CAP: usize = 65536;
/// `step_frame` quanta a `copperline/profile` request may spend per
/// requested frame before it reports what it captured. A quantum is a CPU
/// budget, not a video frame, so a capture can need more quanta than frames.
const PROFILE_QUANTA_PER_FRAME: u64 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunKind {
    /// Running the boot until the program is loaded.
    ToLoad,
    /// Running from the load stop to the program's entry point.
    ToEntry,
    Continue,
    /// A source-line step that outgrew single stepping: temporary
    /// breakpoints on the function's other lines.
    RangeStep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// The program has not been loaded by the guest yet.
    AwaitingLoad,
    /// Loaded and relocated, parked at the load stop until
    /// `configurationDone`.
    LoadedHeld,
    /// A resume of ours is outstanding.
    Running(RunKind),
    /// Running, but not on our behalf (resumed from the window).
    RunningExternal,
    Paused,
}

/// The `launch` request's arguments.
#[derive(Debug)]
struct LaunchArgs {
    program: PathBuf,
    run_args: Option<String>,
    binary: Option<PathBuf>,
    config: Option<String>,
    rom: Option<PathBuf>,
    factory: bool,
    headless: bool,
    noaudio: bool,
    stack: Option<u32>,
    detach: bool,
    ntsc: Option<bool>,
    emulator_log: bool,
    extra: Vec<String>,
    cwd: Option<PathBuf>,
    timeout: Duration,
    /// `coverage`: the lcov file the emulator's `--coverage` run writes.
    coverage: Option<PathBuf>,
}

fn opt_string(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(String::from)
}

fn parse_launch(args: &Value) -> Result<LaunchArgs, String> {
    let program =
        opt_string(args, "program").ok_or("launch needs \"program\": the executable to run")?;
    let program = PathBuf::from(program);
    if !program.is_file() {
        return Err(format!("program {} does not exist", program.display()));
    }
    let mut extra = Vec::new();
    for (key, flag) in [
        ("model", "--model"),
        ("chipset", "--chipset"),
        ("cpu", "--cpu"),
        ("chip", "--chip"),
        ("fast", "--fast"),
        ("slow", "--slow"),
        ("memoryFill", "--ram-init"),
        ("rtcTime", "--rtc-time"),
    ] {
        if let Some(value) = args.get(key).filter(|v| !v.is_null()) {
            let text = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            extra.push(flag.into());
            extra.push(text);
        }
    }
    if let Some(value) = args.get("fpu").filter(|v| !v.is_null()) {
        let on = value.as_bool().ok_or("fpu must be a boolean")?;
        extra.push(if on { "--fpu" } else { "--no-fpu" }.into());
    }
    let stack = match args.get("stack").filter(|v| !v.is_null()) {
        Some(value) => {
            let bytes = value.as_u64().ok_or("stack must be a positive integer")?;
            Some(crate::runprog::validate_stack_size(bytes).map_err(|e| e.to_string())?)
        }
        None => None,
    };
    let ntsc = args
        .get("ntsc")
        .filter(|v| !v.is_null())
        .map(|value| value.as_bool().ok_or("ntsc must be a boolean"))
        .transpose()?;
    if let Some(list) = args.get("extraArgs").and_then(Value::as_array) {
        for item in list {
            match item.as_str() {
                Some(s) => extra.push(s.to_string()),
                None => return Err("extraArgs must be an array of strings".into()),
            }
        }
    }
    let run_args = match args.get("args") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        // An array is argv already split: quote each element for the
        // AmigaDOS command line so it stays one argument.
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .map(|i| {
                    amigados_quote(
                        &i.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| i.to_string()),
                    )
                })
                .collect::<Vec<_>>()
                .join(" "),
        ),
        Some(_) => return Err("args must be a string or an array of strings".into()),
    };
    Ok(LaunchArgs {
        program,
        run_args,
        binary: opt_string(args, "copperline").map(PathBuf::from),
        config: opt_string(args, "config"),
        rom: opt_string(args, "rom").map(PathBuf::from),
        factory: args["factory"].as_bool().unwrap_or(false),
        headless: args["headless"].as_bool().unwrap_or(false),
        noaudio: args["noAudio"].as_bool().unwrap_or(false),
        stack,
        detach: args["detach"].as_bool().unwrap_or(false),
        ntsc,
        emulator_log: args["emulatorLog"].as_bool().unwrap_or(false),
        extra,
        cwd: opt_string(args, "cwd").map(PathBuf::from),
        timeout: Duration::from_millis(args["timeoutMs"].as_u64().unwrap_or(60_000)),
        coverage: opt_string(args, "coverage").map(PathBuf::from),
    })
}

/// One argv element as the AmigaDOS shell (ReadArgs) reads it: quoted
/// when it holds whitespace or the quote/escape characters, with `*` as
/// the escape inside quotes (`*"`, `**`, `*N`).
fn amigados_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && !arg
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '*' | '='));
    if plain {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' => out.push_str("*\""),
            '*' => out.push_str("**"),
            '\n' => out.push_str("*N"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `attach`: connect from `controlInfo` or `address` + `token`.
pub fn connect_from_args(args: &Value) -> Result<Bridge, String> {
    if let Some(path) = opt_string(args, "controlInfo") {
        return Bridge::connect_info_file(Path::new(&path));
    }
    match (opt_string(args, "address"), opt_string(args, "token")) {
        (Some(addr), Some(token)) => Bridge::connect(&addr, &token),
        _ => Err(
            "attach needs \"controlInfo\" (the --control-info file) or \"address\" and \"token\""
                .into(),
        ),
    }
}

/// Numbers sessions, so messages from a replaced session's bridge
/// threads are told apart from the current one's.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static NEXT_PROFILE: AtomicU64 = AtomicU64::new(1);

pub struct Session {
    generation: u64,
    bridge: Arc<Bridge>,
    launched: Option<Launched>,
    windowed: bool,
    program_name: Option<String>,
    program_path: Option<PathBuf>,
    symbol_path: Option<PathBuf>,
    info: Option<DebugInfo>,
    stop_on_entry: bool,
    entry_point: Option<String>,
    source_map: Vec<(String, String)>,
    phase: Phase,
    pending: Option<(u64, RunKind)>,
    waiter: Sender<u64>,
    config_done: bool,
    breaks: BreakTable,
    frames: Vec<Frame>,
    vars: VarStore,
    serial_buf: String,
    serial_idle_ticks: u32,
    last_exception: Option<(String, String)>,
    /// The exception of the catch stop the machine is paused at, until
    /// it resumes: the frames then start at its handler.
    exception_stop: Option<CaughtException>,
    /// The exception stack frame `frames` was unwound through, when it
    /// was (filled with `frames`).
    exception_frame: Option<ExceptionFrame>,
    temp_breaks: Vec<u32>,
    /// The first hunk's address from the load stop, for a session
    /// without debug information.
    first_hunk: Option<u32>,
    /// Exception catches stay dormant only while this session is deliberately
    /// running the operating system toward its named debug target. An attach
    /// without a program has no first hunk either, but is not waiting for one.
    waiting_for_target_load: bool,
    tick: u32,
    /// Events to send after the current request's response.
    deferred: Vec<(String, Value)>,
    emulator_log: bool,
    emulator_log_offset: u64,
    /// *View as* formats chosen through `modifyVariableFormat`, by
    /// variable name, for the rest of the session.
    formats: HashMap<String, NumberFormat>,
    /// The memory dumps a hover, and a Watch entry, on a label show.
    hover_format: MemoryFormat,
    watch_format: MemoryFormat,
    /// The client said it handles `invalidated` events.
    client_invalidated: bool,
}

/// One line of a console command's output: a memory row or an
/// instruction at `address`, or (with no address) a label.
struct Row {
    name: String,
    value: String,
    address: Option<u32>,
}

impl Row {
    fn label(name: &str) -> Self {
        Self {
            name: format!("{name}:"),
            value: String::new(),
            address: None,
        }
    }

    fn at(address: u32, value: String) -> Self {
        Self {
            name: format!("${address:08X}"),
            value,
            address: Some(address),
        }
    }
}

/// Rows as text, a line each.
fn rows_text(rows: &[Row]) -> String {
    let mut text = String::new();
    for row in rows {
        if row.address.is_some() {
            text.push_str(&format!("{}: {}\n", row.name, row.value));
        } else {
            text.push_str(&format!("{}\n", row.name));
        }
    }
    text
}

impl Session {
    // -----------------------------------------------------------------
    // Construction

    pub fn launch(args: &Value, tx: Sender<Msg>) -> Result<Self, String> {
        let launch = parse_launch(args)?;
        let mut spec = LaunchSpec {
            binary: launch.binary.clone(),
            cwd: launch.cwd.clone(),
            timeout: launch.timeout,
            args: Vec::new(),
            windowed: !launch.headless,
            noaudio: launch.headless || launch.noaudio,
        };
        if launch.factory {
            spec.args.push("--factory".into());
        }
        if let Some(config) = &launch.config {
            spec.args.push("--config".into());
            spec.args.push(config.clone());
        }
        if let Some(rom) = &launch.rom {
            spec.args.push(rom.display().to_string());
        }
        let program = launch
            .program
            .canonicalize()
            .unwrap_or_else(|_| launch.program.clone());
        spec.args.push("--run".into());
        spec.args.push(program.display().to_string());
        if let Some(run_args) = &launch.run_args {
            spec.args.push("--run-args".into());
            spec.args.push(run_args.clone());
        }
        if let Some(stack) = launch.stack {
            spec.args.push("--run-stack".into());
            spec.args.push(stack.to_string());
        }
        if launch.detach {
            spec.args.push("--run-detach".into());
        }
        if let Some(ntsc) = launch.ntsc {
            spec.args.push("--video".into());
            spec.args.push(if ntsc { "NTSC" } else { "PAL" }.into());
        }
        // Coverage is the emulator's own --coverage run (it watches the
        // program's load and exit itself), with this launch's sourceMap
        // applied to the file's paths.
        if let Some(coverage) = &launch.coverage {
            spec.args.push("--coverage".into());
            spec.args.push(coverage.display().to_string());
            if let Some(map) = args.get("sourceMap").and_then(Value::as_object) {
                for (from, to) in map {
                    if let Some(to) = to.as_str() {
                        spec.args.push("--coverage-source-map".into());
                        spec.args.push(format!("{from}={to}"));
                    }
                }
            }
        }
        spec.args.extend(launch.extra.iter().cloned());
        // A guest that reads the host clock makes reverse replay diverge
        // (docs/debugger/reverse.md): pin the clock to the launch time
        // unless the configuration named one.
        if !launch.extra.iter().any(|a| a == "--rtc-time") {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            spec.args.push("--rtc-time".into());
            spec.args.push(now.to_string());
        }
        let (bridge, launched) = bridge::launch(&spec)?;
        eprintln!(
            "copperline-dap: launched pid {} on {} (log {})",
            launched.pid(),
            bridge.listen(),
            launched.log_path.display()
        );
        let mut session = Self::new(bridge, Some(launched), !launch.headless, tx)?;
        session.read_memory_formats(args);
        session.emulator_log = launch.emulator_log;
        session.load_program_info(&program, args);
        session.stop_on_entry = args["stopOnEntry"].as_bool().unwrap_or(true);
        session.phase = Phase::AwaitingLoad;
        session.waiting_for_target_load = true;
        session.subscribe();
        Ok(session)
    }

    pub fn attach(
        bridge: Bridge,
        launched: Option<Launched>,
        args: &Value,
        tx: Sender<Msg>,
    ) -> Result<Self, String> {
        let status = match bridge.call("status", json!({}))? {
            Reply::Ok(v) => v,
            Reply::Err { message, .. } => return Err(format!("status: {message}")),
            Reply::TimedOut => unreachable!("no timeout"),
        };
        let windowed = args["windowed"].as_bool().unwrap_or(true);
        let mut session = Self::new(bridge, launched, windowed, tx)?;
        session.read_memory_formats(args);
        session.stop_on_entry = args["stopOnEntry"].as_bool().unwrap_or(false);
        if let Some(program) = opt_string(args, "program") {
            let program = PathBuf::from(program);
            if !program.is_file() {
                return Err(format!("program {} does not exist", program.display()));
            }
            session.load_program_info(&program, args);
        }
        session.subscribe();
        let running = status["state"].as_str() == Some("running");
        // Is the program already running? Its segment list has the
        // hunks the file describes.
        let loaded = session.try_relocate_current();
        session.waiting_for_target_load = !loaded && session.info.is_some();
        session.phase = match (loaded, running) {
            (true, true) => Phase::RunningExternal,
            (true, false) => Phase::Paused,
            (false, _) if session.info.is_some() => {
                session.arm_loadseg_break();
                if running {
                    Phase::RunningExternal
                } else {
                    Phase::AwaitingLoad
                }
            }
            (false, true) => Phase::RunningExternal,
            (false, false) => Phase::Paused,
        };
        Ok(session)
    }

    fn new(
        bridge: Bridge,
        launched: Option<Launched>,
        windowed: bool,
        tx: Sender<Msg>,
    ) -> Result<Self, String> {
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        let bridge = Arc::new(bridge);
        let (waiter, wait_rx) = channel::<u64>();
        {
            // The resume waiter: blocks on the one outstanding resume
            // verb and hands its stop reply to the main loop.
            let bridge = Arc::clone(&bridge);
            let tx = tx.clone();
            std::thread::Builder::new()
                .name("copperline-dap-wait".into())
                .spawn(move || {
                    while let Ok(id) = wait_rx.recv() {
                        match bridge.wait(id, None) {
                            Ok(reply) => {
                                let msg = Msg::Resume {
                                    generation,
                                    id,
                                    reply,
                                };
                                if tx.send(msg).is_err() {
                                    return;
                                }
                            }
                            Err(why) => {
                                let _ = tx.send(Msg::Lost { generation, why });
                                return;
                            }
                        }
                    }
                })
                .map_err(|e| format!("starting the resume waiter: {e}"))?;
        }
        {
            // The event pump: notifications as they arrive.
            let bridge = Arc::clone(&bridge);
            std::thread::Builder::new()
                .name("copperline-dap-events".into())
                .spawn(move || loop {
                    match bridge.next_event(Duration::from_secs(1)) {
                        Some(event) => {
                            if tx.send(Msg::Event { generation, event }).is_err() {
                                return;
                            }
                        }
                        None => {
                            if let Some(why) = bridge.closed() {
                                let _ = tx.send(Msg::Lost { generation, why });
                                return;
                            }
                        }
                    }
                })
                .map_err(|e| format!("starting the event pump: {e}"))?;
        }
        Ok(Self {
            generation,
            bridge,
            launched,
            windowed,
            program_name: None,
            program_path: None,
            symbol_path: None,
            info: None,
            stop_on_entry: true,
            entry_point: None,
            source_map: Vec::new(),
            phase: Phase::Paused,
            pending: None,
            waiter,
            config_done: false,
            breaks: BreakTable::default(),
            frames: Vec::new(),
            vars: VarStore::default(),
            serial_buf: String::new(),
            serial_idle_ticks: 0,
            last_exception: None,
            exception_stop: None,
            exception_frame: None,
            temp_breaks: Vec::new(),
            first_hunk: None,
            waiting_for_target_load: false,
            tick: 0,
            deferred: Vec::new(),
            emulator_log: false,
            emulator_log_offset: 0,
            formats: HashMap::new(),
            hover_format: MemoryFormat::HOVER,
            watch_format: MemoryFormat::WATCH,
            client_invalidated: false,
        })
    }

    /// uae-dap's `memoryFormats` launch and attach argument:
    /// `{"hover": {...}, "watch": {...}}`.
    fn read_memory_formats(&mut self, args: &Value) {
        let formats = &args["memoryFormats"];
        self.hover_format = MemoryFormat::from_value(&formats["hover"], MemoryFormat::HOVER);
        self.watch_format = MemoryFormat::from_value(&formats["watch"], MemoryFormat::WATCH);
    }

    /// Whether the client's `initialize` said it handles `invalidated`.
    pub fn set_client_invalidated(&mut self, supported: bool) {
        self.client_invalidated = supported;
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Read the program's debug information (and an ELF sibling's).
    fn load_program_info(&mut self, program: &Path, args: &Value) {
        self.program_path = Some(program.to_path_buf());
        self.program_name = program
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        self.entry_point = opt_string(args, "entryPoint");
        if let Some(map) = args.get("sourceMap").and_then(Value::as_object) {
            for (from, to) in map {
                if let Some(to) = to.as_str() {
                    self.source_map.push((from.clone(), to.to_string()));
                }
            }
        }
        let bytes = match std::fs::read(program) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.deferred.push((
                    "output".into(),
                    json!({"category": "console", "output": format!("reading {}: {e}\n", program.display())}),
                ));
                return;
            }
        };
        let elf_path = opt_string(args, "symbolFile")
            .map(PathBuf::from)
            .or_else(|| {
                let mut sibling = program.as_os_str().to_owned();
                sibling.push(".elf");
                let sibling = PathBuf::from(sibling);
                sibling.is_file().then_some(sibling)
            });
        self.symbol_path = elf_path.clone();
        let elf = elf_path.as_ref().and_then(|p| std::fs::read(p).ok());
        match DebugInfo::load(&bytes, elf.as_deref()) {
            Ok(info) => self.info = Some(info),
            Err(e) => self.deferred.push((
                "output".into(),
                json!({"category": "console", "output": format!("no debug information from {}: {e}\n", program.display())}),
            )),
        }
    }

    fn subscribe(&mut self) {
        let _ = self
            .bridge
            .call("events.subscribe", json!({"events": ["serial", "debug"]}));
    }

    pub fn launched(&self) -> bool {
        self.launched.is_some()
    }

    /// Adapter hook once the session is installed: the debug-info
    /// notes go to the console.
    pub fn started(&mut self, emit: &mut Emit) {
        for (name, body) in std::mem::take(&mut self.deferred) {
            emit.event(&name, body);
        }
        if let Some(info) = &self.info {
            let notes = info.notes.join("; ");
            emit.note(&format!(
                "{}: {} hunk(s); {notes}",
                self.program_name.as_deref().unwrap_or("program"),
                info.hunks.len()
            ));
        }
        if self.phase == Phase::Paused && self.launched.is_none() {
            // Attached to a paused machine: nothing more to wait for.
        }
    }

    /// Adapter hook after a response: events that belong after it.
    pub fn flush_deferred(&mut self, emit: &mut Emit) {
        for (name, body) in std::mem::take(&mut self.deferred) {
            emit.event(&name, body);
        }
    }

    /// Queue an event for after the current request's response.
    pub fn defer_event(&mut self, name: &str, body: Value) {
        self.deferred.push((name.to_string(), body));
    }

    fn defer_stopped(&mut self, reason: &str, description: Option<&str>, hit: &[i64]) {
        let mut body = json!({
            "reason": reason,
            "threadId": THREAD_ID,
            "allThreadsStopped": true,
        });
        if let Some(text) = description {
            body["description"] = Value::from(text);
        }
        if !hit.is_empty() {
            body["hitBreakpointIds"] = json!(hit);
        }
        self.deferred.push(("stopped".into(), body));
    }

    // -----------------------------------------------------------------
    // Control-protocol helpers

    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        match self.bridge.call(method, params)? {
            Reply::Ok(v) => Ok(v),
            Reply::Err { message, .. } => Err(format!("{method}: {message}")),
            Reply::TimedOut => Err(format!("{method}: timed out")),
        }
    }

    fn running(&self) -> bool {
        matches!(self.phase, Phase::Running(_) | Phase::RunningExternal)
    }

    fn require_paused(&self) -> Result<(), String> {
        if self.running() {
            Err("the machine is running; pause it first".into())
        } else {
            Ok(())
        }
    }

    fn regs(&self) -> Result<(Registers, u16), String> {
        Ok(registers_of(&self.call("regs.get", json!({}))?))
    }

    fn pc(&self) -> Result<u32, String> {
        Ok(self.regs()?.0.pc)
    }

    /// Start an unbounded resume; its stop arrives through the waiter.
    fn resume(&mut self, kind: RunKind, method: &str, params: Value) -> Result<(), String> {
        let id = self.bridge.send(method, params)?;
        if self.waiter.send(id).is_err() {
            // Nobody will ever answer this resume: stay paused rather
            // than wait for a stop that cannot arrive.
            self.clear_temp_breaks();
            return Err("the resume waiter is gone".into());
        }
        self.pending = Some((id, kind));
        self.phase = Phase::Running(kind);
        self.vars.clear();
        self.frames.clear();
        self.exception_stop = None;
        Ok(())
    }

    /// Remove the temporary breakpoints of a range step, on every way
    /// it can end.
    fn clear_temp_breaks(&mut self) {
        for id in std::mem::take(&mut self.temp_breaks) {
            let _ = self.call("break.remove", json!({"id": id}));
        }
    }

    /// A bounded resume verb (steps, reverse steps): its reply is the
    /// stop.
    fn call_stop(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.vars.clear();
        self.frames.clear();
        self.exception_stop = None;
        self.call(method, params)
    }

    // -----------------------------------------------------------------
    // Program load and entry

    /// Relocate the debug info by the scheduled process's segments when
    /// they look like this program's. Returns whether it did.
    fn try_relocate_current(&mut self) -> bool {
        let Some(info) = self.info.as_ref() else {
            return false;
        };
        let Ok(segments) = self.call("segments.list", json!({})) else {
            return false;
        };
        let current: Vec<(u32, u32)> = segments["current"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                (
                    s["start"].as_u64().unwrap_or(0) as u32,
                    s["size"].as_u64().unwrap_or(0) as u32,
                )
            })
            .collect();
        let matches = current.len() == info.hunks.len()
            && current
                .iter()
                .zip(info.hunks.iter())
                .all(|((_, size), hunk)| *size >= hunk.size);
        if !matches {
            return false;
        }
        let bases: Vec<u32> = current.iter().map(|(start, _)| *start).collect();
        self.first_hunk = bases.first().copied();
        if let Some(info) = self.info.as_mut() {
            info.relocate(bases);
        }
        true
    }

    fn arm_loadseg_break(&mut self) {
        let Some(name) = self.program_name.clone() else {
            return;
        };
        if let Ok(v) = self.call("break.add", json!({"kind": "loadseg", "name": name})) {
            self.breaks.loadseg_id = v["id"].as_u64().map(|id| id as u32);
        }
    }

    /// The program was just loaded (the machine is parked before its
    /// first instruction): relocate, announce the module, bind the
    /// breakpoints that were waiting.
    fn on_loaded(&mut self, emit: &mut Emit, detail: &str) {
        self.waiting_for_target_load = false;
        // "Program loaded: NAME (first hunk $XXXXXX)"
        if let Some(hex) = detail.rsplit('$').next() {
            let hex = hex.trim_end_matches(')');
            if let Ok(addr) = u32::from_str_radix(hex, 16) {
                self.first_hunk = Some(addr);
            }
        }
        if let Some(id) = self.breaks.loadseg_id.take() {
            let _ = self.call("break.remove", json!({"id": id}));
        }
        let segments = self.call("segments.list", json!({})).unwrap_or(Value::Null);
        let bases: Vec<u32> = segments["current"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["start"].as_u64().map(|v| v as u32))
            .collect();
        if let Some(first) = bases.first() {
            self.first_hunk = Some(*first);
        }
        let name = self
            .program_name
            .clone()
            .unwrap_or_else(|| "program".into());
        if let Some(info) = self.info.as_mut() {
            if bases.len() != info.hunks.len() {
                emit.note(&format!(
                    "{name}: the guest reports {} segment(s) but the file has {} hunk(s); symbols may be off",
                    bases.len(),
                    info.hunks.len()
                ));
            }
            if !bases.is_empty() {
                info.relocate(bases.clone());
            }
        }
        let addresses: Vec<String> = bases.iter().map(|b| format!("${b:06X}")).collect();
        emit.note(&format!("{name} loaded at {}", addresses.join(", ")));
        emit.event(
            "module",
            json!({
                "reason": "new",
                "module": self.module_value(),
            }),
        );
        if let Some(info) = self.info.as_ref() {
            let changed = self.breaks.rebind(&self.bridge, info);
            for id in changed {
                if let Some(bp) = self.breaks.points.get(&id) {
                    emit.event(
                        "breakpoint",
                        json!({"reason": "changed", "breakpoint": bp.to_value(true)}),
                    );
                }
            }
        }
        for id in self.breaks.install_exceptions(&self.bridge) {
            if let Some(bp) = self.breaks.points.get(&id) {
                emit.event(
                    "breakpoint",
                    json!({"reason": "changed", "breakpoint": bp.to_value(true)}),
                );
            }
        }
    }

    fn module_value(&self) -> Value {
        let name = self
            .program_name
            .clone()
            .unwrap_or_else(|| "program".into());
        let mut m = json!({"id": "program", "name": name});
        if let Some(path) = &self.program_path {
            m["path"] = Value::from(path.display().to_string());
        }
        if let Some(first) = self.first_hunk {
            m["memoryReference"] = Value::from(format!("0x{first:X}"));
        }
        if let Some(info) = &self.info {
            m["symbolStatus"] = Value::from(info.notes.join("; "));
            if let Some(path) = &self.program_path {
                m["symbolFilePath"] = Value::from(path.display().to_string());
            }
        }
        m
    }

    /// Run from the load stop to the entry point.
    fn run_to_entry(&mut self, emit: &mut Emit) {
        let entry = self
            .entry_point
            .as_deref()
            .and_then(|name| {
                let found = self.info.as_ref().and_then(|i| i.lookup(name));
                if found.is_none() {
                    emit.note(&format!(
                        "entryPoint {name} not found; stopping at the first hunk"
                    ));
                }
                found
            })
            .or(self.first_hunk);
        let Some(entry) = entry else {
            emit.note("no entry address known; continuing");
            if let Err(e) = self.resume(RunKind::Continue, "continue", json!({})) {
                emit.note(&format!("continue: {e}"));
            }
            return;
        };
        if let Err(e) = self.resume(RunKind::ToEntry, "run_until", json!({"pc": entry})) {
            emit.note(&format!("run_until: {e}"));
        }
    }

    pub fn configuration_done(&mut self, emit: &mut Emit) -> Result<(), String> {
        self.config_done = true;
        match self.phase {
            Phase::AwaitingLoad => self.resume(RunKind::ToLoad, "continue", json!({})),
            Phase::LoadedHeld => {
                self.run_to_entry(emit);
                Ok(())
            }
            Phase::Paused => {
                self.anchor();
                self.defer_stopped("pause", Some("attached"), &[]);
                Ok(())
            }
            Phase::Running(_) | Phase::RunningExternal => Ok(()),
        }
    }

    // -----------------------------------------------------------------
    // Stops

    /// The outstanding resume's reply arrived.
    pub fn resume_replied(&mut self, emit: &mut Emit, id: u64, reply: Reply) {
        let Some((pending_id, kind)) = self.pending else {
            return;
        };
        if pending_id != id {
            return;
        }
        self.pending = None;
        match reply {
            Reply::Ok(stop) => self.on_stop(emit, &stop, Some(kind)),
            Reply::Err { message, .. } => {
                self.phase = Phase::Paused;
                self.clear_temp_breaks();
                emit.note(&format!("resume failed: {message}"));
                emit.stopped("pause", Some(&message), &[]);
            }
            Reply::TimedOut => {}
        }
    }

    /// A server notification.
    pub fn notification(&mut self, emit: &mut Emit, event: &Value) {
        let method = event["method"].as_str().unwrap_or("");
        let params = &event["params"];
        match method {
            "event.stopped" => {
                if self.pending.is_none() {
                    self.on_stop(emit, params, None);
                }
            }
            "event.serial" => {
                for word in params["words"].as_array().into_iter().flatten() {
                    let byte = (word["word"].as_u64().unwrap_or(0) & 0xFF) as u8;
                    self.serial_byte(emit, byte);
                }
            }
            "event.debug" => {
                if params["kind"].as_str() == Some("log") {
                    if let Some(text) = params["text"].as_str() {
                        emit.output("console", &format!("{text}\n"));
                    }
                }
            }
            "event.warp" => {
                if let Some(on) = params["warp"].as_bool().or_else(|| params["on"].as_bool()) {
                    emit.note(if on { "warp on" } else { "warp off" });
                }
            }
            _ => {}
        }
    }

    fn serial_byte(&mut self, emit: &mut Emit, byte: u8) {
        self.serial_idle_ticks = 0;
        match byte {
            b'\n' => {
                let line = std::mem::take(&mut self.serial_buf);
                emit.output("stdout", &format!("{line}\n"));
            }
            b'\r' => {}
            0x20..=0x7E => self.serial_buf.push(byte as char),
            _ => self.serial_buf.push_str(&format!("\\x{byte:02X}")),
        }
    }

    fn on_stop(&mut self, emit: &mut Emit, stop: &Value, from: Option<RunKind>) {
        self.vars.clear();
        self.frames.clear();
        self.exception_stop = caught_exception_of(stop);
        let reason = stop["reason"].as_str().unwrap_or("").to_string();
        let detail = stop["detail"].as_str().unwrap_or("").to_string();
        let pc = stop["pc"].as_u64().unwrap_or(0) as u32;
        if from == Some(RunKind::RangeStep) {
            self.clear_temp_breaks();
        }
        if reason == "loadseg" {
            let ours = match &self.program_name {
                Some(name) => detail.strip_prefix("Program loaded: ").is_some_and(|rest| {
                    rest.split(" (")
                        .next()
                        .is_some_and(|n| n.eq_ignore_ascii_case(name))
                }),
                None => true,
            };
            if ours {
                self.on_loaded(emit, &detail);
                if self.config_done {
                    self.run_to_entry(emit);
                } else {
                    self.phase = Phase::LoadedHeld;
                }
                return;
            }
            if from == Some(RunKind::ToLoad) {
                // Another program loaded first: keep waiting.
                if let Err(e) = self.resume(RunKind::ToLoad, "continue", json!({})) {
                    emit.note(&format!("continue: {e}"));
                }
                return;
            }
        }
        if from == Some(RunKind::ToEntry) && matches!(reason.as_str(), "target" | "breakpoint") {
            if self.stop_on_entry {
                self.phase = Phase::Paused;
                self.anchor();
                emit.stopped("entry", Some("stopped at the program's entry point"), &[]);
            } else if let Err(e) = self.resume(RunKind::Continue, "continue", json!({})) {
                self.phase = Phase::Paused;
                emit.note(&format!("continue: {e}"));
                emit.stopped("pause", Some(&e), &[]);
            }
            return;
        }
        self.phase = Phase::Paused;
        self.anchor();
        let (dap_reason, description, hit) = self.map_stop(&reason, &detail, pc, from);
        emit.stopped(dap_reason, description.as_deref(), &hit);
    }

    /// Snapshot the machine here so stepping back replays from this stop
    /// (`reverse_anchor`): the boot volume `--run` stages is a host
    /// directory mount whose traffic a replay from an older snapshot
    /// cannot reproduce. Taken at run stops, not after every step.
    fn anchor(&mut self) {
        let _ = self.call("reverse_anchor", json!({}));
    }

    /// The DAP reason for a control-protocol stop.
    fn map_stop(
        &mut self,
        reason: &str,
        detail: &str,
        pc: u32,
        from: Option<RunKind>,
    ) -> (&'static str, Option<String>, Vec<i64>) {
        match reason {
            "breakpoint" => {
                let ids = self.breaks.ids_at(pc);
                if from == Some(RunKind::RangeStep) && ids.is_empty() {
                    return ("step", None, Vec::new());
                }
                ("breakpoint", Some(detail.to_string()), ids)
            }
            "watchpoint" => {
                // "Watch $XXXXXX: ..." names the word that changed.
                let word = detail
                    .strip_prefix("Watch $")
                    .and_then(|rest| rest.split(':').next())
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok());
                let ids: Vec<i64> = self
                    .breaks
                    .points
                    .values()
                    .filter(|b| match (&b.kind, word) {
                        (breaks::Kind::Data { addr, bytes, .. }, Some(word)) => {
                            // Half-open ranges: the word [word, word+2)
                            // against [addr, addr+bytes).
                            word.saturating_add(2) > *addr
                                && word < addr.saturating_add((*bytes).max(1))
                        }
                        _ => false,
                    })
                    .map(|b| b.id)
                    .collect();
                ("data breakpoint", Some(detail.to_string()), ids)
            }
            "catch" | "double_fault" => {
                self.last_exception = Some((reason.to_string(), detail.to_string()));
                // "Caught NAME (vector N), handler $PC"
                let vector = detail
                    .split("(vector ")
                    .nth(1)
                    .and_then(|rest| rest.split(')').next())
                    .and_then(|v| v.parse::<u16>().ok());
                let ids: Vec<i64> = self
                    .breaks
                    .points
                    .values()
                    .filter(|b| match (&b.kind, vector) {
                        (breaks::Kind::Exception { vector: v }, Some(hit)) => {
                            *v == hit || (*v == 6 && hit == 7)
                        }
                        _ => false,
                    })
                    .map(|b| b.id)
                    .collect();
                ("exception", Some(detail.to_string()), ids)
            }
            "step" | "target" | "reverse" => ("step", None, Vec::new()),
            "pause" | "user_pause" | "budget" => {
                let description = if reason == "budget" {
                    Some(format!("step budget exhausted: {detail}"))
                } else if reason == "user_pause" {
                    Some("paused from the window".to_string())
                } else {
                    None
                };
                ("pause", description, Vec::new())
            }
            "reg_watch" | "mmio" | "beam_trap" | "copper_break" | "task_catch" => {
                ("breakpoint", Some(detail.to_string()), Vec::new())
            }
            "loadseg" => ("breakpoint", Some(detail.to_string()), Vec::new()),
            _ => ("pause", Some(format!("{reason}: {detail}")), Vec::new()),
        }
    }

    /// Housekeeping: partial serial lines, and the windowed status poll
    /// that notices pauses and resumes made from the window.
    pub fn tick(&mut self, emit: &mut Emit) {
        self.tick = self.tick.wrapping_add(1);
        self.drain_emulator_log(emit);
        if !self.serial_buf.is_empty() {
            self.serial_idle_ticks += 1;
            if self.serial_idle_ticks >= 4 {
                let line = std::mem::take(&mut self.serial_buf);
                emit.output("stdout", &line);
                self.serial_idle_ticks = 0;
            }
        }
        if !self.windowed || !self.tick.is_multiple_of(2) || self.pending.is_some() {
            return;
        }
        if !matches!(self.phase, Phase::Paused | Phase::RunningExternal) {
            return;
        }
        let Ok(status) = self.call("status", json!({})) else {
            return;
        };
        let running = status["state"].as_str() == Some("running");
        match (self.phase, running) {
            (Phase::Paused, true) => {
                self.phase = Phase::RunningExternal;
                self.vars.clear();
                self.frames.clear();
                self.exception_stop = None;
                emit.continued();
            }
            (Phase::RunningExternal, false) => {
                self.phase = Phase::Paused;
                self.vars.clear();
                self.frames.clear();
                emit.stopped("pause", Some("paused from the window"), &[]);
            }
            _ => {}
        }
    }

    fn drain_emulator_log(&mut self, emit: &mut Emit) {
        use std::io::{Read, Seek, SeekFrom};

        let Some(path) = self
            .launched
            .as_ref()
            .filter(|_| self.emulator_log)
            .map(|launched| &launched.log_path)
        else {
            return;
        };
        let Ok(mut file) = std::fs::File::open(path) else {
            return;
        };
        if file
            .seek(SeekFrom::Start(self.emulator_log_offset))
            .is_err()
        {
            return;
        }
        let mut output = Vec::new();
        if file.read_to_end(&mut output).is_ok() && !output.is_empty() {
            self.emulator_log_offset += output.len() as u64;
            emit.output("console", &String::from_utf8_lossy(&output));
        }
    }

    /// End the session: `terminate` shuts the emulator down (the
    /// adapter asks that of a launched one by default, of an attached
    /// one only when the client says so).
    pub fn close(mut self, terminate: bool) {
        if self.bridge.closed().is_none() {
            self.breaks.clear(&self.bridge);
            for id in std::mem::take(&mut self.temp_breaks) {
                let _ = self.bridge.call("break.remove", json!({"id": id}));
            }
            // An explicit terminate stops the emulator whether this
            // session launched it or attached to it.
            if terminate {
                let _ = self.bridge.call("shutdown", json!({}));
            }
        }
        self.bridge.disconnect();
        if let Some(mut launched) = self.launched.take() {
            if terminate {
                launched.finish(EXIT_GRACE);
            }
        }
    }

    // -----------------------------------------------------------------
    // Requests

    pub fn request(
        &mut self,
        emit: &mut Emit,
        req: &Request,
        lines_at_1: bool,
        columns_at_1: bool,
    ) -> Result<Value, String> {
        let args = &req.arguments;
        let result = match req.command.as_str() {
            "configurationDone" => self.configuration_done(emit).map(|_| Value::Null),
            "threads" => Ok(json!({"threads": [{"id": THREAD_ID, "name": "68k"}]})),
            "stackTrace" => self.stack_trace(args, lines_at_1, columns_at_1),
            "scopes" => self.scopes(args),
            "variables" => self.variables(args),
            "setVariable" => self.set_variable(args),
            "evaluate" => self.evaluate(args),
            "continue" => {
                self.require_paused()?;
                self.resume(RunKind::Continue, "continue", json!({}))?;
                Ok(json!({"allThreadsContinued": true}))
            }
            "next" | "stepIn" | "stepOut" => self.step(emit, &req.command, args),
            "stepBack" => self.step_back(args),
            "reverseContinue" => self.reverse_continue(),
            "pause" => self.pause(),
            "setBreakpoints" => self.set_breakpoints(args, lines_at_1),
            "setFunctionBreakpoints" => self.set_function_breakpoints(args),
            "setInstructionBreakpoints" => self.set_instruction_breakpoints(args),
            "setDataBreakpoints" => self.set_data_breakpoints(args),
            "dataBreakpointInfo" => self.data_breakpoint_info(args),
            "setExceptionBreakpoints" => self.set_exception_breakpoints(args),
            "breakpointLocations" => self.breakpoint_locations(args, lines_at_1),
            "gotoTargets" => self.goto_targets(args, lines_at_1),
            "goto" => self.goto(args),
            "readMemory" => self.read_memory(args),
            "writeMemory" => self.write_memory(args),
            "disassemble" => self.disassemble(args, lines_at_1),
            "modules" => Ok(json!({"modules": [self.module_value()], "totalModules": 1})),
            "loadedSources" => Ok(json!({"sources": self.loaded_sources()})),
            "exceptionInfo" => self.exception_info(),
            "source" => Err("source content is not available from the adapter".into()),
            "completions" => self.completions(args, lines_at_1, columns_at_1),
            // The Amiga Assembly extension's requests, as uae-dap serves
            // them for FS-UAE and WinUAE.
            "disassembledFileContents" => self.disassembled_file_contents(args),
            "modifyVariableFormat" => self.modify_variable_format(args),
            "copperline/profile" => self.profile(emit, args),
            "copperline/chipset" => {
                let mut registers = vars::chipset(&self.bridge);
                self.finish_variables(&mut registers);
                Ok(json!({"registers": registers}))
            }
            "copperline/ui.show" => {
                let window = args["window"].as_str().ok_or("ui.show needs a window")?;
                self.call("ui.show", json!({"window": window}))
            }
            other => Err(format!("{other}: not supported")),
        };
        result
    }

    /// Capture a bounded number of committed frames from the paused session,
    /// then convert them to a source-mapped CPU profile for the editor.
    fn profile(&mut self, emit: &mut Emit, args: &Value) -> Result<Value, String> {
        self.require_paused()?;
        let frames = args["frames"].as_u64().unwrap_or(1).clamp(1, 100_000);
        let info = self
            .info
            .as_ref()
            .filter(|info| info.relocated())
            .ok_or("profiling needs a loaded program with relocated debug information")?;
        let (base, unwind) = info.bartman_unwind_table()?;
        let relocation_bases = info.bases().to_vec();
        let code_ranges: Vec<Value> = info
            .bases()
            .iter()
            .zip(&info.hunks)
            .filter(|(_, hunk)| hunk.kind == crate::debuginfo::hunk::HunkKind::Code)
            .map(|(&base, hunk)| json!({"base": base, "size": hunk.size}))
            .collect();
        let program = self
            .program_path
            .clone()
            .ok_or("profiling needs the session's program path")?;
        let capture = std::env::temp_dir().join(format!(
            "copperline-profile-{}-{}-{}",
            std::process::id(),
            self.generation,
            NEXT_PROFILE.fetch_add(1, Ordering::Relaxed),
        ));
        self.call(
            "profile.start",
            json!({
                "path": capture.display().to_string(),
                "frames": frames,
                "samples": true,
                "registers": true,
                "unwind": {
                    "base": base,
                    "table": proto::encode_base64(&unwind),
                },
                "relocation_bases": relocation_bases,
                "code_ranges": code_ranges,
            }),
        )?;
        // A `step_frame` quantum resumed after a breakpoint only finishes the
        // interrupted CPU budget, which can end inside the same video frame
        // and commit nothing to the capture. Step until the capture has its
        // frames, or anything but a step stops the machine.
        let budget = frames.saturating_mul(PROFILE_QUANTA_PER_FRAME);
        let mut quanta = 0;
        let stepped = loop {
            quanta += 1;
            let stop = match self.call_stop("step_frame", json!({"n": 1})) {
                Ok(stop) => stop,
                Err(e) => break Err(e),
            };
            let finished = stop["reason"].as_str() != Some("step")
                || quanta >= budget
                || self
                    .call("profile.status", json!({}))
                    .map_or(true, |status| status["done"].as_bool() == Some(true));
            if finished {
                break Ok(stop);
            }
        };
        let stopped = self.call("profile.stop", json!({}));
        let stepped = stepped?;
        let stopped = stopped?;

        let out = capture.join("profile.cpuprofile");
        let generated = crate::profile::report::generate(&crate::profile::report::ReportOptions {
            input_dir: capture.clone(),
            program,
            elf: self.symbol_path.clone(),
            out: out.clone(),
            format: crate::profile::report::ReportFormat::Chrome,
            per_frame: false,
            source_map: self.source_map.clone(),
        })?;
        self.vars.clear();
        self.frames.clear();
        self.finish_bounded(emit, &stepped, None);
        Ok(json!({
            "path": generated[0].display().to_string(),
            "capturePath": capture.display().to_string(),
            "framesCaptured": stopped["frames_written"],
            "samples": stopped["samples_total"],
        }))
    }

    // -----------------------------------------------------------------
    // Stepping

    fn step(&mut self, emit: &mut Emit, command: &str, args: &Value) -> Result<Value, String> {
        self.require_paused()?;
        let method = match command {
            "next" => "step_over",
            "stepIn" => "step",
            _ => "step_out",
        };
        let instruction = args["granularity"].as_str() == Some("instruction");
        let pc = self.pc()?;
        let start = self
            .info
            .as_ref()
            .and_then(|i| i.line_for(pc))
            .map(|h| (h.file, h.line));
        if instruction || start.is_none() || method == "step_out" {
            let stop = if method == "step_out"
                && (crate::memory::ROM_BASE as u32..=0x00FF_FFFF).contains(&pc)
            {
                self.call_stop(
                    "run_until",
                    json!({"pc_outside": [crate::memory::ROM_BASE, 0x00FF_FFFFu32]}),
                )?
            } else {
                self.call_stop(method, json!({}))?
            };
            self.finish_bounded(emit, &stop, None);
            return Ok(Value::Null);
        }
        let start = start.expect("checked");
        for _ in 0..LINE_STEP_BUDGET {
            let stop = self.call_stop(method, json!({}))?;
            if stop["reason"].as_str() != Some("step") {
                self.finish_bounded(emit, &stop, None);
                return Ok(Value::Null);
            }
            let pc = stop["pc"].as_u64().unwrap_or(0) as u32;
            let hit = self.info.as_ref().and_then(|i| i.line_for(pc));
            match hit {
                Some(h) if (h.file, h.line) != start => {
                    self.phase = Phase::Paused;
                    self.defer_stopped("step", None, &[]);
                    return Ok(Value::Null);
                }
                Some(_) => {}
                None => {
                    if method == "step" {
                        // Into code without lines (a library, the
                        // wrappers): run it to its return.
                        let stop = self.call_stop("step_out", json!({}))?;
                        if stop["reason"].as_str() != Some("step") {
                            self.finish_bounded(emit, &stop, None);
                            return Ok(Value::Null);
                        }
                        let pc = stop["pc"].as_u64().unwrap_or(0) as u32;
                        let hit = self.info.as_ref().and_then(|i| i.line_for(pc));
                        if hit.is_some_and(|h| (h.file, h.line) != start) {
                            self.phase = Phase::Paused;
                            self.defer_stopped("step", None, &[]);
                            return Ok(Value::Null);
                        }
                    } else {
                        // Returned into code without lines.
                        self.phase = Phase::Paused;
                        self.defer_stopped("step", None, &[]);
                        return Ok(Value::Null);
                    }
                }
            }
        }
        // A long line (a loop): breakpoints on the function's other
        // lines and the return address, then run.
        self.range_step(emit, start)
    }

    fn range_step(&mut self, emit: &mut Emit, start: (u32, u32)) -> Result<Value, String> {
        let pc = self.pc()?;
        let mut targets: Vec<u32> = Vec::new();
        if let Some(info) = &self.info {
            if let Some(f) = info.function_at(pc) {
                for (addr, line) in info.function_line_starts(f) {
                    if line != start.1 && !targets.contains(&addr) {
                        targets.push(addr);
                    }
                }
            }
        }
        self.ensure_frames()?;
        if let Some(caller) = self.frames.get(1) {
            if !targets.contains(&caller.pc) {
                targets.push(caller.pc);
            }
        }
        if targets.is_empty() {
            return Err("cannot step over this line: no other line starts known".into());
        }
        for addr in targets {
            if let Ok(v) = self.call("break.add", json!({"kind": "pc", "addr": addr})) {
                if let Some(id) = v["id"].as_u64() {
                    self.temp_breaks.push(id as u32);
                }
            }
        }
        let _ = emit;
        if let Err(e) = self.resume(RunKind::RangeStep, "continue", json!({})) {
            self.clear_temp_breaks();
            return Err(e);
        }
        Ok(Value::Null)
    }

    /// A bounded verb's stop: map it and queue the `stopped` event for
    /// after the response.
    fn finish_bounded(&mut self, _emit: &mut Emit, stop: &Value, from: Option<RunKind>) {
        self.vars.clear();
        self.frames.clear();
        // A step can end in a caught exception's handler too.
        self.exception_stop = caught_exception_of(stop);
        self.phase = Phase::Paused;
        let reason = stop["reason"].as_str().unwrap_or("").to_string();
        let detail = stop["detail"].as_str().unwrap_or("").to_string();
        let pc = stop["pc"].as_u64().unwrap_or(0) as u32;
        let (dap_reason, description, hit) = self.map_stop(&reason, &detail, pc, from);
        self.defer_stopped(dap_reason, description.as_deref(), &hit);
    }

    fn step_back(&mut self, args: &Value) -> Result<Value, String> {
        self.require_paused()?;
        let instruction = args["granularity"].as_str() == Some("instruction");
        let pc = self.pc()?;
        let start = self
            .info
            .as_ref()
            .and_then(|i| i.line_for(pc))
            .map(|h| (h.file, h.line));
        let budget = if instruction || start.is_none() {
            1
        } else {
            REVERSE_LINE_STEP_BUDGET
        };
        let mut last = Value::Null;
        for _ in 0..budget {
            last = self.call_stop("reverse_step", json!({"n": 1}))?;
            if let Some(start) = start {
                let pc = last["pc"].as_u64().unwrap_or(0) as u32;
                let hit = self.info.as_ref().and_then(|i| i.line_for(pc));
                if hit.is_none_or(|h| (h.file, h.line) != start) {
                    break;
                }
            }
        }
        let _ = last;
        self.phase = Phase::Paused;
        self.defer_stopped("step", None, &[]);
        Ok(Value::Null)
    }

    fn reverse_continue(&mut self) -> Result<Value, String> {
        self.require_paused()?;
        let stop = self.call_stop("reverse_continue", json!({}))?;
        let pc = stop["pc"].as_u64().unwrap_or(0) as u32;
        let ids = self.breaks.ids_at(pc);
        self.phase = Phase::Paused;
        if ids.is_empty() {
            self.defer_stopped("step", stop["detail"].as_str(), &[]);
        } else {
            self.defer_stopped("breakpoint", stop["detail"].as_str(), &ids);
        }
        Ok(Value::Null)
    }

    fn pause(&mut self) -> Result<Value, String> {
        if self.pending.is_some() {
            // The pending resume's reply becomes the stop.
            self.call("pause", json!({}))?;
            return Ok(Value::Null);
        }
        let stop = self.call("pause", json!({}))?;
        self.vars.clear();
        self.frames.clear();
        self.phase = Phase::Paused;
        self.anchor();
        let detail = stop["detail"].as_str().unwrap_or("");
        let description = (detail == "already paused").then(|| detail.to_string());
        self.defer_stopped("pause", description.as_deref(), &[]);
        Ok(Value::Null)
    }

    // -----------------------------------------------------------------
    // Breakpoints

    fn set_breakpoints(&mut self, args: &Value, lines_at_1: bool) -> Result<Value, String> {
        let path = args["source"]["path"]
            .as_str()
            .or_else(|| args["source"]["name"].as_str())
            .ok_or("setBreakpoints needs a source path")?
            .to_string();
        let mut requests = Vec::new();
        for bp in args["breakpoints"].as_array().into_iter().flatten() {
            let line = u32::try_from(bp["line"].as_u64().unwrap_or(0)).unwrap_or(u32::MAX);
            let line = if lines_at_1 {
                line
            } else {
                line.saturating_add(1)
            };
            requests.push((
                line,
                bp["condition"].as_str().map(String::from),
                bp["hitCondition"].as_str().map(String::from),
            ));
        }
        let ids = self
            .breaks
            .set_source(&self.bridge, self.info.as_ref(), &path, &requests);
        Ok(json!({"breakpoints": self.breakpoint_values(&ids, lines_at_1)}))
    }

    fn breakpoint_values(&self, ids: &[i64], lines_at_1: bool) -> Vec<Value> {
        ids.iter()
            .filter_map(|id| self.breaks.points.get(id))
            .map(|bp| bp.to_value(lines_at_1))
            .collect()
    }

    fn set_function_breakpoints(&mut self, args: &Value) -> Result<Value, String> {
        let mut requests = Vec::new();
        for bp in args["breakpoints"].as_array().into_iter().flatten() {
            let Some(name) = bp["name"].as_str() else {
                continue;
            };
            requests.push((
                name.to_string(),
                bp["condition"].as_str().map(String::from),
                bp["hitCondition"].as_str().map(String::from),
            ));
        }
        let ids = self
            .breaks
            .set_functions(&self.bridge, self.info.as_ref(), &requests);
        Ok(json!({"breakpoints": self.breakpoint_values(&ids, true)}))
    }

    fn set_instruction_breakpoints(&mut self, args: &Value) -> Result<Value, String> {
        let mut requests = Vec::new();
        for bp in args["breakpoints"].as_array().into_iter().flatten() {
            let Some(reference) = bp["instructionReference"].as_str() else {
                continue;
            };
            let base = parse_address(reference)?;
            let offset = bp["offset"].as_i64().unwrap_or(0);
            let addr = (i64::from(base) + offset) as u32;
            requests.push((
                addr,
                bp["condition"].as_str().map(String::from),
                bp["hitCondition"].as_str().map(String::from),
            ));
        }
        let ids = self.breaks.set_instructions(&self.bridge, &requests);
        Ok(json!({"breakpoints": self.breakpoint_values(&ids, true)}))
    }

    fn set_data_breakpoints(&mut self, args: &Value) -> Result<Value, String> {
        let mut requests = Vec::new();
        for bp in args["breakpoints"].as_array().into_iter().flatten() {
            let Some(data_id) = bp["dataId"].as_str() else {
                continue;
            };
            let (addr, bytes) = data_id
                .split_once(':')
                .ok_or_else(|| format!("bad dataId {data_id}"))?;
            let access = match bp["accessType"].as_str().unwrap_or("write") {
                "read" => "read",
                "write" => "write",
                "readWrite" => "access",
                other => return Err(format!("unsupported data breakpoint accessType {other}")),
            };
            requests.push((
                parse_address(addr)?,
                bytes.parse::<u32>().unwrap_or(4),
                access.to_string(),
            ));
        }
        let ids = self.breaks.set_data(&self.bridge, &requests);
        Ok(json!({"breakpoints": self.breakpoint_values(&ids, true)}))
    }

    fn data_breakpoint_info(&mut self, args: &Value) -> Result<Value, String> {
        let name = args["name"].as_str().unwrap_or("");
        let bytes = args["bytes"].as_u64().map(|b| b as u32);
        let none = |why: &str| Ok(json!({"dataId": Value::Null, "description": why}));
        if args["asAddress"].as_bool() == Some(true) {
            let addr = parse_address(name)?;
            let bytes = bytes.unwrap_or(2);
            return Ok(json!({
                "dataId": format!("0x{addr:X}:{bytes}"),
                "description": format!("{bytes} byte(s) at 0x{addr:X}"),
                "accessTypes": ["read", "write", "readWrite"],
                "canPersist": true,
            }));
        }
        // A variable of a scope, or a symbol.
        let reference = args["variablesReference"].as_i64().unwrap_or(0);
        let (addr, size) = match self.vars.get(reference).cloned() {
            Some(Node::Typed { addr, ty, .. }) => {
                let Some(info) = &self.info else {
                    return none("no debug information");
                };
                let member = info
                    .resolve_type(Some(ty))
                    .and_then(|t| info.types.get(t))
                    .and_then(|t| match t {
                        crate::debuginfo::TypeDesc::Struct { members, .. } => {
                            members.iter().find(|m| m.name == name).cloned()
                        }
                        _ => None,
                    });
                match member {
                    Some(m) => (
                        addr.wrapping_add(m.offset),
                        info.type_size(m.ty).unwrap_or(4),
                    ),
                    None => return none("not a member"),
                }
            }
            _ => match self.variable_place(name) {
                Some((addr, size)) => (addr, size),
                None => return none(&format!("{name} has no address")),
            },
        };
        let bytes = bytes.unwrap_or(size);
        Ok(json!({
            "dataId": format!("0x{addr:X}:{bytes}"),
            "description": format!("{name} ({bytes} byte(s) at 0x{addr:X})"),
            "accessTypes": ["read", "write", "readWrite"],
            "canPersist": false,
        }))
    }

    /// A named variable's (address, size) at the current frame, or a
    /// symbol's.
    fn variable_place(&mut self, name: &str) -> Option<(u32, u32)> {
        self.info.as_ref()?;
        let frame = self
            .ensure_frames()
            .ok()
            .and_then(|_| self.frames.first().copied())?;
        let info = self.info.as_ref()?;
        if let Some(v) = info.variable_at(name, frame.pc) {
            let function = info.function_at(frame.pc);
            if let Ok(vars::Place::Memory(addr)) =
                vars::place_of(info, &frame, function, &v.location)
            {
                return Some((addr, info.type_size(v.ty).unwrap_or(4)));
            }
        }
        let at = vars::symbol_address(info, name)?;
        let addr = info.runtime(at)?;
        Some((addr, 4))
    }

    fn set_exception_breakpoints(&mut self, args: &Value) -> Result<Value, String> {
        let mut filters: Vec<String> = args["filters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f.as_str().map(String::from))
            .collect();
        for option in args["filterOptions"].as_array().into_iter().flatten() {
            if let Some(id) = option["filterId"].as_str() {
                filters.push(id.to_string());
            }
        }
        let ids = self
            .breaks
            .set_exceptions(&self.bridge, &filters, !self.waiting_for_target_load);
        Ok(json!({"breakpoints": self.breakpoint_values(&ids, true)}))
    }

    fn breakpoint_locations(&self, args: &Value, lines_at_1: bool) -> Result<Value, String> {
        let Some(info) = self.info.as_ref().filter(|i| i.relocated()) else {
            return Ok(json!({"breakpoints": []}));
        };
        let path = args["source"]["path"].as_str().unwrap_or("");
        let Some(file) = info.find_file(path) else {
            return Ok(json!({"breakpoints": []}));
        };
        let to_internal = |l: u64| {
            let l = u32::try_from(l).unwrap_or(u32::MAX);
            if lines_at_1 {
                l
            } else {
                l.saturating_add(1)
            }
        };
        let start = to_internal(args["line"].as_u64().unwrap_or(1));
        let end = args["endLine"].as_u64().map_or(start, to_internal);
        let mut lines: Vec<u32> = info
            .rows
            .iter()
            .filter(|r| r.file == file && r.is_stmt && !r.end_sequence)
            .map(|r| r.line)
            .filter(|l| *l >= start && *l <= end)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        let out: Vec<Value> = lines
            .into_iter()
            .map(|l| json!({"line": if lines_at_1 { l } else { l - 1 }}))
            .collect();
        Ok(json!({"breakpoints": out}))
    }

    // -----------------------------------------------------------------
    // Stack and variables

    fn ensure_frames(&mut self) -> Result<(), String> {
        if !self.frames.is_empty() {
            return Ok(());
        }
        let reply = self.call("regs.get", json!({}))?;
        let (regs, _) = registers_of(&reply);
        let usp = reply["usp"].as_u64().map(|v| v as u32);
        let msp = reply["msp"].as_u64().map(|v| v as u32);
        // Paused at a caught exception's handler entry, A7 points at the
        // frame the CPU pushed: unwind through it into the interrupted
        // context.
        let caught = self
            .exception_stop
            .filter(|caught| caught.handler == regs.pc)
            .zip(usp);
        let style = match caught {
            Some(_) => Some(FrameStyle::for_cpu(
                self.call("status", json!({}))?["cpu"]
                    .as_str()
                    .unwrap_or(""),
            )),
            None => None,
        };
        let empty = DebugInfo::default();
        let info = self.info.as_ref().unwrap_or(&empty);
        let mut mem = GuestMem::new(&self.bridge);
        let mut read32 = |addr: u32| mem.read32(addr);
        let mut mem2 = GuestMem::new(&self.bridge);
        let mut read16 = |addr: u32| mem2.read16(addr);
        self.exception_frame = None;
        if let (Some((caught, usp)), Some(style)) = (caught, style) {
            if let Some(frame) = unwind::locate_exception_frame(
                style,
                caught.vector,
                regs.sp(),
                msp,
                &mut read16,
                &mut read32,
            ) {
                self.frames = unwind::unwind_exception(
                    info,
                    &regs,
                    usp,
                    &frame,
                    caught.fault_pc,
                    &mut read32,
                    &mut read16,
                    MAX_FRAMES,
                );
                self.exception_frame = Some(frame);
                return Ok(());
            }
        }
        self.frames = unwind::unwind(info, &regs, &mut read32, &mut read16, MAX_FRAMES);
        Ok(())
    }

    fn frame(&self, frame_id: i64) -> Result<(usize, Frame), String> {
        let index = usize::try_from(frame_id - FRAME_ID_BASE).map_err(|_| "bad frameId")?;
        self.frames
            .get(index)
            .copied()
            .map(|f| (index, f))
            .ok_or_else(|| "unknown frame (the machine moved on)".into())
    }

    fn stack_trace(
        &mut self,
        args: &Value,
        lines_at_1: bool,
        columns_at_1: bool,
    ) -> Result<Value, String> {
        self.require_paused()?;
        self.ensure_frames()?;
        let start = args["startFrame"].as_u64().unwrap_or(0) as usize;
        let levels = args["levels"].as_u64().unwrap_or(0) as usize;
        let rom_symbols = self.rom_symbols();
        // Paused at a caught exception's handler: frame 0 says so.
        let handling = self.exception_stop.filter(|_| self.unwound_exception());
        let mut out = Vec::new();
        for (i, frame) in self.frames.iter().enumerate().skip(start) {
            if levels > 0 && out.len() >= levels {
                break;
            }
            let lookup = vars::lookup_pc(frame, i);
            let mut name = self.frame_name(frame.pc, lookup, &rom_symbols);
            if let (0, Some(caught)) = (i, handling) {
                name = format!(
                    "{} handler: {name}",
                    crate::debugger::exception_vector_name(caught.vector)
                );
            }
            let mut v = json!({
                "id": FRAME_ID_BASE + i as i64,
                "name": name,
                "line": 0,
                "column": 0,
                "instructionPointerReference": format!("0x{:X}", frame.pc),
            });
            if let Some((source, line, column)) = self.source_at(lookup) {
                v["source"] = source;
                v["line"] = json!(if lines_at_1 {
                    line
                } else {
                    line.saturating_sub(1)
                });
                v["column"] = json!(if columns_at_1 {
                    column.max(1)
                } else {
                    column.saturating_sub(1)
                });
            } else {
                v["presentationHint"] = json!("subtle");
            }
            if self
                .info
                .as_ref()
                .is_some_and(|info| info.locate(frame.pc).is_some())
            {
                v["moduleId"] = json!("program");
            }
            out.push(v);
        }
        Ok(json!({"stackFrames": out, "totalFrames": self.frames.len()}))
    }

    fn frame_name(&self, pc: u32, lookup: u32, rom_symbols: &SymbolSnapshot) -> String {
        if let Some(info) = &self.info {
            if let Some(f) = info.function_at(lookup) {
                return f.name.trim_start_matches('_').to_string();
            }
            if let Some((sym, off)) = info.symbol_at(lookup) {
                let off = off.wrapping_add(pc.wrapping_sub(lookup));
                return if off == 0 {
                    sym.name.clone()
                } else {
                    format!("{}+{off}", sym.name)
                };
            }
        }
        if let Some(symbol) = rom_symbols
            .resolve(lookup)
            .or_else(|| (lookup != pc).then(|| rom_symbols.resolve(pc)).flatten())
        {
            return symbol.display_name();
        }
        format!("0x{pc:06X}")
    }

    fn rom_symbols(&self) -> SymbolSnapshot {
        self.call("symbols.rom", json!({}))
            .ok()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    }

    /// The DAP `Source`, line and column for an address.
    fn source_at(&self, addr: u32) -> Option<(Value, u32, u32)> {
        let info = self.info.as_ref()?;
        let hit = info.line_for(addr)?;
        let recorded = &info.files.get(hit.file as usize)?.path;
        let host = self.host_path(recorded);
        let name = breaks::basename(&host);
        Some((json!({"name": name, "path": host}), hit.line, hit.column))
    }

    /// The host path for a source the program's debug info names.
    fn host_path(&self, recorded: &str) -> String {
        for (from, to) in &self.source_map {
            if let Some(rest) = recorded.strip_prefix(from.as_str()) {
                let mapped = format!("{}{}", to.trim_end_matches('/'), rest);
                if Path::new(&mapped).is_file() {
                    return mapped;
                }
            }
        }
        if Path::new(recorded).is_file() {
            return recorded.to_string();
        }
        if let Some(dir) = self.program_path.as_ref().and_then(|p| p.parent()) {
            // Try the tail of the recorded path under the program's
            // directory, longest first.
            let parts: Vec<&str> = recorded.split('/').filter(|p| !p.is_empty()).collect();
            for start in 0..parts.len() {
                let candidate = dir.join(parts[start..].join("/"));
                if candidate.is_file() {
                    return candidate.display().to_string();
                }
            }
        }
        recorded.to_string()
    }

    fn loaded_sources(&self) -> Vec<Value> {
        let Some(info) = &self.info else {
            return Vec::new();
        };
        info.files
            .iter()
            .map(|f| {
                let host = self.host_path(&f.path);
                json!({"name": breaks::basename(&host), "path": host})
            })
            .collect()
    }

    fn scopes(&mut self, args: &Value) -> Result<Value, String> {
        self.require_paused()?;
        self.ensure_frames()?;
        let (index, _) = self.frame(args["frameId"].as_i64().unwrap_or(FRAME_ID_BASE))?;
        let loaded = self.info.as_ref().is_some_and(DebugInfo::relocated);
        Ok(vars::scopes(
            &mut self.vars,
            index,
            self.info.is_some(),
            loaded,
        ))
    }

    fn variables(&mut self, args: &Value) -> Result<Value, String> {
        self.require_paused()?;
        let reference = args["variablesReference"].as_i64().unwrap_or(0);
        let Some(node) = self.vars.get(reference).cloned() else {
            return Err("stale variablesReference".into());
        };
        self.ensure_frames()?;
        let frame_of = |frames: &Vec<Frame>, i: usize| frames.get(i).copied();
        let mut vars_out: Vec<Value> = match node {
            Node::Registers { frame } => {
                let Some(f) = frame_of(&self.frames, frame) else {
                    return Err("unknown frame".into());
                };
                let mut out = vars::registers(&f.regs, frame == 0);
                if frame == 0 {
                    let regs = self.call("regs.get", json!({}))?;
                    let sr = regs["sr"].as_u64().unwrap_or(0) as u16;
                    out.push(vars::status_register(&mut self.vars, sr, true));
                    if let Some(fpu) = regs.get("fpu") {
                        out.extend(vars::fpu_registers(fpu));
                    }
                } else if f.via == FrameVia::Exception {
                    // The interrupted context's SR is the one its frame
                    // stacked.
                    if let Some(stacked) = self.exception_frame {
                        out.push(vars::status_register(&mut self.vars, stacked.sr, false));
                    }
                }
                out
            }
            Node::StatusRegister { stacked } => {
                // The live SR is read again: an edit leaves the client
                // holding this reference.
                let sr = match stacked {
                    Some(sr) => sr,
                    None => self.regs()?.1,
                };
                vars::status_register_bits(sr)
            }
            Node::Locals { frame } => {
                let (Some(info), Some(f)) = (self.info.as_ref(), frame_of(&self.frames, frame))
                else {
                    return Ok(json!({"variables": []}));
                };
                let mut mem = GuestMem::new(&self.bridge);
                vars::locals(info, &mut mem, &mut self.vars, &f, frame)
            }
            Node::Globals => {
                let (Some(info), Some(f)) = (self.info.as_ref(), frame_of(&self.frames, 0)) else {
                    return Ok(json!({"variables": []}));
                };
                let mut mem = GuestMem::new(&self.bridge);
                vars::globals(info, &mut mem, &mut self.vars, &f)
            }
            Node::Chipset => vars::chipset(&self.bridge),
            Node::Typed { addr, ty, frame } => {
                let Some(info) = self.info.as_ref() else {
                    return Ok(json!({"variables": []}));
                };
                let mut mem = GuestMem::new(&self.bridge);
                vars::typed_children(info, &mut mem, &mut self.vars, addr, ty, frame)
            }
            Node::Segments => self.info.as_ref().map(vars::segments).unwrap_or_default(),
            Node::Rows(rows) => rows,
        };
        self.finish_variables(&mut vars_out);
        Ok(json!({"variables": vars_out}))
    }

    /// Apply the *View as* formats and the address values' label
    /// annotations, and drop the raw values they work from.
    fn finish_variables(&self, variables: &mut [Value]) {
        for v in variables {
            let Some(obj) = v.as_object_mut() else {
                continue;
            };
            let raw = obj.remove(vars::RAW).and_then(|r| r.as_u64());
            let bits = obj
                .remove(vars::RAW_BITS)
                .and_then(|b| b.as_u64())
                .map_or(32, |b| b as u32);
            let Some(raw) = raw else {
                continue;
            };
            let name = obj.get("name").and_then(Value::as_str).unwrap_or("");
            if let Some(format) = self.formats.get(name) {
                obj.insert("value".into(), Value::from(format.render(raw, bits)));
            }
            if obj.get("type").and_then(Value::as_str) == Some("address") {
                if let Some(label) = self.label_of(raw as u32) {
                    let value = obj.get("value").and_then(Value::as_str).unwrap_or("");
                    let value = format!("{value} {label}");
                    obj.insert("value".into(), Value::from(value));
                }
            }
        }
    }

    /// `<symbol+$offset>` for an address inside the program.
    fn label_of(&self, addr: u32) -> Option<String> {
        let (symbol, offset) = self.info.as_ref()?.symbol_at(addr)?;
        Some(if offset == 0 {
            format!("<{}>", symbol.name)
        } else {
            format!("<{}+${offset:X}>", symbol.name)
        })
    }

    fn set_variable(&mut self, args: &Value) -> Result<Value, String> {
        self.require_paused()?;
        let reference = args["variablesReference"].as_i64().unwrap_or(0);
        let name = args["name"].as_str().unwrap_or("").to_string();
        let text = args["value"].as_str().unwrap_or("");
        let value = self.eval_number(without_label(text))?;
        match self.vars.get(reference).cloned() {
            Some(Node::Registers { frame: 0 }) => {
                let reg = eval::register_number(&name).ok_or("not a register")?;
                self.call("regs.set", json!({"reg": name, "value": value as u32}))?;
                self.frames.clear();
                let _ = reg;
                let text = self
                    .rendered_value(reference, &name)
                    .unwrap_or_else(|| format!("0x{:08X}", value as u32));
                Ok(json!({"value": text}))
            }
            Some(Node::Registers { .. }) => {
                Err("only the innermost frame's registers can be set".into())
            }
            Some(Node::Locals { .. } | Node::Globals | Node::Typed { .. }) => {
                let (addr, size) =
                    if let Some(Node::Typed { addr, ty, .. }) = self.vars.get(reference).cloned() {
                        let info = self.info.as_ref().ok_or("no debug information")?;
                        let member = info
                            .resolve_type(Some(ty))
                            .and_then(|t| info.types.get(t))
                            .and_then(|t| match t {
                                crate::debuginfo::TypeDesc::Struct { members, .. } => {
                                    members.iter().find(|m| m.name == name).cloned()
                                }
                                _ => None,
                            })
                            .ok_or("not a settable member")?;
                        (
                            addr.wrapping_add(member.offset),
                            info.type_size(member.ty).unwrap_or(4),
                        )
                    } else {
                        self.variable_place(&name)
                            .ok_or("variable has no address")?
                    };
                let bytes = match size {
                    1 => vec![value as u8],
                    2 => (value as u16).to_be_bytes().to_vec(),
                    4 => (value as u32).to_be_bytes().to_vec(),
                    _ => return Err(format!("cannot set a {size}-byte value")),
                };
                self.call(
                    "mem.write",
                    json!({"addr": addr, "data": proto::encode_hex(&bytes), "encoding": "hex"}),
                )?;
                let text = self
                    .rendered_value(reference, &name)
                    .unwrap_or_else(|| format!("{value}"));
                Ok(json!({"value": text}))
            }
            _ => Err("not settable".into()),
        }
    }

    /// `name`'s value as a `variables` request on `reference` renders it
    /// now, so an edit's reply keeps the entry's *View as* format and
    /// label.
    fn rendered_value(&mut self, reference: i64, name: &str) -> Option<String> {
        let listing = self
            .variables(&json!({"variablesReference": reference}))
            .ok()?;
        listing["variables"]
            .as_array()?
            .iter()
            .find(|v| v["name"] == name)?["value"]
            .as_str()
            .map(str::to_string)
    }

    // -----------------------------------------------------------------
    // Evaluation

    fn eval_number(&mut self, text: &str) -> Result<i64, String> {
        let expr = eval::parse(text)?;
        self.ensure_frames()?;
        let frame = self.frames.first().copied().unwrap_or_default();
        let mut env = EvalEnv {
            session: self,
            frame,
        };
        eval::eval(&expr, &mut env)
    }

    fn evaluate(&mut self, args: &Value) -> Result<Value, String> {
        let text = args["expression"].as_str().unwrap_or("").trim().to_string();
        if let Some(raw) = text.strip_prefix('!') {
            return self.evaluate_raw(raw);
        }
        if text.is_empty() {
            return Err("empty expression".into());
        }
        let context = args["context"].as_str().unwrap_or("repl");
        let command = match console::parse(&text).transpose()? {
            // Help is a Debug Console command. In Hover and Watch a bare
            // `h` or `H` is a name, such as a label called h.
            Some(Command::Help(_)) if context != "repl" => None,
            command => command,
        };
        if let Some(Command::Help(topic)) = &command {
            return Ok(json!({"result": console::help(topic.as_deref()), "variablesReference": 0}));
        }
        if self.running() {
            return Err("the machine is running".into());
        }
        let (index, frame) = match args["frameId"].as_i64() {
            Some(id) => {
                self.ensure_frames()?;
                self.frame(id)?
            }
            None => {
                self.ensure_frames()?;
                (0, self.frames.first().copied().unwrap_or_default())
            }
        };
        if let Some(command) = command {
            return self.console_command(command, frame, context);
        }
        let format = self.formats.get(&text).copied();
        // A bare variable name renders with its type, like the
        // Variables view.
        let is_name = text.chars().all(|c| c.is_alphanumeric() || c == '_')
            && eval::register_number(&text).is_none();
        if is_name {
            if let Some(info) = self.info.as_ref() {
                if let Some(v) = info
                    .variable_at(&text, vars::lookup_pc(&frame, index))
                    .cloned()
                {
                    let function = info.function_at(vars::lookup_pc(&frame, index));
                    let place = vars::place_of(info, &frame, function, &v.location);
                    let mut mem = GuestMem::new(&self.bridge);
                    let value = vars::variable_value(
                        info,
                        &mut mem,
                        &mut self.vars,
                        &text,
                        place,
                        v.ty,
                        &frame,
                        index,
                    );
                    let mut body = json!({
                        "result": value["value"],
                        "type": value["type"],
                        "variablesReference": value["variablesReference"],
                    });
                    if let (Some(format), Some(raw)) = (format, value[vars::RAW].as_u64()) {
                        let bits = value[vars::RAW_BITS].as_u64().map_or(32, |b| b as u32);
                        body["result"] = Value::from(format.render(raw, bits));
                    }
                    if let Some(m) = value.get("memoryReference") {
                        body["memoryReference"] = m.clone();
                    }
                    return Ok(body);
                }
            }
        }
        let expr = eval::parse(&text)?;
        let value = {
            let mut env = EvalEnv {
                session: self,
                frame,
            };
            eval::eval(&expr, &mut env)?
        };
        let unsigned = value as u32;
        let mut result = match format {
            // SR at the width the Registers view gives it.
            Some(format) if expr == eval::Expr::Register(16) => {
                format.render(u64::from(unsigned & 0xFFFF), 16)
            }
            Some(format) => format.render(u64::from(unsigned), 32),
            None => format!("0x{unsigned:X} ({value})"),
        };
        let address_register = matches!(expr, eval::Expr::Register(8..=15 | 17));
        if address_register {
            if let Some(label) = self.label_of(unsigned) {
                result = format!("{result} {label}");
            }
        }
        let mut body = json!({
            "result": result,
            "variablesReference": 0,
        });
        if is_name || matches!(expr, eval::Expr::Register(8..=15)) || value >= 0 {
            body["memoryReference"] = Value::from(format!("0x{unsigned:X}"));
        }
        if is_name {
            if let Some(sym) = self.info.as_ref().and_then(|i| i.symbol_at(unsigned)) {
                if sym.1 == 0 {
                    body["type"] = Value::from("symbol");
                }
            }
        }
        // As with uae-dap, a hover or Watch entry on a label, and a
        // Watch entry on an address register, expands to the memory
        // there. (A bare name that reaches here is a symbol: a variable
        // returned above.)
        let dump = match context {
            "hover" => Some(self.hover_format),
            "watch" => Some(self.watch_format),
            _ => None,
        }
        .filter(|f| f.length > 0)
        .filter(|_| {
            matches!(expr, eval::Expr::Name(_))
                || (context == "watch" && matches!(expr, eval::Expr::Register(8..=15)))
        });
        if let Some(f) = dump {
            if let Ok(rows) = self.memory_rows(unsigned, f.length, f.word, f.row, f.mode) {
                body["variablesReference"] = Value::from(self.rows_reference(rows));
            }
        }
        Ok(body)
    }

    /// An expression's value in `frame`.
    fn eval_at(&mut self, text: &str, frame: Frame) -> Result<i64, String> {
        let expr = eval::parse(text)?;
        let mut env = EvalEnv {
            session: self,
            frame,
        };
        eval::eval(&expr, &mut env)
    }

    /// An expression as a 32-bit address.
    fn address_at(&mut self, text: &str, frame: Frame) -> Result<u32, String> {
        let value = self.eval_at(text, frame)?;
        u32::try_from(value)
            .or_else(|_| i32::try_from(value).map(|v| v as u32))
            .map_err(|_| format!("{text} is not an address"))
    }

    /// An optional size argument, `default` when absent.
    fn size_at(
        &mut self,
        text: Option<&str>,
        frame: Frame,
        what: &str,
        default: u32,
        max: u32,
    ) -> Result<u32, String> {
        match text {
            Some(text) => {
                let value = self.eval_at(text, frame)?;
                console::checked_size(value, what, max)
            }
            None => Ok(default),
        }
    }

    /// The rows as variables behind a new reference.
    fn rows_reference(&mut self, rows: Vec<Row>) -> i64 {
        let variables = rows
            .into_iter()
            .map(|row| {
                let mut v = json!({
                    "name": row.name,
                    "value": row.value,
                    "variablesReference": 0,
                    "presentationHint": {"attributes": ["readOnly"]},
                });
                if let Some(address) = row.address {
                    v["memoryReference"] = Value::from(format!("0x{address:X}"));
                }
                v
            })
            .collect();
        self.vars.add(Node::Rows(variables))
    }

    /// Run a uae-dap debugger command (`m`, `M`, `d`, `c`). In the Debug
    /// Console the output is the reply's text; elsewhere (Watch) the
    /// first row is the value and the rows are its children.
    fn console_command(
        &mut self,
        command: Command,
        frame: Frame,
        context: &str,
    ) -> Result<Value, String> {
        let rows = match command {
            Command::Dump {
                address,
                size,
                word,
                row,
                mode,
            } => {
                let addr = self.address_at(&address, frame)?;
                let size = self.size_at(size.as_deref(), frame, "size", 16, console::MAX_BYTES)?;
                let word = self.size_at(word.as_deref(), frame, "word size", 4, 16)?;
                let row = self.size_at(row.as_deref(), frame, "row size", 4, 64)?;
                match mode {
                    DumpMode::Disassemble => {
                        self.cpu_rows(addr, size, (size as usize).div_ceil(2))?
                    }
                    DumpMode::Copper => self.copper_rows(addr, size.div_ceil(4) as usize)?,
                    _ => self.memory_rows(addr, size, word as usize, row as usize, mode)?,
                }
            }
            Command::Set { address, bytes } => {
                // Watch entries are evaluated again on every refresh, and
                // the write's own invalidation would trigger one: a
                // write there would repeat for as long as the session
                // stays paused.
                if context != "repl" {
                    return Err("M writes memory only from the Debug Console".into());
                }
                let addr = self.address_at(&address, frame)?;
                let reply = self.call(
                    "mem.write",
                    json!({"addr": addr, "data": proto::encode_hex(&bytes), "encoding": "hex"}),
                )?;
                // ROM, overlay ROM and device bytes are skipped.
                let written = reply["written"].as_u64().unwrap_or(0);
                if written == 0 {
                    return Err(format!("nothing written: ${addr:08X} is not RAM"));
                }
                // Register-relative frames and cached values may have
                // moved under the write.
                self.frames.clear();
                if self.client_invalidated {
                    self.defer_event("invalidated", json!({"areas": ["variables"]}));
                }
                let result = if written < bytes.len() as u64 {
                    format!(
                        "wrote {written} of {} byte(s) at ${addr:08X}: the others are not RAM",
                        bytes.len()
                    )
                } else {
                    format!("wrote {written} byte(s) at ${addr:08X}")
                };
                return Ok(json!({"result": result, "variablesReference": 0}));
            }
            Command::Disassemble { address, size } => {
                let addr = self.address_at(&address, frame)?;
                let size = self.size_at(size.as_deref(), frame, "size", 16, console::MAX_BYTES)?;
                self.cpu_rows(addr, size, (size as usize).div_ceil(2))?
            }
            Command::Copper { address, size } => {
                let addr = match self.copper_location(&address) {
                    Some(addr) => addr?,
                    None => self.address_at(&address, frame)?,
                };
                let size = self.size_at(size.as_deref(), frame, "size", 16, console::MAX_BYTES)?;
                self.copper_rows(addr, size.div_ceil(4) as usize)?
            }
            Command::Help(topic) => {
                return Ok(
                    json!({"result": console::help(topic.as_deref()), "variablesReference": 0}),
                );
            }
        };
        if context == "repl" {
            let text = rows_text(&rows);
            return Ok(json!({"result": text.trim_end(), "variablesReference": 0}));
        }
        let first = rows
            .iter()
            .find(|r| r.address.is_some())
            .map(|r| r.value.clone())
            .unwrap_or_default();
        Ok(json!({"result": first, "variablesReference": self.rows_reference(rows)}))
    }

    /// `len` bytes from `addr`, in one or more `mem.read` calls.
    fn read_bytes(&self, addr: u32, len: u32) -> Result<Vec<u8>, String> {
        let mut data = Vec::with_capacity(len as usize);
        while data.len() < len as usize {
            let at = addr.wrapping_add(data.len() as u32);
            let want = (len as usize - data.len()).min(0x10000);
            let v = self.call(
                "mem.read",
                json!({"addr": at, "len": want, "encoding": "base64"}),
            )?;
            match v["data"].as_str().and_then(proto::decode_base64) {
                Some(bytes) if !bytes.is_empty() => data.extend_from_slice(&bytes),
                _ => break,
            }
        }
        if data.is_empty() {
            return Err(format!("cannot read ${addr:08X}"));
        }
        Ok(data)
    }

    fn memory_rows(
        &self,
        addr: u32,
        len: u32,
        word: usize,
        row: usize,
        mode: DumpMode,
    ) -> Result<Vec<Row>, String> {
        let bytes = self.read_bytes(addr, len)?;
        Ok(console::dump_rows(addr, &bytes, word, row, mode)
            .into_iter()
            .map(|(at, text)| Row::at(at, text))
            .collect())
    }

    /// The name of the program or ROM symbol that starts at `addr`.
    fn label_at(&self, addr: u32, rom_symbols: &SymbolSnapshot) -> Option<String> {
        if let Some((symbol, 0)) = self.info.as_ref().and_then(|i| i.symbol_at(addr)) {
            return Some(symbol.name.clone());
        }
        rom_symbols
            .resolve(addr)
            .filter(|symbol| symbol.address == addr)
            .map(|symbol| symbol.display_name())
    }

    /// The CPU instructions in `len` bytes from `addr` (at most `max`),
    /// a label before each that starts a symbol.
    fn cpu_rows(&mut self, addr: u32, len: u32, max: usize) -> Result<Vec<Row>, String> {
        let end = u64::from(addr) + u64::from(len);
        let rom_symbols = self.rom_symbols();
        let mut rows = Vec::new();
        for line in self.disassemble_forward(addr & !1, max.max(1))? {
            let Some(at) = line["address"].as_str().and_then(|a| parse_address(a).ok()) else {
                continue;
            };
            if u64::from(at) >= end {
                break;
            }
            if let Some(label) = self.label_at(at, &rom_symbols) {
                rows.push(Row::label(&label));
            }
            let bytes: String = line["instructionBytes"]
                .as_str()
                .unwrap_or("")
                .split_whitespace()
                .collect();
            let instruction = line["instruction"].as_str().unwrap_or("??");
            rows.push(Row::at(at, format!("{bytes:<20} {instruction}")));
        }
        if rows.is_empty() {
            return Err(format!("cannot disassemble ${addr:08X}"));
        }
        Ok(rows)
    }

    /// `1` and `2` as Copper list addresses: COP1LC and COP2LC, as
    /// uae-dap's copper disassembly takes them.
    fn copper_location(&self, which: &str) -> Option<Result<u32, String>> {
        let key = match which.trim() {
            "1" => "cop1lc",
            "2" => "cop2lc",
            _ => return None,
        };
        Some(
            self.call("copper.list", json!({"max": 1}))
                .map(|v| v[key].as_u64().unwrap_or(0) as u32),
        )
    }

    /// Up to `count` Copper instructions from `addr`, ending early at
    /// the end-of-list WAIT.
    fn copper_rows(&mut self, addr: u32, count: usize) -> Result<Vec<Row>, String> {
        const PER_CALL: usize = 256;
        let mut rows = Vec::new();
        let mut at = addr & !1;
        let mut mem = GuestMem::new(&self.bridge);
        let mut ended = false;
        while rows.len() < count && !ended {
            let want = (count - rows.len()).min(PER_CALL);
            let v = self.call("copper.list", json!({"addr": at, "max": want}))?;
            let entries = v["entries"].as_array().cloned().unwrap_or_default();
            for entry in &entries {
                let a = entry["addr"].as_u64().unwrap_or(0) as u32;
                let word = mem.read32(a);
                let words = word.map_or_else(
                    || "????????".into(),
                    |w| format!("{:04X} {:04X}", w >> 16, w & 0xFFFF),
                );
                let text = entry["text"].as_str().unwrap_or("??");
                rows.push(Row::at(a, format!("{words}  {text}")));
                at = a.wrapping_add(4);
                // The end-of-list WAIT, as copper.list stops at it: it
                // may also be a batch's last entry.
                ended = matches!(word, Some(0xFFFF_FFFE | 0xFFFF_FFFF));
            }
            if entries.len() < want {
                break;
            }
        }
        if rows.is_empty() {
            return Err(format!("cannot read a Copper list at ${addr:08X}"));
        }
        Ok(rows)
    }

    /// The text of a `.dbgasm` document the Amiga Assembly extension
    /// opens for *Disassemble memory* and *Disassemble copper memory*.
    fn disassembled_file_contents(&mut self, args: &Value) -> Result<Value, String> {
        self.require_paused()?;
        let path = args["path"]
            .as_str()
            .ok_or("disassembledFileContents needs a path")?;
        self.ensure_frames()?;
        let rows = match console::parse_document(path)? {
            console::Document::Cpu {
                frame,
                address,
                count,
            } => {
                let frame = self
                    .frames
                    .get(frame)
                    .or_else(|| self.frames.first())
                    .copied()
                    .unwrap_or_default();
                let addr = self.address_at(&address, frame)?;
                self.cpu_rows(addr, u32::MAX, count)?
            }
            console::Document::Copper { address, count } => {
                let frame = self.frames.first().copied().unwrap_or_default();
                let addr = match self.copper_location(&address) {
                    Some(addr) => addr?,
                    None => self.address_at(&address, frame)?,
                };
                self.copper_rows(addr, count)?
            }
            console::Document::Segment(index) => {
                let (base, size) = self
                    .info
                    .as_ref()
                    .filter(|i| i.relocated())
                    .and_then(|i| Some((*i.bases().get(index)?, i.hunks.get(index)?.size)))
                    .ok_or_else(|| format!("the program has no segment {index}"))?;
                let max = (size as usize).div_ceil(2);
                let mut rows = self.cpu_rows(base, size, max.min(SEGMENT_DOCUMENT_CAP))?;
                if max > SEGMENT_DOCUMENT_CAP
                    && rows.iter().filter(|r| r.address.is_some()).count() >= SEGMENT_DOCUMENT_CAP
                {
                    rows.push(Row {
                        name: format!(
                            "; the listing stops after {SEGMENT_DOCUMENT_CAP} instructions"
                        ),
                        value: String::new(),
                        address: None,
                    });
                }
                rows
            }
        };
        Ok(json!({"content": rows_text(&rows)}))
    }

    /// The extension's *View as Decimal / Hex / Binary* on a variable:
    /// that name shows in the format from now on, everywhere.
    fn modify_variable_format(&mut self, args: &Value) -> Result<Value, String> {
        let variable = &args["variableInfo"]["variable"];
        let name = variable["name"]
            .as_str()
            .ok_or("modifyVariableFormat needs variableInfo.variable.name")?;
        let format = args["variableDisplayFormat"]
            .as_u64()
            .and_then(NumberFormat::from_code)
            .ok_or("variableDisplayFormat must be 0 (binary), 1 (decimal) or 2 (hexadecimal)")?;
        self.formats.insert(name.to_string(), format);
        if self.client_invalidated {
            self.defer_event("invalidated", json!({"areas": ["variables"]}));
        }
        Ok(Value::Null)
    }

    /// Debug Console completions: registers, then the program's
    /// functions, symbols and globals, for the word before the cursor.
    /// The cursor is `column` of `line` (the first line when absent),
    /// and a target's `start` is a column of that line.
    fn completions(
        &mut self,
        args: &Value,
        lines_at_1: bool,
        columns_at_1: bool,
    ) -> Result<Value, String> {
        let line = args["line"]
            .as_u64()
            .map_or(0, |l| (l as usize).saturating_sub(usize::from(lines_at_1)));
        let text = args["text"]
            .as_str()
            .unwrap_or("")
            .split('\n')
            .nth(line)
            .unwrap_or("");
        let text = text.strip_suffix('\r').unwrap_or(text);
        let base = usize::from(columns_at_1);
        let utf16_len = text.encode_utf16().count();
        let column = args["column"]
            .as_u64()
            .map_or(utf16_len, |c| (c as usize).saturating_sub(base))
            .min(utf16_len);
        // The cursor as a byte offset.
        let mut cursor = text.len();
        let mut units = 0;
        for (i, c) in text.char_indices() {
            if units >= column {
                cursor = i;
                break;
            }
            units += c.len_utf16();
        }
        let before = &text[..cursor];
        let start = before
            .char_indices()
            .rev()
            .find(|(_, c)| !(c.is_alphanumeric() || *c == '_'))
            .map_or(0, |(i, c)| i + c.len_utf8());
        let word = &before[start..];
        if word.is_empty() {
            return Ok(json!({"targets": []}));
        }
        let lower = word.to_ascii_lowercase();
        let mut candidates: Vec<(String, &str)> = Vec::new();
        for i in 0..8 {
            candidates.push((format!("d{i}"), "property"));
            candidates.push((format!("a{i}"), "property"));
        }
        for name in ["sp", "fp", "pc", "sr"] {
            candidates.push((name.to_string(), "property"));
        }
        if let Some(info) = &self.info {
            for f in &info.functions {
                candidates.push((f.name.clone(), "function"));
            }
            for g in &info.globals {
                candidates.push((g.name.clone(), "variable"));
            }
            for sym in &info.symbols {
                let code = info.hunks.get(sym.at.hunk as usize).map(|h| h.kind)
                    == Some(crate::debuginfo::hunk::HunkKind::Code);
                candidates.push((sym.name.clone(), if code { "function" } else { "variable" }));
            }
        }
        candidates
            .retain(|(name, _)| name.to_ascii_lowercase().starts_with(&lower) && name != word);
        candidates.sort_by(|a, b| a.0.cmp(&b.0));
        candidates.dedup_by(|a, b| a.0 == b.0);
        let start16 = text[..start].encode_utf16().count() + base;
        let length = word.encode_utf16().count();
        let targets: Vec<Value> = candidates
            .into_iter()
            .take(100)
            .map(|(label, kind)| json!({"label": label, "type": kind, "start": start16, "length": length}))
            .collect();
        Ok(json!({"targets": targets}))
    }

    /// `!method {json}`: a raw control-protocol call from the console.
    fn evaluate_raw(&mut self, raw: &str) -> Result<Value, String> {
        let raw = raw.trim();
        let (method, params) = match raw.split_once(char::is_whitespace) {
            Some((m, p)) => (m, p.trim()),
            None => (raw, ""),
        };
        // Run control belongs to the debugger's own requests: a raw
        // `continue` would block this loop until a stop that nothing the
        // client sends could then cause, and a raw step would move the
        // machine without a `stopped` event.
        if matches!(
            method,
            "continue"
                | "run_until"
                | "step"
                | "step_over"
                | "step_out"
                | "step_copper"
                | "step_frame"
                | "reverse_step"
                | "reverse_frame"
                | "reverse_continue"
                | "pause"
                | "shutdown"
        ) {
            return Err(format!(
                "{method}: use the debugger's own run controls (continue, step, pause, stop)"
            ));
        }
        let params: Value = if params.is_empty() {
            json!({})
        } else {
            serde_json::from_str(params).map_err(|e| format!("params must be JSON: {e}"))?
        };
        let result = self.bridge.call(method, params)?;
        let text = match result {
            Reply::Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()),
            Reply::Err { code, message } => format!("error {code}: {message}"),
            Reply::TimedOut => "timed out".into(),
        };
        // A raw resume verb moves the machine; forget cached state.
        self.vars.clear();
        self.frames.clear();
        Ok(json!({"result": text, "variablesReference": 0}))
    }

    // -----------------------------------------------------------------
    // Memory, disassembly, goto

    fn read_memory(&mut self, args: &Value) -> Result<Value, String> {
        let base = parse_address(args["memoryReference"].as_str().unwrap_or(""))?;
        let offset = args["offset"].as_i64().unwrap_or(0);
        let addr = (i64::from(base) + offset) as u32;
        // A bounded read: the client asks for a view's worth, not a
        // whole address space.
        let count = args["count"].as_u64().unwrap_or(0).min(READ_MEMORY_CAP) as usize;
        let mut data = Vec::with_capacity(count);
        let mut at = addr;
        while data.len() < count {
            let len = (count - data.len()).min(0x10000);
            match self.bridge.call(
                "mem.read",
                json!({"addr": at, "len": len, "encoding": "base64"}),
            ) {
                Ok(Reply::Ok(v)) => match v["data"].as_str().and_then(proto::decode_base64) {
                    Some(bytes) if !bytes.is_empty() => {
                        at = at.wrapping_add(bytes.len() as u32);
                        data.extend_from_slice(&bytes);
                    }
                    _ => break,
                },
                _ => break,
            }
        }
        let mut body = json!({
            "address": format!("0x{addr:X}"),
            "data": proto::encode_base64(&data),
        });
        if data.len() < count {
            body["unreadableBytes"] = json!(count - data.len());
        }
        Ok(body)
    }

    fn write_memory(&mut self, args: &Value) -> Result<Value, String> {
        let base = parse_address(args["memoryReference"].as_str().unwrap_or(""))?;
        let offset = args["offset"].as_i64().unwrap_or(0);
        let addr = (i64::from(base) + offset) as u32;
        let data = args["data"]
            .as_str()
            .and_then(proto::decode_base64)
            .ok_or("data must be base64")?;
        self.call(
            "mem.write",
            json!({"addr": addr, "data": proto::encode_base64(&data), "encoding": "base64"}),
        )?;
        self.frames.clear();
        Ok(json!({"bytesWritten": data.len()}))
    }

    fn disassemble(&mut self, args: &Value, lines_at_1: bool) -> Result<Value, String> {
        let base = parse_address(args["memoryReference"].as_str().unwrap_or(""))?;
        let offset = args["offset"].as_i64().unwrap_or(0);
        let reference = ((i64::from(base) + offset) as u32) & !1;
        let instruction_offset = args["instructionOffset"]
            .as_i64()
            .unwrap_or(0)
            .clamp(-DISASSEMBLE_CAP, DISASSEMBLE_CAP);
        let count = args["instructionCount"]
            .as_u64()
            .unwrap_or(0)
            .min(DISASSEMBLE_CAP as u64) as usize;
        let rom_symbols = self.rom_symbols();
        let mut out: Vec<Value> = Vec::new();
        if instruction_offset < 0 {
            let back = instruction_offset.unsigned_abs() as usize;
            let before = self.disassemble_before(reference, back, &rom_symbols)?;
            // Pad the front when fewer than asked could be recovered,
            // with placeholder entries below the earliest real one.
            let first = before
                .first()
                .and_then(|l| l["address"].as_str())
                .and_then(|a| parse_address(a).ok())
                .unwrap_or(reference);
            let missing = back.saturating_sub(before.len());
            for i in 0..missing {
                let fake = first.wrapping_sub(2 * (missing - i) as u32);
                out.push(json!({"address": format!("0x{fake:X}"), "instruction": "??", "instructionBytes": ""}));
            }
            out.extend(before);
        }
        let skip = instruction_offset.max(0) as usize;
        let forward = count.saturating_sub(out.len()).saturating_add(skip);
        if forward > 0 {
            let lines = self.disassemble_forward(reference, forward)?;
            out.extend(lines.into_iter().skip(skip));
        }
        out.truncate(count);
        // Source locations: on the first instruction of each line run.
        let mut last_line: Option<(u32, u32)> = None;
        for entry in &mut out {
            if entry["instruction"] == "??" {
                continue;
            }
            let Some(addr) = entry["address"]
                .as_str()
                .and_then(|a| parse_address(a).ok())
            else {
                continue;
            };
            let mut program_symbol = false;
            if let Some(info) = &self.info {
                if let Some((sym, off)) = info.symbol_at(addr) {
                    if off == 0 {
                        entry["symbol"] = Value::from(sym.name.clone());
                        program_symbol = true;
                    }
                }
            }
            if !program_symbol {
                if let Some(symbol) = rom_symbols.resolve(addr) {
                    entry["symbol"] = Value::from(symbol.display_name());
                }
            }
            if let Some((source, line, _)) = self.source_at(addr) {
                let file = self
                    .info
                    .as_ref()
                    .and_then(|i| i.line_for(addr))
                    .map(|h| h.file);
                let key = (file.unwrap_or(0), line);
                if last_line != Some(key) {
                    entry["location"] = source;
                    entry["line"] = json!(if lines_at_1 {
                        line
                    } else {
                        line.saturating_sub(1)
                    });
                    last_line = Some(key);
                }
            }
        }
        Ok(json!({"instructions": out}))
    }

    /// `count` instructions from `addr` on.
    fn disassemble_forward(&mut self, addr: u32, count: usize) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        let mut at = addr;
        let mut mem = GuestMem::new(&self.bridge);
        while out.len() < count {
            let want = (count - out.len()).min(256);
            let v = self.call("disasm", json!({"addr": at, "count": want}))?;
            let lines = v["lines"].as_array().cloned().unwrap_or_default();
            if lines.is_empty() {
                break;
            }
            for line in lines {
                let a = line["addr"].as_u64().unwrap_or(0) as u32;
                let len = line["len"].as_u64().unwrap_or(2) as u32;
                let bytes = mem
                    .read(a, len as usize)
                    .map(|b| {
                        b.iter()
                            .map(|x| format!("{x:02X}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                let instruction = line["text"].as_str().unwrap_or("??");
                let timing = match (line["cycles_min"].as_u64(), line["cycles_max"].as_u64()) {
                    (Some(minimum), Some(maximum)) if minimum == maximum => {
                        format!(" ; {minimum} cyc")
                    }
                    (Some(minimum), Some(maximum)) => format!(" ; {minimum}-{maximum} cyc"),
                    _ => String::new(),
                };
                out.push(json!({
                    "address": format!("0x{a:X}"),
                    "instruction": format!("{instruction}{timing}"),
                    "instructionBytes": bytes,
                }));
                at = a.wrapping_add(len);
                if out.len() >= count {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Up to `back` instructions ending exactly at `reference`, found
    /// by disassembling forward from the nearest known instruction
    /// boundary (a function or symbol start) below it.
    fn disassemble_before(
        &mut self,
        reference: u32,
        back: usize,
        rom_symbols: &SymbolSnapshot,
    ) -> Result<Vec<Value>, String> {
        let program_anchor = self.info.as_ref().and_then(|info| {
            let f = info.function_at(reference).and_then(|f| info.runtime(f.at));
            let s = info
                .symbol_at(reference)
                .and_then(|(s, _)| info.runtime(s.at));
            match (f, s) {
                (Some(f), Some(s)) => Some(f.max(s)),
                (a, b) => a.or(b),
            }
        });
        let rom_anchor = rom_symbols.resolve(reference).map(|symbol| symbol.address);
        let anchor = match (program_anchor, rom_anchor) {
            (Some(program), Some(rom)) => Some(program.max(rom)),
            (a, b) => a.or(b),
        };
        let Some(anchor) = anchor.filter(|a| *a < reference) else {
            return Ok(Vec::new());
        };
        let lines = self.disassemble_forward(anchor, DISASSEMBLE_BACK_LIMIT)?;
        let mut collected = Vec::new();
        for line in lines {
            let a = line["address"]
                .as_str()
                .and_then(|a| parse_address(a).ok())
                .unwrap_or(0);
            if a >= reference {
                break;
            }
            collected.push(line);
        }
        // The walk must land exactly on the reference to be trusted.
        let landed = collected.last().is_some_and(|l| {
            let a = l["address"]
                .as_str()
                .and_then(|a| parse_address(a).ok())
                .unwrap_or(0);
            let bytes = l["instructionBytes"].as_str().unwrap_or("");
            let len = bytes.split(' ').filter(|b| !b.is_empty()).count() as u32;
            a + len == reference
        });
        if !landed {
            return Ok(Vec::new());
        }
        let keep = collected.len().saturating_sub(back);
        Ok(collected.split_off(keep))
    }

    fn goto_targets(&mut self, args: &Value, lines_at_1: bool) -> Result<Value, String> {
        let Some(info) = self.info.as_ref().filter(|i| i.relocated()) else {
            return Ok(json!({"targets": []}));
        };
        let path = args["source"]["path"].as_str().unwrap_or("");
        let line = u32::try_from(args["line"].as_u64().unwrap_or(0)).unwrap_or(u32::MAX);
        let line = if lines_at_1 {
            line
        } else {
            line.saturating_add(1)
        };
        let Some(file) = info.find_file(path) else {
            return Ok(json!({"targets": []}));
        };
        let Some((actual, addrs)) = info.resolve_line(file, line) else {
            return Ok(json!({"targets": []}));
        };
        let targets: Vec<Value> = addrs
            .iter()
            .map(|addr| {
                json!({
                    "id": *addr,
                    "label": format!("0x{addr:X}"),
                    "line": if lines_at_1 { actual } else { actual - 1 },
                    "instructionPointerReference": format!("0x{addr:X}"),
                })
            })
            .collect();
        Ok(json!({"targets": targets}))
    }

    fn goto(&mut self, args: &Value) -> Result<Value, String> {
        self.require_paused()?;
        let target = args["targetId"].as_u64().ok_or("goto needs a targetId")? as u32;
        self.call("regs.set", json!({"reg": "pc", "value": target}))?;
        self.vars.clear();
        self.frames.clear();
        self.defer_stopped("goto", None, &[]);
        Ok(Value::Null)
    }

    fn exception_info(&mut self) -> Result<Value, String> {
        let Some((reason, detail)) = self.last_exception.clone() else {
            return Err("no exception has been caught".into());
        };
        let mut payload = exception_info_payload(&reason, &detail);
        if let (Some(origin), Some(caught)) = (self.exception_origin(), self.exception_stop) {
            // The raw detail already names an address; start from the
            // exception's name instead.
            let base = exception_description(&detail).map_or_else(
                || {
                    format!(
                        "{} (vector {})",
                        crate::debugger::exception_vector_name(caught.vector),
                        caught.vector
                    )
                },
                str::to_string,
            );
            payload["description"] = json!(format!("{base} {origin}"));
        }
        Ok(payload)
    }

    /// Whether `frames` were unwound through a caught exception's frame.
    fn unwound_exception(&self) -> bool {
        self.exception_frame.is_some()
            && self
                .frames
                .get(1)
                .is_some_and(|frame| frame.via == FrameVia::Exception)
    }

    /// Where the exception the machine is paused at came from: the
    /// instruction, its source line when the debug information has one,
    /// and the faulted access of a bus or address error.
    fn exception_origin(&mut self) -> Option<String> {
        if self.running() || self.exception_stop.is_none() {
            return None;
        }
        self.ensure_frames().ok()?;
        if !self.unwound_exception() {
            return None;
        }
        let frame = self.frames[1];
        let mut origin = format!("at ${:06X}", frame.pc);
        if let Some((source, line, _)) = self.source_at(frame.pc) {
            let name = source["name"].as_str().unwrap_or("");
            origin.push_str(&format!(" ({name}:{line})"));
        }
        if let Some(access) = self.exception_frame.and_then(|f| f.access) {
            origin.push_str(&format!(", accessing ${access:06X}"));
        }
        Some(origin)
    }
}

/// The registers of a `regs.get` reply, and SR.
fn registers_of(v: &Value) -> (Registers, u16) {
    let mut regs = Registers::default();
    for (i, d) in v["d"].as_array().into_iter().flatten().enumerate().take(8) {
        regs.d[i] = d.as_u64().unwrap_or(0) as u32;
    }
    for (i, a) in v["a"].as_array().into_iter().flatten().enumerate().take(8) {
        regs.a[i] = a.as_u64().unwrap_or(0) as u32;
    }
    regs.pc = v["pc"].as_u64().unwrap_or(0) as u32;
    (regs, v["sr"].as_u64().unwrap_or(0) as u16)
}

/// The exception a `catch` stop report carries. `handler` is the stop's
/// own `pc`, the architectural PC that `regs.get` reports: the
/// exception's `handler` is the address the bus sees, which on a 24-bit
/// CPU drops the top byte a vector may set.
fn caught_exception_of(stop: &Value) -> Option<CaughtException> {
    if stop["reason"].as_str() != Some("catch") {
        return None;
    }
    let exception = &stop["exception"];
    Some(CaughtException {
        vector: u16::try_from(exception["vector"].as_u64()?).ok()?,
        handler: u32::try_from(stop["pc"].as_u64().or(exception["handler"].as_u64())?).ok()?,
        fault_pc: exception["fault_pc"]
            .as_u64()
            .and_then(|pc| u32::try_from(pc).ok()),
    })
}

fn exception_info_payload(reason: &str, detail: &str) -> Value {
    let id = detail
        .split("(vector ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .map(|v| format!("vector {v}"))
        .unwrap_or_else(|| reason.to_string());
    let description = exception_description(detail).unwrap_or(detail);
    json!({
        "exceptionId": id,
        "description": description,
        "breakMode": "always",
        "details": {"message": detail, "typeName": reason},
    })
}

fn exception_description(detail: &str) -> Option<&'static str> {
    if detail.contains("vector 3)") {
        Some("SIGBUS: address error (vector 3)")
    } else if detail.contains("vector 4)") {
        Some("SIGILL: illegal instruction (vector 4)")
    } else if detail.contains("vector 39)") {
        Some("SIGTRAP: TRAP #7 (vector 39)")
    } else {
        None
    }
}

/// The evaluator's view of the machine at one frame.
struct EvalEnv<'a> {
    session: &'a mut Session,
    frame: Frame,
}

impl eval::Env for EvalEnv<'_> {
    fn register(&mut self, reg: u16) -> Result<i64, String> {
        match reg {
            0..=15 => self
                .frame
                .regs
                .get(reg)
                .map(i64::from)
                .ok_or_else(|| "register unknown".into()),
            16 => match (self.frame.via, self.session.exception_frame) {
                // An interrupted context's SR is the one its frame stacked.
                (FrameVia::Exception, Some(stacked)) => Ok(i64::from(stacked.sr)),
                _ => Ok(i64::from(self.session.regs()?.1)),
            },
            17 => Ok(i64::from(self.frame.pc)),
            _ => Err("unknown register".into()),
        }
    }

    fn name(&mut self, name: &str) -> Result<i64, String> {
        let info = self
            .session
            .info
            .as_ref()
            .ok_or_else(|| format!("unknown name {name}"))?;
        if let Some(v) = info.variable_at(name, self.frame.pc) {
            let function = info.function_at(self.frame.pc);
            return match vars::place_of(info, &self.frame, function, &v.location)? {
                vars::Place::Memory(addr) => {
                    let size = info.type_size(v.ty).unwrap_or(4);
                    let mut mem = GuestMem::new(&self.session.bridge);
                    let signed = matches!(
                        info.resolve_type(v.ty).and_then(|t| info.types.get(t)),
                        Some(crate::debuginfo::TypeDesc::Base {
                            encoding: crate::debuginfo::Encoding::Signed
                                | crate::debuginfo::Encoding::SignedChar,
                            ..
                        })
                    );
                    read_sized(&mut mem, addr, size, signed)
                }
                vars::Place::Register(reg) => self.register(reg),
            };
        }
        if let Some(addr) = info.lookup(name) {
            return Ok(i64::from(addr));
        }
        Err(format!("unknown name {name}"))
    }

    fn read(&mut self, addr: u32, size: u8) -> Result<i64, String> {
        let mut mem = GuestMem::new(&self.session.bridge);
        read_sized(&mut mem, addr, u32::from(size), false)
    }
}

fn read_sized(mem: &mut GuestMem, addr: u32, size: u32, signed: bool) -> Result<i64, String> {
    let unreadable = || format!("cannot read 0x{addr:X}");
    Ok(match (size, signed) {
        (1, true) => i64::from(mem.read8(addr).ok_or_else(unreadable)? as i8),
        (1, false) => i64::from(mem.read8(addr).ok_or_else(unreadable)?),
        (2, true) => i64::from(mem.read16(addr).ok_or_else(unreadable)? as i16),
        (2, false) => i64::from(mem.read16(addr).ok_or_else(unreadable)?),
        (_, true) => i64::from(mem.read32(addr).ok_or_else(unreadable)? as i32),
        (_, false) => i64::from(mem.read32(addr).ok_or_else(unreadable)?),
    })
}

/// An edited value without the ` <label+$offset>` annotation an address
/// register is shown with: an IDE pre-fills its editor with the shown
/// text.
fn without_label(text: &str) -> &str {
    let text = text.trim();
    match text.rfind(" <") {
        Some(at) if text.ends_with('>') && !text[at + 2..].contains(char::is_whitespace) => {
            text[..at].trim_end()
        }
        _ => text,
    }
}

/// A DAP memory reference or address string: `0x1234`, `$1234`, `1234`.
pub fn parse_address(text: &str) -> Result<u32, String> {
    let t = text.trim();
    let parsed = if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)
    } else if let Some(hex) = t.strip_prefix('$') {
        u64::from_str_radix(hex, 16)
    } else {
        t.parse::<u64>()
    };
    parsed
        .ok()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| format!("bad address {text:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edited_values_drop_the_label_annotation() {
        assert_eq!(without_label("0x00C39FD9 <buffer+$1>"), "0x00C39FD9");
        assert_eq!(without_label(" $100 <start> "), "$100");
        assert_eq!(without_label("d0 >> 2"), "d0 >> 2");
        assert_eq!(without_label("1 << 2"), "1 << 2");
        assert_eq!(without_label("0x10"), "0x10");
    }

    #[test]
    fn addresses_parse_in_every_spelling() {
        assert_eq!(parse_address("0x1234"), Ok(0x1234));
        assert_eq!(parse_address("$C00000"), Ok(0xC0_0000));
        assert_eq!(parse_address("4096"), Ok(4096));
        assert!(parse_address("zz").is_err());
        assert!(parse_address("0x1FFFFFFFF").is_err());
    }

    #[test]
    fn catch_stops_carry_their_exception() {
        // A 24-bit CPU's vector can carry a top byte the bus drops: the
        // handler compared with regs.get is the stop's architectural PC.
        let stop = json!({
            "reason": "catch",
            "detail": "Caught Address error (vector 3) at $C0123A, handler $F80A2E",
            "pc": 0xFFF8_0A2Eu32,
            "exception": {"vector": 3, "name": "Address error", "handler": 0x00F8_0A2E, "fault_pc": 0x00C0_123A},
        });
        assert_eq!(
            caught_exception_of(&stop),
            Some(CaughtException {
                vector: 3,
                handler: 0xFFF8_0A2E,
                fault_pc: Some(0x00C0_123A),
            })
        );
        let interrupt = json!({
            "reason": "catch",
            "exception": {"vector": 27, "handler": 0x00F8_0014, "fault_pc": null},
        });
        assert_eq!(caught_exception_of(&interrupt).unwrap().fault_pc, None);
        // Only a catch carries one, and a report without it unwinds as usual.
        assert_eq!(caught_exception_of(&json!({"reason": "breakpoint"})), None);
        assert_eq!(caught_exception_of(&json!({"reason": "catch"})), None);
    }

    #[test]
    fn registers_come_from_a_regs_get_reply() {
        let reply = json!({
            "d": [1, 2, 3, 4, 5, 6, 7, 8],
            "a": [9, 10, 11, 12, 13, 14, 15, 0x3FF2],
            "pc": 0x00F8_0030,
            "sr": 0x2700,
            "usp": 0x8000,
        });
        let (regs, sr) = registers_of(&reply);
        assert_eq!(regs.d, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(regs.a[7], 0x3FF2);
        assert_eq!(regs.pc, 0x00F8_0030);
        assert_eq!(sr, 0x2700);
    }

    #[test]
    fn exception_info_keeps_the_raw_handler_detail() {
        let payload =
            exception_info_payload("exception", "Caught exception (vector 4), handler $F80010");
        assert_eq!(payload["exceptionId"], "vector 4");
        assert_eq!(
            payload["description"],
            "SIGILL: illegal instruction (vector 4)"
        );
        assert_eq!(
            payload["details"]["message"],
            "Caught exception (vector 4), handler $F80010"
        );
    }

    #[test]
    fn launch_arguments_are_validated() {
        let err = parse_launch(&json!({})).unwrap_err();
        assert!(err.contains("program"));
        let err = parse_launch(&json!({"program": "/nonexistent/prog"})).unwrap_err();
        assert!(err.contains("does not exist"));
        let dir = std::env::temp_dir().join(format!("dap-launch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prog = dir.join("prog");
        std::fs::write(&prog, b"x").unwrap();
        let args = parse_launch(&json!({
            "program": prog.display().to_string(),
            "rom": prog.display().to_string(),
            "args": ["a", "b"],
            "model": "A1200",
            "fast": "8M",
            "memoryFill": "0xDEAD",
            "fpu": true,
            "stack": 32768,
            "ntsc": true,
            "detach": true,
            "emulatorLog": true,
            "headless": true,
            "extraArgs": ["--noaudio"],
            "coverage": "/tmp/cov.info",
        }))
        .unwrap();
        assert_eq!(args.run_args.as_deref(), Some("a b"));
        assert_eq!(args.coverage.as_deref(), Some(Path::new("/tmp/cov.info")));
        assert_eq!(args.rom.as_deref(), Some(prog.as_path()));
        assert_eq!(
            args.extra,
            vec![
                "--model",
                "A1200",
                "--fast",
                "8M",
                "--ram-init",
                "0xDEAD",
                "--fpu",
                "--noaudio"
            ]
        );
        assert!(args.headless);
        assert_eq!(args.stack, Some(32_768));
        assert_eq!(args.ntsc, Some(true));
        assert!(args.detach);
        assert!(args.emulator_log);
        for bytes in [2048u64, 2049, 2_147_483_644] {
            let args = parse_launch(&json!({"program": prog, "stack": bytes})).unwrap();
            assert_eq!(args.stack.map(u64::from), Some(bytes));
        }
        for bytes in [0u64, 4, 2047, 2_147_483_645, 4_294_967_295, 4_294_967_296] {
            let err = parse_launch(&json!({"program": prog, "stack": bytes})).unwrap_err();
            assert!(
                err.contains("stack must be between 2048 and 2147483644"),
                "{err}"
            );
        }
        assert!(parse_launch(&json!({
            "program": prog.display().to_string(),
            "fpu": "yes"
        }))
        .unwrap_err()
        .contains("boolean"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
