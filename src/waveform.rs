//! Trigger-based VCD "logic analyser" export of internal chipset signals.
//!
//! A capture is armed with a [`WaveOptions`] (output path, trigger, duration,
//! signal groups), waits for its trigger (a CPU PC, a beam position, a
//! custom-register write, an emulated time, or immediately), then streams
//! value changes of the selected hardware signals into a VCD file that
//! GTKWave (or any VCD viewer) can display. FST output is produced offline
//! with GTKWave's bundled `vcd2fst`.
//!
//! Time convention: 1 VCD time unit = 1 color clock (cck), declared as
//! `$timescale 1 us` because VCD only allows 1/10/100 of a standard unit
//! and cursor deltas should read directly as cck. Timestamps are relative
//! to the trigger point.
//!
//! The sampling call sites live in the bus (`src/bus/wave.rs`); this module
//! owns the VCD writer, the option/trigger/duration/signal-set parsing, and
//! the capture state machine so it can be unit-tested against an in-memory
//! writer.

use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Bound on a single capture so a typo'd duration cannot fill the disk:
/// 10 emulated seconds of color clocks (a few hundred MB worst case).
const MAX_CAPTURE_SECONDS: f64 = 10.0;
/// Emergency stop when the output file grows past this many bytes even
/// within the duration bound (chip-bus activity is workload dependent).
const MAX_CAPTURE_BYTES: u64 = 512 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Signal groups
// ---------------------------------------------------------------------------

/// Bitmask of signal groups selected for a capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalSet(u8);

impl SignalSet {
    pub const BEAM: SignalSet = SignalSet(1 << 0);
    pub const BUS: SignalSet = SignalSet(1 << 1);
    pub const CPU: SignalSet = SignalSet(1 << 2);
    pub const COPPER: SignalSet = SignalSet(1 << 3);
    pub const BLITTER: SignalSet = SignalSet(1 << 4);
    pub const REGS: SignalSet = SignalSet(1 << 5);
    pub const IRQ: SignalSet = SignalSet(1 << 6);
    pub const AUDIO: SignalSet = SignalSet(1 << 7);
    pub const ALL: SignalSet = SignalSet(0xFF);

    pub fn contains(self, other: SignalSet) -> bool {
        self.0 & other.0 == other.0
    }
}

const SIGNAL_GROUP_NAMES: [(&str, SignalSet); 8] = [
    ("beam", SignalSet::BEAM),
    ("bus", SignalSet::BUS),
    ("cpu", SignalSet::CPU),
    ("copper", SignalSet::COPPER),
    ("blitter", SignalSet::BLITTER),
    ("regs", SignalSet::REGS),
    ("irq", SignalSet::IRQ),
    ("audio", SignalSet::AUDIO),
];

/// Parse a comma-separated signal-group list (`cpu,bus,copper` or `all`).
pub fn parse_signals(spec: &str) -> Option<SignalSet> {
    let mut set = SignalSet(0);
    for part in spec.split(',') {
        let part = part.trim().to_ascii_lowercase();
        if part.is_empty() {
            continue;
        }
        if part == "all" {
            return Some(SignalSet::ALL);
        }
        let group = SIGNAL_GROUP_NAMES.iter().find(|(name, _)| *name == part)?.1;
        set.0 |= group.0;
    }
    (set.0 != 0).then_some(set)
}

