// SPDX-License-Identifier: GPL-3.0-or-later

//! The control protocol as a tool catalogue: one entry per CCP method,
//! with the description, JSON Schema, and example that an MCP client
//! (or any other tool-calling agent) needs to call it without reading
//! the protocol reference.
//!
//! The entries mirror `exec::parse_method` (and the `shutdown` verb the
//! drivers answer themselves); the tests pin the two to each other, so a
//! method added to the parser without a catalogue entry fails the suite,
//! and every example here must parse.
//!
//! Tool names: MCP and the model-side tool namespaces only allow
//! `[a-zA-Z0-9_-]`, so a method's tool name is the method with dots
//! replaced by underscores (`warp.get` -> `warp_get`). The mapping back
//! is a table lookup, never a string edit, because method names such as
//! `run_until` carry underscores of their own.

use serde_json::{json, Map, Value};
use std::sync::OnceLock;

/// One control-protocol method as a tool.
#[derive(Debug, Clone)]
pub struct ToolDef {
    /// The wire method name (`capture.screenshot`).
    pub method: &'static str,
    /// The tool name (`capture_screenshot`).
    pub name: String,
    /// One paragraph for the model: what it does, what it returns, and
    /// whether it blocks.
    pub description: String,
    /// JSON Schema (draft 2020-12 subset) for the params object.
    pub schema: Value,
    /// Example params that parse and are valid against `schema`.
    pub example: Value,
    /// The tool takes the MCP bridge's `wait_ms` (a resume verb whose
    /// reply is the eventual stop event); the bridge strips it before
    /// forwarding.
    pub wait_ms: bool,
}

/// The tool name for a method: dots become underscores.
pub fn tool_name(method: &str) -> String {
    method.replace('.', "_")
}

/// The whole catalogue, in reference order.
pub fn catalogue() -> &'static [ToolDef] {
    static TABLE: OnceLock<Vec<ToolDef>> = OnceLock::new();
    TABLE.get_or_init(build)
}

/// Look a tool up by its tool name.
pub fn find(name: &str) -> Option<&'static ToolDef> {
    catalogue().iter().find(|def| def.name == name)
}

/// Look a tool up by its wire method name.
pub fn find_method(method: &str) -> Option<&'static ToolDef> {
    catalogue().iter().find(|def| def.method == method)
}

// ---------------------------------------------------------------------
// Schema helpers

const ADDR_NOTE: &str =
    "an integer, or a hex string with a 0x or $ prefix (\"0xDFF096\", \"$C00000\")";

/// A 32-bit address or value: integer or hex string.
fn addr(desc: &str) -> Value {
    json!({"type": ["integer", "string"], "description": format!("{desc}: {ADDR_NOTE}")})
}

/// An unsigned integer with optional bounds. The parser also accepts a
/// hex string for these, so the schema allows both.
fn uint(desc: &str, min: Option<u64>, max: Option<u64>) -> Value {
    let mut v = json!({"type": ["integer", "string"], "description": desc});
    if let Some(min) = min {
        v["minimum"] = json!(min);
    }
    if let Some(max) = max {
        v["maximum"] = json!(max);
    }
    v
}

/// A plain (decimal-only) integer.
fn int(desc: &str, min: Option<i64>, max: Option<i64>) -> Value {
    let mut v = json!({"type": "integer", "description": desc});
    if let Some(min) = min {
        v["minimum"] = json!(min);
    }
    if let Some(max) = max {
        v["maximum"] = json!(max);
    }
    v
}

fn number(desc: &str) -> Value {
    json!({"type": "number", "description": desc})
}

fn boolean(desc: &str) -> Value {
    json!({"type": "boolean", "description": desc})
}

fn string(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}

fn enumeration(desc: &str, values: &[&str]) -> Value {
    json!({"type": "string", "enum": values, "description": desc})
}

fn port(desc: &str) -> Value {
    json!({"type": "integer", "enum": [1, 2], "description": desc})
}

fn at_seconds() -> Value {
    number(
        "Emulated time (absolute seconds) at which to apply the input; absent or in the past \
         applies it now",
    )
}

/// An object schema from `(name, schema)` pairs and the required names.
fn object(props: Vec<(&str, Value)>, required: &[&str]) -> Value {
    let mut map = Map::new();
    for (name, schema) in props {
        map.insert(name.to_string(), schema);
    }
    let mut v = json!({"type": "object", "properties": Value::Object(map)});
    if !required.is_empty() {
        v["required"] = json!(required);
    }
    v
}

fn no_params() -> Value {
    json!({"type": "object", "properties": {}})
}

fn collect() -> (&'static str, Value) {
    (
        "collect",
        json!({
            "type": "array",
            "description": "Read-only methods evaluated at the stop and returned as `collect` \
                            on the stop event, so one round trip both runs and inspects. Each \
                            item is {\"method\": ..., \"params\": ...} naming an inspection \
                            method (status, regs.get, mem.read, disasm, custom.read, \
                            custom.dump, beam.get, cia.get, copper.list, capture.digest...).",
            "items": {
                "type": "object",
                "properties": {
                    "method": {"type": "string"},
                    "params": {"type": "object"}
                },
                "required": ["method"]
            }
        }),
    )
}

fn wait_ms() -> (&'static str, Value) {
    (
        "wait_ms",
        int(
            "MCP bridge option: host milliseconds to wait for the stop. If the machine has \
             not stopped by then the bridge sends `pause` and returns the resulting stop \
             event, with `bridge.paused_after_ms` set. Without it the call blocks until the \
             machine stops on its own, which for a run with no stop condition is forever.",
            Some(1),
            None,
        ),
    )
}

const STOP_EVENT: &str = "Returns the stop event: {reason, detail, pc, frame, vpos, hpos, cck, \
                          seconds, retired_instructions} plus `collect` results if requested. \
                          Reasons include breakpoint, watch, step, target, pause, catch, \
                          loadseg, budget, double_fault.";

// ---------------------------------------------------------------------
// The table

struct Entry {
    method: &'static str,
    description: String,
    schema: Value,
    example: Value,
    wait_ms: bool,
}

fn entry(
    method: &'static str,
    description: impl Into<String>,
    schema: Value,
    example: Value,
) -> Entry {
    Entry {
        method,
        description: description.into(),
        schema,
        example,
        wait_ms: false,
    }
}

