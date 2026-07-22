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
                | CoreOp::Screenshot { .. }
        )
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
}

impl RunTarget {
    pub fn describe(&self) -> String {
        match self {
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
    match targets.len() {
        1 => Ok(targets.remove(0)),
        0 => Err(CtlError::invalid_params(
            "run_until needs exactly one of pc, vpos[+hpos], frame, cck, seconds",
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
            Ok(BreakSpec::Watch {
                addr: p.u32_req("addr")?,
                source: match p.str_opt("class")? {
                    None => None,
                    Some(token) => Some(WatchSource::parse(&token).ok_or_else(|| {
                        CtlError::invalid_params("class must be cpu|blitter|disk")
                    })?),
                },
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
        other => Err(CtlError::invalid_params(format!(
            "kind must be pc|watch|reg_watch|beam|copper|catch, got {other}"
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
        "unknown register: {name} (want d0-d7, a0-a7, sp, sr, pc)"
    )))
}

/// A custom register selector: a name ("DMACON"), or an offset as a
/// number or hex string.
fn parse_custom_reg_param(p: &ParamReader) -> Result<u16, CtlError> {
    let Some(v) = p.get("reg") else {
        return Err(CtlError::invalid_params("needs reg (name or offset)"));
    };
    if let Some(s) = v.as_str() {
        return crate::gdbstub::parse_custom_reg(&s.to_ascii_uppercase())
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
        CoreOp::Digest => Ok(digest_value(emu)),
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
            source: None,
        };
        let mut entry = json!({"kind": "watch", "addr": w.addr});
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
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    PathBuf::from(format!("copperline-trace-{stamp}.txt"))
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

/// FNV-1a over the framebuffer words (little-endian byte order), for
/// cheap change detection without pulling pixels over the wire.
fn fnv1a64(words: &[u32]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
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
        ]});
        match parse_method("continue", &params).unwrap() {
            Request::Host(HostOp::Resume(verb)) => assert_eq!(verb.collect.len(), 2),
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
