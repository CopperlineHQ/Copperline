// SPDX-License-Identifier: GPL-3.0-or-later

//! The typed command layer of the control protocol: method+params are
//! parsed into a [`CoreOp`] (executed identically by both server modes
//! through [`exec_core`]) or a [`HostOp`] (resume verbs, input, media --
//! things each driver applies through its own boundary). Everything
//! here calls the same `ui_*` / `debug_*` / `tt_*` machinery the
//! debugger window, console, and GDB stub already share; there is no
//! second debugger implementation.

use super::observe::{EventKind, MAX_FRAME_INTERVAL};
use super::proto::{self, CtlError, StopEvent};
use super::session::{BreakSpec, InputAction, SessionCtx};
use crate::debugger::{BreakCond, CondOp, CondOperand, DebugStop, WatchSource};
use crate::emulator::Emulator;
use crate::inputsched::JoyState;
use crate::pointer::{PointerServo, ServoStep};
use crate::timetravel::ReverseOutcome;
use crate::video::{FB_WIDTH, MAX_CANVAS_PIXELS};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// Longest single memory transfer, matching the wire-line budget.
pub const MEM_TRANSFER_CAP: usize = 1024 * 1024;

/// Instruction budget for bounded run helpers (step-over, run-to-pc...),
/// mirroring the debugger window's transports.
pub const RUN_BUDGET: usize = 5_000_000;

/// Once a cck/seconds run target is within this many colour clocks
/// (about one PAL frame), the drivers finish instruction-by-instruction
/// so the stop lands at the first instruction boundary at or past the
/// target.
pub const CCK_FINE_WINDOW: u64 = 80_000;

const DEFAULT_TRACE_LINE_CAP: u64 = 1_000_000;
const MAX_TRACE_LINE_CAP: u64 = 10_000_000;

/// A parsed request, split by which layer executes it.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Core(CoreOp),
    Host(HostOp),
}

/// Commands executed directly against the `Emulator`, shared verbatim
/// by the headless driver and the windowed drain.
#[derive(Debug, Clone, PartialEq)]
pub enum CoreOp {
    Status,
    RegsGet,
    RegsSet {
        reg: usize,
        value: u32,
    },
    MemRead {
        addr: u32,
        len: usize,
        base64: bool,
    },
    MemWrite {
        addr: u32,
        data: Vec<u8>,
    },
    Disasm {
        addr: Option<u32>,
        count: usize,
    },
    CustomDump,
    CustomRead {
        off: u16,
    },
    /// Who last wrote a custom register, from the last-writer table the
    /// chipset validator arms.
    CustomWriter {
        off: u16,
    },
    /// Arm/disarm the chipset access validator, or read its report.
    ChipsetValidate {
        enabled: Option<bool>,
        clear: bool,
    },
    ChipsetReport,
    /// Arm/disarm the self-modifying-code detector, or read its report.
    SmcDetect {
        enabled: Option<bool>,
        clear: bool,
    },
    SmcReport,
    /// Arm a bus fault over an address window, list the armed ones, or
    /// clear them.
    FaultInject {
        addr: u32,
        len: u32,
        on_read: bool,
        on_write: bool,
        count: Option<u32>,
    },
    FaultList,
    FaultClear,
    /// Arm/disarm the memory heat map over a window, or read it.
    HeatMapSet {
        window: Option<(u32, u32)>,
    },
    HeatMapReport {
        path: Option<PathBuf>,
    },
    CiaGet {
        b: bool,
    },
    BeamGet,
    DisplayGet,
    InputPortsGet,
    RtcGet,
    RtcSet {
        unix: Option<u64>,
        advance: Option<i64>,
        frozen: Option<bool>,
    },
    CopperList {
        addr: Option<u32>,
        max: usize,
    },
    LastWriter {
        addr: u32,
    },
    PcHistory,
    BreakAdd(BreakSpec),
    BreakRemove {
        id: u32,
    },
    BreakList,
    BreakClear,
    FloppyQuery,
    EventsSubscribe {
        events: Vec<EventKind>,
        frame_interval: Option<u64>,
        frame_digest: Option<bool>,
    },
    EventsUnsubscribe {
        events: Option<Vec<EventKind>>,
    },
    EventsList,
    TraceStart {
        path: PathBuf,
        max_lines: u64,
    },
    TraceStop,
    TraceStatus,
    WaveformStart {
        options: crate::waveform::WaveOptions,
    },
    WaveformStop,
    WaveformStatus,
    StateSave {
        path: PathBuf,
    },
    Digest,
    RegionDigest {
        rect: FrameRect,
    },
    Screenshot {
        path: Option<PathBuf>,
    },
    ReverseStep {
        n: u64,
    },
    ReverseFrame,
    ReverseContinue,
}

impl CoreOp {
    /// Whether this op may be serviced at a quantum boundary while a
    /// resume is pending. Repositioning ops (reverse, last-writer) must
    /// not race an in-flight run.
    pub fn allowed_while_running(&self) -> bool {
        !matches!(
            self,
            CoreOp::LastWriter { .. }
                | CoreOp::ReverseStep { .. }
                | CoreOp::ReverseFrame
                | CoreOp::ReverseContinue
        )
    }

    /// Whether this op is read-only, and therefore allowed in a
    /// `collect` list evaluated at a stop.
    pub fn collectable(&self) -> bool {
        matches!(
            self,
            CoreOp::Status
                | CoreOp::RegsGet
                | CoreOp::MemRead { .. }
                | CoreOp::Disasm { .. }
                | CoreOp::CustomDump
                | CoreOp::CustomRead { .. }
                | CoreOp::CustomWriter { .. }
                | CoreOp::ChipsetReport
                | CoreOp::SmcReport
                | CoreOp::FaultList
                | CoreOp::HeatMapReport { .. }
                | CoreOp::CiaGet { .. }
                | CoreOp::BeamGet
                | CoreOp::DisplayGet
                | CoreOp::InputPortsGet
                | CoreOp::RtcGet
                | CoreOp::CopperList { .. }
                | CoreOp::PcHistory
                | CoreOp::BreakList
                | CoreOp::FloppyQuery
                | CoreOp::EventsList
                | CoreOp::TraceStatus
                | CoreOp::WaveformStatus
                | CoreOp::Digest
                | CoreOp::RegionDigest { .. }
                | CoreOp::Screenshot { .. }
        )
    }
}

/// A rectangle of presented pixels, in the coordinate space
/// `capture.screenshot` writes out: origin top-left, one unit per
/// framebuffer pixel column/row at the frame's current canvas scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl FrameRect {
    /// Digest the rectangle out of a rendered frame of `width` pixels per
    /// row and `lines` rows. A rectangle that is not wholly inside the
    /// frame is an error rather than a silent clamp: a script asserting on
    /// a region needs to know its coordinates went stale (the frame
    /// geometry changes with the beam standard and canvas scale).
    fn digest(&self, fb: &[u32], width: usize, lines: usize) -> Result<u64, CtlError> {
        // Checked rather than plain addition: the parser range-checks
        // what it builds, but FrameRect is public and this is the method
        // that indexes the framebuffer.
        let (Some(right), Some(bottom)) = (self.x.checked_add(self.w), self.y.checked_add(self.h))
        else {
            return Err(CtlError::invalid_params(
                "region overflows the address space",
            ));
        };
        if right > width || bottom > lines {
            return Err(CtlError::invalid_params(format!(
                "region {}x{}+{}+{} is outside the {width}x{lines} frame",
                self.w, self.h, self.x, self.y
            )));
        }
        let mut hash = FNV1A64_OFFSET;
        for row in self.y..bottom {
            let start = row * width;
            hash = fnv1a64_from(hash, &fb[start + self.x..start + right]);
        }
        Ok(hash)
    }
}

/// Move the guest's pointer to an absolute presented-pixel position by
/// servoing relative mouse deltas until sprite 0 lands there; see
/// [`crate::pointer`] for why absolute positioning has to be closed-loop.
///
/// `inject` hands each delta to the caller's own input path, so the motion
/// is journaled for reverse debugging and input recording exactly like a
/// human's would be.
pub fn mouse_to(
    emu: &mut Emulator,
    port: u8,
    target: (i32, i32),
    tolerance: i32,
    max_frames: u32,
    mut inject: impl FnMut(&mut Emulator, InputAction),
) -> Result<Value, CtlError> {
    let mut servo = PointerServo::new(port, target, tolerance, max_frames);
    loop {
        match servo.poll(emu.bus()) {
            ServoStep::Move { port, dx, dy } => {
                inject(emu, InputAction::MouseMove { port, dx, dy });
                emu.step_frame()
                    .map_err(|e| CtlError::internal(format!("stepping the mouse servo: {e:#}")))?;
                // step_frame reports Ok even when it ended early on a
                // breakpoint or watch, so without this the servo would
                // keep stepping and swallow the stop entirely. The stop
                // stays pending for the normal path to surface.
                if emu.machine.ui_debug_stop_pending() {
                    return Err(CtlError::invalid_state(format!(
                        "a debugger stop interrupted the pointer servo after {} frame(s); \
                         the pointer is short of ({}, {})",
                        servo.frames(),
                        target.0,
                        target.1,
                    )));
                }
            }
            ServoStep::Arrived { x, y, frames } => {
                return Ok(json!({
                    "x": x,
                    "y": y,
                    "target_x": target.0,
                    "target_y": target.1,
                    "port": u32::from(port) + 1,
                    "frames": frames,
                }))
            }
            // Not converging is an error, not an "ok, but": a script that
            // carried on would click somewhere it did not intend to.
            ServoStep::Failed(why) => return Err(CtlError::invalid_state(why)),
        }
    }
}

/// A "wait until the picture stops changing" run target: keep running
/// until `frames` consecutive rendered frames hash identically. This is
/// how a script waits for a GUI to finish drawing without guessing at an
/// emulated-seconds delay -- the guest tells you it is done by producing
/// the same picture twice.
///
/// `rect` narrows the comparison to one region, which is what makes the
/// target usable on a real Workbench screen: a blinking cursor or a
/// clock in the title bar never lets the whole frame settle, but the
/// dialog you are waiting for does. `max_frames` bounds the wait so a
/// display that never settles ends the run instead of hanging the
/// client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StableSpec {
    pub frames: u32,
    pub max_frames: Option<u64>,
    pub rect: Option<FrameRect>,
}

/// What one sample of the display told the stable-frame watcher.
pub enum StableStep {
    /// Not settled yet, and still within budget.
    Running,
    /// `frames` consecutive frames hashed identically.
    Settled(String),
    /// The run ended without settling: out of budget, or the region
    /// stopped fitting the frame. The string is the stop detail.
    GaveUp(String),
}

/// Per-run state behind [`StableSpec`], shared by both server modes so
/// the two cannot drift on what "stable" means.
#[derive(Debug, Clone)]
pub struct StableWatch {
    spec: StableSpec,
    /// Digest of the last frame sampled, and how many consecutive frames
    /// (including it) have carried that digest.
    last: Option<u64>,
    run: u32,
    /// Longest run seen, reported when the budget runs out: "it got to 3
    /// of 8" is a much more useful failure than a bare timeout.
    longest: u32,
    seen: u64,
}

impl StableWatch {
    pub fn new(spec: StableSpec) -> Self {
        Self {
            spec,
            last: None,
            run: 0,
            longest: 0,
            seen: 0,
        }
    }

    /// Sample the currently rendered frame. Call exactly once per
    /// emulated frame; the first call establishes the baseline, so a
    /// `frames: 2` target settles on the first repeat.
    pub fn sample(&mut self, emu: &Emulator) -> StableStep {
        let (fb, lines, width) = render_frame(emu);
        let digest = match self.spec.rect {
            None => fnv1a64(&fb[..width * lines]),
            Some(rect) => match rect.digest(&fb, width, lines) {
                Ok(digest) => digest,
                Err(_) => {
                    return StableStep::GaveUp(format!(
                        "region {}x{}+{}+{} does not fit the {width}x{lines} frame",
                        rect.w, rect.h, rect.x, rect.y
                    ))
                }
            },
        };
        self.note(digest)
    }

    /// Fold one frame's digest into the run, separately from rendering it
    /// so the settle/give-up state machine is testable on its own.
    fn note(&mut self, digest: u64) -> StableStep {
        self.seen += 1;
        self.run = if self.last == Some(digest) {
            self.run + 1
        } else {
            1
        };
        self.last = Some(digest);
        self.longest = self.longest.max(self.run);
        if self.run >= self.spec.frames {
            return StableStep::Settled(format!(
                "stable for {} frame(s) after {} (digest {digest:016x})",
                self.run, self.seen
            ));
        }
        match self.spec.max_frames {
            Some(max) if self.seen >= max => StableStep::GaveUp(format!(
                "not stable within {max} frame(s) (longest run {} of {})",
                self.longest, self.spec.frames
            )),
            _ => StableStep::Running,
        }
    }
}

/// Commands the drivers execute through their own boundary: run control
/// (whose responses are deferred to the stop), input, media, state
/// restore, and reset.
#[derive(Debug, Clone, PartialEq)]
pub enum HostOp {
    Pause,
    Resume(ResumeVerb),
    Input(InputCmd),
    FloppyInsert {
        drive: usize,
        path: PathBuf,
        write_protected: bool,
    },
    FloppyEject {
        drive: usize,
    },
    CdInsert {
        path: PathBuf,
    },
    CdEject,
    /// Hot-plug a controller device into a port (0 = port 1, 1 = port 2).
    /// Releases every line the previous device drove; not journaled for
    /// reverse replay (like a media change, it is host state).
    SetPortDevice {
        port: u8,
        device: crate::bus::PortDevice,
    },
    StateLoad {
        path: PathBuf,
    },
    Reset {
        warm: bool,
    },
    /// Servo the guest pointer to an absolute presented-pixel position;
    /// see [`mouse_to`]. A host op because it both injects input and runs
    /// the machine, so each driver applies it through its own boundary.
    MouseTo {
        port: u8,
        x: i32,
        y: i32,
        tolerance: i32,
        max_frames: u32,
    },
}