impl fmt::Display for SignalSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == SignalSet::ALL {
            return f.write_str("all");
        }
        let mut first = true;
        for (name, group) in SIGNAL_GROUP_NAMES {
            if self.contains(group) {
                if !first {
                    f.write_str(",")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Trigger
// ---------------------------------------------------------------------------

/// What starts the capture window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trigger {
    /// Start immediately.
    Now,
    /// The CPU retires an instruction at this address.
    Pc(u32),
    /// The beam crosses this position (any hpos when None).
    Beam { vpos: u16, hpos: Option<u16> },
    /// A custom-register write to this word offset ($000-$1FE).
    RegWrite(u16),
    /// Emulated time reaches this many seconds.
    Time(f64),
}

fn parse_hex_u32(text: &str) -> Option<u32> {
    let text = text.trim();
    let text = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .or_else(|| text.strip_prefix('$'))
        .unwrap_or(text);
    u32::from_str_radix(text, 16).ok()
}

/// Parse a trigger spec: `now`, `pc=ADDR`, `beam=VPOS[:HPOS]`, `reg=OFF`,
/// or `time=SECS`. Addresses and register offsets are hex (`0x`/`$`
/// prefixes optional); beam positions are decimal; time is fractional
/// seconds.
pub fn parse_trigger(spec: &str) -> Option<Trigger> {
    let spec = spec.trim();
    if spec.eq_ignore_ascii_case("now") {
        return Some(Trigger::Now);
    }
    let (key, value) = spec.split_once('=')?;
    match key.trim().to_ascii_lowercase().as_str() {
        "pc" => Some(Trigger::Pc(parse_hex_u32(value)?)),
        "beam" => {
            let value = value.trim();
            let (vpos, hpos) = match value.split_once(':') {
                Some((v, h)) => (v.trim().parse().ok()?, Some(h.trim().parse().ok()?)),
                None => (value.parse().ok()?, None),
            };
            Some(Trigger::Beam { vpos, hpos })
        }
        "reg" => {
            // Custom registers are word offsets; reject odd values rather
            // than silently rounding a typo down to the wrong register.
            let off = parse_hex_u32(value)?;
            (off < 0x200 && off & 1 == 0).then_some(Trigger::RegWrite(off as u16))
        }
        "time" => {
            let secs: f64 = value.trim().parse().ok()?;
            (secs >= 0.0 && secs.is_finite()).then_some(Trigger::Time(secs))
        }
        _ => None,
    }
}

impl fmt::Display for Trigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Trigger::Now => f.write_str("now"),
            Trigger::Pc(pc) => write!(f, "pc={pc:#08X}"),
            Trigger::Beam { vpos, hpos: None } => write!(f, "beam={vpos}"),
            Trigger::Beam {
                vpos,
                hpos: Some(h),
            } => write!(f, "beam={vpos}:{h}"),
            Trigger::RegWrite(off) => write!(f, "reg={off:#05X}"),
            Trigger::Time(secs) => write!(f, "time={secs}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Duration
// ---------------------------------------------------------------------------

/// How long the capture runs once triggered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WaveDuration {
    Cck(u64),
    Frames(u64),
    Millis(u64),
    Secs(f64),
}

/// Parse a duration spec: `Ncck` (bare `N` is cck), `Nf`/`Nframes`,
/// `Nms`, or `Ns`.
pub fn parse_duration(spec: &str) -> Option<WaveDuration> {
    let spec = spec.trim().to_ascii_lowercase();
    let digits_end = spec
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(spec.len());
    let (number, unit) = spec.split_at(digits_end);
    if number.is_empty() {
        return None;
    }
    let duration = match unit.trim() {
        "" | "cck" => WaveDuration::Cck(number.parse().ok()?),
        "f" | "frames" => WaveDuration::Frames(number.parse().ok()?),
        "ms" => WaveDuration::Millis(number.parse().ok()?),
        "s" => WaveDuration::Secs(number.parse().ok()?),
        _ => return None,
    };
    Some(duration)
}

impl WaveDuration {
    /// Resolve to a color-clock count, bounded by the safety cap.
    /// `cck_per_frame` and `cck_hz` come from the live machine at trigger
    /// time so frame/time durations track the configured video standard.
    pub fn to_cck(self, cck_per_frame: u64, cck_hz: f64) -> u64 {
        let cck = match self {
            WaveDuration::Cck(n) => n,
            WaveDuration::Frames(n) => n.saturating_mul(cck_per_frame.max(1)),
            WaveDuration::Millis(n) => (n as f64 / 1000.0 * cck_hz) as u64,
            WaveDuration::Secs(s) => (s.max(0.0) * cck_hz) as u64,
        };
        cck.max(1).min((MAX_CAPTURE_SECONDS * cck_hz) as u64)
    }
}

impl fmt::Display for WaveDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WaveDuration::Cck(n) => write!(f, "{n}cck"),
            WaveDuration::Frames(n) => write!(f, "{n}f"),
            WaveDuration::Millis(n) => write!(f, "{n}ms"),
            WaveDuration::Secs(s) => write!(f, "{s}s"),
        }
    }
}

// ---------------------------------------------------------------------------
// Options and status
// ---------------------------------------------------------------------------

/// Everything needed to arm a capture.
#[derive(Debug, Clone)]
pub struct WaveOptions {
    pub path: PathBuf,
    pub trigger: Trigger,
    pub duration: WaveDuration,
    pub signals: SignalSet,
}

impl WaveOptions {
    pub fn new(path: PathBuf) -> Self {
        WaveOptions {
            path,
            trigger: Trigger::Now,
            duration: WaveDuration::Frames(1),
            signals: SignalSet::ALL,
        }
    }
}

