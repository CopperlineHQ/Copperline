// SPDX-License-Identifier: GPL-3.0-or-later

//! Deterministic A/B divergence finder (`copperline-ctl diverge`,
//! docs/debugger/diverge.md).
//!
//! Two headless sessions -- two builds, or one build under two configs --
//! are driven in lockstep over the control protocol and compared at every
//! frame boundary: the rendered frame's digest, the CPU registers and,
//! optionally, a server-side digest of RAM (`mem.digest`). The first
//! differing frame is then narrowed to the first differing instruction by
//! restoring both sides to the last matching boundary and stepping them
//! together, and a memory difference is bisected to its first byte by
//! digesting halves.
//!
//! The emulated core is deterministic and unpaced when headless, so the
//! comparison is exact and repeatable: a mismatch is a real behavioural
//! difference between the two sides, not host jitter.
//!
//! Everything talks to a session through the small [`Session`] trait, so
//! the search itself is unit-tested against a scripted machine; the
//! `copperline-ctl` binary supplies the real launcher over
//! [`super::bridge`].

use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One control-protocol connection, as much of it as the search needs.
pub trait Session {
    /// One request-reply round trip: the reply's `result`, or the server's
    /// error message (or a transport failure) as text.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String>;

    /// Ask the emulator to exit; failures are irrelevant because the
    /// launcher kills the process afterwards anyway.
    fn shutdown(&mut self) {
        let _ = self.call("shutdown", json!({}));
    }
}

/// Which of the two sides a session is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    A,
    B,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::A => "A",
            Side::B => "B",
        }
    }

    fn lower(self) -> &'static str {
        match self {
            Side::A => "a",
            Side::B => "b",
        }
    }
}

/// A freshly launched side: its session and, for the report, how it was
/// started.
pub struct LaunchedSide {
    pub session: Box<dyn Session>,
    pub command: Vec<String>,
}

/// Starts (and restarts) the two sides. Restarting is needed when a side
/// cannot save or load a snapshot: the search then replays from the
/// beginning instead.
pub trait Launcher {
    fn launch(&mut self, side: Side) -> Result<LaunchedSide, String>;
}

/// How much RAM to compare at every checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryScope {
    None,
    Chip,
    All,
}

impl MemoryScope {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "none" => Some(Self::None),
            "chip" => Some(Self::Chip),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    fn region(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Chip => Some("chip"),
            Self::All => Some("all"),
        }
    }
}

/// Tunables for one search.
#[derive(Debug, Clone)]
pub struct Options {
    /// Stop (identical) once the emulated clock reaches this many seconds.
    pub until_seconds: Option<f64>,
    /// Stop (identical) after comparing this many frames.
    pub frames: Option<u64>,
    /// Frames between comparisons in the first pass; a mismatch is then
    /// narrowed frame by frame from the last matching checkpoint.
    pub stride: u64,
    pub memory: MemoryScope,
    /// Instructions per step while narrowing inside the frame; a mismatch
    /// is then narrowed instruction by instruction from the block start.
    pub step_block: u64,
    /// Most instructions stepped inside the first differing frame before
    /// the CPU-level search gives up.
    pub max_steps: u64,
    /// Save both sides' frames at the divergence as PNGs into this
    /// directory.
    pub screenshots: Option<PathBuf>,
    /// Where snapshot files go; created by the caller, removed by it.
    pub work_dir: PathBuf,
    /// Polled between requests; returning true abandons the search with
    /// an "interrupted" error (the launcher's cleanup still runs).
    pub cancel: fn() -> bool,
}

impl Options {
    pub fn new(work_dir: PathBuf) -> Self {
        Self {
            until_seconds: None,
            frames: None,
            stride: DEFAULT_STRIDE,
            memory: MemoryScope::Chip,
            step_block: DEFAULT_STEP_BLOCK,
            max_steps: DEFAULT_MAX_STEPS,
            screenshots: None,
            work_dir,
            cancel: never,
        }
    }
}

fn never() -> bool {
    false
}

pub const DEFAULT_STRIDE: u64 = 10;
pub const DEFAULT_STEP_BLOCK: u64 = 64;
pub const DEFAULT_MAX_STEPS: u64 = 1_000_000;
/// The narrowest span the memory bisection digests before reading the
/// bytes themselves.
const BISECT_LEAF: u64 = 16;

// ---------------------------------------------------------------------
// Report

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Identical,
    Diverged,
}