/// A resume-type command: the machine runs and the JSON-RPC response is
/// the eventual [`StopEvent`], with `collect` evaluated at the stop.
#[derive(Debug, Clone, PartialEq)]
pub struct ResumeVerb {
    pub kind: ResumeKind,
    pub collect: Vec<CoreOp>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResumeKind {
    Continue,
    Step { n: u32 },
    StepOver,
    StepOut,
    StepCopper,
    StepFrame { n: u32 },
    RunUntil(RunTarget),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RunTarget {
    Pc(u32),
    Beam { vpos: u16, hpos: Option<u16> },
    Frame(u64),
    Cck(u64),
    Seconds(f64),
    Stable(StableSpec),
}

impl RunTarget {
    pub fn describe(&self) -> String {
        match self {
            RunTarget::Stable(spec) => format!("{} stable frame(s)", spec.frames),
            RunTarget::Pc(pc) => format!("pc ${pc:06X}"),
            RunTarget::Beam { vpos, hpos } => format!(
                "beam v{vpos}{}",
                hpos.map(|h| format!(" h{h}")).unwrap_or_default()
            ),
            RunTarget::Frame(f) => format!("frame {f}"),
            RunTarget::Cck(c) => format!("cck {c}"),
            RunTarget::Seconds(s) => format!("{s}s"),
        }
    }
}

/// A parsed input command, before the driver expands it into immediate
/// and scheduled transitions. Port fields are 0-based (0 = port 1), the
/// bus convention; the wire protocol's 1-based `port` param is converted
/// at parse time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputCmd {
    Key {
        rawkey: u8,
        kind: KeyKind,
        at_seconds: Option<f64>,
    },
    Mouse {
        port: u8,
        left: Option<bool>,
        right: Option<bool>,
        middle: Option<bool>,
        dx: i32,
        dy: i32,
    },
    Joy {
        port: u8,
        state: JoyState,
    },
    Analogue {
        port: u8,
        x: u8,
        y: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyKind {
    Press,
    Release,
    /// Press now, release after `hold_ms` of emulated time.
    Tap {
        hold_ms: u32,
    },
}

impl InputCmd {
    /// Expand into (immediate transitions, scheduled transitions),
    /// given the current emulated time. Shared by both drivers so tap
    /// and `at_seconds` semantics cannot diverge.
    pub fn expand(&self, now_secs: f64) -> (Vec<InputAction>, Vec<super::session::ScheduledInput>) {
        let mut now = Vec::new();
        let mut later = Vec::new();
        let mut emit = |at: Option<f64>, action: InputAction| match at {
            Some(t) if t > now_secs => {
                later.push(super::session::ScheduledInput {
                    at_seconds: t,
                    action,
                });
            }
            _ => now.push(action),
        };
        match *self {
            InputCmd::Key {
                rawkey,
                kind,
                at_seconds,
            } => match kind {
                KeyKind::Press => emit(
                    at_seconds,
                    InputAction::Key {
                        rawkey,
                        pressed: true,
                    },
                ),
                KeyKind::Release => emit(
                    at_seconds,
                    InputAction::Key {
                        rawkey,
                        pressed: false,
                    },
                ),
                KeyKind::Tap { hold_ms } => {
                    let press_at = at_seconds.unwrap_or(now_secs);
                    emit(
                        at_seconds,
                        InputAction::Key {
                            rawkey,
                            pressed: true,
                        },
                    );
                    emit(
                        Some(press_at + f64::from(hold_ms) / 1000.0),
                        InputAction::Key {
                            rawkey,
                            pressed: false,
                        },
                    );
                }
            },
            InputCmd::Mouse {
                port,
                left,
                right,
                middle,
                dx,
                dy,
            } => {
                for (index, state) in [(0u8, left), (1, right), (2, middle)] {
                    if let Some(pressed) = state {
                        emit(
                            None,
                            InputAction::MouseButton {
                                port,
                                index,
                                pressed,
                            },
                        );
                    }
                }
                if dx != 0 || dy != 0 {
                    emit(None, InputAction::MouseMove { port, dx, dy });
                }
            }
            InputCmd::Joy { port, state } => emit(None, InputAction::Joy { port, state }),
            InputCmd::Analogue { port, x, y } => emit(None, InputAction::Pot { port, x, y }),
        }
        (now, later)
    }
}

// ---------------------------------------------------------------------
// Parsing

/// Parse the optional 1-based `port` param (1 or 2), defaulting to
/// `default`, into the bus's 0-based port index.
fn parse_port_param(p: &ParamReader, default: u32) -> Result<u8, CtlError> {
    let port = p.u32_or("port", default)?;
    if !(1..=2).contains(&port) {
        return Err(CtlError::invalid_params("port must be 1 or 2"));
    }
    Ok((port - 1) as u8)
}

/// Parse a required 1-based `port` param into the 0-based port index.
fn parse_port_req(p: &ParamReader) -> Result<u8, CtlError> {
    let port = p.u32_req("port")?;
    if !(1..=2).contains(&port) {
        return Err(CtlError::invalid_params("port must be 1 or 2"));
    }
    Ok((port - 1) as u8)
}

/// Parse a method name and params object into a typed request.
pub fn parse_method(method: &str, params: &Value) -> Result<Request, CtlError> {
    let p = ParamReader::new(params)?;
    let core = |op: CoreOp| Ok(Request::Core(op));
    let host = |op: HostOp| Ok(Request::Host(op));
    match method {
        "status" => core(CoreOp::Status),
        "pause" => host(HostOp::Pause),
        "continue" => resume(ResumeKind::Continue, &p),
        "step" => resume(
            ResumeKind::Step {
                n: p.u32_or("n", 1)?.clamp(1, 1_000_000),
            },
            &p,
        ),
        "step_over" => resume(ResumeKind::StepOver, &p),
        "step_out" => resume(ResumeKind::StepOut, &p),
        "step_copper" => resume(ResumeKind::StepCopper, &p),
        "step_frame" => resume(
            ResumeKind::StepFrame {
                n: p.u32_or("n", 1)?.clamp(1, 1_000_000),
            },
            &p,
        ),
        "run_until" => resume(ResumeKind::RunUntil(parse_run_target(&p)?), &p),
        "reverse_step" => core(CoreOp::ReverseStep {
            n: u64::from(p.u32_or("n", 1)?.max(1)),
        }),
        "reverse_frame" => core(CoreOp::ReverseFrame),
        "reverse_continue" => core(CoreOp::ReverseContinue),
        "regs.get" => core(CoreOp::RegsGet),
        "regs.set" => core(CoreOp::RegsSet {
            reg: parse_reg_name(&p.str_req("reg")?)?,
            value: p.u32_req("value")?,
        }),
        "mem.read" => {
            let len = p.usize_or("len", 2)?;
            if len == 0 || len > MEM_TRANSFER_CAP {
                return Err(CtlError::invalid_params(format!(
                    "len must be 1..={MEM_TRANSFER_CAP}"
                )));
            }
            core(CoreOp::MemRead {
                addr: p.u32_req("addr")?,
                len,
                base64: match p.str_opt("encoding")?.as_deref() {
                    None | Some("hex") => false,
                    Some("base64") => true,
                    Some(other) => {
                        return Err(CtlError::invalid_params(format!(
                            "unknown encoding: {other}"
                        )))
                    }
                },
            })
        }
        "mem.write" => {
            let data = p.str_req("data")?;
            let bytes = match p.str_opt("encoding")?.as_deref() {
                None | Some("hex") => proto::decode_hex(&data),
                Some("base64") => proto::decode_base64(&data),
                Some(other) => {
                    return Err(CtlError::invalid_params(format!(
                        "unknown encoding: {other}"
                    )))
                }
            };
            let Some(bytes) = bytes else {
                return Err(CtlError::invalid_params("malformed data payload"));
            };
            if bytes.is_empty() || bytes.len() > MEM_TRANSFER_CAP {
                return Err(CtlError::invalid_params(format!(
                    "data must be 1..={MEM_TRANSFER_CAP} bytes"
                )));
            }
            core(CoreOp::MemWrite {
                addr: p.u32_req("addr")?,
                data: bytes,
            })
        }
        "disasm" => core(CoreOp::Disasm {
            addr: p.u32_opt("addr")?,
            count: p.usize_or("count", 16)?.clamp(1, 256),
        }),
        "custom.dump" => core(CoreOp::CustomDump),
        "custom.read" => core(CoreOp::CustomRead {
            off: parse_custom_reg_param(&p)?,
        }),
        "custom.writer" => core(CoreOp::CustomWriter {
            off: parse_custom_reg_param(&p)?,
        }),
        "chipset.validate" => core(CoreOp::ChipsetValidate {
            enabled: p.bool_opt("enabled")?,
            clear: p.bool_or("clear", false)?,
        }),
        "chipset.report" => core(CoreOp::ChipsetReport),
        "smc.detect" => core(CoreOp::SmcDetect {
            enabled: p.bool_opt("enabled")?,
            clear: p.bool_or("clear", false)?,
        }),
        "smc.report" => core(CoreOp::SmcReport),
        "fault.inject" => {
            let (on_read, on_write) = match p.str_opt("on")?.as_deref() {
                None | Some("both") => (true, true),
                Some("read") => (true, false),
                Some("write") => (false, true),
                Some(other) => {
                    return Err(CtlError::invalid_params(format!(
                        "on must be read|write|both, got {other}"
                    )))
                }
            };
            let len = p.u32_or("len", 2)?;
            if len == 0 {
                return Err(CtlError::invalid_params("len must be non-zero"));
            }
            let addr = p.u32_req("addr")?;
            // A window whose end wraps would match nothing at all, so the
            // fault would silently never fire.
            if addr.checked_add(len - 1).is_none() {
                return Err(CtlError::invalid_params(
                    "addr + len overflows the address space",
                ));
            }
            core(CoreOp::FaultInject {
                addr,
                len,
                on_read,
                on_write,
                count: match p.u32_opt("count")? {
                    // A zero-shot injection is armed and inert; saying so
                    // beats installing something that never fires.
                    Some(0) => return Err(CtlError::invalid_params("count must be at least 1")),
                    other => other,
                },
            })
        }
        "memory.heatmap" => {
            let enabled = p.bool_or("enabled", true)?;
            // Parsed outside the `then`, and whether or not the call is
            // arming: inside a closure the error had nowhere to go and
            // was being swallowed into the default, which turns a
            // client's typo into a silently wrong window.
            let base = p.u32_or("base", 0)?;
            let span = p.u32_or("span", crate::heatmap::DEFAULT_SPAN)?;
            core(CoreOp::HeatMapSet {
                window: enabled.then_some((base, span)),
            })
        }
        "memory.heatmap.report" => core(CoreOp::HeatMapReport {
            path: p.str_opt("path")?.map(PathBuf::from),
        }),
        "fault.list" => core(CoreOp::FaultList),
        "fault.clear" => core(CoreOp::FaultClear),
        "cia.get" => core(CoreOp::CiaGet {
            b: match p.str_req("cia")?.as_str() {
                "a" | "A" => false,
                "b" | "B" => true,
                other => {
                    return Err(CtlError::invalid_params(format!(
                        "cia must be \"a\" or \"b\", got {other}"
                    )))
                }
            },
        }),
        "beam.get" => core(CoreOp::BeamGet),
        "display.get" => core(CoreOp::DisplayGet),
        "rtc.get" => core(CoreOp::RtcGet),
        "rtc.set" => {
            let unix = p.u64_opt("unix")?;
            let time = match p.str_opt("time")? {
                Some(s) => Some(
                    crate::rtc::parse_rtc_time(&s)
                        .map_err(|e| CtlError::invalid_params(format!("time: {e}")))?,
                ),
                None => None,
            };
            let advance = p.i64_opt("advance")?;
            let frozen = p.bool_opt("frozen")?;
            let absolute = match (unix, time) {
                (Some(_), Some(_)) => {
                    return Err(CtlError::invalid_params("give unix or time, not both"))
                }
                (u, t) => u.or(t),
            };
            if absolute.is_some() && advance.is_some() {
                return Err(CtlError::invalid_params(
                    "give an absolute time or advance, not both",
                ));
            }
            if absolute.is_none() && advance.is_none() && frozen.is_none() {
                return Err(CtlError::invalid_params(
                    "nothing to set: give unix, time, advance, or frozen",
                ));
            }
            core(CoreOp::RtcSet {
                unix: absolute,
                advance,
                frozen,
            })
        }
        "copper.list" => core(CoreOp::CopperList {
            addr: p.u32_opt("addr")?,
            max: p.usize_or("max", 32)?.clamp(1, 256),
        }),
        "last_writer" => core(CoreOp::LastWriter {
            addr: p.u32_req("addr")?,
        }),
        "pc_history" => core(CoreOp::PcHistory),
        "break.add" => core(CoreOp::BreakAdd(parse_break_spec(&p)?)),
        "break.remove" => core(CoreOp::BreakRemove {
            id: p.u32_req("id")?,
        }),
        "break.list" => core(CoreOp::BreakList),
        "break.clear" => core(CoreOp::BreakClear),
        "input.key" => {
            let rawkey = p.u32_req("rawkey")?;
            if rawkey > 0xFF {
                return Err(CtlError::invalid_params("rawkey must be 0..=255"));
            }
            let kind = match p.str_opt("action")?.as_deref() {
                None | Some("tap") => KeyKind::Tap {
                    hold_ms: p.u32_or("hold_ms", 80)?,
                },
                Some("press") => KeyKind::Press,
                Some("release") => KeyKind::Release,
                Some(other) => {
                    return Err(CtlError::invalid_params(format!(
                        "action must be press|release|tap, got {other}"
                    )))
                }
            };
            host(HostOp::Input(InputCmd::Key {
                rawkey: rawkey as u8,
                kind,
                at_seconds: match p.f64_opt("at_seconds")? {
                    Some(t) if !t.is_finite() => {
                        return Err(CtlError::invalid_params(
                            "at_seconds must be a finite number",
                        ))
                    }
                    other => other,
                },
            }))
        }
        "input.mouse" => host(HostOp::Input(InputCmd::Mouse {
            port: parse_port_param(&p, 1)?,
            left: p.bool_opt("left")?,
            right: p.bool_opt("right")?,
            middle: p.bool_opt("middle")?,
            dx: p.i32_or("dx", 0)?,
            dy: p.i32_or("dy", 0)?,
        })),
        "input.mouse_to" => host(HostOp::MouseTo {
            port: parse_port_param(&p, 1)?,
            x: parse_pointer_target(&p, "x")?,
            y: parse_pointer_target(&p, "y")?,
            tolerance: p
                .i32_or("tolerance", crate::pointer::DEFAULT_TOLERANCE)?
                .clamp(0, crate::pointer::TOLERANCE_LIMIT),
            max_frames: p
                .u32_or("max_frames", crate::pointer::DEFAULT_MAX_FRAMES)?
                .clamp(1, crate::pointer::FRAME_LIMIT),
        }),
        "input.joy" => host(HostOp::Input(InputCmd::Joy {
            port: parse_port_param(&p, 2)?,
            state: JoyState {
                up: p.bool_or("up", false)?,
                down: p.bool_or("down", false)?,
                left: p.bool_or("left", false)?,
                right: p.bool_or("right", false)?,
                red: p.bool_or("red", false)? || p.bool_or("fire1", false)?,
                blue: p.bool_or("blue", false)? || p.bool_or("fire2", false)?,
                play: p.bool_or("play", false)?,
                rwd: p.bool_or("rwd", false)?,
                ffw: p.bool_or("ffw", false)?,
                green: p.bool_or("green", false)?,
                yellow: p.bool_or("yellow", false)?,
            },
        })),
        "input.analogue" => {
            let (x, y) = (p.u32_req("x")?, p.u32_req("y")?);
            if x > 0xFF || y > 0xFF {
                return Err(CtlError::invalid_params("x and y must be 0..=255"));
            }
            host(HostOp::Input(InputCmd::Analogue {
                port: parse_port_param(&p, 2)?,
                x: x as u8,
                y: y as u8,
            }))
        }
        "input.set_port" => {
            let device = p.str_req("device")?;
            let device = crate::bus::PortDevice::parse(&device).ok_or_else(|| {
                CtlError::invalid_params(format!(
                    "device must be mouse|joystick|cd32|analogue|none, got {device}"
                ))
            })?;
            host(HostOp::SetPortDevice {
                port: parse_port_req(&p)?,
                device,
            })
        }
        "input.get_ports" => core(CoreOp::InputPortsGet),
        "media.floppy.insert" => host(HostOp::FloppyInsert {
            drive: p.usize_req("drive")?,
            path: PathBuf::from(p.str_req("path")?),
            write_protected: p.bool_or("write_protected", false)?,
        }),
        "media.floppy.eject" => host(HostOp::FloppyEject {
            drive: p.usize_req("drive")?,
        }),
        "media.floppy.query" => core(CoreOp::FloppyQuery),
        "media.cd.insert" => host(HostOp::CdInsert {
            path: PathBuf::from(p.str_req("path")?),
        }),
        "media.cd.eject" => host(HostOp::CdEject),
        "events.subscribe" => {
            let events = parse_event_list(&p, true)?.expect("required event list");
            let frame_interval = p.u64_opt("frame_interval")?;
            if frame_interval.is_some_and(|interval| interval == 0 || interval > MAX_FRAME_INTERVAL)
            {
                return Err(CtlError::invalid_params(format!(
                    "frame_interval must be 1..={MAX_FRAME_INTERVAL}"
                )));
            }
            core(CoreOp::EventsSubscribe {
                events,
                frame_interval,
                frame_digest: p.bool_opt("frame_digest")?,
            })
        }
        "events.unsubscribe" => core(CoreOp::EventsUnsubscribe {
            events: parse_event_list(&p, false)?,
        }),
        "events.list" => core(CoreOp::EventsList),
        "trace.start" => {
            let max_lines = p.u64_opt("max_lines")?.unwrap_or(DEFAULT_TRACE_LINE_CAP);
            if max_lines == 0 || max_lines > MAX_TRACE_LINE_CAP {
                return Err(CtlError::invalid_params(format!(
                    "max_lines must be 1..={MAX_TRACE_LINE_CAP}"
                )));
            }
            core(CoreOp::TraceStart {
                path: p
                    .str_opt("path")?
                    .map(PathBuf::from)
                    .unwrap_or_else(default_trace_path),
                max_lines,
            })
        }
        "trace.stop" => core(CoreOp::TraceStop),
        "trace.status" => core(CoreOp::TraceStatus),
        "waveform.start" => {
            let mut options = crate::waveform::WaveOptions::new(
                p.str_opt("path")?
                    .map(PathBuf::from)
                    .unwrap_or_else(crate::waveform::default_wave_path),
            );
            if let Some(trigger) = p.str_opt("trigger")? {
                options.trigger = crate::waveform::parse_trigger(&trigger).ok_or_else(|| {
                    CtlError::invalid_params(
                        "bad trigger; expected now|pc=ADDR|beam=VPOS[:HPOS]|reg=OFF|time=SECS",
                    )
                })?;
            }
            if let Some(duration) = p.str_opt("duration")? {
                options.duration = crate::waveform::parse_duration(&duration).ok_or_else(|| {
                    CtlError::invalid_params("bad duration; expected Ncck|Nf|Nframes|Nms|Ns")
                })?;
            }
            if let Some(signals) = p.str_opt("signals")? {
                options.signals = crate::waveform::parse_signals(&signals).ok_or_else(|| {
                    CtlError::invalid_params(
                        "bad signals; expected beam,bus,cpu,copper,blitter,regs,irq,audio|all",
                    )
                })?;
            }
            core(CoreOp::WaveformStart { options })
        }
        "waveform.stop" => core(CoreOp::WaveformStop),
        "waveform.status" => core(CoreOp::WaveformStatus),
        "state.save" => core(CoreOp::StateSave {
            path: PathBuf::from(p.str_req("path")?),
        }),
        "state.load" => host(HostOp::StateLoad {
            path: PathBuf::from(p.str_req("path")?),
        }),
        "capture.screenshot" => core(CoreOp::Screenshot {
            path: p.str_opt("path")?.map(PathBuf::from),
        }),
        "capture.digest" => core(CoreOp::Digest),
        "capture.region_digest" => core(CoreOp::RegionDigest {
            rect: parse_frame_rect(&p)?,
        }),
        "machine.reset" => host(HostOp::Reset {
            warm: match p.str_opt("kind")?.as_deref() {
                None | Some("warm") => true,
                Some("cold") => false,
                Some(other) => {
                    return Err(CtlError::invalid_params(format!(
                        "kind must be warm|cold, got {other}"
                    )))
                }
            },
        }),
        other => Err(CtlError::method_not_found(other)),
    }
}

fn parse_event_list(
    p: &ParamReader<'_>,
    required: bool,
) -> Result<Option<Vec<EventKind>>, CtlError> {
    let Some(value) = p.get("events") else {
        return if required {
            Err(CtlError::invalid_params("missing events"))
        } else {
            Ok(None)
        };
    };
    if value.is_null() && !required {
        return Ok(None);
    }
    let Some(items) = value.as_array() else {
        return Err(CtlError::invalid_params("events must be an array"));
    };
    if items.is_empty() {
        return Err(CtlError::invalid_params("events must not be empty"));
    }
    let mut events = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.as_str() else {
            return Err(CtlError::invalid_params("event names must be strings"));
        };
        let Some(event) = EventKind::from_name(name) else {
            return Err(CtlError::invalid_params(format!(
                "unknown event {name}; expected frame|serial|interrupt|media"
            )));
        };
        if !events.contains(&event) {
            events.push(event);
        }
    }
    Ok(Some(events))
}

fn resume(kind: ResumeKind, p: &ParamReader) -> Result<Request, CtlError> {
    Ok(Request::Host(HostOp::Resume(ResumeVerb {
        kind,
        collect: parse_collect(p)?,
    })))
}

fn parse_collect(p: &ParamReader) -> Result<Vec<CoreOp>, CtlError> {
    let Some(items) = p.get("collect") else {
        return Ok(Vec::new());
    };
    let Some(items) = items.as_array() else {
        return Err(CtlError::invalid_params("collect must be an array"));
    };
    let mut ops = Vec::with_capacity(items.len());
    for item in items {
        let Some(method) = item.get("method").and_then(Value::as_str) else {
            return Err(CtlError::invalid_params(
                "collect entries are {method, params?} objects",
            ));
        };
        let params = item.get("params").cloned().unwrap_or(Value::Null);
        match parse_method(method, &params)? {
            Request::Core(op) if op.collectable() => ops.push(op),
            _ => {
                return Err(CtlError::invalid_params(format!(
                    "method not allowed in collect: {method}"
                )))
            }
        }
    }
    Ok(ops)
}

/// Largest pointer target accepted, in presented pixels. The canvas is
/// at most 1432 wide and 626 tall; this leaves generous room around that
/// while keeping the servo's error arithmetic away from i32's edges,
/// where `target - at` would overflow and `abs()` of i32::MIN stays
/// negative -- which would satisfy the arrival test at a wild position.
const POINTER_TARGET_LIMIT: i32 = 1 << 16;

/// Parse one axis of a pointer target.
fn parse_pointer_target(p: &ParamReader, key: &str) -> Result<i32, CtlError> {
    let value = p.i32_req(key)?;
    if !(-POINTER_TARGET_LIMIT..=POINTER_TARGET_LIMIT).contains(&value) {
        return Err(CtlError::invalid_params(format!(
            "{key} must be within +/-{POINTER_TARGET_LIMIT} presented pixels"
        )));
    }
    Ok(value)
}

/// Parse the `x`/`y`/`w`/`h` params of a frame region. The rectangle is
/// bounds-checked against the live frame at digest time, not here: the
/// geometry is not known until the frame is rendered.
fn parse_frame_rect(p: &ParamReader) -> Result<FrameRect, CtlError> {
    let rect = FrameRect {
        x: p.usize_or("x", 0)?,
        y: p.usize_or("y", 0)?,
        w: p.usize_req("w")?,
        h: p.usize_req("h")?,
    };
    if rect.w == 0 || rect.h == 0 {
        return Err(CtlError::invalid_params("w and h must be non-zero"));
    }
    if rect.x.checked_add(rect.w).is_none() || rect.y.checked_add(rect.h).is_none() {
        return Err(CtlError::invalid_params(
            "region overflows the address space",
        ));
    }
    Ok(rect)
}

fn parse_run_target(p: &ParamReader) -> Result<RunTarget, CtlError> {
    let mut targets = Vec::new();
    if let Some(pc) = p.u32_opt("pc")? {
        targets.push(RunTarget::Pc(pc));
    }
    if let Some(vpos) = p.u16_opt("vpos")? {
        targets.push(RunTarget::Beam {
            vpos,
            hpos: p.u16_opt("hpos")?,
        });
    }
    if let Some(frame) = p.u64_opt("frame")? {
        targets.push(RunTarget::Frame(frame));
    }
    if let Some(cck) = p.u64_opt("cck")? {
        targets.push(RunTarget::Cck(cck));
    }
    if let Some(secs) = p.f64_opt("seconds")? {
        if !secs.is_finite() || secs < 0.0 {
            return Err(CtlError::invalid_params(
                "seconds must be a finite, non-negative number",
            ));
        }
        targets.push(RunTarget::Seconds(secs));
    }
    if let Some(frames) = p.u32_opt("stable_frames")? {
        if frames < 2 {
            return Err(CtlError::invalid_params(
                "stable_frames must be at least 2 (one frame is trivially stable)",
            ));
        }
        let max_frames = p.u64_opt("max_frames")?;
        if max_frames.is_some_and(|max| max < u64::from(frames)) {
            return Err(CtlError::invalid_params(
                "max_frames must not be below stable_frames",
            ));
        }
        // The region is optional here, unlike capture.region_digest: with
        // no region param at all the whole frame has to settle. But any
        // of them means a region was intended, so an incomplete one is an
        // error rather than a silent fall back to whole-frame -- `x` and
        // `y` without `w`/`h` would otherwise be quietly ignored and the
        // caller would wait on the wrong thing.
        let region_named = ["x", "y", "w", "h"].iter().any(|k| p.get(k).is_some());
        targets.push(RunTarget::Stable(StableSpec {
            frames,
            max_frames,
            rect: region_named.then(|| parse_frame_rect(p)).transpose()?,
        }));
    }
    match targets.len() {
        1 => Ok(targets.remove(0)),
        0 => Err(CtlError::invalid_params(
            "run_until needs exactly one of pc, vpos[+hpos], frame, cck, seconds, stable_frames",
        )),
        _ => Err(CtlError::invalid_params(
            "run_until takes exactly one target",
        )),
    }
}

fn parse_break_spec(p: &ParamReader) -> Result<BreakSpec, CtlError> {
    match p.str_req("kind")?.as_str() {
        "pc" => Ok(BreakSpec::Pc {
            addr: p.u32_req("addr")?,
            cond: match p.get("cond") {
                None | Some(Value::Null) => None,
                Some(cond) => Some(parse_break_cond(cond)?),
            },
            ignore: p.u32_or("ignore", 0)?,
        }),
        "watch" => {
            let source = match p.str_opt("class")? {
                None => None,
                Some(token) => Some(WatchSource::parse(&token).ok_or_else(|| {
                    CtlError::invalid_params(
                        "class must be cpu|blitter|disk|copper, or a DMA channel \
                         (bpl1..bpl8, spr0..spr7, aud0..aud3)",
                    )
                })?),
            };
            let pc = p.u32_opt("pc")?;
            // Only the CPU has an instruction behind an access, so this
            // pair describes something that cannot happen; accepting it
            // would install a watch that never fires.
            if pc.is_some() && source.is_some_and(|s| !s.takes_pc_qualifier()) {
                return Err(CtlError::invalid_params(
                    "pc only qualifies cpu accesses; a DMA engine's access has no \
                     instruction behind it",
                ));
            }
            Ok(BreakSpec::Watch {
                addr: p.u32_req("addr")?,
                source,
                pc,
            })
        }
        "reg_watch" => Ok(BreakSpec::RegWatch {
            off: parse_custom_reg_param(p)?,
        }),
        "beam" => Ok(BreakSpec::Beam {
            vpos: p.u16_req("vpos")?,
            hpos: p.u16_opt("hpos")?,
        }),
        "copper" => Ok(BreakSpec::Copper {
            addr: p.u32_req("addr")?,
        }),
        "catch" => Ok(BreakSpec::Catch {
            vector: p.u16_req("vector")?,
        }),
        "loadseg" => Ok(BreakSpec::LoadSeg {
            name: p.str_opt("name")?,
        }),
        other => Err(CtlError::invalid_params(format!(
            "kind must be pc|watch|reg_watch|beam|copper|catch|loadseg, got {other}"
        ))),
    }
}

fn parse_break_cond(cond: &Value) -> Result<BreakCond, CtlError> {
    let get = |key: &str| {
        cond.get(key)
            .ok_or_else(|| CtlError::invalid_params(format!("cond needs {key}")))
    };
    let op = match get("op")?.as_str().unwrap_or_default() {
        "eq" => CondOp::Eq,
        "ne" => CondOp::Ne,
        "lt" => CondOp::Lt,
        "gt" => CondOp::Gt,
        "le" => CondOp::Le,
        "ge" => CondOp::Ge,
        "and" => CondOp::And,
        other => {
            return Err(CtlError::invalid_params(format!(
                "cond op must be eq|ne|lt|gt|le|ge|and, got {other:?}"
            )))
        }
    };
    Ok(BreakCond {
        lhs: parse_cond_operand(get("lhs")?)?,
        op,
        rhs: parse_cond_operand(get("rhs")?)?,
    })
}

fn parse_cond_operand(v: &Value) -> Result<CondOperand, CtlError> {
    if let Some(imm) = value_as_u32(v) {
        return Ok(CondOperand::Imm(imm));
    }
    if let Some(mem) = v.get("mem") {
        return value_as_u32(mem)
            .map(CondOperand::Mem)
            .ok_or_else(|| CtlError::invalid_params("cond mem operand needs an address"));
    }
    let Some(name) = v.as_str() else {
        return Err(CtlError::invalid_params(
            "cond operand must be a register name, a number, or {mem: addr}",
        ));
    };
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "pc" => Ok(CondOperand::Pc),
        "sr" => Ok(CondOperand::Sr),
        _ => {
            let reg = parse_reg_name(&lower)?;
            Ok(match reg {
                0..=7 => CondOperand::Data(reg),
                8..=15 => CondOperand::Addr(reg - 8),
                _ => unreachable!("parse_reg_name yields 0..=17"),
            })
        }
    }
}

/// Parse a register selector into the GDB-style register number
/// (D0-D7 = 0-7, A0-A7 = 8-15, SR = 16, PC = 17) used by
/// `debug_register` / `debug_set_register`.
fn parse_reg_name(name: &str) -> Result<usize, CtlError> {
    let lower = name.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    match bytes {
        b"pc" => return Ok(17),
        b"sr" => return Ok(16),
        b"sp" => return Ok(15),
        b"fp" => return Ok(14),
        _ => {}
    }
    if bytes.len() == 2 && bytes[1].is_ascii_digit() {
        let n = (bytes[1] - b'0') as usize;
        if n < 8 {
            match bytes[0] {
                b'd' => return Ok(n),
                b'a' => return Ok(8 + n),
                _ => {}
            }
        }
    }
    Err(CtlError::invalid_params(format!(
        "unknown register: {name} (want d0-d7, a0-a7, fp, sp, sr, pc)"
    )))
}

/// A custom register selector: a name ("DMACON"), or an offset as a
/// number or hex string.
fn parse_custom_reg_param(p: &ParamReader) -> Result<u16, CtlError> {
    let Some(v) = p.get("reg") else {
        return Err(CtlError::invalid_params("needs reg (name or offset)"));
    };
    if let Some(s) = v.as_str() {
        return crate::debugger::parse_custom_reg(&s.to_ascii_uppercase())
            .ok_or_else(|| CtlError::invalid_params(format!("unknown custom register: {s}")));
    }
    match value_as_u32(v) {
        Some(off) if off < 0x200 => Ok((off as u16) & !1),
        _ => Err(CtlError::invalid_params("reg offset must be below 0x200")),
    }
}

/// Numbers are decimal values; strings are hex with an optional `0x` or
/// `$` prefix (the notation every Amiga reference uses for addresses).
fn value_as_u32(v: &Value) -> Option<u32> {
    value_as_u64(v).and_then(|n| u32::try_from(n).ok())
}

fn value_as_u64(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    let s = v.as_str()?.trim();
    let hex = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .or_else(|| s.strip_prefix('$'))
        .unwrap_or(s);
    u64::from_str_radix(hex, 16).ok()
}

/// Typed access to the params object with uniform error messages.
struct ParamReader<'a> {
    obj: Option<&'a Map<String, Value>>,
}