/// Classify the order-free console/GUI WAVE arguments: a trigger spec
/// contains `=` (or is `now`), a duration starts with a digit, a signal
/// list is made of known group names, and anything else is the output
/// path (at most one). An omitted path gets a timestamped default in
/// the working directory.
pub fn parse_wave_args<'a, I>(tokens: I) -> Result<WaveOptions, String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut opts = WaveOptions::new(PathBuf::new());
    let mut path: Option<PathBuf> = None;
    for token in tokens {
        if let Some(trigger) = parse_trigger(token) {
            opts.trigger = trigger;
        } else if token.contains('=') {
            // A malformed trigger must not fall through to the path slot.
            return Err(format!(
                "bad trigger {token:?} (now, pc=ADDR, beam=VPOS[:HPOS], reg=OFF, time=SECS)"
            ));
        } else if let Some(duration) = parse_duration(token) {
            opts.duration = duration;
        } else if let Some(signals) = parse_signals(token) {
            opts.signals = signals;
        } else if path.is_none() {
            path = Some(PathBuf::from(token));
        } else {
            return Err(format!("cannot parse {token:?}"));
        }
    }
    opts.path = path.unwrap_or_else(default_wave_path);
    Ok(opts)
}

/// Timestamped default output path in the working directory.
pub fn default_wave_path() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    PathBuf::from(format!("copperline-wave-{stamp}.vcd"))
}

/// Snapshot of a capture's state for the console/debugger UI.
#[derive(Debug, Clone)]
pub struct WaveStatus {
    pub path: PathBuf,
    pub state: &'static str,
    pub trigger: String,
    pub duration: String,
    pub signals: String,
    pub samples: u64,
    /// Color clocks captured so far and the resolved window length
    /// (None until the trigger fires).
    pub captured_cck: u64,
    pub window_cck: Option<u64>,
}

// ---------------------------------------------------------------------------
// VCD writer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VarId(usize);

#[derive(Clone, Copy, PartialEq)]
enum LastValue {
    None,
    Num(u64),
    Text(&'static str),
}

struct Var {
    id: String,
    width: u8,
    last: LastValue,
}

/// Minimal streaming VCD writer: declaration section, then change-only
/// value emission with lazily written `#time` stamps.
struct VcdWriter<W: Write> {
    out: W,
    vars: Vec<Var>,
    /// The `#time` most recently written to the change section.
    emitted_time: Option<u64>,
    bytes: u64,
    /// Latched on the first change-section write error (disk full,
    /// deleted file). A failed writer emits nothing further, and the
    /// capture's expiry check aborts the window instead of silently
    /// producing a truncated file.
    failed: bool,
}

fn vcd_identifier(index: usize) -> String {
    // Printable ASCII '!'..='~' in as many digits as needed.
    let mut id = String::new();
    let mut n = index;
    loop {
        id.push((b'!' + (n % 94) as u8) as char);
        n /= 94;
        if n == 0 {
            break;
        }
    }
    id
}

impl<W: Write> VcdWriter<W> {
    fn new(out: W) -> Self {
        VcdWriter {
            out,
            vars: Vec::new(),
            emitted_time: None,
            bytes: 0,
            failed: false,
        }
    }

    fn write_line(&mut self, line: &str) -> io::Result<()> {
        self.bytes += line.len() as u64 + 1;
        writeln!(self.out, "{line}")
    }

    fn header(&mut self, comments: &[String]) -> io::Result<()> {
        self.write_line(&format!(
            "$version Copperline {} $end",
            env!("CARGO_PKG_VERSION")
        ))?;
        self.write_line(
            "$comment 1 VCD time unit = 1 color clock (cck); \
             timestamps are relative to the trigger $end",
        )?;
        for comment in comments {
            self.write_line(&format!("$comment {comment} $end"))?;
        }
        self.write_line("$timescale 1 us $end")
    }

    fn scope(&mut self, name: &str) -> io::Result<()> {
        self.write_line(&format!("$scope module {name} $end"))
    }

    fn upscope(&mut self) -> io::Result<()> {
        self.write_line("$upscope $end")
    }

    fn add_wire(&mut self, width: u8, name: &str) -> io::Result<VarId> {
        let id = vcd_identifier(self.vars.len());
        self.write_line(&format!("$var wire {width} {id} {name} $end"))?;
        self.vars.push(Var {
            id,
            width,
            last: LastValue::None,
        });
        Ok(VarId(self.vars.len() - 1))
    }