#[derive(Debug, Clone, Serialize)]
pub struct SideInfo {
    pub side: Side,
    /// The server's `hello` emulator string ("copperline 0.19.0").
    pub emulator: String,
    pub command: Vec<String>,
    /// The save-state container version this side writes, when a
    /// snapshot was taken.
    pub state_version: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegDiff {
    pub reg: String,
    pub a: Value,
    pub b: Value,
}

/// Where each side stood at the moment of the first CPU-visible
/// difference.
#[derive(Debug, Clone, Serialize)]
pub struct Point {
    pub pc: u32,
    pub frame: u64,
    pub vpos: u64,
    pub hpos: u64,
    pub cck: u64,
    pub seconds: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CpuDivergence {
    /// Instructions retired from the last matching frame boundary to the
    /// first mismatch (1 = the first instruction of the frame).
    pub step: u64,
    pub a: Point,
    pub b: Point,
    pub registers: Vec<RegDiff>,
    /// The two sides retired the same instruction at different colour
    /// clocks (bus timing, not architectural state).
    pub timing: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryDivergence {
    pub bank_base: u32,
    pub bank_len: u64,
    pub first_diff_addr: u32,
    /// The [`BISECT_LEAF`] bytes at the leaf span, as hex, both sides.
    pub a: String,
    pub b: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Divergence {
    pub frame: u64,
    pub last_matching_frame: u64,
    pub seconds: f64,
    /// What differed at the frame boundary: `display`, `registers`,
    /// `memory`, `timeline`.
    pub frame_mismatch: Vec<String>,
    pub display: Option<(String, String)>,
    /// `cpu`, `timing`, `memory`, `display`, or `unknown` when the step
    /// cap ended the search first.
    pub kind: String,
    pub cpu: Option<CpuDivergence>,
    pub memory: Option<MemoryDivergence>,
    /// The display differs but neither CPU registers nor (when compared)
    /// RAM ever did inside the frame: the difference is in the chipset's
    /// DMA or render path.
    pub dma_only: bool,
    pub step_cap_reached: bool,
    /// Whether the divergence was already present at the start (the two
    /// sides differ before any frame ran), so nothing could be narrowed.
    pub at_start: bool,
    pub screenshots: Option<(PathBuf, PathBuf)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub outcome: Outcome,
    pub start_frame: u64,
    pub end_frame: u64,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub frames_compared: u64,
    pub memory: MemoryScope,
    pub sides: Vec<SideInfo>,
    pub notes: Vec<String>,
    pub divergence: Option<Divergence>,
}

impl Report {
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// The human-readable summary the CLI prints without `--json`.
    pub fn render_text(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "compared {} frame(s): frame {} to {} ({:.3}s to {:.3}s), memory {}",
            self.frames_compared,
            self.start_frame,
            self.end_frame,
            self.start_seconds,
            self.end_seconds,
            match self.memory {
                MemoryScope::None => "not compared",
                MemoryScope::Chip => "chip",
                MemoryScope::All => "all",
            }
        );
        for side in &self.sides {
            let version = side
                .state_version
                .map(|v| format!(" (state format {v})"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "  {}: {}{}: {}",
                side.side.label(),
                side.emulator,
                version,
                side.command.join(" ")
            );
        }
        for note in &self.notes {
            let _ = writeln!(out, "note: {note}");
        }
        let Some(d) = &self.divergence else {
            let _ = writeln!(out, "result: identical");
            return out;
        };
        if d.at_start {
            let _ = writeln!(
                out,
                "result: DIVERGED before frame {} ran (the sides differ at the start)",
                d.frame
            );
        } else {
            let _ = writeln!(
                out,
                "result: DIVERGED at frame {} ({:.3}s); last matching frame {}",
                d.frame, d.seconds, d.last_matching_frame
            );
        }
        let _ = writeln!(out, "  frame mismatch: {}", d.frame_mismatch.join(", "));
        if let Some((a, b)) = &d.display {
            let _ = writeln!(out, "  display digest: A {a}  B {b}");
        }
        let _ = writeln!(out, "  kind: {}", d.kind);
        if let Some(cpu) = &d.cpu {
            let what = if cpu.timing && cpu.registers.is_empty() {
                "same instruction at different colour clocks"
            } else {
                "first register difference"
            };
            let _ = writeln!(
                out,
                "  {what} after {} instruction(s) from the frame boundary",
                cpu.step
            );
            for (label, p) in [("A", &cpu.a), ("B", &cpu.b)] {
                let _ = writeln!(
                    out,
                    "    {label}: pc ${:08X} frame {} vpos {} hpos {} cck {} ({:.6}s)",
                    p.pc, p.frame, p.vpos, p.hpos, p.cck, p.seconds
                );
            }
            for r in &cpu.registers {
                let _ = writeln!(
                    out,
                    "    {}: A {}  B {}",
                    r.reg,
                    render_reg(&r.a),
                    render_reg(&r.b)
                );
            }
        }
        if let Some(mem) = &d.memory {
            let _ = writeln!(
                out,
                "  memory: first differing byte at ${:08X} (bank ${:08X}+${:X})",
                mem.first_diff_addr, mem.bank_base, mem.bank_len
            );
            let _ = writeln!(out, "    A: {}", mem.a);
            let _ = writeln!(out, "    B: {}", mem.b);
        }
        if d.dma_only {
            let _ = writeln!(
                out,
                "  DMA-only: the CPU state never differed inside the frame; the \
                 difference is in the chipset DMA or render path"
            );
        }
        if d.step_cap_reached {
            let _ = writeln!(
                out,
                "  step cap reached before the CPU-level difference was found; raise --max-steps"
            );
        }
        if let Some((a, b)) = &d.screenshots {
            let _ = writeln!(out, "  screenshots: {} {}", a.display(), b.display());
        }
        out
    }
}

fn render_reg(value: &Value) -> String {
    match value.as_u64() {
        Some(n) => format!("${n:08X}"),
        None => value.to_string(),
    }
}

// ---------------------------------------------------------------------
// Samples and comparison

/// What one side looked like at a stop.
#[derive(Debug, Clone)]
struct Sample {
    frame: u64,
    seconds: f64,
    cck: u64,
    vpos: u64,
    hpos: u64,
    pc: u32,
    regs: Value,
    display: Option<String>,
    mem: Option<Value>,
}

impl Sample {
    fn point(&self) -> Point {
        Point {
            pc: self.pc,
            frame: self.frame,
            vpos: self.vpos,
            hpos: self.hpos,
            cck: self.cck,
            seconds: self.seconds,
        }
    }
}

/// Everything that can differ between two samples.
#[derive(Debug, Default)]
struct Diff {
    display: bool,
    registers: Vec<RegDiff>,
    memory: bool,
    /// Frame counter or colour clock differ.
    timeline: bool,
}

impl Diff {
    fn any(&self) -> bool {
        self.display || !self.registers.is_empty() || self.memory || self.timeline
    }

    fn names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if self.display {
            names.push("display".into());
        }
        if !self.registers.is_empty() {
            names.push("registers".into());
        }
        if self.memory {
            names.push("memory".into());
        }
        if self.timeline {
            names.push("timeline".into());
        }
        names
    }
}

fn compare(a: &Sample, b: &Sample) -> Diff {
    Diff {
        display: a.display != b.display,
        registers: regs_diff(&a.regs, &b.regs),
        memory: mem_digest_of(&a.mem) != mem_digest_of(&b.mem),
        timeline: a.frame != b.frame || a.cck != b.cck,
    }
}

fn mem_digest_of(mem: &Option<Value>) -> Option<&str> {
    mem.as_ref().and_then(|m| m["digest"].as_str())
}

/// Register-by-register comparison of two `regs.get` replies.
pub fn regs_diff(a: &Value, b: &Value) -> Vec<RegDiff> {
    let mut diffs = Vec::new();
    let mut check = |name: String, va: &Value, vb: &Value| {
        if va != vb {
            diffs.push(RegDiff {
                reg: name,
                a: va.clone(),
                b: vb.clone(),
            });
        }
    };
    for (bank, key) in [("d", "d"), ("a", "a")] {
        for n in 0..8 {
            check(format!("{bank}{n}"), &a[key][n], &b[key][n]);
        }
    }
    check("pc".into(), &a["pc"], &b["pc"]);
    check("sr".into(), &a["sr"], &b["sr"]);
    check("stopped".into(), &a["stopped"], &b["stopped"]);
    if !a["fpu"].is_null() || !b["fpu"].is_null() {
        for n in 0..8 {
            check(format!("fp{n}"), &a["fpu"]["fp"][n], &b["fpu"]["fp"][n]);
        }
        for key in ["fpcr", "fpsr", "fpiar"] {
            check(key.into(), &a["fpu"][key], &b["fpu"][key]);
        }
    }
    diffs
}

/// The (base, len) list a `mem.digest` reply describes.
fn mem_layout(mem: &Option<Value>) -> Vec<(u64, u64)> {
    mem.as_ref()
        .and_then(|m| m["regions"].as_array())
        .map(|regions| {
            regions
                .iter()
                .map(|r| {
                    (
                        r["base"].as_u64().unwrap_or(0),
                        r["len"].as_u64().unwrap_or(0),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The first bank whose digest differs between two `mem.digest` replies.
fn first_differing_bank(a: &Option<Value>, b: &Option<Value>) -> Option<(u64, u64)> {
    let (Some(a), Some(b)) = (a, b) else {
        return None;
    };
    let (Some(ra), Some(rb)) = (a["regions"].as_array(), b["regions"].as_array()) else {
        return None;
    };
    ra.iter()
        .zip(rb)
        .find(|(x, y)| x["digest"] != y["digest"])
        .map(|(x, _)| {
            (
                x["base"].as_u64().unwrap_or(0),
                x["len"].as_u64().unwrap_or(0),
            )
        })
}

// ---------------------------------------------------------------------
// Driving one side

struct SideState {
    side: Side,
    session: Box<dyn Session>,
    command: Vec<String>,
    emulator: String,
}

impl SideState {
    fn start(launcher: &mut dyn Launcher, side: Side) -> Result<Self, String> {
        let launched = launcher
            .launch(side)
            .map_err(|e| format!("launching side {}: {e}", side.label()))?;
        let mut state = Self {
            side,
            session: launched.session,
            command: launched.command,
            emulator: String::new(),
        };
        let hello = state.call("hello", json!({}))?;
        state.emulator = hello["emulator"]
            .as_str()
            .unwrap_or("unknown emulator")
            .to_string();
        Ok(state)
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.session
            .call(method, params)
            .map_err(|e| format!("side {}: {method}: {e}", self.side.label()))
    }

    /// The collect list for a resume: registers, then the frame digest
    /// when asked, then the memory digest when enabled.
    fn collect(display: bool, memory: MemoryScope) -> Vec<Value> {
        let mut items = vec![json!({"method": "regs.get"})];
        if display {
            items.push(json!({"method": "capture.digest"}));
        }
        if let Some(region) = memory.region() {
            items.push(json!({"method": "mem.digest", "params": {"region": region}}));
        }
        items
    }

    /// Turn a stop event with our collect list into a sample.
    fn sample_from_stop(
        &self,
        stop: &Value,
        display: bool,
        memory: MemoryScope,
    ) -> Result<Sample, String> {
        let collect = stop["collect"].as_array().cloned().unwrap_or_default();
        let mut items = collect.iter();
        let mut take = |what: &str| -> Result<Value, String> {
            let item = items
                .next()
                .ok_or_else(|| format!("side {}: stop event lacks {what}", self.side.label()))?;
            if let Some(err) = item.get("err") {
                return Err(format!(
                    "side {}: {what} at the stop failed: {}",
                    self.side.label(),
                    err["message"].as_str().unwrap_or("error")
                ));
            }
            Ok(item["ok"].clone())
        };
        let regs = take("regs.get")?;
        let display = if display {
            Some(
                take("capture.digest")?["digest"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        } else {
            None
        };
        let mem = if memory.region().is_some() {
            Some(take("mem.digest")?)
        } else {
            None
        };
        Ok(Sample {
            frame: stop["frame"].as_u64().unwrap_or(0),
            seconds: stop["seconds"].as_f64().unwrap_or(0.0),
            cck: stop["cck"].as_u64().unwrap_or(0),
            vpos: stop["vpos"].as_u64().unwrap_or(0),
            hpos: stop["hpos"].as_u64().unwrap_or(0),
            pc: stop["pc"].as_u64().unwrap_or(0) as u32,
            regs,
            display,
            mem,
        })
    }

    /// Sample the side where it stands, without moving it.
    fn sample_now(&mut self, memory: MemoryScope) -> Result<Sample, String> {
        let status = self.call("status", json!({}))?;
        let regs = self.call("regs.get", json!({}))?;
        let display = self.call("capture.digest", json!({}))?["digest"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let mem = match memory.region() {
            Some(region) => Some(self.call("mem.digest", json!({"region": region}))?),
            None => None,
        };
        Ok(Sample {
            frame: status["frame"].as_u64().unwrap_or(0),
            seconds: status["seconds"].as_f64().unwrap_or(0.0),
            cck: status["cck"].as_u64().unwrap_or(0),
            vpos: status["vpos"].as_u64().unwrap_or(0),
            hpos: status["hpos"].as_u64().unwrap_or(0),
            pc: status["pc"].as_u64().unwrap_or(0) as u32,
            regs,
            display: Some(display),
            mem,
        })
    }

    /// Advance `n` video frames, stopping on the vertical blank each
    /// time. `step_frame` and `run_until {"frame"}` stop on the headless
    /// server's host quantum, mid-frame; a beam target stops at
    /// instruction resolution when line 0 is reached, so both sides halt
    /// at the same point of the same frame.
    fn step_frames(&mut self, n: u64, memory: MemoryScope) -> Result<Sample, String> {
        let mut last = None;
        for i in 0..n {
            let mut params = json!({"vpos": 0, "hpos": 0});
            if i + 1 == n {
                params["collect"] = Value::Array(Self::collect(true, memory));
            }
            let stop = self.call("run_until", params)?;
            if stop["reason"] != "target" {
                return Err(format!(
                    "side {}: stopped for {} ({}) at frame {} instead of reaching the next \
                     vertical blank",
                    self.side.label(),
                    stop["reason"].as_str().unwrap_or("?"),
                    stop["detail"].as_str().unwrap_or(""),
                    stop["frame"]
                ));
            }
            last = Some(stop);
        }
        let stop = last.ok_or("step_frames needs n >= 1")?;
        self.sample_from_stop(&stop, true, memory)
    }

    fn step_instructions(&mut self, n: u64, memory: MemoryScope) -> Result<Sample, String> {
        let stop = self.call(
            "step",
            json!({"n": n, "collect": Self::collect(false, memory)}),
        )?;
        self.sample_from_stop(&stop, false, memory)
    }

    fn digest_span(&mut self, addr: u64, len: u64) -> Result<String, String> {
        Ok(
            self.call("mem.digest", json!({"addr": addr, "len": len}))?["digest"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        )
    }

    fn read_hex(&mut self, addr: u64, len: u64) -> Result<String, String> {
        Ok(
            self.call("mem.read", json!({"addr": addr, "len": len}))?["data"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        )
    }
}

// ---------------------------------------------------------------------
// Checkpoints

/// A position both sides can be brought back to.
#[derive(Debug, Clone)]
enum Checkpoint {
    /// Each side's own snapshot file (a side only ever reloads what it
    /// wrote itself, so the two builds need not share a state format).
    Files { frame: u64, a: PathBuf, b: PathBuf },
    /// No snapshots: relaunch both sides and step to the frame again.
    Replay { frame: u64 },
}

impl Checkpoint {
    fn frame(&self) -> u64 {
        match self {
            Self::Files { frame, .. } | Self::Replay { frame } => *frame,
        }
    }
}

/// Read the container version a Copperline save state starts with.
pub fn state_file_version(path: &Path) -> Option<u32> {
    let bytes = std::fs::read(path).ok()?;
    let magic = b"CLSSTATE";
    if bytes.len() < magic.len() + 4 || &bytes[..magic.len()] != magic {
        return None;
    }
    let mut version = [0u8; 4];
    version.copy_from_slice(&bytes[magic.len()..magic.len() + 4]);
    Some(u32::from_le_bytes(version))
}

// ---------------------------------------------------------------------
// The search

struct Search<'a> {
    launcher: &'a mut dyn Launcher,
    opts: &'a Options,
    a: SideState,
    b: SideState,
    memory: MemoryScope,
    notes: Vec<String>,
    /// Frame both sides started at (after any `--load-state`).
    start_frame: u64,
    /// Snapshots have failed on a side: replay from the start instead.
    snapshots_unsupported: bool,
    state_versions: [Option<u32>; 2],
    checkpoints_taken: u64,
}

impl Search<'_> {
    fn check_cancel(&self) -> Result<(), String> {
        if (self.opts.cancel)() {
            Err("interrupted".to_string())
        } else {
            Ok(())
        }
    }

    fn both<T>(
        &mut self,
        mut f: impl FnMut(&mut SideState) -> Result<T, String>,
    ) -> Result<(T, T), String> {
        self.check_cancel()?;
        let a = f(&mut self.a)?;
        let b = f(&mut self.b)?;
        Ok((a, b))
    }

    fn lockstep(&self, a: &Sample, b: &Sample) -> Result<(), String> {
        if a.frame != b.frame {
            return Err(format!(
                "lockstep broken: side A stopped at frame {} but side B at frame {}",
                a.frame, b.frame
            ));
        }
        Ok(())
    }

    /// Snapshot both sides where they stand, or record that they cannot.
    fn checkpoint(&mut self, frame: u64) -> Result<Checkpoint, String> {
        if self.snapshots_unsupported {
            return Ok(Checkpoint::Replay { frame });
        }
        self.checkpoints_taken += 1;
        let seq = self.checkpoints_taken;
        let path_a = self.opts.work_dir.join(format!("a-{seq}.clstate"));
        let path_b = self.opts.work_dir.join(format!("b-{seq}.clstate"));
        let saved = {
            let (pa, pb) = (path_a.clone(), path_b.clone());
            self.both(move |side| {
                let path = if side.side == Side::A { &pa } else { &pb };
                side.call("state.save", json!({"path": path.display().to_string()}))
            })
        };
        match saved {
            Ok(_) => {
                if self.state_versions == [None, None] {
                    self.state_versions =
                        [state_file_version(&path_a), state_file_version(&path_b)];
                    if let [Some(va), Some(vb)] = self.state_versions {
                        if va != vb {
                            self.notes.push(format!(
                                "the sides write different save-state formats (A {va}, B {vb}); \
                                 each side reloads only its own snapshots, but a shared \
                                 --load-state cannot load into both"
                            ));
                        }
                    }
                }
                Ok(Checkpoint::Files {
                    frame,
                    a: path_a,
                    b: path_b,
                })
            }
            Err(e) if e.contains("interrupted") => Err(e),
            Err(e) => {
                self.notes.push(format!(
                    "snapshots unavailable ({e}); narrowing replays from the start instead"
                ));
                self.snapshots_unsupported = true;
                Ok(Checkpoint::Replay { frame })
            }
        }
    }

    /// Bring both sides back to `checkpoint`.
    fn restore(&mut self, checkpoint: &Checkpoint) -> Result<(), String> {
        match checkpoint {
            Checkpoint::Files { a, b, frame } => {
                let (pa, pb) = (a.clone(), b.clone());
                let loaded = self.both(move |side| {
                    let path = if side.side == Side::A { &pa } else { &pb };
                    side.call("state.load", json!({"path": path.display().to_string()}))
                });
                match loaded {
                    Ok(_) => Ok(()),
                    Err(e) if e.contains("interrupted") => Err(e),
                    Err(e) => {
                        self.notes.push(format!(
                            "reloading a snapshot failed ({e}); replaying from the start instead"
                        ));
                        self.snapshots_unsupported = true;
                        self.restore(&Checkpoint::Replay { frame: *frame })
                    }
                }
            }
            Checkpoint::Replay { frame } => {
                self.check_cancel()?;
                self.a.session.shutdown();
                self.b.session.shutdown();
                self.a = SideState::start(self.launcher, Side::A)?;
                self.b = SideState::start(self.launcher, Side::B)?;
                let ahead = frame.saturating_sub(self.start_frame);
                if ahead > 0 {
                    let memory = self.memory;
                    let (sa, sb) = self.both(|side| side.step_frames(ahead, memory))?;
                    self.lockstep(&sa, &sb)?;
                    if sa.frame != *frame {
                        return Err(format!(
                            "replay reached frame {} instead of {frame}",
                            sa.frame
                        ));
                    }
                }
                Ok(())
            }
        }
    }

    fn reached_limit(&self, frames_compared: u64, sample: &Sample) -> bool {
        let by_frames = self.opts.frames.is_some_and(|n| frames_compared >= n);
        let by_time = self.opts.until_seconds.is_some_and(|t| sample.seconds >= t);
        by_frames || by_time
    }

    /// Frames to advance next: the stride, but never past `--frames`.
    fn next_stride(&self, frames_compared: u64) -> u64 {
        let stride = self.opts.stride.max(1);
        match self.opts.frames {
            Some(limit) => stride.min(limit.saturating_sub(frames_compared)).max(1),
            None => stride,
        }
    }

    fn run(&mut self) -> Result<Report, String> {
        let memory = self.memory;
        let (start_a, start_b) = self.both(|side| side.sample_now(memory))?;
        self.lockstep(&start_a, &start_b)?;
        self.start_frame = start_a.frame;

        // Different memory layouts cannot be compared bank by bank; say
        // so once and compare the rest.
        if self.memory != MemoryScope::None && mem_layout(&start_a.mem) != mem_layout(&start_b.mem)
        {
            self.notes.push(format!(
                "the sides have different RAM layouts (A {:?}, B {:?}); memory is not compared",
                mem_layout(&start_a.mem),
                mem_layout(&start_b.mem)
            ));
            self.memory = MemoryScope::None;
        }
        let memory = self.memory;
        let start_a = Sample {
            mem: if memory == MemoryScope::None {
                None
            } else {
                start_a.mem
            },
            ..start_a
        };
        let start_b = Sample {
            mem: if memory == MemoryScope::None {
                None
            } else {
                start_b.mem
            },
            ..start_b
        };

        let start_diff = compare(&start_a, &start_b);
        if start_diff.any() {
            let divergence = Divergence {
                frame: start_a.frame,
                last_matching_frame: start_a.frame,
                seconds: start_a.seconds,
                frame_mismatch: start_diff.names(),
                display: display_pair(&start_a, &start_b),
                kind: initial_kind(&start_diff),
                cpu: (!start_diff.registers.is_empty() || start_diff.timeline).then(|| {
                    CpuDivergence {
                        step: 0,
                        a: start_a.point(),
                        b: start_b.point(),
                        registers: start_diff.registers.clone(),
                        timing: start_a.cck != start_b.cck,
                    }
                }),
                memory: if start_diff.memory {
                    self.bisect_memory(&start_a.mem, &start_b.mem)?
                } else {
                    None
                },
                dma_only: false,
                step_cap_reached: false,
                at_start: true,
                screenshots: self.screenshots(start_a.frame)?,
            };
            return Ok(self.report(&start_a, &start_a, 0, Some(divergence)));
        }

        let mut checkpoint = self.checkpoint(start_a.frame)?;
        let mut last_good = start_a.clone();
        let mut frames_compared = 0u64;
        let (first_bad_a, first_bad_b, diff) = loop {
            if self.reached_limit(frames_compared, &last_good) {
                return Ok(self.report(&start_a, &last_good, frames_compared, None));
            }
            let n = self.next_stride(frames_compared);
            let (sa, sb) = self.both(|side| side.step_frames(n, memory))?;
            self.lockstep(&sa, &sb)?;
            frames_compared += n;
            let diff = compare(&sa, &sb);
            if !diff.any() {
                last_good = sa;
                checkpoint = self.checkpoint(last_good.frame)?;
                continue;
            }
            if n == 1 {
                break (sa, sb, diff);
            }
            // Narrow the stride frame by frame from the last matching
            // checkpoint, keeping a snapshot of the frame before the
            // mismatch for the instruction-level search.
            self.restore(&checkpoint)?;
            let mut walked = 0u64;
            loop {
                let before = self.checkpoint(checkpoint.frame() + walked)?;
                let (sa, sb) = self.both(|side| side.step_frames(1, memory))?;
                self.lockstep(&sa, &sb)?;
                walked += 1;
                let diff = compare(&sa, &sb);
                if diff.any() {
                    checkpoint = before;
                    break;
                }
                last_good = sa;
                if walked >= n {
                    return Err(format!(
                        "the sides differed after {n} frames from frame {} but match when \
                         replayed one frame at a time: the run is not deterministic",
                        checkpoint.frame()
                    ));
                }
            }
            // The stride result is the one recorded; re-read the samples
            // at the exact frame for the report.
            let (sa, sb) = self.both(|side| side.sample_now(memory))?;
            break (sa, sb, diff);
        };
        let first_bad_frame = first_bad_a.frame;
        frames_compared = first_bad_frame.saturating_sub(start_a.frame);

        let screenshots = self.screenshots(first_bad_frame)?;

        // Instruction-level narrowing from the last matching boundary.
        self.restore(&checkpoint)?;
        let narrowed = self.narrow_instructions(&checkpoint, first_bad_frame)?;

        let kind = match (&narrowed.cpu, &narrowed.memory) {
            (Some(cpu), _) if !cpu.registers.is_empty() => "cpu",
            (Some(cpu), _) if cpu.timing => "timing",
            (None, Some(_)) => "memory",
            (None, None) if narrowed.step_cap_reached => "unknown",
            (None, None) => "display",
            (Some(_), _) => "cpu",
        };
        let dma_only = narrowed.cpu.is_none()
            && narrowed.memory.is_none()
            && !narrowed.step_cap_reached
            && diff.display;
        let divergence = Divergence {
            frame: first_bad_frame,
            last_matching_frame: last_good.frame,
            seconds: first_bad_a.seconds,
            frame_mismatch: diff.names(),
            display: display_pair(&first_bad_a, &first_bad_b),
            kind: kind.to_string(),
            cpu: narrowed.cpu,
            memory: narrowed.memory,
            dma_only,
            step_cap_reached: narrowed.step_cap_reached,
            at_start: false,
            screenshots,
        };
        Ok(self.report(&start_a, &first_bad_a, frames_compared, Some(divergence)))
    }

    fn report(
        &mut self,
        start: &Sample,
        end: &Sample,
        frames_compared: u64,
        divergence: Option<Divergence>,
    ) -> Report {
        let sides = vec![
            SideInfo {
                side: Side::A,
                emulator: self.a.emulator.clone(),
                command: self.a.command.clone(),
                state_version: self.state_versions[0],
            },
            SideInfo {
                side: Side::B,
                emulator: self.b.emulator.clone(),
                command: self.b.command.clone(),
                state_version: self.state_versions[1],
            },
        ];
        Report {
            outcome: if divergence.is_some() {
                Outcome::Diverged
            } else {
                Outcome::Identical
            },
            start_frame: start.frame,
            end_frame: end.frame,
            start_seconds: start.seconds,
            end_seconds: end.seconds,
            frames_compared,
            memory: self.memory,
            sides,
            notes: std::mem::take(&mut self.notes),
            divergence,
        }
    }

    /// Both sides sit at `frame`: save their frames when asked.
    fn screenshots(&mut self, frame: u64) -> Result<Option<(PathBuf, PathBuf)>, String> {
        let Some(dir) = self.opts.screenshots.clone() else {
            return Ok(None);
        };
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let (pa, pb) = self.both(|side| {
            let path = dir.join(format!("{}-frame-{frame}.png", side.side.lower()));
            side.call(
                "capture.screenshot",
                json!({"path": path.display().to_string()}),
            )?;
            Ok(path)
        })?;
        Ok(Some((pa, pb)))
    }

    /// Step both sides from the frame boundary until their CPU state or
    /// memory differs, or the frame ends, or the cap is hit.
    fn narrow_instructions(
        &mut self,
        checkpoint: &Checkpoint,
        first_bad_frame: u64,
    ) -> Result<Narrowed, String> {
        let memory = self.memory;
        let mut done = 0u64;
        loop {
            let block = self
                .opts
                .step_block
                .max(1)
                .min(self.opts.max_steps.saturating_sub(done));
            if block == 0 {
                return Ok(Narrowed {
                    cpu: None,
                    memory: None,
                    step_cap_reached: true,
                });
            }
            let (sa, sb) = self.both(|side| side.step_instructions(block, memory))?;
            done += block;
            let diff = compare(&sa, &sb);
            if diff.any() {
                if block == 1 {
                    return self.narrowed_at(done, &sa, &sb, &diff);
                }
                // Back to the boundary, replay up to the block, then one
                // instruction at a time.
                self.restore(checkpoint)?;
                let before = done - block;
                if before > 0 {
                    let (sa, sb) = self.both(|side| side.step_instructions(before, memory))?;
                    if compare(&sa, &sb).any() {
                        return Err(format!(
                            "the sides differed {done} instructions into frame {first_bad_frame} \
                             but also {before} in when replayed: the run is not deterministic"
                        ));
                    }
                }
                for step in 1..=block {
                    let (sa, sb) = self.both(|side| side.step_instructions(1, memory))?;
                    let diff = compare(&sa, &sb);
                    if diff.any() {
                        return self.narrowed_at(before + step, &sa, &sb, &diff);
                    }
                }
                return Err(format!(
                    "the sides differed {done} instructions into frame {first_bad_frame} but \
                     not when replayed one instruction at a time: the run is not deterministic"
                ));
            }
            if sa.frame >= first_bad_frame {
                // The frame ended with the CPU (and compared memory) in
                // step: whatever differs is not CPU-visible state.
                return Ok(Narrowed {
                    cpu: None,
                    memory: None,
                    step_cap_reached: false,
                });
            }
        }
    }

    fn narrowed_at(
        &mut self,
        step: u64,
        sa: &Sample,
        sb: &Sample,
        diff: &Diff,
    ) -> Result<Narrowed, String> {
        let cpu = (!diff.registers.is_empty() || diff.timeline).then(|| CpuDivergence {
            step,
            a: sa.point(),
            b: sb.point(),
            registers: diff.registers.clone(),
            timing: sa.cck != sb.cck,
        });
        let memory = if diff.memory {
            self.bisect_memory(&sa.mem, &sb.mem)?
        } else {
            None
        };
        Ok(Narrowed {
            cpu,
            memory,
            step_cap_reached: false,
        })
    }

    /// Find the first differing byte of the first differing bank by
    /// digesting halves, then reading the leaf span on both sides.
    fn bisect_memory(
        &mut self,
        mem_a: &Option<Value>,
        mem_b: &Option<Value>,
    ) -> Result<Option<MemoryDivergence>, String> {
        let Some((bank_base, bank_len)) = first_differing_bank(mem_a, mem_b) else {
            return Ok(None);
        };
        let (mut addr, mut len) = (bank_base, bank_len);
        while len > BISECT_LEAF {
            let half = len / 2;
            let (da, db) = self.both(|side| side.digest_span(addr, half))?;
            if da != db {
                len = half;
            } else {
                addr += half;
                len -= half;
            }
        }
        let (hex_a, hex_b) = self.both(|side| side.read_hex(addr, len))?;
        let first = hex_a
            .as_bytes()
            .chunks(2)
            .zip(hex_b.as_bytes().chunks(2))
            .position(|(x, y)| x != y)
            .unwrap_or(0) as u64;
        Ok(Some(MemoryDivergence {
            bank_base: bank_base as u32,
            bank_len,
            first_diff_addr: (addr + first) as u32,
            a: hex_a,
            b: hex_b,
        }))
    }
}

struct Narrowed {
    cpu: Option<CpuDivergence>,
    memory: Option<MemoryDivergence>,
    step_cap_reached: bool,
}

fn display_pair(a: &Sample, b: &Sample) -> Option<(String, String)> {
    match (&a.display, &b.display) {
        (Some(x), Some(y)) if x != y => Some((x.clone(), y.clone())),
        _ => None,
    }
}

fn initial_kind(diff: &Diff) -> String {
    if !diff.registers.is_empty() {
        "cpu"
    } else if diff.memory {
        "memory"
    } else if diff.timeline {
        "timing"
    } else {
        "display"
    }
    .to_string()
}

/// Launch both sides and search for the first divergence. Both sessions
/// are asked to shut down before this returns, on every path.
pub fn run(launcher: &mut dyn Launcher, opts: &Options) -> Result<Report, String> {
    if opts.until_seconds.is_none() && opts.frames.is_none() {
        return Err("diverge needs --until SECS or --frames N".to_string());
    }
    let a = SideState::start(launcher, Side::A)?;
    let b = SideState::start(launcher, Side::B)?;
    let mut search = Search {
        launcher,
        opts,
        a,
        b,
        memory: opts.memory,
        notes: Vec::new(),
        start_frame: 0,
        snapshots_unsupported: false,
        state_versions: [None, None],
        checkpoints_taken: 0,
    };
    let result = search.run();
    search.a.session.shutdown();
    search.b.session.shutdown();
    result
}

/// A [`Session`] over a live [`super::bridge::Bridge`], polling `cancel`
/// while a request is outstanding so an interrupt ends a long resume
/// promptly instead of after the frame.
pub struct BridgeSession {
    bridge: super::bridge::Bridge,
    cancel: fn() -> bool,
}

impl BridgeSession {
    pub fn new(bridge: super::bridge::Bridge, cancel: fn() -> bool) -> Self {
        Self { bridge, cancel }
    }
}

impl Session for BridgeSession {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        use super::bridge::Reply;
        let id = self.bridge.send(method, params)?;
        loop {
            if (self.cancel)() {
                self.bridge.forget(id);
                return Err("interrupted".to_string());
            }
            match self.bridge.wait(id, Some(Duration::from_millis(200)))? {
                Reply::Ok(value) => return Ok(value),
                Reply::Err { code, message } => return Err(format!("{message} (code {code})")),
                Reply::TimedOut => continue,
            }
        }
    }

    fn shutdown(&mut self) {
        // Fire and forget: the launcher kills the process if it lingers.
        let _ = self.bridge.send("shutdown", json!({}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::exec::fnv1a64_bytes;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    /// Where side B misbehaves; side A never does.
    #[derive(Debug, Clone, Copy)]
    enum Fault {
        None,
        /// d1 changes after the instruction at (frame, step).
        Reg {
            frame: u64,
            step: u64,
        },
        /// One byte of RAM changes after the instruction at (frame, step).
        Mem {
            frame: u64,
            step: u64,
            addr: usize,
        },
        /// The rendered frame differs from this frame counter on, with the
        /// CPU and RAM in step.
        Display {
            frame: u64,
        },
        /// The instruction at (frame, step) takes one colour clock longer.
        Timing {
            frame: u64,
            step: u64,
        },
    }

    #[derive(Debug, Clone)]
    struct Machine {
        frame: u64,
        step: u64,
        total: u64,
        cck: u64,
        d: [u32; 8],
        a: [u32; 8],
        pc: u32,
        mem: Vec<u8>,
        /// A fault has fired: the picture looks different from here on.
        corrupt: bool,
    }

    const STEPS_PER_FRAME: u64 = 50;
    const CCK_PER_STEP: u64 = 100;

    struct Fake {
        side: Side,
        m: Machine,
        fault: Fault,
        no_snapshots: bool,
        saves: HashMap<String, Machine>,
        log: Rc<RefCell<Vec<String>>>,
        screenshots: Rc<RefCell<Vec<String>>>,
    }

    impl Fake {
        fn fires(&self, frame: u64, step: u64) -> bool {
            self.side == Side::B && self.m.frame == frame && self.m.step == step
        }

        fn exec_one(&mut self) {
            let fire = match self.fault {
                Fault::Reg { frame, step }
                | Fault::Mem { frame, step, .. }
                | Fault::Timing { frame, step } => self.fires(frame, step),
                Fault::None | Fault::Display { .. } => false,
            };
            let m = &mut self.m;
            m.pc = m.pc.wrapping_add(2);
            m.d[0] = m.d[0].wrapping_add(1);
            m.a[7] = m.a[7].wrapping_sub(4);
            let idx = (m.total as usize * 7) % m.mem.len();
            m.mem[idx] = m.mem[idx].wrapping_add(1);
            m.cck += CCK_PER_STEP;
            m.total += 1;
            m.step += 1;
            if fire {
                match self.fault {
                    Fault::Reg { .. } => m.d[1] = 0xBAD,
                    Fault::Mem { addr, .. } => m.mem[addr] ^= 0xEE,
                    Fault::Timing { .. } => m.cck += 1,
                    _ => {}
                }
                m.corrupt = true;
            }
            if m.step == STEPS_PER_FRAME {
                m.frame += 1;
                m.step = 0;
            }
        }

        fn display_digest(&self) -> String {
            let odd = match self.fault {
                Fault::Display { frame } => self.side == Side::B && self.m.frame >= frame,
                _ => false,
            } || self.m.corrupt;
            let mut bytes = self.m.frame.to_le_bytes().to_vec();
            bytes.push(u8::from(odd));
            format!("{:016x}", fnv1a64_bytes(&bytes))
        }

        fn regs(&self) -> Value {
            json!({"d": self.m.d, "a": self.m.a, "pc": self.m.pc, "sr": 0x2700, "stopped": false})
        }

        fn mem_digest(&self, params: &Value) -> Value {
            if let (Some(addr), Some(len)) = (params["addr"].as_u64(), params["len"].as_u64()) {
                let addr = addr as usize;
                let end = (addr + len as usize).min(self.m.mem.len());
                let digest = fnv1a64_bytes(&self.m.mem[addr.min(end)..end]);
                return json!({"digest": format!("{digest:016x}"), "addr": addr, "len": len,
                    "regions": [{"base": addr, "len": len, "digest": format!("{digest:016x}")}]});
            }
            let digest = format!("{:016x}", fnv1a64_bytes(&self.m.mem));
            json!({"digest": digest, "regions": [{"base": 0, "len": self.m.mem.len(), "digest": digest}]})
        }

        fn stop(&self, collect: &Value) -> Value {
            let m = &self.m;
            let collected: Vec<Value> = collect
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(|item| match item["method"].as_str() {
                            Some("regs.get") => json!({"ok": self.regs()}),
                            Some("capture.digest") => {
                                json!({"ok": {"digest": self.display_digest()}})
                            }
                            Some("mem.digest") => json!({"ok": self.mem_digest(&item["params"])}),
                            other => json!({"err": {"code": -1, "message": format!("{other:?}")}}),
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "reason": "step",
                "pc": m.pc,
                "frame": m.frame,
                "vpos": m.step % 313,
                "hpos": m.step % 227,
                "cck": m.cck,
                "seconds": m.cck as f64 / 100_000.0,
                "retired_instructions": m.total,
                "collect": collected,
            })
        }
    }

    impl Session for Fake {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            self.log
                .borrow_mut()
                .push(format!("{}:{method}", self.side.lower()));
            match method {
                "hello" => Ok(json!({"proto": 1, "emulator": "fake", "authed": true})),
                "status" => {
                    let m = &self.m;
                    Ok(
                        json!({"frame": m.frame, "seconds": m.cck as f64 / 100_000.0,
                        "cck": m.cck, "vpos": m.step % 313, "hpos": m.step % 227, "pc": m.pc}),
                    )
                }
                "regs.get" => Ok(self.regs()),
                "capture.digest" => Ok(json!({"digest": self.display_digest()})),
                "mem.digest" => Ok(self.mem_digest(&params)),
                "mem.read" => {
                    let addr = params["addr"].as_u64().unwrap() as usize;
                    let len = params["len"].as_u64().unwrap() as usize;
                    Ok(
                        json!({"data": crate::control::proto::encode_hex(&self.m.mem[addr..addr + len])}),
                    )
                }
                "run_until" => {
                    assert_eq!(params["vpos"], 0, "the search runs to the vertical blank");
                    let target = self.m.frame + 1;
                    while self.m.frame < target {
                        self.exec_one();
                    }
                    let mut stop = self.stop(&params["collect"]);
                    stop["reason"] = json!("target");
                    Ok(stop)
                }
                "step" => {
                    for _ in 0..params["n"].as_u64().unwrap_or(1) {
                        self.exec_one();
                    }
                    Ok(self.stop(&params["collect"]))
                }
                "state.save" => {
                    if self.no_snapshots {
                        return Err("snapshots are not supported here".into());
                    }
                    let path = params["path"].as_str().unwrap().to_string();
                    self.saves.insert(path.clone(), self.m.clone());
                    Ok(json!({"path": path}))
                }
                "state.load" => {
                    let path = params["path"].as_str().unwrap();
                    self.m = self
                        .saves
                        .get(path)
                        .cloned()
                        .ok_or_else(|| format!("no snapshot at {path}"))?;
                    Ok(json!({"reconfigured": false}))
                }
                "capture.screenshot" => {
                    let path = params["path"].as_str().unwrap().to_string();
                    self.screenshots.borrow_mut().push(path.clone());
                    Ok(json!({"path": path}))
                }
                "shutdown" => Ok(json!({})),
                other => Err(format!("unknown method {other}")),
            }
        }
    }

    struct FakeLauncher {
        fault: Fault,
        no_snapshots: bool,
        mem_len_b: usize,
        initial_d2_b: u32,
        launches: u32,
        log: Rc<RefCell<Vec<String>>>,
        screenshots: Rc<RefCell<Vec<String>>>,
    }

    impl FakeLauncher {
        fn new(fault: Fault) -> Self {
            Self {
                fault,
                no_snapshots: false,
                mem_len_b: 4096,
                initial_d2_b: 0,
                launches: 0,
                log: Rc::new(RefCell::new(Vec::new())),
                screenshots: Rc::new(RefCell::new(Vec::new())),
            }
        }

        fn count(&self, needle: &str) -> usize {
            self.log
                .borrow()
                .iter()
                .filter(|entry| entry.as_str() == needle)
                .count()
        }
    }

    impl Launcher for FakeLauncher {
        fn launch(&mut self, side: Side) -> Result<LaunchedSide, String> {
            self.launches += 1;
            let mem_len = if side == Side::B {
                self.mem_len_b
            } else {
                4096
            };
            let mut d = [0u32; 8];
            if side == Side::B {
                d[2] = self.initial_d2_b;
            }
            let fake = Fake {
                side,
                m: Machine {
                    frame: 0,
                    step: 0,
                    total: 0,
                    cck: 0,
                    d,
                    a: [0x1000; 8],
                    pc: 0xF80010,
                    mem: (0..mem_len).map(|i| (i % 251) as u8).collect(),
                    corrupt: false,
                },
                fault: self.fault,
                no_snapshots: self.no_snapshots,
                saves: HashMap::new(),
                log: Rc::clone(&self.log),
                screenshots: Rc::clone(&self.screenshots),
            };
            Ok(LaunchedSide {
                session: Box::new(fake),
                command: vec![format!("fake-{}", side.lower())],
            })
        }
    }

    fn options() -> Options {
        Options::new(std::env::temp_dir().join("copperline-diverge-fake"))
    }

    fn reg_names(cpu: &CpuDivergence) -> Vec<&str> {
        cpu.registers.iter().map(|r| r.reg.as_str()).collect()
    }

    #[test]
    fn identical_sides_report_identical_and_shut_down() {
        let mut launcher = FakeLauncher::new(Fault::None);
        let mut opts = options();
        opts.frames = Some(30);
        opts.stride = 7;
        let report = run(&mut launcher, &opts).unwrap();
        assert_eq!(report.outcome, Outcome::Identical);
        assert!(report.divergence.is_none());
        assert_eq!(report.frames_compared, 30, "the last stride is clamped");
        assert_eq!(report.end_frame, 30);
        assert_eq!(report.start_frame, 0);
        assert_eq!(report.memory, MemoryScope::Chip);
        assert_eq!(report.sides[0].emulator, "fake");
        assert_eq!(report.sides[1].command, vec!["fake-b"]);
        assert_eq!(launcher.count("a:shutdown"), 1);
        assert_eq!(launcher.count("b:shutdown"), 1);
        assert_eq!(launcher.launches, 2);
        assert!(report.render_text().contains("result: identical"));
        assert_eq!(report.to_json()["outcome"], "identical");
    }

    #[test]
    fn register_fault_is_narrowed_to_the_instruction() {
        let mut launcher = FakeLauncher::new(Fault::Reg { frame: 7, step: 33 });
        let mut opts = options();
        opts.frames = Some(40);
        opts.stride = 5;
        let report = run(&mut launcher, &opts).unwrap();
        assert_eq!(report.outcome, Outcome::Diverged);
        let d = report.divergence.as_ref().unwrap();
        assert_eq!(d.frame, 8);
        assert_eq!(d.last_matching_frame, 7);
        assert!(!d.at_start);
        assert_eq!(d.kind, "cpu");
        assert!(!d.dma_only);
        assert!(!d.step_cap_reached);
        assert_eq!(d.frame_mismatch, vec!["display", "registers"]);
        assert!(d.display.is_some());
        let cpu = d.cpu.as_ref().unwrap();
        assert_eq!(cpu.step, 34, "the 34th instruction of frame 7");
        assert_eq!(reg_names(cpu), vec!["d1"]);
        assert_eq!(cpu.registers[0].a, 0);
        assert_eq!(cpu.registers[0].b, 0xBAD);
        assert!(!cpu.timing);
        assert_eq!(cpu.a.frame, 7);
        assert_eq!(cpu.a.cck, cpu.b.cck);
        assert!(d.memory.is_none());
        assert_eq!(report.frames_compared, 8);
        assert_eq!(launcher.launches, 2, "snapshots avoid relaunching");
        assert!(
            launcher.count("a:state.load") >= 2,
            "stride, then instruction narrowing"
        );
        let text = report.render_text();
        assert!(text.contains("DIVERGED at frame 8"), "{text}");
        assert!(text.contains("d1: A $00000000  B $00000BAD"), "{text}");
        assert_eq!(report.to_json()["divergence"]["cpu"]["step"], 34);
    }

    #[test]
    fn memory_fault_is_bisected_to_the_byte() {
        let mut launcher = FakeLauncher::new(Fault::Mem {
            frame: 3,
            step: 10,
            addr: 0x123,
        });
        let mut opts = options();
        opts.frames = Some(20);
        opts.stride = 1;
        let report = run(&mut launcher, &opts).unwrap();
        let d = report.divergence.as_ref().unwrap();
        assert_eq!(d.frame, 4);
        assert_eq!(d.last_matching_frame, 3);
        assert_eq!(d.kind, "memory");
        assert!(d.cpu.is_none());
        assert!(!d.dma_only);
        assert_eq!(d.frame_mismatch, vec!["display", "memory"]);
        let mem = d.memory.as_ref().unwrap();
        assert_eq!(mem.bank_base, 0);
        assert_eq!(mem.bank_len, 4096);
        assert_eq!(mem.first_diff_addr, 0x123);
        assert_eq!(mem.a.len(), 32);
        assert_ne!(mem.a, mem.b);
        assert!(report.render_text().contains("$00000123"));
    }

    #[test]
    fn display_only_fault_is_dma_only() {
        let mut launcher = FakeLauncher::new(Fault::Display { frame: 5 });
        let mut opts = options();
        opts.frames = Some(20);
        opts.stride = 10;
        opts.screenshots = Some(
            std::env::temp_dir().join(format!("copperline-diverge-shots-{}", std::process::id())),
        );
        let report = run(&mut launcher, &opts).unwrap();
        let d = report.divergence.as_ref().unwrap();
        assert_eq!(d.frame, 5);
        assert_eq!(d.last_matching_frame, 4);
        assert_eq!(d.kind, "display");
        assert!(d.dma_only);
        assert!(d.cpu.is_none() && d.memory.is_none());
        assert_eq!(d.frame_mismatch, vec!["display"]);
        let shots = launcher.screenshots.borrow();
        assert_eq!(shots.len(), 2);
        assert!(shots[0].ends_with("a-frame-5.png"), "{}", shots[0]);
        assert!(shots[1].ends_with("b-frame-5.png"), "{}", shots[1]);
        let (a, b) = d.screenshots.as_ref().unwrap();
        assert_eq!(a.display().to_string(), shots[0]);
        assert_eq!(b.display().to_string(), shots[1]);
        std::fs::remove_dir_all(opts.screenshots.unwrap()).ok();
        assert!(report.render_text().contains("DMA-only"));
    }

    #[test]
    fn timing_fault_is_reported_as_timing() {
        let mut launcher = FakeLauncher::new(Fault::Timing { frame: 2, step: 5 });
        let mut opts = options();
        opts.frames = Some(10);
        opts.stride = 4;
        let report = run(&mut launcher, &opts).unwrap();
        let d = report.divergence.as_ref().unwrap();
        assert_eq!(d.frame, 3);
        assert_eq!(d.kind, "timing");
        let cpu = d.cpu.as_ref().unwrap();
        assert!(cpu.timing);
        assert!(cpu.registers.is_empty());
        assert_eq!(cpu.step, 6);
        assert_eq!(cpu.b.cck, cpu.a.cck + 1);
        assert!(d.frame_mismatch.contains(&"timeline".to_string()));
        assert!(report
            .render_text()
            .contains("same instruction at different colour clocks"));
    }

    #[test]
    fn step_cap_ends_the_search_as_unknown() {
        let mut launcher = FakeLauncher::new(Fault::Display { frame: 2 });
        let mut opts = options();
        opts.frames = Some(10);
        opts.stride = 1;
        opts.step_block = 4;
        opts.max_steps = 10;
        let report = run(&mut launcher, &opts).unwrap();
        let d = report.divergence.as_ref().unwrap();
        assert_eq!(d.frame, 2);
        assert!(d.step_cap_reached);
        assert_eq!(d.kind, "unknown");
        assert!(!d.dma_only);
        assert!(report.render_text().contains("step cap reached"));
    }

    #[test]
    fn until_seconds_bounds_the_comparison() {
        let mut launcher = FakeLauncher::new(Fault::None);
        let mut opts = options();
        // One frame is 50 steps of 100 cck = 0.05 s on the fake clock.
        opts.until_seconds = Some(0.15);
        opts.stride = 1;
        let report = run(&mut launcher, &opts).unwrap();
        assert_eq!(report.outcome, Outcome::Identical);
        assert_eq!(report.frames_compared, 3);
        assert!((report.end_seconds - 0.15).abs() < 1e-9);
    }

    #[test]
    fn without_snapshots_the_search_replays_from_the_start() {
        let mut launcher = FakeLauncher::new(Fault::Reg { frame: 7, step: 33 });
        launcher.no_snapshots = true;
        let mut opts = options();
        opts.frames = Some(40);
        opts.stride = 5;
        let report = run(&mut launcher, &opts).unwrap();
        let d = report.divergence.as_ref().unwrap();
        assert_eq!(d.frame, 8);
        assert_eq!(d.last_matching_frame, 7);
        let cpu = d.cpu.as_ref().unwrap();
        assert_eq!(cpu.step, 34);
        assert_eq!(reg_names(cpu), vec!["d1"]);
        assert!(launcher.launches > 2, "relaunched to replay");
        assert_eq!(launcher.count("a:state.load"), 0);
        assert!(report
            .notes
            .iter()
            .any(|n| n.contains("snapshots unavailable")));
        assert!(report.render_text().contains("note: snapshots unavailable"));
    }

    #[test]
    fn different_ram_layouts_disable_memory_comparison() {
        let mut launcher = FakeLauncher::new(Fault::None);
        launcher.mem_len_b = 8192;
        let mut opts = options();
        opts.frames = Some(4);
        opts.memory = MemoryScope::All;
        let report = run(&mut launcher, &opts).unwrap();
        assert_eq!(report.outcome, Outcome::Identical);
        assert_eq!(report.memory, MemoryScope::None);
        assert!(report
            .notes
            .iter()
            .any(|n| n.contains("different RAM layouts")));
        // Nothing after the first look asks for memory.
        assert_eq!(launcher.count("a:mem.digest"), 1);
    }

    #[test]
    fn a_difference_at_the_start_is_reported_without_narrowing() {
        let mut launcher = FakeLauncher::new(Fault::None);
        launcher.initial_d2_b = 7;
        let mut opts = options();
        opts.frames = Some(10);
        let report = run(&mut launcher, &opts).unwrap();
        let d = report.divergence.as_ref().unwrap();
        assert!(d.at_start);
        assert_eq!(d.frame, 0);
        assert_eq!(d.kind, "cpu");
        let cpu = d.cpu.as_ref().unwrap();
        assert_eq!(cpu.step, 0);
        assert_eq!(reg_names(cpu), vec!["d2"]);
        assert_eq!(report.frames_compared, 0);
        assert_eq!(launcher.count("a:run_until"), 0);
        assert!(report.render_text().contains("differ at the start"));
    }

    fn always_cancel() -> bool {
        true
    }

    #[test]
    fn cancellation_aborts_and_still_shuts_down() {
        let mut launcher = FakeLauncher::new(Fault::None);
        let mut opts = options();
        opts.frames = Some(10);
        opts.cancel = always_cancel;
        let err = run(&mut launcher, &opts).unwrap_err();
        assert!(err.contains("interrupted"), "{err}");
        assert_eq!(launcher.count("a:shutdown"), 1);
        assert_eq!(launcher.count("b:shutdown"), 1);
    }

    #[test]
    fn a_limit_is_required() {
        let mut launcher = FakeLauncher::new(Fault::None);
        let err = run(&mut launcher, &options()).unwrap_err();
        assert!(err.contains("--until"), "{err}");
        assert_eq!(launcher.launches, 0);
    }

    #[test]
    fn regs_diff_names_every_register_including_fpu() {
        let a = json!({"d": [0, 1, 2, 3, 4, 5, 6, 7], "a": vec![0u32; 8], "pc": 0x1000, "sr": 0x2700,
            "stopped": false, "fpu": {"fp": vec!["0x0"; 8], "fpcr": 0, "fpsr": 0, "fpiar": 0}});
        let mut b = a.clone();
        b["d"][3] = json!(9);
        b["a"][6] = json!(0x20);
        b["sr"] = json!(0x2704);
        b["fpu"]["fp"][2] = json!("0x1");
        b["fpu"]["fpsr"] = json!(8);
        let names: Vec<String> = regs_diff(&a, &b).into_iter().map(|r| r.reg).collect();
        assert_eq!(names, vec!["d3", "a6", "sr", "fp2", "fpsr"]);
        assert!(regs_diff(&a, &a).is_empty());
    }

    #[test]
    fn state_file_version_reads_the_header_only() {
        let dir =
            std::env::temp_dir().join(format!("copperline-diverge-hdr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.clstate");
        let mut bytes = b"CLSSTATE".to_vec();
        bytes.extend_from_slice(&81u32.to_le_bytes());
        bytes.extend_from_slice(b"trailing garbage");
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(state_file_version(&path), Some(81));
        std::fs::write(&path, b"not a state").unwrap();
        assert_eq!(state_file_version(&path), None);
        assert_eq!(state_file_version(&dir.join("absent")), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