impl<'a> ParamReader<'a> {
    fn new(params: &'a Value) -> Result<Self, CtlError> {
        match params {
            Value::Null => Ok(Self { obj: None }),
            Value::Object(map) => Ok(Self { obj: Some(map) }),
            _ => Err(CtlError::invalid_params("params must be an object")),
        }
    }

    fn get(&self, key: &str) -> Option<&'a Value> {
        self.obj.and_then(|o| o.get(key))
    }

    fn u32_req(&self, key: &str) -> Result<u32, CtlError> {
        self.u32_opt(key)?
            .ok_or_else(|| CtlError::invalid_params(format!("missing {key}")))
    }

    fn u32_opt(&self, key: &str) -> Result<Option<u32>, CtlError> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => value_as_u32(v)
                .map(Some)
                .ok_or_else(|| CtlError::invalid_params(format!("bad {key}"))),
        }
    }

    fn u32_or(&self, key: &str, default: u32) -> Result<u32, CtlError> {
        Ok(self.u32_opt(key)?.unwrap_or(default))
    }

    fn u16_req(&self, key: &str) -> Result<u16, CtlError> {
        self.u16_opt(key)?
            .ok_or_else(|| CtlError::invalid_params(format!("missing {key}")))
    }

    fn u16_opt(&self, key: &str) -> Result<Option<u16>, CtlError> {
        match self.u32_opt(key)? {
            None => Ok(None),
            Some(value) => u16::try_from(value)
                .map(Some)
                .map_err(|_| CtlError::invalid_params(format!("{key} must be 0..={}", u16::MAX))),
        }
    }

    fn u64_opt(&self, key: &str) -> Result<Option<u64>, CtlError> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => value_as_u64(v)
                .map(Some)
                .ok_or_else(|| CtlError::invalid_params(format!("bad {key}"))),
        }
    }

    fn usize_req(&self, key: &str) -> Result<usize, CtlError> {
        Ok(self.u32_req(key)? as usize)
    }

    fn usize_or(&self, key: &str, default: usize) -> Result<usize, CtlError> {
        Ok(self.u64_opt(key)?.map(|n| n as usize).unwrap_or(default))
    }

    fn i32_or(&self, key: &str, default: i32) -> Result<i32, CtlError> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(v) => v
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| CtlError::invalid_params(format!("bad {key}"))),
        }
    }

    fn i32_req(&self, key: &str) -> Result<i32, CtlError> {
        match self.get(key) {
            None | Some(Value::Null) => Err(CtlError::invalid_params(format!("missing {key}"))),
            Some(v) => v
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| CtlError::invalid_params(format!("bad {key}"))),
        }
    }

    fn i64_opt(&self, key: &str) -> Result<Option<i64>, CtlError> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_i64()
                .map(Some)
                .ok_or_else(|| CtlError::invalid_params(format!("bad {key}"))),
        }
    }

    fn f64_opt(&self, key: &str) -> Result<Option<f64>, CtlError> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_f64()
                .map(Some)
                .ok_or_else(|| CtlError::invalid_params(format!("bad {key}"))),
        }
    }

    fn bool_opt(&self, key: &str) -> Result<Option<bool>, CtlError> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_bool()
                .map(Some)
                .ok_or_else(|| CtlError::invalid_params(format!("bad {key}"))),
        }
    }

    fn bool_or(&self, key: &str, default: bool) -> Result<bool, CtlError> {
        Ok(self.bool_opt(key)?.unwrap_or(default))
    }

    fn str_req(&self, key: &str) -> Result<String, CtlError> {
        self.str_opt(key)?
            .ok_or_else(|| CtlError::invalid_params(format!("missing {key}")))
    }

    fn str_opt(&self, key: &str) -> Result<Option<String>, CtlError> {
        match self.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_str()
                .map(|s| Some(s.to_string()))
                .ok_or_else(|| CtlError::invalid_params(format!("bad {key}"))),
        }
    }
}