    /// A GTKWave-supported string variable (`$var string`, `s<text>`
    /// value changes).
    fn add_string(&mut self, name: &str) -> io::Result<VarId> {
        let id = vcd_identifier(self.vars.len());
        self.write_line(&format!("$var string 1 {id} {name} $end"))?;
        self.vars.push(Var {
            id,
            width: 0,
            last: LastValue::None,
        });
        Ok(VarId(self.vars.len() - 1))
    }

    fn enddefinitions(&mut self) -> io::Result<()> {
        self.write_line("$enddefinitions $end")
    }

    fn stamp(&mut self, time: u64) -> io::Result<()> {
        // Keep the change section monotonic even if a caller's clock
        // reads behind (it cannot, but a malformed file helps nobody).
        let time = self.emitted_time.map_or(time, |t| time.max(t));
        if self.emitted_time != Some(time) {
            self.bytes += 12;
            writeln!(self.out, "#{time}")?;
            self.emitted_time = Some(time);
        }
        Ok(())
    }

    /// Emit a vector/bit change if the value differs from the last one,
    /// returning whether one was written. A write error latches `failed`
    /// (silencing further output) so the capture aborts at its next
    /// expiry check instead of quietly truncating the file.
    fn set(&mut self, time: u64, var: VarId, value: u64) -> bool {
        if self.failed {
            return false;
        }
        let slot = &mut self.vars[var.0];
        if slot.last == LastValue::Num(value) {
            return false;
        }
        slot.last = LastValue::Num(value);
        let width = slot.width;
        if self.stamp(time).is_err() {
            self.failed = true;
            return false;
        }
        let var = &self.vars[var.0];
        let written = if width == 1 {
            self.bytes += 2 + var.id.len() as u64;
            writeln!(self.out, "{}{}", value & 1, var.id)
        } else {
            let mut bits = String::with_capacity(width as usize + 2);
            bits.push('b');
            for bit in (0..width).rev() {
                bits.push(if value >> bit & 1 != 0 { '1' } else { '0' });
            }
            self.bytes += bits.len() as u64 + 2 + var.id.len() as u64;
            writeln!(self.out, "{bits} {}", var.id)
        };
        if written.is_err() {
            self.failed = true;
            return false;
        }
        true
    }