fn resume_entry(
    method: &'static str,
    description: impl Into<String>,
    mut props: Vec<(&str, Value)>,
    required: &[&str],
    example: Value,
) -> Entry {
    props.push(collect());
    props.push(wait_ms());
    Entry {
        method,
        description: description.into(),
        schema: object(props, required),
        example,
        wait_ms: true,
    }
}

fn build() -> Vec<ToolDef> {
    let entries: Vec<Entry> = vec![
        // Session
        entry(
            "status",
            "Report the emulation state: `state` (paused/running), `pc`, `frame`, `vpos`, \
             `hpos`, `cck` (colour clocks), `seconds` (emulated), `retired_instructions`, \
             `cpu`, `paced`/`warp`, `tt_armed` (reverse execution available), \
             `double_faulted`, and host counters (`host_busy_ms`, `pacer_slips`, audio lead \
             and underruns). Cheap and side-effect free; call it to orient before stepping.",
            no_params(),
            json!({}),
        ),
        entry(
            "shutdown",
            "Terminate the emulator process cleanly. The reply arrives before the process \
             exits; the bridge then closes its session. Use session_close to disconnect \
             without stopping an emulator you did not launch.",
            no_params(),
            json!({}),
        ),
        // Execution control
        resume_entry(
            "continue",
            format!(
                "Resume execution and block until the machine stops (a breakpoint, watch, \
                 catch, loadseg trap, or a bridge pause after `wait_ms`). {STOP_EVENT} A \
                 continue with no breakpoint and no wait_ms never returns."
            ),
            vec![],
            &[],
            json!({"wait_ms": 1000}),
        ),
        resume_entry(
            "step",
            format!("Execute `n` CPU instructions (default 1, at most 1000000) and stop. {STOP_EVENT}"),
            vec![("n", int("Instructions to execute", Some(1), Some(1_000_000)))],
            &[],
            json!({"n": 1}),
        ),
        resume_entry(
            "step_over",
            format!(
                "Execute the instruction at PC treating a subroutine call (JSR/BSR) as one \
                 step, bounded by a 5000000-instruction budget. {STOP_EVENT}"
            ),
            vec![],
            &[],
            json!({}),
        ),
        resume_entry(
            "step_out",
            format!(
                "Run until the current subroutine returns, bounded by a \
                 5000000-instruction budget. {STOP_EVENT}"
            ),
            vec![],
            &[],
            json!({}),
        ),
        resume_entry(
            "step_copper",
            format!("Run until the Copper executes its next instruction. {STOP_EVENT}"),
            vec![],
            &[],
            json!({}),
        ),
        resume_entry(
            "step_frame",
            format!(
                "Run `n` complete video frames (default 1, at most 1000000) and stop at \
                 the frame boundary; a breakpoint hit inside the run stops early. \
                 {STOP_EVENT}"
            ),
            vec![("n", int("Frames to run", Some(1), Some(1_000_000)))],
            &[],
            json!({"n": 1}),
        ),
        resume_entry(
            "run_until",
            format!(
                "Run until exactly one target is reached: `pc` (an address), `pc_outside` \
                 ([low, high], or true for the $F80000-$FFFFFF ROM window), `vpos` \
                 (optionally with `hpos`, the next time the beam reaches that line), \
                 `frame` (absolute frame number), `cck` (absolute colour clock), \
                 `seconds` (absolute emulated seconds), or `stable_frames` (the display \
                 stops changing: that many consecutive identical frames, optionally only \
                 within the region x/y/w/h, giving up after `max_frames`). A breakpoint \
                 hit on the way stops early. {STOP_EVENT}"
            ),
            vec![
                ("pc", addr("Stop when PC reaches this address")),
                (
                    "pc_outside",
                    json!({
                        "description": "Stop when PC leaves [low, high]; true uses the ROM window",
                        "oneOf": [
                            {"type": "boolean", "const": true},
                            {"type": "array", "minItems": 2, "maxItems": 2, "items": {"description": "address"}}
                        ]
                    }),
                ),
                ("vpos", uint("Stop at this beam line (0-based)", Some(0), Some(65535))),
                (
                    "hpos",
                    uint(
                        "With vpos: stop at this horizontal position (colour clock) on the line",
                        Some(0),
                        Some(65535),
                    ),
                ),
                ("frame", uint("Stop at this absolute frame number", Some(0), None)),
                ("cck", uint("Stop at this absolute colour clock", Some(0), None)),
                ("seconds", number("Stop at this absolute emulated time in seconds")),
                (
                    "stable_frames",
                    int(
                        "Stop once this many consecutive frames render identically (at least 2)",
                        Some(2),
                        None,
                    ),
                ),
                (
                    "max_frames",
                    int(
                        "With stable_frames: give up after this many frames (reason \"budget\")",
                        Some(2),
                        None,
                    ),
                ),
                ("x", int("With stable_frames: region left edge (default 0)", Some(0), None)),
                ("y", int("With stable_frames: region top edge (default 0)", Some(0), None)),
                ("w", int("With stable_frames: region width", Some(1), None)),
                ("h", int("With stable_frames: region height", Some(1), None)),
            ],
            &[],
            json!({"seconds": 2.0}),
        ),
        entry(
            "pause",
            "Stop a running machine at the next quantum boundary and return its position as \
             a stop event. Through this bridge the machine is only running inside a \
             blocking resume call, so this normally reports the current paused position; \
             use `wait_ms` on the resume verbs to bound a run instead.",
            no_params(),
            json!({}),
        ),
        entry(
            "machine.reset",
            "Reset the emulated machine: `warm` (default, like Ctrl-Amiga-Amiga) or \
             `cold` (power cycle, memory cleared). Breakpoints stay installed.",
            object(vec![("kind", enumeration("Reset kind (default warm)", &["warm", "cold"]))], &[]),
            json!({"kind": "warm"}),
        ),
        // Speed
        entry(
            "warp.get",
            "Report whether warp (unpaced emulation) is on, whether the machine is paced \
             to real time, and who holds warp (`source`: none, manual, control, guest, \
             launch, boot, capture, headless). A headless (--control) server is unpaced \
             end to end and reports `headless: true`.",
            no_params(),
            json!({}),
        ),
        entry(
            "warp.set",
            "Engage or release warp on a windowed (--control-gui) session: `on: true` runs \
             the machine unpaced with audio muted, `on: false` re-paces it and cancels a \
             pending --run/--warp-boot phase. Accepted as a no-op with a note by a headless \
             server. Released automatically when the session disconnects.",
            object(vec![("on", boolean("true to engage warp, false to release it"))], &["on"]),
            json!({"on": true}),
        ),
        // Reverse execution
        entry(
            "reverse_step",
            "Step backward `n` instructions (default 1) through the recorded timeline. \
             Needs time travel armed (`tt_armed` in status). Returns the new position or \
             `history_exhausted`.",
            object(vec![("n", int("Instructions to step back", Some(1), None))], &[]),
            json!({"n": 1}),
        ),
        entry(
            "reverse_frame",
            "Step backward one video frame through the recorded timeline (time travel \
             must be armed).",
            no_params(),
            json!({}),
        ),
        entry(
            "reverse_continue",
            "Run backward until the most recent earlier breakpoint or watch hit, or the \
             start of the recorded history.",
            no_params(),
            json!({}),
        ),
        entry(
            "reverse_anchor",
            "Snapshot the machine into the reverse-debug ring at the current position, so \
             reverse_step / reverse_continue replay from here instead of an older frame \
             boundary. Take one at a stop you will step back from when the guest has \
             talked to a host directory mount or disk image since the last snapshot: that \
             host-side state is not rolled back, and a replay from before it diverges.",
            no_params(),
            json!({}),
        ),
        entry(
            "last_writer",
            "Find the instruction that last wrote the given memory address by replaying \
             the recorded history: returns the writer PC, position, and value. Time travel \
             must be armed; the machine is left at the write.",
            object(vec![("addr", addr("Memory address to trace"))], &["addr"]),
            json!({"addr": "0x20000"}),
        ),
        // State inspection
        entry(
            "regs.get",
            "Read the CPU registers: d0-d7, a0-a7, pc, sr and stopped state; when an FPU \
             is fitted, `fpu` contains exact raw fp0-fp7 values plus fpcr/fpsr/fpiar.",
            no_params(),
            json!({}),
        ),
        entry(
            "regs.set",
            "Write one CPU register. `reg` is d0-d7, a0-a7, sp, fp (a6), sr or pc; \
             `value` is a 32-bit value.",
            object(
                vec![
                    ("reg", string("Register name: d0-d7, a0-a7, sp, fp, sr, pc")),
                    ("value", addr("New value")),
                ],
                &["reg", "value"],
            ),
            json!({"reg": "d0", "value": "0x1234"}),
        ),
        entry(
            "mem.read",
            "Read `len` bytes (default 2, at most 1048576) at `addr` through the CPU's \
             address map (chip, slow, fast RAM, ROM; custom registers read as the CPU \
             sees them). Returns `data` as lowercase hex, or base64 with \
             `encoding: \"base64\"`.",
            object(
                vec![
                    ("addr", addr("Start address")),
                    ("len", uint("Bytes to read (default 2)", Some(1), Some(1_048_576))),
                    (
                        "encoding",
                        enumeration("Payload encoding (default hex)", &["hex", "base64"]),
                    ),
                ],
                &["addr"],
            ),
            json!({"addr": "0x400", "len": 16}),
        ),
        entry(
            "mem.write",
            "Write bytes at `addr`: `data` is hex (default) or base64 (`encoding`), 1 to \
             1048576 bytes. Lands at a deterministic timeline boundary and is journaled \
             for reverse execution.",
            object(
                vec![
                    ("addr", addr("Start address")),
                    ("data", string("Bytes to write, hex (default) or base64")),
                    (
                        "encoding",
                        enumeration("Payload encoding (default hex)", &["hex", "base64"]),
                    ),
                ],
                &["addr", "data"],
            ),
            json!({"addr": "0x400", "data": "deadbeef"}),
        ),
        entry(
            "disasm",
            "Disassemble `count` 68k instructions (default 16, at most 256) starting at \
             `addr` (default: the current PC). Each line carries the address, opcode words \
             and mnemonic.",
            object(
                vec![
                    ("addr", addr("Start address (default: PC)")),
                    ("count", uint("Instructions to disassemble", Some(1), Some(256))),
                ],
                &[],
            ),
            json!({"count": 8}),
        ),
        entry(
            "custom.dump",
            "Dump the custom chip register file ($DFF000-$DFF1FE) as name/offset/value \
             entries: DMACON, INTENA, BPLCON0, COPxLC, sprite and audio pointers, and the \
             rest. Read-only.",
            no_params(),
            json!({}),
        ),
        entry(
            "custom.read",
            "Read one custom chip register by name (\"DMACON\", \"VPOSR\", \"COLOR00\") \
             or by offset (\"0x096\"); write-only registers return the last value written.",
            object(vec![("reg", addr("Register name or offset below 0x200"))], &["reg"]),
            json!({"reg": "DMACON"}),
        ),
        entry(
            "custom.writer",
            "Report the PC and beam position (frame, vpos, hpos) of the last write to a \
             custom register, from the last-writer table the chipset validator maintains; \
             arm it with chipset.validate first.",
            object(vec![("reg", addr("Register name or offset below 0x200"))], &["reg"]),
            json!({"reg": "COLOR00"}),
        ),
        entry(
            "palette.dump",
            "Dump the live Denise palette: all 256 AGA entries with their high and low \
             nibble-plane words (32 on OCS/ECS).",
            no_params(),
            json!({}),
        ),
        entry(
            "cia.get",
            "Report one CIA (`a` or `b`): timers A/B with control bits, TOD counter and \
             alarm, port data and direction registers, serial, and interrupt mask/flags.",
            object(vec![("cia", enumeration("Which CIA", &["a", "b"]))], &["cia"]),
            json!({"cia": "a"}),
        ),
        entry(
            "beam.get",
            "Report the raster beam position: `vpos`, `hpos` (colour clock), `frame`, \
             the frame's line count, and whether the field is long.",
            no_params(),
            json!({}),
        ),
        entry(
            "display.get",
            "Report the active display: video standard, canvas size and pixel format, \
             the display window and fetch registers as decoded, bitplane count, \
             resolution mode (lores/hires/shres), HAM/dual-playfield, and interlace.",
            no_params(),
            json!({}),
        ),
        entry(
            "rtc.get",
            "Report the emulated battery-backed clock: fitted, frozen, unix seconds and \
             the calendar time the guest reads.",
            no_params(),
            json!({}),
        ),
        entry(
            "rtc.set",
            "Move the emulated clock: an absolute time as `unix` seconds or `time` \
             (\"YYYY-MM-DD HH:MM[:SS]\"), or a relative `advance` in seconds (negative \
             allowed); `frozen` pins or releases it. Give at least one; absolute and \
             advance are exclusive.",
            object(
                vec![
                    ("unix", uint("Absolute time as Unix seconds (UTC)", Some(0), None)),
                    ("time", string("Absolute time as \"YYYY-MM-DD HH:MM[:SS]\"")),
                    ("advance", int("Seconds to add to the current clock", None, None)),
                    ("frozen", boolean("true to stop the clock ticking, false to resume")),
                ],
                &[],
            ),
            json!({"time": "2005-03-18 01:58:29"}),
        ),
        entry(
            "cartridge.get",
            "Describe the fitted freezer cartridge (`[cartridge] model`): model, the base \
             and size of its bank, the monitor's version, whether the monitor is entered, \
             whether a press is still waiting for the CPU (`nmi_pending`), and the count \
             of freezes so far. Not found without a cartridge.",
            no_params(),
            json!({}),
        ),
        entry(
            "cartridge.freeze",
            "Press the freezer cartridge's button: the level-7 vector under the current VBR \
             is pointed at the monitor and a non-maskable interrupt is raised for the next \
             instruction boundary. The machine keeps running (resume it if stopped) and \
             enters the monitor; the reply carries the `cartridge.get` fields plus the \
             `vector` slot written and the `entry` address it holds. Not found without a \
             cartridge.",
            no_params(),
            json!({}),
        ),
        entry(
            "copper.list",
            "Disassemble up to `max` Copper instructions (default 32, at most 256) from \
             `addr` (default: the Copper's current program counter): MOVE, WAIT, SKIP, \
             with register names and beam positions decoded.",
            object(
                vec![
                    ("addr", addr("Copper list address (default: current Copper PC)")),
                    ("max", uint("Instructions to list", Some(1), Some(256))),
                ],
                &[],
            ),
            json!({"max": 16}),
        ),
        entry(
            "pc_history",
            "Return the most recently executed instruction addresses (newest last), the \
             trail that led to the current PC.",
            no_params(),
            json!({}),
        ),
        entry(
            "segments.list",
            "List the hunk segments of the program the scheduled process is running \
             (`current`: {start, size} per hunk, first hunk first) and the programs an armed \
             loadseg catch has seen loaded (`modules`: name, task, seglist, segments). At a \
             `loadseg` stop `current` is the just-loaded program: the addresses to relocate \
             its symbols and debug information by.",
            no_params(),
            json!({}),
        ),
        // Diagnostics
        entry(
            "chipset.validate",
            "Arm (`enabled: true`) or disarm the custom register access validator, which \
             records undefined bits, absent registers, byte accesses, stray pointers, \
             read-only writes, impossible blits and disk DMA against empty drives, and \
             maintains the last-writer table custom.writer reads. `clear` drops the \
             findings so far.",
            object(
                vec![
                    ("enabled", boolean("Arm or disarm the validator; absent leaves it as is")),
                    ("clear", boolean("Clear the findings (default false)")),
                ],
                &[],
            ),
            json!({"enabled": true}),
        ),
        entry(
            "chipset.report",
            "Report the chipset validator's findings so far, each with kind, register, \
             writer PC and beam position, and a hit count.",
            no_params(),
            json!({}),
        ),
        entry(
            "smc.detect",
            "Arm (`enabled: true`) or disarm the self-modifying-code detector, which \
             records writes to memory that was later executed; `clear` drops the findings.",
            object(
                vec![
                    ("enabled", boolean("Arm or disarm the detector; absent leaves it as is")),
                    ("clear", boolean("Clear the findings (default false)")),
                ],
                &[],
            ),
            json!({"enabled": true}),
        ),
        entry(
            "smc.report",
            "Report the self-modifying-code detector's findings: written address, the \
             writer PC, and the execution that followed.",
            no_params(),
            json!({}),
        ),
        entry(
            "fault.inject",
            "Arm a bus fault over `len` bytes (default 2) from `addr`: the next `count` \
             matching accesses (default: every access) of the chosen kind (`on`: read, \
             write, or both) raise a bus error in the guest. Returns the fault id.",
            object(
                vec![
                    ("addr", addr("Window start")),
                    ("len", uint("Window length in bytes (default 2)", Some(1), None)),
                    (
                        "on",
                        enumeration("Which accesses fault (default both)", &["read", "write", "both"]),
                    ),
                    (
                        "count",
                        uint("Fire this many times then disarm (default: unlimited)", Some(1), None),
                    ),
                ],
                &["addr"],
            ),
            json!({"addr": "0x400", "len": 2, "on": "write"}),
        ),
        entry(
            "fault.list",
            "List the armed bus faults with their windows, kinds and remaining counts.",
            no_params(),
            json!({}),
        ),
        entry(
            "fault.clear",
            "Disarm every bus fault.",
            no_params(),
            json!({}),
        ),
        entry(
            "memory.heatmap",
            "Enable (default) or disable address-space access tracking over the window \
             `base`..`base+span` (default the whole 16 MB map). Enabling resets the counts.",
            object(
                vec![
                    ("enabled", boolean("true (default) to arm, false to disarm")),
                    ("base", addr("Window start (default 0)")),
                    ("span", addr("Window length in bytes (default 0x1000000)")),
                ],
                &[],
            ),
            json!({"enabled": true, "base": 0, "span": "0x200000"}),
        ),
        entry(
            "memory.heatmap.report",
            "Report the memory heat map: per-bucket read/write/execute counts over the \
             tracked window, optionally also written as a file to `path`.",
            object(vec![("path", string("Host file to write the report to (optional)"))], &[]),
            json!({}),
        ),
        entry(
            "debug.resources",
            "List the bitmaps, palettes and copper lists the guest registered through the \
             WinUAE-compatible uaelib trap (debug_register_*): address, size, name, type, \
             flags, geometry and the frame they were registered in. Not found when the \
             guest registered nothing.",
            no_params(),
            json!({}),
        ),
        entry(
            "debug.resource.export",
            "Export one registered bitmap or palette as a PNG through the same safe, \
             clamped decoder the Frame Analyzer Resources tab uses.",
            object(
                vec![
                    ("address", addr("Registered resource address")),
                    ("path", string("Host PNG path")),
                ],
                &["address", "path"],
            ),
            json!({"address": "0x20000", "path": "/tmp/resource.png"}),
        ),
        entry(
            "debug.idle",
            "Report the guest's uaelib idle markers (debug_start_idle/stop_idle): current \
             state, whether ever used, and the last completed frame's idle_cck/frame_cck.",
            no_params(),
            json!({}),
        ),
        entry(
            "trace.start",
            "Start writing an instruction execution trace (one line per instruction) to \
             `path` (default: a file in the working directory), stopping by itself after \
             `max_lines` (default 1000000, at most 10000000).",
            object(
                vec![
                    ("path", string("Host file for the trace (optional)")),
                    ("max_lines", uint("Line cap", Some(1), Some(10_000_000))),
                ],
                &[],
            ),
            json!({"path": "/tmp/trace.txt", "max_lines": 10000}),
        ),
        entry(
            "trace.stop",
            "Stop the instruction trace and report the lines written.",
            no_params(),
            json!({}),
        ),
        entry(
            "trace.status",
            "Report whether an instruction trace is running, its path and line count.",
            no_params(),
            json!({}),
        ),
        entry(
            "profile.start",
            "Start a per-frame profile export: DMA ownership, blit records, CPU chip-bus \
             wait attribution (waited clocks by denier and access kind, top stalled \
             PCs), guest idle time and retired instructions per frame, streamed as \
             `profile.jsonl` under `path` (default: the configured profile directory) \
             with a `profile.json` summary written at stop. Stops by itself after \
             `frames` (default 500, at most 100000); `slots` adds per-frame chip-bus \
             owner and CPU-wait grids, `screenshots` saves the frame image for none, \
             every or the last frame, `pc_samples` adds a frame-boundary PC. \
             `samples` records every instruction in Bartman/WinUAE binary form; \
             `registers` appends D0-D7/A0-A7/SR, `unwind` supplies the text base \
             plus a base64 compact unwind table, `relocation_bases` preserves \
             every loaded hunk base for offline source mapping, and `code_ranges` \
             identifies executable hunks outside that table. An \
             optional `trigger` ({frame:N} or {busy_cck_over:N}) defers recording while \
             leaving the capture armed. Arms the frame analyzer's trace for the session, \
             which suspends run-ahead.",
            object(
                vec![
                    ("path", string("Host directory for the profile files (optional)")),
                    ("frames", uint("Frames to profile", Some(1), Some(100_000))),
                    ("slots", boolean("Include per-frame chip-bus owner grids")),
                    (
                        "screenshots",
                        enumeration(
                            "Which profiled frames to save as PNG",
                            &["none", "every", "last"],
                        ),
                    ),
                    ("pc_samples", boolean("Include the frame-boundary PC")),
                    ("samples", boolean("Record precise instruction samples per frame")),
                    ("registers", boolean("Append 17 CPU registers to each sample")),
                    (
                        "unwind",
                        object(
                            vec![
                                ("base", addr("Runtime base of program text")),
                                ("table", string("Base64 compact unwind table")),
                            ],
                            &["base", "table"],
                        ),
                    ),
                    (
                        "relocation_bases",
                        json!({
                            "type": "array",
                            "description": "Runtime base of each loaded program hunk, in file order",
                            "items": {"oneOf": [{"type": "integer", "minimum": 0}, {"type": "string"}]}
                        }),
                    ),
                    (
                        "code_ranges",
                        json!({
                            "type": "array",
                            "description": "Runtime ranges of loaded executable hunks",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "base": {"oneOf": [{"type": "integer", "minimum": 0}, {"type": "string"}]},
                                    "size": {"oneOf": [{"type": "integer", "minimum": 1}, {"type": "string"}]}
                                },
                                "required": ["base", "size"],
                                "additionalProperties": false
                            }
                        }),
                    ),
                    (
                        "trigger",
                        json!({
                            "type": "object",
                            "description": "Begin at an absolute frame or when busy cck exceeds a threshold",
                            "properties": {
                                "frame": {"type": "integer", "minimum": 0},
                                "busy_cck_over": {"type": "integer", "minimum": 0}
                            },
                            "minProperties": 1,
                            "maxProperties": 1
                        }),
                    ),
                ],
                &[],
            ),
            json!({"path": "/tmp/profile", "frames": 100, "samples": true, "registers": true}),
        ),
        entry(
            "profile.stop",
            "Stop the profile export, write the `profile.json` summary and report the \
             frames recorded.",
            no_params(),
            json!({}),
        ),
        entry(
            "profile.status",
            "Report the profile export: `active`, `path`, `frames_written`, `frames_limit` \
             and `done` (or just `active: false` when none was started).",
            no_params(),
            json!({}),
        ),
        entry(
            "waveform.start",
            "Arm a VCD chip-signal capture (for GTKWave): `trigger` is `now`, `pc=ADDR`, \
             `beam=VPOS[:HPOS]`, `reg=OFF` (a custom register write) or `time=SECS`; \
             `duration` is `Ncck`, `Nf`/`Nframes`, `Nms` or `Ns`; `signals` is a comma \
             list of beam, bus, cpu, copper, blitter, regs, irq, audio, or `all`. The \
             capture runs as the machine runs and finishes on its own.",
            object(
                vec![
                    ("path", string("Host .vcd file (default: a file in the working directory)")),
                    ("trigger", string("now | pc=ADDR | beam=VPOS[:HPOS] | reg=OFF | time=SECS")),
                    ("duration", string("Ncck | Nf | Nframes | Nms | Ns")),
                    ("signals", string("Comma-separated groups or all")),
                ],
                &[],
            ),
            json!({"path": "/tmp/out.vcd", "trigger": "now", "duration": "2frames"}),
        ),
        entry(
            "waveform.stop",
            "Stop the waveform capture early and finish the VCD file.",
            no_params(),
            json!({}),
        ),
        entry(
            "waveform.status",
            "Report the waveform capture state: armed, triggered, running, finished, and \
             the path.",
            no_params(),
            json!({}),
        ),
        // Breakpoints
        entry(
            "break.add",
            "Install a breakpoint and return its `id`. `kind` selects the trap: `pc` (an \
             `addr`, optional `cond` {lhs, op, rhs} over registers, immediates or \
             {\"mem\": addr} words with op eq/ne/lt/gt/le/ge/and, and `ignore` count), \
             `watch` (`access` write/read/access at `addr`, optionally by `class` cpu|blitter|disk|copper| \
             bpl1..bpl8|spr0..spr7|aud0..aud3 and, for cpu, a `pc`), `reg_watch` (a write \
             to custom register `reg`), `beam` (`vpos`, optional `hpos`), `copper` (the \
             Copper fetching `addr`), `catch` (exception `vector` number), `loadseg` (an \
             AmigaDOS program load, optionally matching its `name`). Hits end a resume with \
             the matching stop reason.",
            object(
                vec![
                    (
                        "kind",
                        enumeration(
                            "Breakpoint kind",
                            &["pc", "watch", "reg_watch", "beam", "copper", "catch", "loadseg"],
                        ),
                    ),
                    ("addr", addr("pc/watch/copper: the address")),
                    (
                        "cond",
                        json!({
                            "type": "object",
                            "description": "pc: only stop when lhs op rhs holds",
                            "properties": {
                                "lhs": {"description": "register name, number, or {\"mem\": addr}"},
                                "op": {"type": "string", "enum": ["eq", "ne", "lt", "gt", "le", "ge", "and"]},
                                "rhs": {"description": "register name, number, or {\"mem\": addr}"}
                            },
                            "required": ["lhs", "op", "rhs"]
                        }),
                    ),
                    ("ignore", uint("pc: skip this many hits first", Some(0), None)),
                    ("class", string("watch: cpu | blitter | disk | copper | bpl1..bpl8 | spr0..spr7 | aud0..aud3")),
                    (
                        "access",
                        enumeration("watch access type (default write)", &["write", "read", "access"]),
                    ),
                    ("pc", addr("watch (cpu class): only accesses by the instruction at this PC")),
                    ("reg", addr("reg_watch: custom register name or offset")),
                    ("vpos", uint("beam: the line", Some(0), Some(65535))),
                    ("hpos", uint("beam: the colour clock on the line (optional)", Some(0), Some(65535))),
                    ("vector", uint("catch: exception vector number", Some(0), Some(65535))),
                    ("name", string("loadseg: program name to match, case-insensitive (optional)")),
                ],
                &["kind"],
            ),
            json!({"kind": "pc", "addr": "0xFC0100"}),
        ),
        entry(
            "break.remove",
            "Remove the breakpoint with this `id` (from break.add or break.list).",
            object(vec![("id", uint("Breakpoint id", Some(0), None))], &["id"]),
            json!({"id": 1}),
        ),
        entry(
            "break.list",
            "List the installed breakpoints, watches and traps with ids and hit counts; \
             points set from the debugger window are listed without ids.",
            no_params(),
            json!({}),
        ),
        entry(
            "break.clear",
            "Remove every breakpoint this session installed.",
            no_params(),
            json!({}),
        ),
        // Input
        entry(
            "input.key",
            "Inject a keyboard event by Amiga raw key code (`rawkey` 0-255, e.g. 0x45 \
             Esc, 0x44 Return, 0x40 Space, 0x50-0x59 F1-F10; letters follow the Amiga \
             matrix, not ASCII). `action` is tap (default: press, then release after \
             `hold_ms`, default 80), press, or release. `at_seconds` schedules it in \
             emulated time. Input is journaled, so a rewind replays it.",
            object(
                vec![
                    ("rawkey", uint("Amiga raw key code", Some(0), Some(255))),
                    ("action", enumeration("Key action (default tap)", &["press", "release", "tap"])),
                    ("hold_ms", uint("tap: emulated milliseconds to hold (default 80)", Some(0), None)),
                    ("at_seconds", at_seconds()),
                ],
                &["rawkey"],
            ),
            json!({"rawkey": "0x45"}),
        ),
        entry(
            "input.mouse",
            "Inject relative mouse motion (`dx`, `dy` in mouse counts) and/or button \
             transitions (`left`, `right`, `middle`: true pressed, false released, absent \
             unchanged) on `port` (default 1). For an absolute pointer position use \
             input.mouse_to.",
            object(
                vec![
                    ("dx", int("Horizontal motion, positive right", None, None)),
                    ("dy", int("Vertical motion, positive down", None, None)),
                    ("left", boolean("Left button state")),
                    ("right", boolean("Right button state")),
                    ("middle", boolean("Middle button state")),
                    ("port", port("Controller port (default 1)")),
                    ("at_seconds", at_seconds()),
                ],
                &[],
            ),
            json!({"dx": 10, "dy": -5}),
        ),
        entry(
            "input.mouse_to",
            "Steer the guest's pointer to the presented-pixel position (`x`, `y`) by \
             servoing mouse deltas until sprite 0 lands within `tolerance` pixels \
             (default 2), running the machine up to `max_frames` frames (default 60) to \
             do it. Coordinates are those of capture.screenshot. Fails if the pointer \
             does not converge (no mouse pointer sprite on screen, or a hit breakpoint).",
            object(
                vec![
                    ("x", int("Target column", None, None)),
                    ("y", int("Target row", None, None)),
                    ("port", port("Controller port (default 1)")),
                    ("tolerance", int("Arrival tolerance in pixels (0-64)", Some(0), Some(64))),
                    ("max_frames", int("Frame budget (1-600)", Some(1), Some(600))),
                ],
                &["x", "y"],
            ),
            json!({"x": 160, "y": 100}),
        ),
        entry(
            "input.joy",
            "Set the joystick or CD32 pad state on `port` (default 2): each of `up`, \
             `down`, `left`, `right`, `red` (fire), `blue`, `green`, `yellow`, `play`, \
             `rwd`, `ffw` is true for held, absent or false for released. The state \
             persists until the next input.joy, so send a second call with the buttons \
             cleared to release them.",
            object(
                vec![
                    ("up", boolean("Direction up held")),
                    ("down", boolean("Direction down held")),
                    ("left", boolean("Direction left held")),
                    ("right", boolean("Direction right held")),
                    ("red", boolean("Fire / CD32 red held")),
                    ("blue", boolean("Second fire / CD32 blue held")),
                    ("green", boolean("CD32 green held")),
                    ("yellow", boolean("CD32 yellow held")),
                    ("play", boolean("CD32 play/pause held")),
                    ("rwd", boolean("CD32 reverse shoulder held")),
                    ("ffw", boolean("CD32 forward shoulder held")),
                    ("port", port("Controller port (default 2)")),
                    ("at_seconds", at_seconds()),
                ],
                &[],
            ),
            json!({"red": true}),
        ),
        entry(
            "input.analogue",
            "Set the analogue stick or paddle position on `port` (default 2): `x` and \
             `y` are 0-255 (128 centred).",
            object(
                vec![
                    ("x", uint("Horizontal position 0-255", Some(0), Some(255))),
                    ("y", uint("Vertical position 0-255", Some(0), Some(255))),
                    ("port", port("Controller port (default 2)")),
                    ("at_seconds", at_seconds()),
                ],
                &["x", "y"],
            ),
            json!({"x": 128, "y": 128}),
        ),
        entry(
            "input.set_port",
            "Hot-plug a controller device into `port` 1 or 2: mouse, gamepad-mouse (port \
             1 only), joystick, cd32, analogue, or none. Releases every line the previous \
             device drove.",
            object(
                vec![
                    ("port", port("Controller port")),
                    (
                        "device",
                        enumeration(
                            "Device to fit",
                            &["mouse", "gamepad-mouse", "joystick", "cd32", "analogue", "none"],
                        ),
                    ),
                ],
                &["port", "device"],
            ),
            json!({"port": 2, "device": "joystick"}),
        ),
        entry(
            "input.get_ports",
            "Report which device is fitted to each controller port.",
            no_params(),
            json!({}),
        ),
        // Media
        entry(
            "media.floppy.insert",
            "Insert a disk image (ADF, ADZ, DMS, IPF, SCP...) into drive `drive` (0-3), \
             optionally write-protected. The change is a live media event the guest sees \
             as a disk change.",
            object(
                vec![
                    ("drive", uint("Drive number 0-3 (df0..df3)", Some(0), Some(3))),
                    ("path", string("Host path of the disk image")),
                    ("write_protected", boolean("Insert write-protected (default false)")),
                ],
                &["drive", "path"],
            ),
            json!({"drive": 0, "path": "/path/to/game.adf"}),
        ),
        entry(
            "media.floppy.eject",
            "Eject the disk from drive `drive` (0-3).",
            object(vec![("drive", uint("Drive number 0-3", Some(0), Some(3)))], &["drive"]),
            json!({"drive": 0}),
        ),
        entry(
            "media.floppy.query",
            "Report the connected floppy drives, the image in each, write protection, and \
             motor/track state.",
            no_params(),
            json!({}),
        ),
        entry(
            "media.cd.insert",
            "Swap the CD image (ISO, CUE/BIN, CHD) in the machine's CD drive (CDTV, CD32, \
             or a SCSI CD-ROM).",
            object(vec![("path", string("Host path of the CD image"))], &["path"]),
            json!({"path": "/path/to/game.cue"}),
        ),
        entry(
            "media.cd.eject",
            "Eject the CD image from the machine's CD drive.",
            no_params(),
            json!({}),
        ),
        entry(
            "copperhf.attach",
            "Hot-attach a copperhf.device unit's media: opens `path` exactly like a \
             boot-time [copperhf] unit and replaces whatever media the unit had, bumping \
             its change counter and flagging the change to the guest (the same disk-change \
             machinery the guest's own TD_EJECT drives). Fails if no [copperhf] controller \
             is configured.",
            object(
                vec![
                    ("unit", int("Unit slot, 0-6", Some(0), Some(6))),
                    ("path", string("Host path of the hardfile image")),
                    (
                        "volume_name",
                        string("Volume name written into a synthesized RDB (bare images only)"),
                    ),
                    (
                        "boot_pri",
                        int(
                            "Boot priority written into a synthesized RDB (bare images only)",
                            Some(-128),
                            Some(127),
                        ),
                    ),
                ],
                &["unit", "path"],
            ),
            json!({"unit": 0, "path": "/path/to/disk.hdf"}),
        ),
        entry(
            "copperhf.eject",
            "Hot-eject a copperhf.device unit's media: the slot stays configured (opens \
             still succeed, like a diskless drive), only its media drops -- the same as \
             the guest's own TD_EJECT.",
            object(
                vec![("unit", int("Unit slot, 0-6", Some(0), Some(6)))],
                &["unit"],
            ),
            json!({"unit": 0}),
        ),
        // Events
        entry(
            "events.subscribe",
            "Subscribe this session to asynchronous notifications, queued by the bridge \
             for events_next / events_drain: `frame` (every `frame_interval` frames, \
             default 1, with an optional framebuffer digest), `serial` (Paula serial \
             output), `interrupt` (INTREQ/INTENA transitions), `media` (disk and CD \
             changes), `debug` (guest uaelib log lines and resource registrations). The \
             queues are bounded; check the drop counts.",
            object(
                vec![
                    (
                        "events",
                        json!({
                            "type": "array",
                            "description": "Event families to subscribe to",
                            "items": {"type": "string", "enum": ["frame", "serial", "interrupt", "media", "debug"]},
                            "minItems": 1
                        }),
                    ),
                    ("frame_interval", uint("Emit one frame event per this many frames (default 1)", Some(1), Some(1_000_000))),
                    ("frame_digest", boolean("Include an FNV-1a digest of each reported frame")),
                ],
                &["events"],
            ),
            json!({"events": ["frame"], "frame_interval": 50}),
        ),
        entry(
            "events.unsubscribe",
            "Unsubscribe from the listed event families, or from all of them when \
             `events` is absent.",
            object(
                vec![(
                    "events",
                    json!({
                        "type": "array",
                        "description": "Event families to drop (absent: all)",
                        "items": {"type": "string", "enum": ["frame", "serial", "interrupt", "media", "debug"]}
                    }),
                )],
                &[],
            ),
            json!({"events": ["frame"]}),
        ),
        entry(
            "events.list",
            "List the active event subscriptions and their settings.",
            no_params(),
            json!({}),
        ),
        // State files
        entry(
            "state.save",
            "Snapshot the whole machine to a save-state file at `path`; state.load or \
             `--load-state` resumes it byte-identically.",
            object(vec![("path", string("Host path for the .clstate file"))], &["path"]),
            json!({"path": "/tmp/at120.clstate"}),
        ),
        entry(
            "state.load",
            "Restore the machine from the save-state file at `path`. The machine must be \
             paused; scheduled input and the reverse-execution history are dropped.",
            object(vec![("path", string("Host path of the .clstate file"))], &["path"]),
            json!({"path": "/tmp/at120.clstate"}),
        ),
        // Capture
        entry(
            "capture.screenshot",
            "Render the current frame as a PNG. Through this bridge the image is also \
             returned as an image content block so you can look at the screen; with no \
             `path` the file is temporary and deleted after it is read, with a `path` it \
             is kept there. Returns the path, width and height.",
            object(vec![("path", string("Host path for the PNG (optional)"))], &[]),
            json!({}),
        ),
        entry(
            "capture.digest",
            "Return an FNV-1a hash of the current rendered frame, for cheap \"did the \
             picture change\" comparisons across steps.",
            no_params(),
            json!({}),
        ),
        entry(
            "capture.region_digest",
            "Return an FNV-1a hash of the `w` x `h` rectangle at (`x`, `y`) of the \
             rendered frame, in capture.screenshot's pixel coordinates; fails if the \
             rectangle falls outside the frame.",
            object(
                vec![
                    ("x", uint("Left edge (default 0)", Some(0), None)),
                    ("y", uint("Top edge (default 0)", Some(0), None)),
                    ("w", uint("Width in pixels", Some(1), None)),
                    ("h", uint("Height in pixels", Some(1), None)),
                ],
                &["w", "h"],
            ),
            json!({"x": 0, "y": 0, "w": 64, "h": 32}),
        ),
    ];
    entries
        .into_iter()
        .map(|e| ToolDef {
            method: e.method,
            name: tool_name(e.method),
            description: e.description,
            schema: e.schema,
            example: e.example,
            wait_ms: e.wait_ms,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::exec;
    use crate::control::proto;

    /// Method names the drivers answer before `parse_method` sees them.
    const DRIVER_METHODS: &[&str] = &["shutdown"];

    /// The method names `parse_method` dispatches on, read from its
    /// source: the string-literal arms at the match's own indentation
    /// (nested matches on parameter values sit deeper).
    fn parser_methods() -> Vec<String> {
        let src = include_str!("exec.rs");
        let start = src
            .find("pub fn parse_method(")
            .expect("parse_method in exec.rs");
        let body = &src[start..];
        let end = body.find("\n}\n").expect("end of parse_method");
        let mut names = Vec::new();
        for line in body[..end].lines() {
            let Some(rest) = line.strip_prefix("        \"") else {
                continue;
            };
            if line.starts_with("         ") {
                continue;
            }
            let Some(close) = rest.find('"') else {
                continue;
            };
            if !rest[close..].starts_with("\" =>") {
                continue;
            }
            names.push(rest[..close].to_string());
        }
        assert!(names.len() > 60, "found only {} arms", names.len());
        names
    }

    fn valid_tool_name(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }

    #[test]
    fn every_parser_arm_has_a_catalogue_entry() {
        let missing: Vec<String> = parser_methods()
            .into_iter()
            .filter(|m| find_method(m).is_none())
            .collect();
        assert!(missing.is_empty(), "methods without a tool: {missing:?}");
    }

    #[test]
    fn every_catalogue_entry_is_a_real_method() {
        let known = parser_methods();
        for def in catalogue() {
            assert!(
                known.iter().any(|m| m == def.method) || DRIVER_METHODS.contains(&def.method),
                "{} is not a method the parser or a driver answers",
                def.method
            );
        }
    }

    #[test]
    fn every_example_parses() {
        for def in catalogue() {
            if DRIVER_METHODS.contains(&def.method) {
                continue;
            }
            match exec::parse_method(def.method, &def.example) {
                Ok(_) => {}
                Err(e) if e.code == proto::METHOD_NOT_FOUND || e.code == proto::INVALID_PARAMS => {
                    panic!(
                        "{} example {} rejected: {}",
                        def.method, def.example, e.message
                    )
                }
                Err(e) => panic!("{} example failed: {}", def.method, e.message),
            }
        }
    }

    #[test]
    fn names_are_valid_unique_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for def in catalogue() {
            assert!(valid_tool_name(&def.name), "bad tool name {}", def.name);
            assert!(seen.insert(def.name.clone()), "duplicate tool {}", def.name);
            assert_eq!(tool_name(def.method), def.name);
            let back = find(&def.name).expect("tool name resolves");
            assert_eq!(back.method, def.method, "round trip of {}", def.name);
        }
        assert_eq!(find("warp_get").map(|d| d.method), Some("warp.get"));
        assert!(find("warp.get").is_none());
    }

    #[test]
    fn schemas_are_objects_whose_required_keys_exist() {
        for def in catalogue() {
            assert_eq!(def.schema["type"], "object", "{} schema type", def.method);
            let props = def.schema["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{} schema has properties", def.method));
            if let Some(required) = def.schema.get("required") {
                for key in required.as_array().expect("required is an array") {
                    let key = key.as_str().expect("required names are strings");
                    assert!(
                        props.contains_key(key),
                        "{} requires unknown {key}",
                        def.method
                    );
                }
            }
            for prop in props.values() {
                assert!(prop.is_object(), "{} property schema", def.method);
            }
            assert!(!def.description.is_empty());
            assert!(
                def.description.is_ascii(),
                "{} description is not ASCII",
                def.method
            );
        }
    }

    #[test]
    fn examples_carry_only_schema_keys() {
        for def in catalogue() {
            let props = def.schema["properties"].as_object().unwrap();
            for key in def.example.as_object().unwrap().keys() {
                assert!(
                    props.contains_key(key),
                    "{} example key {key} not in schema",
                    def.method
                );
            }
            if let Some(required) = def.schema.get("required") {
                for key in required.as_array().unwrap() {
                    assert!(
                        def.example.get(key.as_str().unwrap()).is_some(),
                        "{} example lacks required {key}",
                        def.method
                    );
                }
            }
        }
    }

    #[test]
    fn only_resume_verbs_take_wait_ms() {
        let waiting: Vec<&str> = catalogue()
            .iter()
            .filter(|d| d.wait_ms)
            .map(|d| d.method)
            .collect();
        assert_eq!(
            waiting,
            [
                "continue",
                "step",
                "step_over",
                "step_out",
                "step_copper",
                "step_frame",
                "run_until"
            ]
        );
        for def in catalogue() {
            assert_eq!(
                def.schema["properties"].get("wait_ms").is_some(),
                def.wait_ms,
                "{} wait_ms schema",
                def.method
            );
        }
    }
}