// ---------------------------------------------------------------------
// Execution

/// Execute a [`CoreOp`] against the machine. Both server modes call
/// this for everything that is not run control, input, or media.
pub fn exec_core(emu: &mut Emulator, ctx: &mut SessionCtx, op: &CoreOp) -> Result<Value, CtlError> {
    match op {
        CoreOp::Status => Ok(status_value(emu, ctx)),
        CoreOp::RegsGet => Ok(regs_value(emu)),
        CoreOp::RegsSet { reg, value } => {
            if !emu.machine.debug_set_register(*reg, *value) {
                return Err(CtlError::invalid_params("register write refused"));
            }
            emu.machine.refresh_irq_line();
            Ok(json!({}))
        }
        CoreOp::MemRead { addr, len, base64 } => {
            let bytes = emu.machine.debug_read_memory(*addr, *len);
            let data = if *base64 {
                proto::encode_base64(&bytes)
            } else {
                proto::encode_hex(&bytes)
            };
            Ok(json!({"addr": addr, "len": bytes.len(), "data": data}))
        }
        CoreOp::MemWrite { addr, data } => {
            let written = emu.machine.debug_write_memory(*addr, data);
            // Rebaseline memory watches so the poke itself does not fire
            // them (matching the GDB stub's refresh after `M`).
            emu.machine.ui_rebaseline_watches();
            let mut result = json!({"written": written});
            if emu.time_travel_enabled() {
                // Memory pokes are not part of the replay journal, so a
                // reverse replay across this write can diverge.
                result["replay_unsafe"] = Value::Bool(true);
            }
            Ok(result)
        }
        CoreOp::Disasm { addr, count } => {
            let cpu_type = emu.machine.cpu_type();
            let mut pc = addr.unwrap_or_else(|| emu.machine.pc());
            let bus = emu.machine.bus();
            let mut lines = Vec::with_capacity(*count);
            for _ in 0..*count {
                let (text, len) =
                    crate::disasm::disassemble(|a| bus.peek_word_any(a), pc, cpu_type);
                lines.push(json!({"addr": pc, "text": text, "len": len}));
                pc = pc.wrapping_add(len);
            }
            Ok(json!({"lines": lines}))
        }
        CoreOp::CustomDump => {
            let bus = emu.bus();
            let mut regs = Map::new();
            for off in (0u16..0x200).step_by(2) {
                if let Some(value) = bus.debug_custom_word(off) {
                    regs.insert(crate::debugger::custom_reg_name(off), Value::from(value));
                }
            }
            Ok(json!({"regs": regs}))
        }
        CoreOp::CustomRead { off } => match emu.bus().debug_custom_word(*off) {
            Some(value) => Ok(json!({
                "off": off,
                "name": crate::debugger::custom_reg_name(*off),
                "value": value,
            })),
            None => Err(CtlError::not_found(format!(
                "custom register ${off:03X} is not readable"
            ))),
        },
        CoreOp::CiaGet { b } => {
            let bus = emu.bus();
            let cia = if *b { &bus.cia_b } else { &bus.cia_a };
            let regs: Vec<u8> = (0..16).map(|r| cia.peek_register(r)).collect();
            Ok(json!({
                "cia": if *b { "b" } else { "a" },
                "regs": regs,
                "icr_data": cia.debug_icr_data(),
                "timer_a": {
                    "count": cia.ta_count, "latch": cia.ta_latch,
                    "running": cia.ta_running, "oneshot": cia.ta_oneshot,
                },
                "timer_b": {
                    "count": cia.tb_count, "latch": cia.tb_latch,
                    "running": cia.tb_running, "oneshot": cia.tb_oneshot,
                },
            }))
        }
        CoreOp::BeamGet => {
            let bus = emu.bus();
            Ok(json!({
                "vpos": bus.agnus.vpos,
                "hpos": bus.agnus.hpos,
                "cck": bus.emulated_cck(),
                "frame": bus.emulated_frames(),
                "seconds": bus.emulated_seconds(),
            }))
        }
        CoreOp::DisplayGet => {
            let bus = emu.bus();
            Ok(json!({
                "dmacon": bus.debug_dmacon(),
                "display": bus.debug_display_state(),
            }))
        }
        CoreOp::InputPortsGet => {
            let input = &emu.bus().input;
            Ok(json!({
                "port1": input.ports[0].device.label(),
                "port2": input.ports[1].device.label(),
            }))
        }
        CoreOp::RtcGet => {
            let bus = emu.bus();
            let secs = bus.emulated_seconds();
            Ok(json!({
                "present": bus.rtc_present(),
                "chip": bus.rtc.chip().label(),
                "seeded": bus.rtc.seed().is_some(),
                "frozen": bus.rtc.frozen(),
                "unix": bus.rtc.current_unix(secs),
                "time": bus.rtc.current_display(secs),
            }))
        }
        CoreOp::RtcSet {
            unix,
            advance,
            frozen,
        } => {
            if !emu.bus().rtc_present() {
                return Err(CtlError::not_found(
                    "no battery clock fitted ([machine] rtc = true or --rtc-time fits one)",
                ));
            }
            let secs = emu.bus().emulated_seconds();
            let current = emu.bus().rtc.current_unix(secs);
            let target = match (unix, advance) {
                (Some(u), _) => *u,
                (None, Some(d)) => current.checked_add_signed(*d).ok_or_else(|| {
                    CtlError::invalid_params(
                        "advance moves the clock outside the Unix-seconds range",
                    )
                })?,
                (None, None) => current,
            };
            let frozen = frozen.unwrap_or_else(|| emu.bus().rtc.frozen());
            let seed = if frozen {
                target
            } else {
                // The seed is the clock's value at emulated time zero, so
                // subtract the elapsed timeline to make it read `target`
                // from this instant onward.
                target.checked_sub(secs as u64).ok_or_else(|| {
                    CtlError::invalid_params(
                        "time is earlier than the elapsed emulated timeline allows",
                    )
                })?
            };
            emu.bus_mut().rtc.set_seed(Some(seed), frozen);
            let bus = emu.bus();
            let mut result = json!({
                "unix": bus.rtc.current_unix(secs),
                "time": bus.rtc.current_display(secs),
                "frozen": bus.rtc.frozen(),
            });
            if emu.time_travel_enabled() {
                // Like mem.write, a clock change is not part of the replay
                // journal, so a reverse replay across it can diverge.
                result["replay_unsafe"] = Value::Bool(true);
            }
            Ok(result)
        }
        CoreOp::CopperList { addr, max } => {
            let bus = emu.bus();
            let copper_pc = bus.copper.pc();
            let start = addr.unwrap_or_else(|| copper_pc.saturating_sub(4 * 4));
            let entries: Vec<Value> = crate::disasm::dump_copper_list(
                |a| bus.peek_word_any(a),
                start,
                *max,
            )
            .into_iter()
            .map(|(addr, text)| json!({"addr": addr, "text": text, "cursor": addr == copper_pc}))
            .collect();
            Ok(json!({
                "cop1lc": bus.agnus.cop1lc,
                "cop2lc": bus.agnus.cop2lc,
                "coppc": copper_pc,
                "running": bus.copper.is_running(),
                "entries": entries,
            }))
        }
        CoreOp::LastWriter { addr } => {
            require_time_travel(emu)?;
            let before = emu.retired_instructions();
            let outcome = emu
                .tt_last_writer(*addr, before)
                .map_err(|e| CtlError::internal(format!("last-writer scan: {e:#}")))?;
            let (outcome_name, record) = match outcome {
                ReverseOutcome::Found(rec) => (
                    "found",
                    Some(json!({
                        "addr": rec.addr, "old": rec.old, "new": rec.new,
                        "pc": rec.pc, "pos": rec.pos, "cck": rec.cck,
                        "frame": rec.frame,
                    })),
                ),
                ReverseOutcome::NotFound => ("never_written", None),
                ReverseOutcome::BeyondHistory => ("beyond_history", None),
            };
            // On "found" the machine is parked at the writing
            // instruction; always report where it ended up.
            let position = stop_snapshot(emu, "last_writer", &format!("${addr:06X}"));
            let mut result = json!({
                "outcome": outcome_name,
                "position": serde_json::to_value(&position)
                    .map_err(|e| CtlError::internal(e.to_string()))?,
            });
            if let Some(record) = record {
                result["record"] = record;
            }
            Ok(result)
        }
        CoreOp::PcHistory => Ok(json!({"pcs": emu.machine.ui_pc_history()})),
        CoreOp::BreakAdd(spec) => match ctx.install_break(emu, spec.clone()) {
            Ok(id) => Ok(json!({"id": id})),
            Err(msg) => Err(CtlError::invalid_params(msg)),
        },
        CoreOp::BreakRemove { id } => {
            if ctx.remove_break(emu, *id) {
                Ok(json!({}))
            } else {
                Err(CtlError::not_found(format!("no break with id {id}")))
            }
        }
        CoreOp::BreakList => Ok(break_list_value(emu, ctx)),
        CoreOp::BreakClear => {
            emu.machine.ui_breaks_clear();
            // ui_breaks_clear covers the bus mirrors for reg/mem watches
            // but beam traps and copper breaks live only bus-side.
            emu.bus_mut().ui_clear_beam_traps();
            emu.bus_mut().ui_clear_copper_breaks();
            let ids: Vec<u32> = ctx.breaks().map(|(id, _)| id).collect();
            for id in ids {
                ctx.remove_break(emu, id);
            }
            Ok(json!({}))
        }
        CoreOp::FloppyQuery => {
            let floppy = &emu.bus().floppy;
            let drives: Vec<Value> = (0..4)
                .map(|idx| {
                    json!({
                        "drive": idx,
                        "inserted": floppy.disk_inserted(idx),
                        "name": floppy.inserted_disk_name(idx),
                    })
                })
                .collect();
            Ok(json!({"drives": drives}))
        }
        CoreOp::EventsSubscribe {
            events,
            frame_interval,
            frame_digest,
        } => Ok(ctx.subscribe_events(emu, events, *frame_interval, *frame_digest)),
        CoreOp::EventsUnsubscribe { events } => Ok(ctx.unsubscribe_events(emu, events.as_deref())),
        CoreOp::EventsList => Ok(ctx.event_subscriptions()),
        CoreOp::TraceStart { path, max_lines } => {
            emu.machine
                .ui_trace_start(path.clone(), *max_lines)
                .map_err(|e| CtlError::io(format!("starting instruction trace: {e}")))?;
            Ok(trace_status_value(emu))
        }
        CoreOp::TraceStop => match emu.machine.ui_trace_stop() {
            Some((path, lines)) => Ok(json!({
                "active": false,
                "path": path.display().to_string(),
                "lines": lines,
            })),
            None => Ok(json!({"active": false})),
        },
        CoreOp::TraceStatus => Ok(trace_status_value(emu)),
        CoreOp::WaveformStart { options } => {
            emu.machine
                .ui_wave_start(options.clone())
                .map_err(|e| CtlError::io(format!("starting waveform capture: {e}")))?;
            Ok(waveform_status_value(emu))
        }
        CoreOp::WaveformStop => match emu.machine.ui_wave_stop() {
            Some(status) => Ok(json!({
                "active": false,
                "present": false,
                "capture": wave_status_value(&status),
            })),
            None => Ok(json!({"active": false, "present": false})),
        },
        CoreOp::WaveformStatus => Ok(waveform_status_value(emu)),
        CoreOp::StateSave { path } => {
            emu.save_state(path)
                .map_err(|e| CtlError::io(format!("saving state: {e:#}")))?;
            Ok(json!({"path": path.display().to_string()}))
        }
        CoreOp::CustomWriter { off } => {
            if !emu.bus().chipset_validation_armed() {
                return Err(CtlError::invalid_state(
                    "the last-writer table is not armed; chipset.validate {\"enabled\": true}",
                ));
            }
            let name = crate::debugger::custom_reg_name(*off);
            Ok(match emu.bus().custom_reg_last_write(*off) {
                None => json!({"reg": name, "written": false}),
                Some(write) => json!({
                    "reg": name,
                    "written": true,
                    "value": write.value,
                    "by": write.writer.label(),
                    "addr": write.writer.address(),
                    "frame": write.frame,
                    "vpos": write.vpos,
                    "hpos": write.hpos,
                }),
            })
        }
        CoreOp::ChipsetValidate { enabled, clear } => {
            if let Some(on) = enabled {
                emu.bus_mut().set_chipset_validation(*on);
            }
            if *clear {
                emu.bus_mut().clear_chipset_findings();
            }
            Ok(json!({"armed": emu.bus().chipset_validation_armed()}))
        }
        CoreOp::ChipsetReport => {
            let (reports, dropped) = emu.bus().chipset_findings();
            let findings: Vec<Value> = reports
                .iter()
                .map(|r| {
                    json!({
                        "kind": r.finding.name(),
                        // The keyboard handshake is not a register access,
                        // so it does not name one; everything else does.
                        "reg": (r.finding != crate::regcheck::Finding::KeyboardHandshakeShort)
                            .then(|| crate::debugger::custom_reg_name(r.reg)),
                        "by": r.writer.label(),
                        "addr": r.writer.address(),
                        "count": r.count,
                        "vpos": r.vpos,
                        "hpos": r.hpos,
                        "detail": crate::regcheck::RegCheck::describe(r),
                    })
                })
                .collect();
            Ok(json!({
                "armed": emu.bus().chipset_validation_armed(),
                "findings": findings,
                "dropped": dropped,
            }))
        }
        CoreOp::SmcDetect { enabled, clear } => {
            if let Some(on) = enabled {
                emu.bus_mut().set_smc_detection(*on);
            }
            if *clear {
                emu.bus_mut().clear_smc_reports();
            }
            Ok(json!({"armed": emu.bus().smc_detection_armed()}))
        }
        CoreOp::SmcReport => {
            let (reports, dropped) = emu.bus().smc_reports();
            let writes: Vec<Value> = reports
                .iter()
                .map(|r| {
                    json!({
                        "addr": r.addr,
                        "writer_pc": r.writer_pc,
                        "distance": r.distance,
                        "count": r.count,
                        "detail": crate::smc::SmcTracker::describe(r),
                    })
                })
                .collect();
            Ok(json!({
                "armed": emu.bus().smc_detection_armed(),
                "writes": writes,
                "dropped": dropped,
            }))
        }
        CoreOp::FaultInject {
            addr,
            len,
            on_read,
            on_write,
            count,
        } => {
            let id = emu.bus_mut().inject_bus_fault(crate::bus::FaultInjection {
                start: *addr,
                end: addr.wrapping_add(len - 1),
                on_read: *on_read,
                on_write: *on_write,
                remaining: *count,
                hits: 0,
            });
            Ok(json!({"id": id}))
        }
        CoreOp::FaultList => {
            let faults: Vec<Value> = emu
                .bus()
                .injected_bus_faults()
                .iter()
                .enumerate()
                .map(|(id, f)| {
                    json!({
                        "id": id,
                        "start": f.start,
                        "end": f.end,
                        "on": match (f.on_read, f.on_write) {
                            (true, true) => "both",
                            (true, false) => "read",
                            _ => "write",
                        },
                        "remaining": f.remaining,
                        "hits": f.hits,
                    })
                })
                .collect();
            Ok(json!({"faults": faults}))
        }
        CoreOp::FaultClear => {
            emu.bus_mut().clear_injected_bus_faults();
            Ok(json!({"faults": 0}))
        }
        CoreOp::HeatMapSet { window } => {
            emu.bus_mut().set_heat_map(*window);
            Ok(match emu.bus().heat_map() {
                None => json!({"armed": false}),
                Some(map) => json!({
                    "armed": true,
                    "base": map.base(),
                    "span": map.span(),
                    "bytes_per_cell": map.bytes_per_cell(),
                    "grid": crate::heatmap::GRID,
                }),
            })
        }
        CoreOp::HeatMapReport { path } => {
            let frame = emu.bus().emulated_frames();
            let Some(map) = emu.bus().heat_map() else {
                return Err(CtlError::invalid_state(
                    "the heat map is not armed; memory.heatmap {\"enabled\": true}",
                ));
            };
            let census: Vec<Value> = map
                .census(frame)
                .into_iter()
                .map(|(by, cells)| json!({"by": by.name(), "cells": cells}))
                .collect();
            let mut reply = json!({
                "base": map.base(),
                "span": map.span(),
                "bytes_per_cell": map.bytes_per_cell(),
                "grid": crate::heatmap::GRID,
                "frame": frame,
                "census": census,
            });
            if let Some(path) = path {
                let mut image = vec![0u32; crate::heatmap::CELLS];
                map.render(frame, &mut image);
                crate::screenshot::save(
                    path,
                    &image,
                    crate::heatmap::GRID as u32,
                    crate::heatmap::GRID as u32,
                )
                .map_err(|e| CtlError::io(format!("writing heat map: {e:#}")))?;
                reply["path"] = json!(path.display().to_string());
            }
            Ok(reply)
        }
        CoreOp::Digest => Ok(digest_value(emu)),
        CoreOp::RegionDigest { rect } => region_digest_value(emu, *rect),
        CoreOp::Screenshot { path } => {
            let (fb, lines, width) = render_frame(emu);
            let path = path
                .clone()
                .unwrap_or_else(crate::screenshot::auto_filename);
            crate::screenshot::save(&path, &fb[..width * lines], width as u32, lines as u32)
                .map_err(|e| CtlError::io(format!("saving screenshot: {e:#}")))?;
            Ok(json!({
                "path": path.display().to_string(),
                "width": width,
                "height": lines,
            }))
        }
        CoreOp::ReverseStep { n } => {
            require_time_travel(emu)?;
            let outcome = emu
                .tt_reverse_step(*n)
                .map_err(|e| CtlError::internal(format!("reverse step: {e:#}")))?;
            // Found carries the new (earlier) position the machine was
            // left at, same as the stop event's retired_instructions.
            reverse_result(
                emu,
                map_outcome(outcome, |pos| format!("stepped back {n} to position {pos}")),
            )
        }
        CoreOp::ReverseFrame => {
            require_time_travel(emu)?;
            let outcome = emu
                .tt_reverse_frame()
                .map_err(|e| CtlError::internal(format!("reverse frame: {e:#}")))?;
            reverse_result(
                emu,
                map_outcome(outcome, |pos| {
                    format!("stepped back one frame to position {pos}")
                }),
            )
        }
        CoreOp::ReverseContinue => {
            require_time_travel(emu)?;
            let outcome = emu
                .tt_reverse_continue()
                .map_err(|e| CtlError::internal(format!("reverse continue: {e:#}")))?;
            reverse_result(emu, map_outcome(outcome, |(_, desc)| desc))
        }
    }
}