    /// Emit a string change if it differs from the last one, with the
    /// same failure latching as `set`. The text must contain no
    /// whitespace.
    fn set_text(&mut self, time: u64, var: VarId, text: &'static str) -> bool {
        if self.failed {
            return false;
        }
        let slot = &mut self.vars[var.0];
        if slot.last == LastValue::Text(text) {
            return false;
        }
        slot.last = LastValue::Text(text);
        if self.stamp(time).is_err() {
            self.failed = true;
            return false;
        }
        let var = &self.vars[var.0];
        self.bytes += text.len() as u64 + 3 + var.id.len() as u64;
        if writeln!(self.out, "s{text} {}", var.id).is_err() {
            self.failed = true;
            return false;
        }
        true
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

// ---------------------------------------------------------------------------
// Capture state machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaveState {
    Armed,
    Capturing { end_cck: u64 },
    Done,
}

/// Per-group variable handles; None when the group is not selected.
#[derive(Default)]
struct WaveVars {
    vpos: Option<VarId>,
    hpos: Option<VarId>,
    frame: Option<VarId>,
    bus_owner: Option<VarId>,
    bus_owner_s: Option<VarId>,
    dmacon: Option<VarId>,
    bus_data: Option<VarId>,
    cpu_addr: Option<VarId>,
    cpu_kind_s: Option<VarId>,
    cpu_rw: Option<VarId>,
    cpu_wait: Option<VarId>,
    cop_pc: Option<VarId>,
    cop_state_s: Option<VarId>,
    blt_busy: Option<VarId>,
    blt_slot_s: Option<VarId>,
    blt_pt: [Option<VarId>; 4],
    regw_off: Option<VarId>,
    regw_val: Option<VarId>,
    regw_src_s: Option<VarId>,
    regw_stb: Option<VarId>,
    ipl: Option<VarId>,
    intreq: Option<VarId>,
    intena: Option<VarId>,
    aud_ch: Option<VarId>,
    aud_stb: Option<VarId>,
}

/// One quantum's worth of bus-level signals, gathered at the chip-bus
/// arbitration tap. All values are raw hardware state; enum-ish values
/// arrive pre-stringified so this module stays chipset-agnostic.
pub struct QuantumSample {
    pub cck: u64,
    pub vpos: u32,
    pub hpos: u32,
    pub frame: u64,
    pub owner_index: u8,
    pub owner_name: &'static str,
    pub dmacon: u16,
    pub data_bus: u16,
    pub cop_pc: u32,
    pub cop_state: &'static str,
    pub blt_busy: bool,
    pub blt_slot: &'static str,
    pub blt_pt: [u32; 4],
    pub ipl: u8,
    pub intreq: u16,
    pub intena: u16,
    /// Audio channel granted a DMA slot this quantum, if any.
    pub audio_channel: Option<u8>,
}

/// A running (armed, capturing, or finished) waveform export.
pub struct WaveCapture {
    writer: VcdWriter<io::BufWriter<std::fs::File>>,
    path: PathBuf,
    trigger: Trigger,
    duration: WaveDuration,
    signals: SignalSet,
    state: WaveState,
    start_cck: u64,
    samples: u64,
    last_cck: u64,
    regw_stb_level: bool,
    aud_stb_level: bool,
    vars: WaveVars,
}

impl WaveCapture {
    /// Create the output file and write the declaration section. The
    /// capture starts in the Armed state.
    pub fn create(opts: WaveOptions) -> io::Result<Self> {
        let file = std::fs::File::create(&opts.path)?;
        let mut writer = VcdWriter::new(io::BufWriter::new(file));
        writer.header(&[format!(
            "trigger: {}; duration: {}; signals: {}",
            opts.trigger, opts.duration, opts.signals
        )])?;
        let mut vars = WaveVars::default();
        writer.scope("copperline")?;
        if opts.signals.contains(SignalSet::BEAM) {
            writer.scope("beam")?;
            vars.vpos = Some(writer.add_wire(16, "vpos")?);
            vars.hpos = Some(writer.add_wire(8, "hpos")?);
            vars.frame = Some(writer.add_wire(32, "frame")?);
            writer.upscope()?;
        }
        if opts.signals.contains(SignalSet::BUS) {
            writer.scope("bus")?;
            vars.bus_owner = Some(writer.add_wire(4, "owner")?);
            vars.bus_owner_s = Some(writer.add_string("owner_name")?);
            vars.dmacon = Some(writer.add_wire(16, "dmacon")?);
            vars.bus_data = Some(writer.add_wire(16, "data")?);
            writer.upscope()?;
        }
        if opts.signals.contains(SignalSet::CPU) {
            writer.scope("cpu")?;
            vars.cpu_addr = Some(writer.add_wire(24, "addr")?);
            vars.cpu_kind_s = Some(writer.add_string("kind")?);
            vars.cpu_rw = Some(writer.add_wire(1, "rw")?);
            vars.cpu_wait = Some(writer.add_wire(16, "wait_cck")?);
            writer.upscope()?;
        }
        if opts.signals.contains(SignalSet::COPPER) {
            writer.scope("copper")?;
            vars.cop_pc = Some(writer.add_wire(24, "pc")?);
            vars.cop_state_s = Some(writer.add_string("state")?);
            writer.upscope()?;
        }
        if opts.signals.contains(SignalSet::BLITTER) {
            writer.scope("blitter")?;
            vars.blt_busy = Some(writer.add_wire(1, "busy")?);
            vars.blt_slot_s = Some(writer.add_string("slot")?);
            for (idx, name) in ["apt", "bpt", "cpt", "dpt"].iter().enumerate() {
                vars.blt_pt[idx] = Some(writer.add_wire(24, name)?);
            }
            writer.upscope()?;
        }
        if opts.signals.contains(SignalSet::REGS) {
            writer.scope("regs")?;
            vars.regw_off = Some(writer.add_wire(9, "off")?);
            vars.regw_val = Some(writer.add_wire(16, "value")?);
            vars.regw_src_s = Some(writer.add_string("source")?);
            vars.regw_stb = Some(writer.add_wire(1, "strobe")?);
            writer.upscope()?;
        }
        if opts.signals.contains(SignalSet::IRQ) {
            writer.scope("irq")?;
            vars.ipl = Some(writer.add_wire(3, "ipl")?);
            vars.intreq = Some(writer.add_wire(16, "intreq")?);
            vars.intena = Some(writer.add_wire(16, "intena")?);
            writer.upscope()?;
        }
        if opts.signals.contains(SignalSet::AUDIO) {
            writer.scope("audio")?;
            vars.aud_ch = Some(writer.add_wire(2, "channel")?);
            vars.aud_stb = Some(writer.add_wire(1, "strobe")?);
            writer.upscope()?;
        }
        writer.upscope()?;
        writer.enddefinitions()?;
        Ok(WaveCapture {
            writer,
            path: opts.path,
            trigger: opts.trigger,
            duration: opts.duration,
            signals: opts.signals,
            state: WaveState::Armed,
            start_cck: 0,
            samples: 0,
            last_cck: 0,
            regw_stb_level: false,
            aud_stb_level: false,
            vars,
        })
    }