/// Evaluate a `collect` list at a stop; each entry independently
/// reports `{ok: result}` or `{err: {code, message}}` so one failing
/// item cannot poison the whole stop event.
pub fn eval_collect(emu: &mut Emulator, ctx: &mut SessionCtx, items: &[CoreOp]) -> Vec<Value> {
    items
        .iter()
        .map(|op| match exec_core(emu, ctx, op) {
            Ok(value) => json!({"ok": value}),
            Err(err) => json!({"err": {"code": err.code, "message": err.message}}),
        })
        .collect()
}

/// The consistent stop coordinate every resume verb returns.
pub fn stop_snapshot(emu: &Emulator, reason: &str, detail: &str) -> StopEvent {
    let bus = emu.bus();
    StopEvent {
        reason: reason.to_string(),
        detail: detail.to_string(),
        pc: emu.machine.pc(),
        frame: bus.emulated_frames(),
        vpos: bus.agnus.vpos.min(u32::from(u16::MAX)) as u16,
        hpos: bus.agnus.hpos.min(u32::from(u16::MAX)) as u16,
        cck: bus.emulated_cck(),
        seconds: bus.emulated_seconds(),
        retired_instructions: emu.retired_instructions(),
        collect: None,
    }
}

/// Map a machine [`DebugStop`] onto the protocol's stop reason plus its
/// human-readable description.
pub fn stop_reason_of(stop: &DebugStop) -> (&'static str, String) {
    let reason = match stop {
        DebugStop::Breakpoint { .. } => "breakpoint",
        DebugStop::Watch { .. } => "watchpoint",
        DebugStop::ChipReg { .. } => "reg_watch",
        DebugStop::Beam { .. } => "beam_trap",
        DebugStop::CopperBreak { .. } => "copper_break",
        DebugStop::Exception { .. } => "catch",
        DebugStop::Task { .. } => "task_catch",
        DebugStop::LoadSeg { .. } => "loadseg",
    };
    (reason, stop.describe())
}

fn require_time_travel(emu: &Emulator) -> Result<(), CtlError> {
    if emu.time_travel_enabled() {
        Ok(())
    } else {
        Err(CtlError::invalid_state(
            "time travel is not armed on this session",
        ))
    }
}

fn map_outcome<T>(
    outcome: ReverseOutcome<T>,
    describe: impl FnOnce(T) -> String,
) -> ReverseOutcome<String> {
    match outcome {
        ReverseOutcome::Found(v) => ReverseOutcome::Found(describe(v)),
        ReverseOutcome::NotFound => ReverseOutcome::NotFound,
        ReverseOutcome::BeyondHistory => ReverseOutcome::BeyondHistory,
    }
}

fn reverse_result(emu: &Emulator, outcome: ReverseOutcome<String>) -> Result<Value, CtlError> {
    match outcome {
        ReverseOutcome::Found(detail) => {
            let stop = stop_snapshot(emu, "reverse", &detail);
            serde_json::to_value(&stop).map_err(|e| CtlError::internal(e.to_string()))
        }
        ReverseOutcome::NotFound | ReverseOutcome::BeyondHistory => Err(CtlError::new(
            proto::HISTORY_EXHAUSTED,
            "no retained history at that distance",
        )),
    }
}

fn status_value(emu: &Emulator, ctx: &SessionCtx) -> Value {
    let bus = emu.bus();
    // Cumulative counters: a client derives rates (emulated fps, host
    // utilisation) from the deltas between two status calls.
    let perf = emu.perf_counters();
    let audio = bus.live_audio_status();
    json!({
        "state": if ctx.running { "running" } else { "paused" },
        "pending_resume": ctx.pending,
        "powered_on": ctx.powered_on,
        "double_faulted": emu.machine.cpu_double_faulted(),
        "cpu": format!("{:?}", emu.machine.cpu_type()),
        "cpu_stopped": emu.machine.stopped(),
        "tt_armed": emu.time_travel_enabled(),
        "pc": emu.machine.pc(),
        "frame": bus.emulated_frames(),
        "vpos": bus.agnus.vpos,
        "hpos": bus.agnus.hpos,
        "cck": bus.emulated_cck(),
        "seconds": bus.emulated_seconds(),
        "retired_instructions": emu.retired_instructions(),
        "host_busy_ms": perf.busy.as_secs_f64() * 1000.0,
        "pacer_slips": perf.pacer_slips,
        "audio_lead_ms": audio.output_lead_seconds * 1000.0,
        "audio_underrun_frames": audio.callback_underrun_frames,
    })
}

fn regs_value(emu: &Emulator) -> Value {
    let machine = &emu.machine;
    let d: Vec<u32> = (0..8).map(|n| machine.d(n)).collect();
    let a: Vec<u32> = (0..8).map(|n| machine.a(n)).collect();
    json!({
        "d": d,
        "a": a,
        "pc": machine.pc(),
        "sr": machine.sr(),
        "stopped": machine.stopped(),
    })
}

fn break_list_value(emu: &Emulator, ctx: &SessionCtx) -> Value {
    let mut entries = Vec::new();
    let breaks = emu.machine.ui_breaks();
    for bp in &breaks.breakpoints {
        let spec = BreakSpec::Pc {
            addr: bp.addr,
            cond: bp.cond,
            ignore: bp.ignore,
        };
        let mut entry = json!({
            "kind": "pc",
            "addr": bp.addr,
            "ignore": bp.ignore,
            "hits": bp.hits,
        });
        if let Some(cond) = &bp.cond {
            entry["cond"] = Value::from(cond.describe());
        }
        push_id(&mut entry, ctx.id_for(&spec));
        entries.push(entry);
    }
    for w in &breaks.watches {
        let spec = BreakSpec::Watch {
            addr: w.addr,
            source: w.filter,
            pc: w.pc,
        };
        let mut entry = json!({"kind": "watch", "addr": w.addr});
        if let Some(class) = w.filter {
            entry["class"] = Value::from(class.describe());
        }
        if let Some(pc) = w.pc {
            entry["pc"] = Value::from(pc);
        }
        push_id(&mut entry, ctx.id_for(&spec));
        entries.push(entry);
    }
    for &off in &breaks.reg_watches {
        let mut entry = json!({
            "kind": "reg_watch",
            "off": off,
            "name": crate::debugger::custom_reg_name(off),
        });
        push_id(&mut entry, ctx.id_for(&BreakSpec::RegWatch { off }));
        entries.push(entry);
    }
    for &vector in &breaks.catches {
        let mut entry = json!({
            "kind": "catch",
            "vector": vector,
            "name": crate::debugger::exception_vector_name(vector),
        });
        push_id(&mut entry, ctx.id_for(&BreakSpec::Catch { vector }));
        entries.push(entry);
    }
    if let Some(catch) = &breaks.loadseg_catch {
        let mut entry = json!({"kind": "loadseg"});
        if let Some(name) = &catch.name {
            entry["name"] = Value::from(name.clone());
        }
        push_id(
            &mut entry,
            ctx.id_for(&BreakSpec::LoadSeg {
                name: catch.name.clone(),
            }),
        );
        entries.push(entry);
    }
    for trap in emu.bus().ui_beam_traps() {
        if trap.once {
            continue; // internal one-shot run-to-position trap
        }
        let mut entry = json!({"kind": "beam", "vpos": trap.vpos});
        if let Some(hpos) = trap.hpos {
            entry["hpos"] = Value::from(hpos);
        }
        push_id(
            &mut entry,
            ctx.id_for(&BreakSpec::Beam {
                vpos: trap.vpos,
                hpos: trap.hpos,
            }),
        );
        entries.push(entry);
    }
    for &addr in emu.bus().ui_copper_breaks() {
        let mut entry = json!({"kind": "copper", "addr": addr});
        push_id(&mut entry, ctx.id_for(&BreakSpec::Copper { addr }));
        entries.push(entry);
    }
    json!({"breaks": entries})
}

fn push_id(entry: &mut Value, id: Option<u32>) {
    if let Some(id) = id {
        entry["id"] = Value::from(id);
    }
}

fn default_trace_path() -> PathBuf {
    crate::paths::trace_file()
}

fn trace_status_value(emu: &Emulator) -> Value {
    match emu.machine.ui_trace_status() {
        Some((path, lines)) => json!({
            "active": true,
            "path": path.display().to_string(),
            "lines": lines,
        }),
        None => json!({"active": false}),
    }
}

fn waveform_status_value(emu: &Emulator) -> Value {
    match emu.machine.ui_wave_status() {
        Some(status) => json!({
            "active": matches!(status.state, "armed" | "capturing"),
            "present": true,
            "capture": wave_status_value(&status),
        }),
        None => json!({"active": false, "present": false}),
    }
}

fn wave_status_value(status: &crate::waveform::WaveStatus) -> Value {
    json!({
        "path": status.path.display().to_string(),
        "state": status.state,
        "trigger": status.trigger,
        "duration": status.duration,
        "signals": status.signals,
        "samples": status.samples,
        "captured_cck": status.captured_cck,
        "window_cck": status.window_cck,
    })
}

/// Render the current frame into a fresh buffer via the side-effect-free
/// display path, returning the buffer and its visible line count. Both
/// `capture.digest` and `capture.screenshot` use this in BOTH server
/// modes, so captures are mode-identical and comparable.
fn render_frame(emu: &Emulator) -> (Vec<u32>, usize, usize) {
    // An RTG board driving the display supersedes the chipset output,
    // exactly as the window presentation does.
    let mut fb = Vec::new();
    let mut scratch = Vec::new();
    if let Some((rows, _, _)) =
        crate::video::present_common::compose_rtg_present(emu.bus(), &mut scratch, &mut fb)
    {
        return (fb, rows, FB_WIDTH);
    }
    fb = vec![0u32; MAX_CANVAS_PIXELS];
    crate::video::bitplane::render_display_only(emu.bus(), &mut fb);
    let lines = emu.bus().frame_geometry().visible_lines;
    let width = FB_WIDTH * emu.bus().frame_canvas_scale();
    (fb, lines, width)
}

const FNV1A64_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a over the framebuffer words (little-endian byte order), for
/// cheap change detection without pulling pixels over the wire.
fn fnv1a64(words: &[u32]) -> u64 {
    fnv1a64_from(FNV1A64_OFFSET, words)
}