    pub fn trigger(&self) -> Trigger {
        self.trigger
    }

    pub fn is_armed(&self) -> bool {
        self.state == WaveState::Armed
    }

    pub fn is_capturing(&self) -> bool {
        matches!(self.state, WaveState::Capturing { .. })
    }

    pub fn is_done(&self) -> bool {
        self.state == WaveState::Done
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Start the capture window at `now_cck`. Frame/time durations
    /// resolve against the machine's live frame geometry and cck rate.
    pub fn fire(&mut self, now_cck: u64, cck_per_frame: u64, cck_hz: f64) {
        if self.state != WaveState::Armed {
            return;
        }
        self.start_cck = now_cck;
        self.last_cck = now_cck;
        self.state = WaveState::Capturing {
            end_cck: now_cck.saturating_add(self.duration.to_cck(cck_per_frame, cck_hz)),
        };
    }

    /// Whether the capture window has run out at `now_cck` -- or must
    /// abort because the file grew past the emergency byte cap or a
    /// write failed. The caller finishes the capture.
    pub fn expired(&self, now_cck: u64) -> bool {
        match self.state {
            WaveState::Capturing { end_cck } => {
                now_cck >= end_cck || self.writer.bytes >= MAX_CAPTURE_BYTES || self.writer.failed
            }
            _ => false,
        }
    }

    /// Whether a change-section write failed (the file is incomplete).
    pub fn write_failed(&self) -> bool {
        self.writer.failed
    }

    fn rel(&self, cck: u64) -> u64 {
        cck.saturating_sub(self.start_cck)
    }

    /// Record one chip-bus quantum's signals.
    pub fn sample_quantum(&mut self, s: &QuantumSample) {
        if !self.is_capturing() {
            return;
        }
        self.last_cck = s.cck;
        let t = self.rel(s.cck);
        let mut changed = false;
        let w = &mut self.writer;
        let v = &self.vars;
        let mut set = |var: Option<VarId>, value: u64| {
            if let Some(var) = var {
                changed |= w.set(t, var, value);
            }
        };
        set(v.vpos, u64::from(s.vpos as u16));
        set(v.hpos, u64::from(s.hpos as u8));
        set(v.frame, s.frame & 0xFFFF_FFFF);
        set(v.bus_owner, u64::from(s.owner_index));
        set(v.dmacon, u64::from(s.dmacon));
        set(v.bus_data, u64::from(s.data_bus));
        set(v.cop_pc, u64::from(s.cop_pc & 0x00FF_FFFF));
        set(v.blt_busy, u64::from(s.blt_busy));
        for (idx, pt) in s.blt_pt.iter().enumerate() {
            set(v.blt_pt[idx], u64::from(pt & 0x00FF_FFFF));
        }
        set(v.ipl, u64::from(s.ipl));
        set(v.intreq, u64::from(s.intreq));
        set(v.intena, u64::from(s.intena));
        if let Some(channel) = s.audio_channel {
            set(v.aud_ch, u64::from(channel));
            if let Some(stb) = v.aud_stb {
                self.aud_stb_level = !self.aud_stb_level;
                changed |= w.set(t, stb, u64::from(self.aud_stb_level));
            }
        }
        let mut set_text = |var: Option<VarId>, text: &'static str| {
            if let Some(var) = var {
                changed |= w.set_text(t, var, text);
            }
        };
        set_text(v.bus_owner_s, s.owner_name);
        set_text(v.cop_state_s, s.cop_state);
        set_text(v.blt_slot_s, s.blt_slot);
        if changed {
            self.samples += 1;
        }
    }

    /// Record a custom-register write (`regs` group).
    pub fn sample_reg_write(&mut self, cck: u64, off: u16, value: u16, source: &'static str) {
        if !self.is_capturing() || !self.signals.contains(SignalSet::REGS) {
            return;
        }
        let t = self.rel(cck);
        let w = &mut self.writer;
        let v = &self.vars;
        if let Some(var) = v.regw_off {
            w.set(t, var, u64::from(off & 0x1FF));
        }
        if let Some(var) = v.regw_val {
            w.set(t, var, u64::from(value));
        }
        if let Some(var) = v.regw_src_s {
            w.set_text(t, var, source);
        }
        if let Some(var) = v.regw_stb {
            self.regw_stb_level = !self.regw_stb_level;
            w.set(t, var, u64::from(self.regw_stb_level));
        }
        self.samples += 1;
    }