/// Continue an FNV-1a digest over another run of words, so a region
/// digest can chain its rows without materialising a copy.
fn fnv1a64_from(mut hash: u64, words: &[u32]) -> u64 {
    for word in words {
        for byte in word.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// Frame digest payload shared by the request/response capture method and the
/// opt-in streaming frame notification.
pub(crate) fn digest_value(emu: &Emulator) -> Value {
    let (fb, lines, width) = render_frame(emu);
    let digest = fnv1a64(&fb[..width * lines]);
    json!({
        "algo": "fnv1a64",
        "digest": format!("{digest:016x}"),
        "width": width,
        "height": lines,
        "frame": emu.bus().emulated_frames(),
    })
}

/// Digest one rectangle of the presented frame, so a script can assert on
/// a single widget ("is this button highlighted?") without the whole
/// frame's unrelated motion changing the answer. `width`/`height` in the
/// reply are the frame's, not the region's, so a caller whose coordinates
/// have gone stale can see the geometry that rejected them.
pub(crate) fn region_digest_value(emu: &Emulator, rect: FrameRect) -> Result<Value, CtlError> {
    let (fb, lines, width) = render_frame(emu);
    let digest = rect.digest(&fb, width, lines)?;
    Ok(json!({
        "algo": "fnv1a64",
        "digest": format!("{digest:016x}"),
        "x": rect.x,
        "y": rect.y,
        "w": rect.w,
        "h": rect.h,
        "width": width,
        "height": lines,
        "frame": emu.bus().emulated_frames(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::test_emulator;
    use serde_json::json;

    fn core(method: &str, params: Value) -> CoreOp {
        match parse_method(method, &params).expect("parse should succeed") {
            Request::Core(op) => op,
            other => panic!("expected core op, got {other:?}"),
        }
    }

    #[test]
    fn parse_accepts_hex_strings_and_numbers_for_addresses() {
        assert_eq!(
            core("mem.read", json!({"addr": "$F80010", "len": 4})),
            CoreOp::MemRead {
                addr: 0xF80010,
                len: 4,
                base64: false
            }
        );
        assert_eq!(
            core("mem.read", json!({"addr": "0xF80010"})),
            CoreOp::MemRead {
                addr: 0xF80010,
                len: 2,
                base64: false
            }
        );
        assert_eq!(
            core("disasm", json!({"addr": 16252944, "count": 2})),
            CoreOp::Disasm {
                addr: Some(0xF80010),
                count: 2
            }
        );
    }

    #[test]
    fn parse_rejects_out_of_range_targets() {
        // u16 beam coordinates must not silently wrap.
        let err = parse_method("run_until", &json!({"vpos": 70000})).unwrap_err();
        assert_eq!(err.code, proto::INVALID_PARAMS);
        let err = parse_method("run_until", &json!({"vpos": 100, "hpos": 65536})).unwrap_err();
        assert_eq!(err.code, proto::INVALID_PARAMS);
        // Negative or non-finite seconds would otherwise saturate to a
        // cck target of 0 and complete immediately.
        let err = parse_method("run_until", &json!({"seconds": -1.0})).unwrap_err();
        assert_eq!(err.code, proto::INVALID_PARAMS);
        let err = parse_method("break.add", &json!({"kind": "beam", "vpos": 70000})).unwrap_err();
        assert_eq!(err.code, proto::INVALID_PARAMS);
        let err =
            parse_method("break.add", &json!({"kind": "catch", "vector": 70000})).unwrap_err();
        assert_eq!(err.code, proto::INVALID_PARAMS);
    }

    #[test]
    fn parse_run_until_requires_exactly_one_target() {
        assert!(parse_method("run_until", &json!({})).is_err());
        assert!(parse_method("run_until", &json!({"pc": 16, "frame": 3})).is_err());
        match parse_method("run_until", &json!({"vpos": 100, "hpos": 60})).unwrap() {
            Request::Host(HostOp::Resume(verb)) => assert_eq!(
                verb.kind,
                ResumeKind::RunUntil(RunTarget::Beam {
                    vpos: 100,
                    hpos: Some(60)
                })
            ),
            other => panic!("expected resume, got {other:?}"),
        }
    }

    #[test]
    fn parse_collect_whitelists_read_only_ops() {
        let params = json!({"collect": [
            {"method": "regs.get"},
            {"method": "mem.read", "params": {"addr": 0, "len": 8}},
            {"method": "capture.region_digest", "params": {"w": 8, "h": 8}},
        ]});
        match parse_method("continue", &params).unwrap() {
            Request::Host(HostOp::Resume(verb)) => assert_eq!(verb.collect.len(), 3),
            other => panic!("expected resume, got {other:?}"),
        }
        let bad = json!({"collect": [
            {"method": "mem.write", "params": {"addr": 0, "data": "00"}},
        ]});
        let err = parse_method("continue", &bad).unwrap_err();
        assert_eq!(err.code, proto::INVALID_PARAMS);
    }

    #[test]
    fn parse_unknown_method_reports_method_not_found() {
        let err = parse_method("warp.nine", &Value::Null).unwrap_err();
        assert_eq!(err.code, proto::METHOD_NOT_FOUND);
    }

    #[test]
    fn parse_event_subscriptions_validates_names_and_interval() {
        assert_eq!(
            core(
                "events.subscribe",
                json!({
                    "events": ["frame", "serial", "frame"],
                    "frame_interval": 25,
                    "frame_digest": true,
                }),
            ),
            CoreOp::EventsSubscribe {
                events: vec![EventKind::Frame, EventKind::Serial],
                frame_interval: Some(25),
                frame_digest: Some(true),
            }
        );
        assert_eq!(
            core("events.unsubscribe", json!({})),
            CoreOp::EventsUnsubscribe { events: None }
        );
        for params in [
            json!({}),
            json!({"events": ["unknown"]}),
            json!({"events": ["frame"], "frame_interval": 0}),
            json!({"events": ["frame"], "frame_interval": MAX_FRAME_INTERVAL + 1}),
        ] {
            assert!(parse_method("events.subscribe", &params).is_err());
        }
    }

    #[test]
    fn parse_trace_and_waveform_controls_validate_bounds_and_specs() {
        assert_eq!(
            core(
                "trace.start",
                json!({"path": "/tmp/trace.txt", "max_lines": 123}),
            ),
            CoreOp::TraceStart {
                path: PathBuf::from("/tmp/trace.txt"),
                max_lines: 123,
            }
        );
        assert_eq!(
            core(
                "waveform.start",
                json!({
                    "path": "/tmp/wave.vcd",
                    "trigger": "beam=100:64",
                    "duration": "2f",
                    "signals": "cpu,bus",
                }),
            ),
            CoreOp::WaveformStart {
                options: crate::waveform::WaveOptions {
                    path: PathBuf::from("/tmp/wave.vcd"),
                    trigger: crate::waveform::Trigger::Beam {
                        vpos: 100,
                        hpos: Some(64),
                    },
                    duration: crate::waveform::WaveDuration::Frames(2),
                    signals: crate::waveform::parse_signals("cpu,bus").unwrap(),
                },
            }
        );
        for (method, params) in [
            ("trace.start", json!({"max_lines": 0})),
            ("waveform.start", json!({"trigger": "later"})),
            ("waveform.start", json!({"duration": "forever"})),
            ("waveform.start", json!({"signals": "cpu,mystery"})),
        ] {
            assert!(parse_method(method, &params).is_err());
        }
    }

    #[test]
    fn trace_and_waveform_controls_create_report_and_finish_files() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        let dir = std::env::temp_dir().join(format!("ccp-captures-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let trace_path = dir.join("instructions.txt");
        let started = exec_core(
            &mut emu,
            &mut ctx,
            &CoreOp::TraceStart {
                path: trace_path.clone(),
                max_lines: 100,
            },
        )
        .unwrap();
        assert_eq!(started["active"], true);
        assert_eq!(
            exec_core(&mut emu, &mut ctx, &CoreOp::TraceStatus).unwrap()["active"],
            true
        );
        let stopped = exec_core(&mut emu, &mut ctx, &CoreOp::TraceStop).unwrap();
        assert_eq!(stopped["active"], false);
        assert!(trace_path.exists());

        let wave_path = dir.join("signals.vcd");
        let started = exec_core(
            &mut emu,
            &mut ctx,
            &CoreOp::WaveformStart {
                options: crate::waveform::WaveOptions::new(wave_path.clone()),
            },
        )
        .unwrap();
        assert_eq!(started["active"], true);
        assert_eq!(started["capture"]["state"], "capturing");
        let stopped = exec_core(&mut emu, &mut ctx, &CoreOp::WaveformStop).unwrap();
        assert_eq!(stopped["active"], false);
        assert_eq!(stopped["capture"]["state"], "done");
        assert!(wave_path.exists());

        std::fs::remove_file(trace_path).ok();
        std::fs::remove_file(wave_path).ok();
        std::fs::remove_dir(dir).ok();
    }

    #[test]
    fn regs_set_and_get_round_trip() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        exec_core(
            &mut emu,
            &mut ctx,
            &core("regs.set", json!({"reg": "d3", "value": 0xCAFE})),
        )
        .unwrap();
        let regs = exec_core(&mut emu, &mut ctx, &CoreOp::RegsGet).unwrap();
        assert_eq!(regs["d"][3], 0xCAFE);
        assert_eq!(regs["pc"], 0xF80010);
    }

    #[test]
    fn mem_write_and_read_round_trip_across_encodings() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        let write = exec_core(
            &mut emu,
            &mut ctx,
            &core(
                "mem.write",
                json!({"addr": 0x30000, "data": "deadbeef0102"}),
            ),
        )
        .unwrap();
        assert_eq!(write["written"], 6);
        assert!(write.get("replay_unsafe").is_none(), "tt is not armed");
        let read = exec_core(
            &mut emu,
            &mut ctx,
            &core(
                "mem.read",
                json!({"addr": 0x30000, "len": 6, "encoding": "base64"}),
            ),
        )
        .unwrap();
        assert_eq!(
            proto::decode_base64(read["data"].as_str().unwrap()).unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef, 0x01, 0x02]
        );
    }

    #[test]
    fn rtc_set_rejects_conflicting_or_empty_params() {
        let err = parse_method("rtc.set", &json!({})).unwrap_err();
        assert_eq!(err.code, proto::INVALID_PARAMS);
        let err = parse_method(
            "rtc.set",
            &json!({"unix": 1, "time": "2005-03-18 01:58:29"}),
        )
        .unwrap_err();
        assert_eq!(err.code, proto::INVALID_PARAMS);
        let err = parse_method("rtc.set", &json!({"unix": 1, "advance": 30})).unwrap_err();
        assert_eq!(err.code, proto::INVALID_PARAMS);
        let err = parse_method("rtc.set", &json!({"time": "later"})).unwrap_err();
        assert_eq!(err.code, proto::INVALID_PARAMS);
    }

    #[test]
    fn rtc_set_seeds_freezes_and_advances_the_clock() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();

        // Seed with an RFC 6238 vector instant via the calendar notation.
        let set = exec_core(
            &mut emu,
            &mut ctx,
            &core("rtc.set", json!({"time": "2005-03-18 01:58:29"})),
        )
        .unwrap();
        assert_eq!(set["unix"], 1_111_111_109u64);
        assert_eq!(set["time"], "2005-03-18T01:58:29");
        assert_eq!(set["frozen"], false);

        let get = exec_core(&mut emu, &mut ctx, &CoreOp::RtcGet).unwrap();
        assert_eq!(get["present"], true);
        assert_eq!(get["seeded"], true);
        assert_eq!(get["unix"], 1_111_111_109u64);

        // Freeze in place, then step the frozen clock forward one TOTP
        // window at a time.
        let set = exec_core(
            &mut emu,
            &mut ctx,
            &core("rtc.set", json!({"frozen": true})),
        )
        .unwrap();
        assert_eq!(set["frozen"], true);
        assert_eq!(set["unix"], 1_111_111_109u64);
        let set = exec_core(&mut emu, &mut ctx, &core("rtc.set", json!({"advance": 30}))).unwrap();
        assert_eq!(set["unix"], 1_111_111_139u64);
        assert_eq!(set["frozen"], true);

        // Unfreeze without a time: the clock resumes from where it stood.
        let set = exec_core(
            &mut emu,
            &mut ctx,
            &core("rtc.set", json!({"frozen": false})),
        )
        .unwrap();
        assert_eq!(set["unix"], 1_111_111_139u64);
        assert_eq!(set["frozen"], false);
    }

    #[test]
    fn break_ids_track_the_machine_store() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        let add = core("break.add", json!({"kind": "pc", "addr": "$F8001A"}));
        let id = exec_core(&mut emu, &mut ctx, &add).unwrap()["id"]
            .as_u64()
            .unwrap() as u32;
        assert!(emu.machine.ui_breaks().is_breakpoint(0xF8001A));

        // Duplicate installs are refused, not silently toggled away.
        let err = exec_core(&mut emu, &mut ctx, &add).unwrap_err();
        assert_eq!(err.code, proto::INVALID_PARAMS);
        assert!(emu.machine.ui_breaks().is_breakpoint(0xF8001A));

        let list = exec_core(&mut emu, &mut ctx, &CoreOp::BreakList).unwrap();
        assert_eq!(list["breaks"][0]["kind"], "pc");
        assert_eq!(list["breaks"][0]["id"], id);

        exec_core(&mut emu, &mut ctx, &CoreOp::BreakRemove { id }).unwrap();
        assert!(!emu.machine.ui_breaks().is_breakpoint(0xF8001A));
        let err = exec_core(&mut emu, &mut ctx, &CoreOp::BreakRemove { id }).unwrap_err();
        assert_eq!(err.code, proto::NOT_FOUND);
    }

    #[test]
    fn break_list_reports_watch_qualifiers() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        let add = core(
            "break.add",
            json!({"kind": "watch", "addr": "$2000", "class": "spr3"}),
        );
        let id = exec_core(&mut emu, &mut ctx, &add).unwrap()["id"]
            .as_u64()
            .unwrap() as u32;
        let add = core(
            "break.add",
            json!({"kind": "watch", "addr": "$3000", "class": "cpu", "pc": "$F80010"}),
        );
        let pc_id = exec_core(&mut emu, &mut ctx, &add).unwrap()["id"]
            .as_u64()
            .unwrap() as u32;

        let list = exec_core(&mut emu, &mut ctx, &CoreOp::BreakList).unwrap();
        let entries = list["breaks"].as_array().unwrap();
        let spr = entries.iter().find(|e| e["addr"] == 0x2000).unwrap();
        assert_eq!(spr["kind"], "watch");
        assert_eq!(spr["class"], "spr3");
        assert!(spr.get("pc").is_none());
        assert_eq!(spr["id"], id);
        let cpu = entries.iter().find(|e| e["addr"] == 0x3000).unwrap();
        assert_eq!(cpu["class"], "cpu");
        assert_eq!(cpu["pc"], 0xF80010);
        assert_eq!(cpu["id"], pc_id);
    }

    #[test]
    fn loadseg_break_kind_parses_toggles_and_lists() {
        // Parse: bare and name-filtered forms; an unknown kind still names
        // the full kind list.
        let spec = parse_method("break.add", &json!({"kind": "loadseg"})).unwrap();
        assert!(matches!(
            spec,
            Request::Core(CoreOp::BreakAdd(BreakSpec::LoadSeg { name: None }))
        ));
        let spec = parse_method("break.add", &json!({"kind": "loadseg", "name": "hello"})).unwrap();
        assert!(matches!(
            spec,
            Request::Core(CoreOp::BreakAdd(BreakSpec::LoadSeg { name: Some(ref n) }))
                if n == "hello"
        ));
        let err = parse_method("break.add", &json!({"kind": "loadprog"})).unwrap_err();
        assert!(err.message.contains("loadseg"), "err: {}", err.message);

        // Install/list/remove round trip through the machine store.
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        let add = core("break.add", json!({"kind": "loadseg", "name": "hello"}));
        let id = exec_core(&mut emu, &mut ctx, &add).unwrap()["id"]
            .as_u64()
            .unwrap() as u32;
        assert!(emu.machine.ui_breaks().loadseg_catch.is_some());
        let list = exec_core(&mut emu, &mut ctx, &CoreOp::BreakList).unwrap();
        let entry = list["breaks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["kind"] == "loadseg")
            .expect("loadseg entry listed");
        assert_eq!(entry["name"], "hello");
        assert_eq!(entry["id"], id);
        exec_core(&mut emu, &mut ctx, &CoreOp::BreakRemove { id }).unwrap();
        assert!(emu.machine.ui_breaks().loadseg_catch.is_none());
    }

    #[test]
    fn break_list_reports_gui_set_points_without_ids() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        emu.machine.ui_set_breakpoint(0xF80014, None, 0);
        let list = exec_core(&mut emu, &mut ctx, &CoreOp::BreakList).unwrap();
        assert_eq!(list["breaks"][0]["addr"], 0xF80014);
        assert!(list["breaks"][0].get("id").is_none());
    }

    #[test]
    fn digest_is_stable_without_stepping() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        let a = exec_core(&mut emu, &mut ctx, &CoreOp::Digest).unwrap();
        let b = exec_core(&mut emu, &mut ctx, &CoreOp::Digest).unwrap();
        assert_eq!(a["digest"], b["digest"]);
        assert_eq!(a["width"], FB_WIDTH);
    }

    /// Findings of one kind from a report, for the validator tests.
    fn findings_of(report: &Value, kind: &str) -> Vec<Value> {
        report["findings"]
            .as_array()
            .expect("findings array")
            .iter()
            .filter(|f| f["kind"] == kind)
            .cloned()
            .collect()
    }

    #[test]
    fn a_write_above_the_register_bank_does_not_panic_the_validator() {
        let mut emu = test_emulator();
        emu.bus_mut().set_chipset_validation(true);
        // $DFF200 upward is inside the custom page but past the register
        // bank; a guest can write there and the validator must survive it.
        emu.bus_mut().custom_write(0xDFF200, 2, 0x1234);
        emu.bus_mut().custom_write(0xDFFFFE, 2, 0x1234);
    }

    #[test]
    fn the_validator_flags_undefined_bits_and_names_the_writer() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        exec_core(
            &mut emu,
            &mut ctx,
            &core("chipset.validate", json!({"enabled": true})),
        )
        .unwrap();
        // BPLCON2 bit 15 has no function on any chipset revision.
        emu.bus_mut().custom_write(0xDFF104, 2, 0x8020);
        let report = exec_core(&mut emu, &mut ctx, &CoreOp::ChipsetReport).unwrap();
        let bits = findings_of(&report, "unused-bits");
        assert_eq!(bits.len(), 1, "{report}");
        assert_eq!(bits[0]["reg"], "BPLCON2");
        assert_eq!(bits[0]["by"], "cpu");
        assert!(
            bits[0]["detail"]
                .as_str()
                .unwrap()
                .contains("undefined bits 0x8000"),
            "{}",
            bits[0]["detail"]
        );
    }

    #[test]
    fn the_validator_flags_absent_registers_byte_access_and_stray_pointers() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        emu.bus_mut().set_chipset_validation(true);
        // FMODE exists only on AGA Alice; the test machine is an OCS
        // A500, so this write is decoded and then dropped.
        emu.bus_mut().custom_write(0xDFF1FC, 2, 0x0003);
        // A byte write to a word register: no byte lanes on the custom bus.
        emu.bus_mut().custom_write(0xDFF181, 1, 0x0F);
        // A bitplane pointer aimed at $00200000, far past the 512 KiB of
        // chip RAM this machine's Agnus can address.
        emu.bus_mut().custom_write(0xDFF0E0, 2, 0x0020);
        let report = exec_core(&mut emu, &mut ctx, &CoreOp::ChipsetReport).unwrap();
        assert_eq!(findings_of(&report, "absent-register").len(), 1, "{report}");
        assert_eq!(findings_of(&report, "byte-access").len(), 1, "{report}");
        let stray = findings_of(&report, "pointer-outside-chip-ram");
        assert_eq!(stray.len(), 1, "{report}");
        assert_eq!(stray[0]["reg"], "BPL1PTH");
    }

    #[test]
    fn the_validator_flags_a_write_to_a_read_only_register() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        emu.bus_mut().set_chipset_validation(true);
        // DMACONR is the read side; DMACON is $096. Writing $002 is a
        // write that lands nowhere.
        emu.bus_mut().custom_write(0xDFF002, 2, 0x8200);
        let report = exec_core(&mut emu, &mut ctx, &CoreOp::ChipsetReport).unwrap();
        let wrong = findings_of(&report, "wrong-direction");
        assert_eq!(wrong.len(), 1, "{report}");
        assert_eq!(wrong[0]["reg"], "DMACONR");
    }

    #[test]
    fn an_unarmed_machine_reports_nothing_and_refuses_the_writer_query() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        emu.bus_mut().custom_write(0xDFF104, 2, 0x8020);
        let report = exec_core(&mut emu, &mut ctx, &CoreOp::ChipsetReport).unwrap();
        assert_eq!(report["armed"], false);
        assert!(report["findings"].as_array().unwrap().is_empty());
        let err = exec_core(&mut emu, &mut ctx, &CoreOp::CustomWriter { off: 0x104 }).unwrap_err();
        assert_eq!(err.code, proto::INVALID_STATE);
    }

    #[test]
    fn the_last_writer_table_records_value_and_writer() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        emu.bus_mut().set_chipset_validation(true);
        emu.bus_mut().custom_write(0xDFF180, 2, 0x0123);
        let who = exec_core(
            &mut emu,
            &mut ctx,
            &core("custom.writer", json!({"reg": "COLOR00"})),
        )
        .unwrap();
        assert_eq!(who["written"], true);
        assert_eq!(who["value"], 0x0123);
        assert_eq!(who["by"], "cpu");
        // A register nothing has written says so rather than inventing a
        // writer.
        let never = exec_core(&mut emu, &mut ctx, &CoreOp::CustomWriter { off: 0x1BE }).unwrap();
        assert_eq!(never["written"], false);
    }

    #[test]
    fn the_validator_flags_a_blit_that_cannot_run() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        emu.bus_mut().set_chipset_validation(true);
        // BLTSIZE with blitter DMA off: the blit never runs and never
        // raises its completion interrupt, which is a hang, not a glitch.
        emu.bus_mut().custom_write(0xDFF058, 2, 0x0041);
        let report = exec_core(&mut emu, &mut ctx, &CoreOp::ChipsetReport).unwrap();
        let dead = findings_of(&report, "blitter-dma-off");
        assert_eq!(dead.len(), 1, "{report}");
        assert_eq!(dead[0]["reg"], "BLTSIZE");
        assert!(
            dead[0]["detail"]
                .as_str()
                .unwrap()
                .contains("never run or raise its completion interrupt"),
            "{}",
            dead[0]["detail"]
        );
    }

    #[test]
    fn the_validator_flags_disk_dma_armed_against_an_empty_drive() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        emu.bus_mut().set_chipset_validation(true);
        // The test machine has no disk in df0, so a read armed here can
        // never be served -- the class that produced the Gods and Shadow
        // of the Beast dead-spins. Written twice, because that is Paula's
        // arming interlock and the first write only latches the value.
        emu.bus_mut().custom_write(0xDFF024, 2, 0x8000 | 0x1000);
        emu.bus_mut().custom_write(0xDFF024, 2, 0x8000 | 0x1000);
        let report = exec_core(&mut emu, &mut ctx, &CoreOp::ChipsetReport).unwrap();
        let stuck = findings_of(&report, "disk-not-ready");
        assert_eq!(stuck.len(), 1, "{report}");
        assert_eq!(stuck[0]["reg"], "DSKLEN");

        // A single write arms nothing, so it is not reported: saying so
        // would name a write after which DMA is by design not running.
        let mut emu = test_emulator();
        emu.bus_mut().set_chipset_validation(true);
        emu.bus_mut().custom_write(0xDFF024, 2, 0x8000 | 0x1000);
        let report = exec_core(&mut emu, &mut ctx, &CoreOp::ChipsetReport).unwrap();
        assert!(
            findings_of(&report, "disk-not-ready").is_empty(),
            "{report}"
        );
    }

    #[test]
    fn the_heat_map_records_what_touched_the_address_space() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        let armed = exec_core(
            &mut emu,
            &mut ctx,
            &core("memory.heatmap", json!({"enabled": true})),
        )
        .unwrap();
        assert_eq!(armed["armed"], true);
        assert_eq!(armed["bytes_per_cell"], 256);
        // The test ROM loops and stores to chip RAM, so after a few
        // instructions the map has both CPU reads (the fetches) and a
        // CPU write.
        for _ in 0..8 {
            emu.debug_step_instructions(1).unwrap();
        }
        let report = exec_core(&mut emu, &mut ctx, &CoreOp::HeatMapReport { path: None }).unwrap();
        let census = report["census"].as_array().unwrap();
        let kinds: Vec<&str> = census.iter().map(|c| c["by"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"cpu-read"), "{report}");
        assert!(kinds.contains(&"cpu-write"), "{report}");
    }

    #[test]
    fn heat_map_parameters_are_validated_rather_than_defaulted() {
        assert!(parse_method("memory.heatmap", &json!({"base": "nope"})).is_err());
        assert!(parse_method("memory.heatmap", &json!({"span": {}})).is_err());
        // A bad value is still a bad value when the call is disarming.
        assert!(parse_method("memory.heatmap", &json!({"enabled": false, "base": []})).is_err());
        assert_eq!(
            parse_method("memory.heatmap", &json!({"enabled": false})).unwrap(),
            Request::Core(CoreOp::HeatMapSet { window: None })
        );
    }

    #[test]
    fn an_unarmed_heat_map_refuses_to_report() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        let err = exec_core(&mut emu, &mut ctx, &CoreOp::HeatMapReport { path: None }).unwrap_err();
        assert_eq!(err.code, proto::INVALID_STATE);
    }

    #[test]
    fn region_digest_covers_only_its_rectangle() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        let rect = FrameRect {
            x: 4,
            y: 8,
            w: 16,
            h: 4,
        };
        let a = exec_core(&mut emu, &mut ctx, &CoreOp::RegionDigest { rect }).unwrap();
        let b = exec_core(&mut emu, &mut ctx, &CoreOp::RegionDigest { rect }).unwrap();
        assert_eq!(a["digest"], b["digest"]);
        assert_eq!(a["x"], 4);
        assert_eq!(a["w"], 16);
        // The reply reports the frame geometry, not the region's, so a
        // caller can tell why stale coordinates were rejected.
        assert_eq!(a["width"], FB_WIDTH);
        // A different rectangle of the same frame is a different digest
        // (the test ROM's display is not uniform across these rows).
        let whole = exec_core(&mut emu, &mut ctx, &CoreOp::Digest).unwrap();
        assert_ne!(a["digest"], whole["digest"]);
    }

    #[test]
    fn region_digest_rejects_a_rectangle_off_the_frame() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        let err = exec_core(
            &mut emu,
            &mut ctx,
            &CoreOp::RegionDigest {
                rect: FrameRect {
                    x: 0,
                    y: 0,
                    w: FB_WIDTH + 1,
                    h: 1,
                },
            },
        )
        .unwrap_err();
        assert_eq!(err.code, proto::INVALID_PARAMS);
    }

    /// The parsed `run_until` target, for the stable-frame parse tests.
    fn run_target(params: Value) -> RunTarget {
        match parse_method("run_until", &params).expect("parse should succeed") {
            Request::Host(HostOp::Resume(ResumeVerb {
                kind: ResumeKind::RunUntil(target),
                ..
            })) => target,
            other => panic!("expected a run_until target, got {other:?}"),
        }
    }

    #[test]
    fn run_until_parses_stable_frames_with_an_optional_region() {
        assert_eq!(
            run_target(json!({"stable_frames": 3})),
            RunTarget::Stable(StableSpec {
                frames: 3,
                max_frames: None,
                rect: None,
            })
        );
        // Region params narrow what has to hold still; a partial region
        // is a parse error from parse_frame_rect, not a silent default.
        assert_eq!(
            run_target(
                json!({"stable_frames": 2, "max_frames": 600, "x": 10, "y": 20, "w": 4, "h": 4})
            ),
            RunTarget::Stable(StableSpec {
                frames: 2,
                max_frames: Some(600),
                rect: Some(FrameRect {
                    x: 10,
                    y: 20,
                    w: 4,
                    h: 4
                }),
            })
        );
        // A partial region is an error, not a quiet whole-frame wait:
        // that includes an origin with no size.
        assert!(parse_method("run_until", &json!({"stable_frames": 2, "w": 4})).is_err());
        assert!(parse_method("run_until", &json!({"stable_frames": 2, "x": 10, "y": 20})).is_err());
        assert!(parse_method("run_until", &json!({"stable_frames": 2, "h": 4})).is_err());
    }

    #[test]
    fn stable_watch_needs_consecutive_repeats_and_gives_up_on_budget() {
        let spec = StableSpec {
            frames: 3,
            max_frames: Some(6),
            rect: None,
        };
        let mut watch = StableWatch::new(spec);
        // A repeat that is broken before reaching the target restarts the
        // run: "3 consecutive" must not be satisfied by 3 frames total.
        assert!(matches!(watch.note(1), StableStep::Running));
        assert!(matches!(watch.note(1), StableStep::Running));
        assert!(matches!(watch.note(2), StableStep::Running));
        assert!(matches!(watch.note(1), StableStep::Running));
        assert!(matches!(watch.note(1), StableStep::Running));
        // Sixth sample: the budget expires before a third repeat, and the
        // detail reports the longest run reached rather than the current.
        match watch.note(3) {
            StableStep::GaveUp(detail) => {
                assert!(detail.contains("longest run 2 of 3"), "{detail}");
            }
            other => panic!("expected the budget to expire, got {}", step_name(&other)),
        }

        let mut watch = StableWatch::new(StableSpec {
            max_frames: None,
            ..spec
        });
        assert!(matches!(watch.note(7), StableStep::Running));
        assert!(matches!(watch.note(7), StableStep::Running));
        assert!(matches!(watch.note(7), StableStep::Settled(_)));
    }

    fn step_name(step: &StableStep) -> &'static str {
        match step {
            StableStep::Running => "running",
            StableStep::Settled(_) => "settled",
            StableStep::GaveUp(_) => "gave up",
        }
    }

    #[test]
    fn run_until_stable_frames_rejects_degenerate_specs() {
        // One frame is trivially stable, so it would return instantly and
        // tell the caller nothing.
        assert!(parse_method("run_until", &json!({"stable_frames": 1})).is_err());
        // A budget below the target could never be met.
        assert!(parse_method("run_until", &json!({"stable_frames": 4, "max_frames": 3})).is_err());
        // Targets stay mutually exclusive.
        assert!(parse_method("run_until", &json!({"stable_frames": 2, "frame": 10})).is_err());
    }

    #[test]
    fn a_pc_qualified_watch_on_a_dma_class_is_refused() {
        // The pair can never match, so accepting it would install a
        // watch that silently never fires.
        for class in ["blitter", "disk", "copper", "spr3", "bpl1", "aud0"] {
            let bad = json!({"kind": "watch", "addr": 0x1000, "class": class, "pc": 0x2000});
            assert!(
                parse_method("break.add", &bad).is_err(),
                "{class} + pc should be refused"
            );
        }
        // The useful combination, and each qualifier alone, still parse.
        for good in [
            json!({"kind": "watch", "addr": 0x1000, "class": "cpu", "pc": 0x2000}),
            json!({"kind": "watch", "addr": 0x1000, "pc": 0x2000}),
            json!({"kind": "watch", "addr": 0x1000, "class": "spr3"}),
        ] {
            assert!(parse_method("break.add", &good).is_ok(), "{good}");
        }
    }

    #[test]
    fn a_region_that_overflows_is_rejected_by_the_digest_itself() {
        // FrameRect is public, so the bounds check cannot assume the JSON
        // parser built it.
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        let err = exec_core(
            &mut emu,
            &mut ctx,
            &CoreOp::RegionDigest {
                rect: FrameRect {
                    x: usize::MAX,
                    y: 0,
                    w: 8,
                    h: 8,
                },
            },
        )
        .unwrap_err();
        assert_eq!(err.code, proto::INVALID_PARAMS);
    }

    #[test]
    fn a_fault_window_that_wraps_the_address_space_is_refused() {
        // A wrapped window matches nothing, so the injected fault would
        // silently never fire.
        assert!(parse_method("fault.inject", &json!({"addr": 0xFFFF_FFFFu32, "len": 2})).is_err());
        assert!(parse_method("fault.inject", &json!({"addr": 0xFFFF_FFFEu32, "len": 2})).is_ok());
    }

    #[test]
    fn region_digest_parses_defaults_and_rejects_empty_rectangles() {
        assert_eq!(
            core("capture.region_digest", json!({"w": 8, "h": 4})),
            CoreOp::RegionDigest {
                rect: FrameRect {
                    x: 0,
                    y: 0,
                    w: 8,
                    h: 4
                }
            }
        );
        assert!(parse_method("capture.region_digest", &json!({"w": 0, "h": 4})).is_err());
        assert!(parse_method("capture.region_digest", &json!({"h": 4})).is_err());
    }

    #[test]
    fn collect_evaluates_each_item_independently() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        let items = vec![
            CoreOp::RegsGet,
            CoreOp::CustomRead { off: 0x1FF }, // odd/unreadable: per-item err
            CoreOp::BeamGet,
        ];
        let results = eval_collect(&mut emu, &mut ctx, &items);
        assert_eq!(results.len(), 3);
        assert!(results[0].get("ok").is_some());
        assert!(results[1].get("err").is_some());
        assert!(results[2].get("ok").is_some());
    }

    #[test]
    fn status_reports_position_and_host_flags() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        ctx.running = true;
        ctx.pending = true;
        let status = exec_core(&mut emu, &mut ctx, &CoreOp::Status).unwrap();
        assert_eq!(status["state"], "running");
        assert_eq!(status["pending_resume"], true);
        assert_eq!(status["pc"], 0xF80010);
        assert_eq!(status["tt_armed"], false);
    }

    #[test]
    fn reverse_without_time_travel_is_an_invalid_state() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        let err = exec_core(&mut emu, &mut ctx, &CoreOp::ReverseStep { n: 1 }).unwrap_err();
        assert_eq!(err.code, proto::INVALID_STATE);
    }

    #[test]
    fn input_key_tap_expands_to_press_plus_scheduled_release() {
        let cmd = InputCmd::Key {
            rawkey: 0x45,
            kind: KeyKind::Tap { hold_ms: 100 },
            at_seconds: None,
        };
        let (now, later) = cmd.expand(2.0);
        assert_eq!(
            now,
            vec![InputAction::Key {
                rawkey: 0x45,
                pressed: true
            }]
        );
        assert_eq!(later.len(), 1);
        assert!((later[0].at_seconds - 2.1).abs() < 1e-9);
        assert_eq!(
            later[0].action,
            InputAction::Key {
                rawkey: 0x45,
                pressed: false
            }
        );
    }

    #[test]
    fn stop_reason_mapping_names_the_hardware_event() {
        let (reason, detail) = stop_reason_of(&DebugStop::Beam {
            vpos: 100,
            hpos: 60,
        });
        assert_eq!(reason, "beam_trap");
        assert!(detail.contains("100"));
        let (reason, _) = stop_reason_of(&DebugStop::Breakpoint { pc: 0x1000 });
        assert_eq!(reason, "breakpoint");
    }
}