    /// Record a granted CPU chip-bus access (`cpu` group).
    pub fn sample_cpu_access(
        &mut self,
        cck: u64,
        addr: Option<u32>,
        kind: &'static str,
        is_write: bool,
        wait_cck: u32,
    ) {
        if !self.is_capturing() || !self.signals.contains(SignalSet::CPU) {
            return;
        }
        let t = self.rel(cck);
        let w = &mut self.writer;
        let v = &self.vars;
        if let (Some(var), Some(addr)) = (v.cpu_addr, addr) {
            w.set(t, var, u64::from(addr & 0x00FF_FFFF));
        }
        if let Some(var) = v.cpu_kind_s {
            w.set_text(t, var, kind);
        }
        if let Some(var) = v.cpu_rw {
            w.set(t, var, u64::from(is_write));
        }
        if let Some(var) = v.cpu_wait {
            w.set(t, var, u64::from(wait_cck.min(0xFFFF) as u16));
        }
        self.samples += 1;
    }

    /// Flush and close out the capture. Idempotent.
    pub fn finish(&mut self) {
        if self.state == WaveState::Done {
            return;
        }
        self.state = WaveState::Done;
        let _ = self.writer.flush();
    }

    pub fn status(&self) -> WaveStatus {
        let (state, window_cck) = match self.state {
            WaveState::Armed => ("armed", None),
            WaveState::Capturing { end_cck } => {
                ("capturing", Some(end_cck.saturating_sub(self.start_cck)))
            }
            WaveState::Done if self.writer.failed => ("failed (write error)", None),
            WaveState::Done => ("done", None),
        };
        WaveStatus {
            path: self.path.clone(),
            state,
            trigger: self.trigger.to_string(),
            duration: self.duration.to_string(),
            signals: self.signals.to_string(),
            samples: self.samples,
            captured_cck: self.last_cck.saturating_sub(self.start_cck),
            window_cck,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_list_parses_groups_and_all() {
        let set = parse_signals("cpu,bus,copper").unwrap();
        assert!(set.contains(SignalSet::CPU));
        assert!(set.contains(SignalSet::BUS));
        assert!(set.contains(SignalSet::COPPER));
        assert!(!set.contains(SignalSet::AUDIO));
        assert_eq!(parse_signals("all"), Some(SignalSet::ALL));
        assert_eq!(parse_signals("beam").unwrap().to_string(), "beam");
        assert_eq!(parse_signals(""), None);
        assert_eq!(parse_signals("bogus"), None);
    }

    #[test]
    fn trigger_specs_round_trip() {
        assert_eq!(parse_trigger("now"), Some(Trigger::Now));
        assert_eq!(parse_trigger("pc=0x00C033C2"), Some(Trigger::Pc(0xC033C2)));
        assert_eq!(parse_trigger("pc=$c033c2"), Some(Trigger::Pc(0xC033C2)));
        assert_eq!(
            parse_trigger("beam=100"),
            Some(Trigger::Beam {
                vpos: 100,
                hpos: None
            })
        );
        assert_eq!(
            parse_trigger("beam=44:129"),
            Some(Trigger::Beam {
                vpos: 44,
                hpos: Some(129)
            })
        );
        assert_eq!(parse_trigger("reg=180"), Some(Trigger::RegWrite(0x180)));
        assert_eq!(parse_trigger("time=1.5"), Some(Trigger::Time(1.5)));
        assert_eq!(parse_trigger("reg=200"), None);
        // Odd register offsets are typos, not something to round down.
        assert_eq!(parse_trigger("reg=181"), None);
        assert_eq!(parse_trigger("pc="), None);
        assert_eq!(parse_trigger("bogus=1"), None);
    }

    #[test]
    fn duration_specs_resolve_to_cck() {
        assert_eq!(parse_duration("20000cck"), Some(WaveDuration::Cck(20000)));
        assert_eq!(parse_duration("500"), Some(WaveDuration::Cck(500)));
        assert_eq!(parse_duration("2f"), Some(WaveDuration::Frames(2)));
        assert_eq!(parse_duration("3frames"), Some(WaveDuration::Frames(3)));
        assert_eq!(parse_duration("10ms"), Some(WaveDuration::Millis(10)));
        assert_eq!(parse_duration("2s"), Some(WaveDuration::Secs(2.0)));
        assert_eq!(parse_duration("x"), None);
        let frame = 312 * 227;
        assert_eq!(
            WaveDuration::Frames(2).to_cck(frame, 3_546_895.0),
            2 * frame
        );
        assert_eq!(
            WaveDuration::Millis(1).to_cck(frame, 3_546_895.0),
            3_546 // 1 ms of PAL color clocks
        );
        // The safety cap bounds silly durations.
        let capped = WaveDuration::Secs(1e9).to_cck(frame, 3_546_895.0);
        assert_eq!(capped, (10.0 * 3_546_895.0) as u64);
    }

    #[test]
    fn wave_args_classify_order_free() {
        let opts = parse_wave_args(["out.vcd", "pc=0xC033C2", "2f", "cpu,bus"]).unwrap();
        assert_eq!(opts.path, PathBuf::from("out.vcd"));
        assert_eq!(opts.trigger, Trigger::Pc(0xC033C2));
        assert_eq!(opts.duration, WaveDuration::Frames(2));
        assert!(opts.signals.contains(SignalSet::CPU));
        assert!(!opts.signals.contains(SignalSet::REGS));
        // Order-free, uppercased (the debugger entry box uppercases).
        let opts = parse_wave_args(["BEAM=100:64", "500CCK"]).unwrap();
        assert_eq!(
            opts.trigger,
            Trigger::Beam {
                vpos: 100,
                hpos: Some(64)
            }
        );
        assert_eq!(opts.duration, WaveDuration::Cck(500));
        assert!(opts.path.to_string_lossy().starts_with("copperline-wave-"));
        // Defaults with no arguments at all.
        let opts = parse_wave_args([]).unwrap();
        assert_eq!(opts.trigger, Trigger::Now);
        assert_eq!(opts.duration, WaveDuration::Frames(1));
        // A malformed trigger is an error, not a path.
        assert!(parse_wave_args(["pc=zz"]).is_err());
        // Two path-looking tokens are an error.
        assert!(parse_wave_args(["a.vcd", "b.vcd"]).is_err());
    }

    #[test]
    fn vcd_writer_emits_changes_once() {
        let mut w = VcdWriter::new(Vec::new());
        w.header(&["test".into()]).unwrap();
        w.scope("top").unwrap();
        let a = w.add_wire(4, "a").unwrap();
        let b = w.add_wire(1, "b").unwrap();
        let s = w.add_string("s").unwrap();
        w.upscope().unwrap();
        w.enddefinitions().unwrap();
        assert!(w.set(0, a, 5));
        assert!(!w.set(0, a, 5)); // dedup
        assert!(w.set(0, b, 1));
        assert!(w.set_text(0, s, "run"));
        assert!(!w.set_text(0, s, "run"));
        assert!(w.set(3, a, 6));
        let text = String::from_utf8(w.out).unwrap();
        assert!(text.contains("$timescale 1 us $end"));
        assert!(text.contains("$var wire 4 ! a $end"));
        assert!(text.contains("$var wire 1 \" b $end"));
        assert!(text.contains("$var string 1 # s $end"));
        assert!(text.contains("$enddefinitions $end"));
        // One #0 stamp, then the three changes, then #3 with the new value.
        let tail: Vec<&str> = text
            .lines()
            .skip_while(|l| !l.starts_with("$enddefinitions"))
            .skip(1)
            .collect();
        assert_eq!(tail, ["#0", "b0101 !", "1\"", "srun #", "#3", "b0110 !"]);
    }

    #[test]
    fn vcd_writer_latches_failure_on_write_error() {
        /// A sink that accepts `remaining` bytes and then fails, like a
        /// full disk.
        struct FailAfter {
            remaining: usize,
        }
        impl Write for FailAfter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if self.remaining < buf.len() {
                    return Err(io::Error::other("disk full"));
                }
                self.remaining -= buf.len();
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut w = VcdWriter::new(FailAfter { remaining: 64 });
        let a = w.add_wire(4, "a").unwrap();
        assert!(!w.failed);
        for i in 0..100u64 {
            w.set(i, a, i);
        }
        assert!(w.failed, "the byte budget must have run out");
        // A failed writer emits nothing further.
        assert!(!w.set(1000, a, 0xF));
    }

    #[test]
    fn vcd_identifiers_stay_printable_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for index in 0..500 {
            let id = vcd_identifier(index);
            assert!(id.bytes().all(|b| (b'!'..=b'~').contains(&b)));
            assert!(seen.insert(id));
        }
    }
}
