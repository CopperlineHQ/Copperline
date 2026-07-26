// SPDX-License-Identifier: GPL-3.0-or-later

//! A lightweight, headless-friendly CPU/memory debugger driven by `COPPERLINE_DBG_*`
//! environment variables. It supports PC breakpoints, memory write-watchpoints,
//! per-hit register and memory dumps, a screenshot of the current frame on each
//! hit, and a raw instruction trace within a time window. All output goes
//! through `log` at info level. It is meant for investigating demo/timing
//! behaviour during `--screenshot-after` runs, where attaching an interactive
//! debugger is not practical.
//!
//! Configuration (all optional; the debugger stays disabled unless at least one
//! of BREAK / WATCH / TRACE is set). Addresses are hex, with or without `0x`:
//!
//! ```text
//! COPPERLINE_DBG_BREAK   = comma-separated PCs to break on     e.g. "C033C2,C033C8"
//! COPPERLINE_DBG_WATCH   = comma-separated write-watch ranges,
//!                      "ADDR" or "ADDR:LEN" (LEN in bytes) e.g. "C09580:2"
//! COPPERLINE_DBG_DUMP    = comma-separated "ADDR:WORDS" memory
//!                      regions hexdumped on every hit      e.g. "C09580:4"
//! COPPERLINE_DBG_TRACE   = set to log every executed instruction in the window
//! COPPERLINE_DBG_TRACE_FULL = like TRACE, but each line is a fixed-width all-hex
//!                      record of the whole register file (D0-D7/A0-A7) and CCR,
//!                      for diffing against a reference 68000 (e.g. vAmiga). Implies
//!                      TRACE. Format: "ft pc=.. op=.. ccr=.. d=.. a=.. | <disasm>"
//! COPPERLINE_DBG_TRACE_LO/HI = only trace instructions with LO <= pc <= HI, to
//!                      isolate one routine (e.g. a depacker loop) from the rest
//!                      of the system     e.g. LO="DE488" HI="DE578"
//! COPPERLINE_DBG_CATCH = comma-separated exception catches. Entries are
//!                      decimal vector numbers, or "irq N", "trap N", "vec N"
//!                      e.g. "3,4,irq 3,trap 0"
//! COPPERLINE_DBG_CATCHALERT = set to break at exec Alert() once ExecBase is valid
//! COPPERLINE_DBG_AFTER   = emulated seconds before which the debugger is inert
//! COPPERLINE_DBG_UNTIL   = emulated seconds after which the debugger is inert
//! COPPERLINE_DBG_MAXHITS = stop reporting after this many hits (default 200)
//! COPPERLINE_DBG_SHOT    = path prefix; saves "<prefix>-<seq>.png" of the current
//!                      frame on each breakpoint/watch hit
//! COPPERLINE_DBG_COPPER  = disassemble the Copper list once when the debugger
//!                      first activates. "auto"/"1" uses the live COP1LC;
//!                      "ADDR" / "ADDR:LEN" dumps LEN instructions from ADDR
//!                      e.g. "auto:64" or "C00100:200"
//! ```
//!
//! Reverse debugging ("rr"-style) is armed by a separate group of knobs,
//! parsed by `reverse_config_from_env` and applied to the emulator's snapshot
//! ring (see `crate::timetravel`). It needs `COPPERLINE_RTC_FIXED_SECS` set
//! for deterministic replay; see `docs/debugger`.
//!
//! ```text
//! COPPERLINE_DBG_RWATCH  = "last writer" reverse watchpoint, "ADDR" or
//!                      "ADDR:LEN". At COPPERLINE_DBG_UNTIL (the target time,
//!                      or run end if unset) it reports the last instruction
//!                      that wrote the word, then resumes.  e.g. "DE488"
//! COPPERLINE_DBG_RR      = "1" to arm the snapshot ring without a watchpoint
//!                      (so reverse-step navigation has history to work from)
//! COPPERLINE_DBG_RR_BUDGET_MB = snapshot-ring memory cap, MiB (default 512)
//! COPPERLINE_DBG_RR_INTERVAL  = emulated frames between snapshots (default 5)
//! ```
//!
//! The instruction trace (COPPERLINE_DBG_TRACE) disassembles each executed
//! instruction (see `crate::disasm`).

/// A memory write-watch range, `[addr, addr+len)`.
pub struct Watch {
    pub addr: u32,
    pub len: u32,
}

/// The 68000/EC020 address-bus mask (A0-A23). Debugger surfaces compare
/// and display through the machine's model mask (`ui_addr_mask()`); this
/// constant remains for tests and 24-bit callers.
pub const UI_ADDR_MASK: u32 = 0x00FF_FFFF;

/// An interactive memory watchpoint: a 16-bit word and the value it held
/// when the watch was set or last hit. The CPU loop stops when the live
/// word differs, whoever wrote it (CPU, Copper, blitter, disk DMA).
pub struct UiWatch {
    pub addr: u32,
    pub last: u16,
    /// Only stop when the change was made by this writer; None = any.
    pub filter: Option<WatchSource>,
    /// Only stop when the CPU was executing this instruction; None = any.
    /// A word a dozen routines all poke is otherwise unwatchable: this is
    /// how you ask about the one caller you care about.
    pub pc: Option<u32>,
}

/// Who touched a watched memory word: attributed at the access site (the
/// CPU write path, the blitter's D/line/fill writes, disk read DMA, and
/// the read-side DMA channels that fetch through the chip bus).
///
/// The channel-numbered variants exist because "which bitplane fetched
/// this word" and "which sprite" are the questions a display bug
/// actually poses; a bare "some DMA engine" answer sends you back to the
/// slot map to work it out by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchSource {
    Cpu,
    Blitter,
    Disk,
    /// Bitplane DMA fetch, plane 0-7 (BPL1DAT is plane 0).
    Bitplane(u8),
    /// Sprite DMA fetch, sprite 0-7.
    Sprite(u8),
    /// Audio sample DMA fetch, channel 0-3.
    Audio(u8),
    /// A Copper instruction-word fetch.
    Copper,
}

impl WatchSource {
    pub fn label(self) -> &'static str {
        match self {
            WatchSource::Cpu => "cpu",
            WatchSource::Blitter => "blitter",
            WatchSource::Disk => "disk",
            WatchSource::Bitplane(_) => "bitplane",
            WatchSource::Sprite(_) => "sprite",
            WatchSource::Audio(_) => "audio",
            WatchSource::Copper => "copper",
        }
    }

    /// The label with its channel number, as reports print it.
    pub fn describe(self) -> String {
        match self {
            WatchSource::Bitplane(n) => format!("bpl{}", n + 1),
            WatchSource::Sprite(n) => format!("spr{n}"),
            WatchSource::Audio(n) => format!("aud{n}"),
            other => other.label().to_string(),
        }
    }

    /// Parse a console filter token (case-insensitive): an engine name
    /// (`cpu`, `blitter`, `disk`, `copper`) or a numbered DMA channel
    /// (`bpl1`-`bpl8`, `spr0`-`spr7`, `aud0`-`aud3`, each bounded by the
    /// channels the hardware has). A DMA filter names
    /// exactly one channel; there is no "any bitplane" form, because the
    /// question a display bug poses is which one.
    pub fn parse(token: &str) -> Option<Self> {
        // Channel counts are the hardware's, not a shared bound: Paula
        // has four audio channels where Denise has eight sprites, so
        // `aud4` must not parse into a filter nothing can ever match.
        for (name, make, channels) in [
            ("bpl", (|n| WatchSource::Bitplane(n)) as fn(u8) -> Self, 8u8),
            ("spr", |n| WatchSource::Sprite(n), 8),
            ("aud", |n| WatchSource::Audio(n), 4),
        ] {
            if let Some(rest) = token
                .to_ascii_lowercase()
                .strip_prefix(name)
                .map(str::to_string)
            {
                let n: u8 = rest.parse().ok()?;
                // BPL channels are named from 1 on the hardware; the
                // others from 0.
                let index = if name == "bpl" { n.checked_sub(1)? } else { n };
                return (index < channels).then(|| make(index));
            }
        }
        for (name, source) in [
            ("cpu", WatchSource::Cpu),
            ("blitter", WatchSource::Blitter),
            ("disk", WatchSource::Disk),
            ("copper", WatchSource::Copper),
        ] {
            if token.eq_ignore_ascii_case(name) {
                return Some(source);
            }
        }
        None
    }

    /// Whether a watch filtered on `self` accepts an access attributed
    /// to `actual`. A DMA filter matches its own channel only.
    pub fn accepts(self, actual: WatchSource) -> bool {
        self == actual
    }

    /// Whether a PC qualifier means anything for this access class.
    ///
    /// Only the CPU has an instruction behind an access. A DMA engine's
    /// fetch or write is issued by the chip bus with no PC to compare,
    /// so pairing a PC with a channel filter describes something that
    /// cannot happen, and the watch would simply never fire.
    pub fn takes_pc_qualifier(self) -> bool {
        matches!(self, WatchSource::Cpu)
    }
}

/// Why the interactive debugger stopped the machine.
#[derive(Debug)]
pub enum DebugStop {
    /// The next instruction's address matches a breakpoint (it has not
    /// executed yet).
    Breakpoint { pc: u32 },
    /// A watched memory word changed during the last instruction.
    Watch {
        addr: u32,
        old: u16,
        new: u16,
        /// The instruction that was executing when the change was seen
        /// (the true writer when `source` is Cpu).
        writer_pc: u32,
        source: WatchSource,
        vpos: u16,
        hpos: u16,
    },
    /// A watched custom chipset register was written (by any source: CPU
    /// or Copper), at the given beam position.
    ChipReg {
        off: u16,
        value: u16,
        source: &'static str,
        vpos: u16,
        hpos: u16,
    },
    /// The Agnus beam crossed a beam trap's position.
    Beam { vpos: u16, hpos: u16 },
    /// The Copper's PC arrived at a Copper breakpoint (the instruction
    /// there has not executed yet).
    CopperBreak { pc: u32, vpos: u16, hpos: u16 },
    /// The CPU entered a caught exception vector; `pc` is the handler
    /// entry the machine stopped at.
    Exception { vector: u16, pc: u32 },
    /// Exec scheduled a task matching the armed task catch.
    Task { name: String, addr: u32 },
}

/// Human name of a 68000 exception vector, for catchpoint listings and
/// stop reasons.
pub fn exception_vector_name(vector: u16) -> String {
    match vector {
        2 => "Bus error".to_string(),
        3 => "Address error".to_string(),
        4 => "Illegal instruction".to_string(),
        5 => "Zero divide".to_string(),
        6 => "CHK".to_string(),
        7 => "TRAPV".to_string(),
        8 => "Privilege violation".to_string(),
        9 => "Trace".to_string(),
        10 => "Line-A".to_string(),
        11 => "Line-F".to_string(),
        24 => "Spurious interrupt".to_string(),
        25..=31 => format!("IRQ level {}", vector - 24),
        32..=47 => format!("TRAP #{}", vector - 32),
        _ => format!("vector {vector}"),
    }
}

impl DebugStop {
    /// A one-line human-readable reason, shown as the OSD/panel message.
    pub fn describe(&self) -> String {
        match self {
            DebugStop::Breakpoint { pc } => format!("Breakpoint at ${pc:06X}"),
            DebugStop::Watch {
                addr,
                old,
                new,
                writer_pc,
                source,
                vpos,
                hpos,
            } => match source {
                WatchSource::Cpu => {
                    format!("Watch ${addr:06X}: {old:04X}->{new:04X} (pc ${writer_pc:06X})")
                }
                // A read-side DMA channel leaves the word alone, so an
                // unchanged value is the tell that this was a fetch, not
                // a write.
                _ if old == new => format!(
                    "Watch ${addr:06X}: {new:04X} read by {} (v{vpos} h{hpos})",
                    source.describe()
                ),
                _ => format!(
                    "Watch ${addr:06X}: {old:04X}->{new:04X} ({} write, v{vpos} h{hpos})",
                    source.describe()
                ),
            },
            DebugStop::ChipReg {
                off,
                value,
                source,
                vpos,
                hpos,
            } => format!(
                "{} = {value:04X} ({source} write, v{vpos} h{hpos})",
                custom_reg_name(*off)
            ),
            DebugStop::Beam { vpos, hpos } => format!("Beam trap at v{vpos} h{hpos}"),
            DebugStop::CopperBreak { pc, vpos, hpos } => {
                format!("Copper breakpoint at ${pc:06X} (v{vpos} h{hpos})")
            }
            DebugStop::Exception { vector, pc } => format!(
                "Caught {} (vector {vector}), handler ${pc:06X}",
                exception_vector_name(*vector)
            ),
            DebugStop::Task { name, addr } => {
                format!("Task scheduled: {name} (task ${addr:06X})")
            }
        }
    }
}

/// Decoded bit/field lines for a custom register's value, for the
/// debugger's IO Map tab. Registers without a decode table return an
/// empty vec (the raw hex is always shown alongside).
pub fn custom_reg_bit_decode(off: u16, value: u16) -> Vec<String> {
    let off = off & 0x1FE;
    let named_bits: &[(u16, &str)] = match off {
        0x002 | 0x096 => &[
            (14, "BBUSY"),
            (13, "BZERO"),
            (10, "BLTPRI"),
            (9, "DMAEN"),
            (8, "BPLEN"),
            (7, "COPEN"),
            (6, "BLTEN"),
            (5, "SPREN"),
            (4, "DSKEN"),
            (3, "AUD3"),
            (2, "AUD2"),
            (1, "AUD1"),
            (0, "AUD0"),
        ],
        0x01C | 0x01E | 0x09A | 0x09C => &[
            (14, "INTEN"),
            (13, "EXTER"),
            (12, "DSKSYN"),
            (11, "RBF"),
            (10, "AUD3"),
            (9, "AUD2"),
            (8, "AUD1"),
            (7, "AUD0"),
            (6, "BLIT"),
            (5, "VERTB"),
            (4, "COPER"),
            (3, "PORTS"),
            (2, "SOFT"),
            (1, "DSKBLK"),
            (0, "TBE"),
        ],
        0x010 | 0x09E => &[
            (14, "PRECOMP1"),
            (13, "PRECOMP0"),
            (12, "MFMPREC"),
            (11, "WORDSYNC"),
            (10, "MSBSYNC"),
            (9, "FAST"),
            (7, "USE3PN"),
            (6, "USE2P3"),
            (5, "USE1P2"),
            (4, "USE0P1"),
            (3, "USE3VN"),
            (2, "USE2V3"),
            (1, "USE1V2"),
            (0, "USE0V1"),
        ],
        0x100 => &[
            (15, "HIRES"),
            (11, "HAM"),
            (10, "DPF"),
            (9, "COLOR"),
            (8, "GAUD"),
            (6, "SHRES"),
            (3, "LPEN"),
            (2, "LACE"),
            (1, "ERSY"),
            (0, "ECSENA"),
        ],
        0x104 => &[(6, "PF2PRI"), (10, "KILLEHB")],
        0x098 => &[
            (15, "ENSP7"),
            (14, "ENSP5"),
            (13, "ENSP3"),
            (12, "ENSP1"),
            (11, "ENBP6"),
            (10, "ENBP5"),
            (9, "ENBP4"),
            (8, "ENBP3"),
            (7, "ENBP2"),
            (6, "ENBP1"),
        ],
        0x1DC => &[
            (14, "HARDDIS"),
            (13, "LPENDIS"),
            (12, "VARVBEN"),
            (11, "LOLDIS"),
            (10, "CSCBEN"),
            (9, "VARVSYEN"),
            (8, "VARHSYEN"),
            (7, "VARBEAMEN"),
            (6, "DUAL"),
            (5, "PAL"),
        ],
        0x1FC => &[
            (15, "SSCAN2"),
            (14, "BSCAN2"),
            (3, "SPAGEM"),
            (2, "SPR32"),
            (1, "BPAGEM"),
            (0, "BPL32"),
        ],
        _ => &[],
    };
    let mut lines = Vec::new();
    if !named_bits.is_empty() {
        let set: Vec<&str> = named_bits
            .iter()
            .filter(|(bit, _)| value & (1 << bit) != 0)
            .map(|(_, name)| *name)
            .collect();
        lines.push(if set.is_empty() {
            "(no named bits set)".to_string()
        } else {
            set.join(" ")
        });
    }
    // Multi-bit fields.
    match off {
        0x100 => {
            let bpu = ((value >> 12) & 7) + (((value >> 4) & 1) << 3);
            lines.push(format!("BPU={bpu}"));
        }
        0x102 => lines.push(format!(
            "PF1H={} PF2H={}",
            value & 0x000F,
            (value >> 4) & 0x000F
        )),
        0x104 => lines.push(format!(
            "PF1P={} PF2P={}",
            value & 0x0007,
            (value >> 3) & 0x0007
        )),
        0x08E | 0x090 => lines.push(format!("v={} h={}", (value >> 8) & 0xFF, value & 0xFF)),
        0x092 | 0x094 => lines.push(format!("cck ${:02X}", value & 0x00FC)),
        _ => {}
    }
    lines
}

/// Decode an AmigaOS alert ("guru meditation") code: the deadend flag,
/// the owning subsystem, the general cause, and the CPU-trap alerts
/// exec raises for processor exceptions.
pub fn guru_decode(code: u32) -> String {
    let deadend = code & 0x8000_0000 != 0;
    // Alerts exec raises for CPU exceptions carry the vector number in
    // the low word with no subsystem byte.
    if code & 0x7FFF_0000 == 0 && (2..=0x2F).contains(&(code & 0xFFFF)) {
        return format!(
            "{}CPU exception: {}",
            if deadend { "DEADEND " } else { "" },
            exception_vector_name((code & 0xFFFF) as u16)
        );
    }
    let subsystem = match (code >> 24) & 0x7F {
        0x01 => "exec.library",
        0x02 => "graphics.library",
        0x03 => "layers.library",
        0x04 => "intuition.library",
        0x05 => "mathlibs",
        0x07 => "dos.library",
        0x08 => "ramlib",
        0x09 => "icon.library",
        0x0A => "expansion.library",
        0x0B => "diskfont.library",
        0x10 => "audio.device",
        0x11 => "console.device",
        0x12 => "gameport.device",
        0x13 => "keyboard.device",
        0x14 => "trackdisk.device",
        0x15 => "timer.device",
        0x20 => "cia.resource",
        0x21 => "disk.resource",
        0x22 => "misc.resource",
        0x30 => "bootstrap",
        0x31 => "workbench",
        0x32 => "diskcopy",
        0x33 => "gadtools",
        _ => "unknown subsystem",
    };
    let general = match (code >> 16) & 0xFF {
        0x01 => ", no memory",
        0x02 => ", MakeLibrary failed",
        0x03 => ", OpenLibrary failed",
        0x04 => ", OpenDevice failed",
        0x05 => ", OpenResource failed",
        0x06 => ", I/O error",
        0x07 => ", no signal",
        0x08 => ", bad parameter",
        0x09 => ", CloseLibrary failed",
        0x0A => ", CloseDevice failed",
        0x0B => ", process creation failed",
        _ => "",
    };
    format!(
        "{}{subsystem}{general} (specific ${:04X})",
        if deadend { "DEADEND " } else { "recoverable " },
        code & 0xFFFF
    )
}

/// The hardware name of a custom-register word offset into $DFF000
/// ($000-$1FE), e.g. 0x096 -> "DMACON". Banked registers (audio channels,
/// bitplane/sprite pointers and data, colors) are derived; offsets without
/// an assigned register fall back to the hex offset.
pub fn custom_reg_name(off: u16) -> String {
    let off = off & 0x1FE;
    match off {
        0x0A0..=0x0DE => {
            let channel = (off - 0x0A0) / 0x10;
            const PARTS: [&str; 6] = ["LCH", "LCL", "LEN", "PER", "VOL", "DAT"];
            if let Some(part) = PARTS.get(((off & 0x0E) >> 1) as usize) {
                return format!("AUD{channel}{part}");
            }
        }
        0x0E0..=0x0FE => {
            let plane = (off - 0x0E0) / 4 + 1;
            let half = if off & 2 == 0 { "H" } else { "L" };
            return format!("BPL{plane}PT{half}");
        }
        0x110..=0x11E => {
            return format!("BPL{}DAT", (off - 0x110) / 2 + 1);
        }
        0x120..=0x13E => {
            let sprite = (off - 0x120) / 4;
            let half = if off & 2 == 0 { "H" } else { "L" };
            return format!("SPR{sprite}PT{half}");
        }
        0x140..=0x17E => {
            let sprite = (off - 0x140) / 8;
            const PARTS: [&str; 4] = ["POS", "CTL", "DATA", "DATB"];
            return format!("SPR{sprite}{}", PARTS[((off >> 1) & 3) as usize]);
        }
        0x180..=0x1BE => {
            return format!("COLOR{:02}", (off - 0x180) / 2);
        }
        _ => {}
    }
    let fixed = match off {
        0x000 => "BLTDDAT",
        0x002 => "DMACONR",
        0x004 => "VPOSR",
        0x006 => "VHPOSR",
        0x008 => "DSKDATR",
        0x00A => "JOY0DAT",
        0x00C => "JOY1DAT",
        0x00E => "CLXDAT",
        0x010 => "ADKCONR",
        0x012 => "POT0DAT",
        0x014 => "POT1DAT",
        0x016 => "POTGOR",
        0x018 => "SERDATR",
        0x01A => "DSKBYTR",
        0x01C => "INTENAR",
        0x01E => "INTREQR",
        0x020 => "DSKPTH",
        0x022 => "DSKPTL",
        0x024 => "DSKLEN",
        0x026 => "DSKDAT",
        0x028 => "REFPTR",
        0x02A => "VPOSW",
        0x02C => "VHPOSW",
        0x02E => "COPCON",
        0x030 => "SERDAT",
        0x032 => "SERPER",
        0x034 => "POTGO",
        0x036 => "JOYTEST",
        0x038 => "STREQU",
        0x03A => "STRVBL",
        0x03C => "STRHOR",
        0x03E => "STRLONG",
        0x040 => "BLTCON0",
        0x042 => "BLTCON1",
        0x044 => "BLTAFWM",
        0x046 => "BLTALWM",
        0x048 => "BLTCPTH",
        0x04A => "BLTCPTL",
        0x04C => "BLTBPTH",
        0x04E => "BLTBPTL",
        0x050 => "BLTAPTH",
        0x052 => "BLTAPTL",
        0x054 => "BLTDPTH",
        0x056 => "BLTDPTL",
        0x058 => "BLTSIZE",
        0x05A => "BLTCON0L",
        0x05C => "BLTSIZV",
        0x05E => "BLTSIZH",
        0x060 => "BLTCMOD",
        0x062 => "BLTBMOD",
        0x064 => "BLTAMOD",
        0x066 => "BLTDMOD",
        0x070 => "BLTCDAT",
        0x072 => "BLTBDAT",
        0x074 => "BLTADAT",
        0x078 => "SPRHDAT",
        0x07A => "BPLHDAT",
        0x07C => "DENISEID",
        0x07E => "DSKSYNC",
        0x080 => "COP1LCH",
        0x082 => "COP1LCL",
        0x084 => "COP2LCH",
        0x086 => "COP2LCL",
        0x088 => "COPJMP1",
        0x08A => "COPJMP2",
        0x08C => "COPINS",
        0x08E => "DIWSTRT",
        0x090 => "DIWSTOP",
        0x092 => "DDFSTRT",
        0x094 => "DDFSTOP",
        0x096 => "DMACON",
        0x098 => "CLXCON",
        0x09A => "INTENA",
        0x09C => "INTREQ",
        0x09E => "ADKCON",
        0x100 => "BPLCON0",
        0x102 => "BPLCON1",
        0x104 => "BPLCON2",
        0x106 => "BPLCON3",
        0x108 => "BPL1MOD",
        0x10A => "BPL2MOD",
        0x10C => "BPLCON4",
        0x10E => "CLXCON2",
        0x1C0 => "HTOTAL",
        0x1C2 => "HSSTOP",
        0x1C4 => "HBSTRT",
        0x1C6 => "HBSTOP",
        0x1C8 => "VTOTAL",
        0x1CA => "VSSTOP",
        0x1CC => "VBSTRT",
        0x1CE => "VBSTOP",
        0x1D0 => "SPRHSTRT",
        0x1D2 => "SPRHSTOP",
        0x1D4 => "BPLHSTRT",
        0x1D6 => "BPLHSTOP",
        0x1D8 => "HHPOSW",
        0x1DA => "HHPOSR",
        0x1DC => "BEAMCON0",
        0x1DE => "HSSTRT",
        0x1E0 => "VSSTRT",
        0x1E2 => "HCENTER",
        0x1E4 => "DIWHIGH",
        0x1E6 => "BPLHMOD",
        0x1E8 => "SPRHPTH",
        0x1EA => "SPRHPTL",
        0x1EC => "BPLHPTH",
        0x1EE => "BPLHPTL",
        0x1FC => "FMODE",
        0x1FE => "NO-OP",
        _ => return format!("${off:03X}"),
    };
    fixed.to_string()
}

/// One operand in a breakpoint condition: a register, an immediate, or a
/// 16-bit memory word. Memory and immediates are written in hex; the memory
/// form is `M<hex>` (e.g. `MC00002`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CondOperand {
    Data(usize),
    Addr(usize),
    Pc,
    Sr,
    Imm(u32),
    Mem(u32),
}

/// Comparison used in a breakpoint condition. `And` is a bit test: it is true
/// when `lhs & rhs` is non-zero, handy for checking flag/register bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CondOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
}

/// A breakpoint condition: `lhs op rhs`, evaluated against live CPU/memory
/// state through [`BreakContext`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BreakCond {
    pub lhs: CondOperand,
    pub op: CondOp,
    pub rhs: CondOperand,
}

/// Live CPU/memory state a [`BreakCond`] reads. Implemented over the running
/// machine at the breakpoint gate, kept as a trait so the condition logic and
/// its tests stay independent of the CPU core type.
pub trait BreakContext {
    fn data(&self, n: usize) -> u32;
    fn addr_reg(&self, n: usize) -> u32;
    fn pc(&self) -> u32;
    fn sr(&self) -> u32;
    fn mem_word(&self, addr: u32) -> u16;
}

impl CondOperand {
    fn value(self, ctx: &dyn BreakContext) -> u32 {
        match self {
            CondOperand::Data(n) => ctx.data(n),
            CondOperand::Addr(n) => ctx.addr_reg(n),
            CondOperand::Pc => ctx.pc(),
            CondOperand::Sr => ctx.sr(),
            CondOperand::Imm(v) => v,
            CondOperand::Mem(a) => u32::from(ctx.mem_word(a)),
        }
    }

    fn describe(self) -> String {
        match self {
            CondOperand::Data(n) => format!("D{n}"),
            CondOperand::Addr(n) => format!("A{n}"),
            CondOperand::Pc => "PC".to_string(),
            CondOperand::Sr => "SR".to_string(),
            CondOperand::Imm(v) => format!("${v:X}"),
            CondOperand::Mem(a) => format!("M{a:X}"),
        }
    }
}

impl CondOp {
    pub fn mnemonic(self) -> &'static str {
        match self {
            CondOp::Eq => "EQ",
            CondOp::Ne => "NE",
            CondOp::Lt => "LT",
            CondOp::Gt => "GT",
            CondOp::Le => "LE",
            CondOp::Ge => "GE",
            CondOp::And => "AND",
        }
    }
}

impl BreakCond {
    pub fn eval(&self, ctx: &dyn BreakContext) -> bool {
        let l = self.lhs.value(ctx);
        let r = self.rhs.value(ctx);
        match self.op {
            CondOp::Eq => l == r,
            CondOp::Ne => l != r,
            CondOp::Lt => l < r,
            CondOp::Gt => l > r,
            CondOp::Le => l <= r,
            CondOp::Ge => l >= r,
            CondOp::And => (l & r) != 0,
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{} {} {}",
            self.lhs.describe(),
            self.op.mnemonic(),
            self.rhs.describe()
        )
    }
}

/// One interactive PC breakpoint: an address, an optional condition that must
/// hold for it to fire, and an ignore count (skip this many qualifying hits
/// before stopping).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breakpoint {
    pub addr: u32,
    pub cond: Option<BreakCond>,
    pub ignore: u32,
    pub hits: u32,
}

/// The debugger window's breakpoint/watchpoint set. Owned by the CPU
/// machine so it stays armed while the window is closed; `armed` is the
/// single per-instruction gate the hot loop checks.
pub struct InteractiveBreaks {
    /// Address-bus mask breakpoint/watch addresses and PCs are compared
    /// through: A0-A23 on 24-bit models, full 32 bits on 020+ (set from
    /// the CPU model at machine construction).
    pub addr_mask: u32,
    pub breakpoints: Vec<Breakpoint>,
    pub watches: Vec<UiWatch>,
    /// Watched custom-register word offsets into $DFF000 ($000-$1FE).
    /// Hits are recorded by the bus's custom-register write path (which
    /// sees every writer, CPU and Copper alike), so the offsets are
    /// mirrored into the Bus whenever this list changes.
    pub reg_watches: Vec<u16>,
    /// Caught exception vector numbers: the machine stops when the CPU
    /// enters one of these vectors (trap, fault, or interrupt).
    pub catches: Vec<u16>,
    /// Stop when exec schedules a task whose name contains this
    /// (case-insensitive) fragment; None = disabled.
    pub task_catch: Option<String>,
    armed: bool,
}

impl InteractiveBreaks {
    /// An empty break set comparing addresses through `addr_mask` (the
    /// owning machine's address-bus mask; see `address_mask_for_model`).
    pub fn new(addr_mask: u32) -> Self {
        Self {
            addr_mask,
            breakpoints: Vec::new(),
            watches: Vec::new(),
            reg_watches: Vec::new(),
            catches: Vec::new(),
            task_catch: None,
            armed: false,
        }
    }

    pub fn armed(&self) -> bool {
        self.armed
    }

    fn rearm(&mut self) {
        self.armed = !(self.breakpoints.is_empty()
            && self.watches.is_empty()
            && self.reg_watches.is_empty()
            && self.catches.is_empty()
            && self.task_catch.is_none());
    }

    /// Whether any breakpoint is set at `pc`, ignoring its condition. Used for
    /// display (marking the address) and the reverse-debug scan.
    pub fn is_breakpoint(&self, pc: u32) -> bool {
        let pc = pc & self.addr_mask;
        self.breakpoints.iter().any(|bp| bp.addr == pc)
    }

    /// Add a breakpoint with an optional condition and ignore count, or remove
    /// the breakpoint at `addr` when one already exists. Returns true when the
    /// breakpoint is now set.
    pub fn toggle_breakpoint_full(
        &mut self,
        addr: u32,
        cond: Option<BreakCond>,
        ignore: u32,
    ) -> bool {
        let addr = addr & self.addr_mask;
        let added = match self.breakpoints.iter().position(|bp| bp.addr == addr) {
            Some(pos) => {
                self.breakpoints.remove(pos);
                false
            }
            None => {
                self.breakpoints.push(Breakpoint {
                    addr,
                    cond,
                    ignore,
                    hits: 0,
                });
                true
            }
        };
        self.rearm();
        added
    }

    /// Decide whether reaching `pc` should stop. A breakpoint stops when its
    /// address matches, its condition (if any) holds against `ctx`, and its
    /// ignore count has been exhausted -- each qualifying hit before that is
    /// counted and skipped.
    pub fn breakpoint_stops(&mut self, pc: u32, ctx: &dyn BreakContext) -> bool {
        let pc = pc & self.addr_mask;
        let Some(bp) = self.breakpoints.iter_mut().find(|bp| bp.addr == pc) else {
            return false;
        };
        if let Some(cond) = &bp.cond {
            if !cond.eval(ctx) {
                return false;
            }
        }
        if bp.hits < bp.ignore {
            bp.hits = bp.hits.saturating_add(1);
            return false;
        }
        true
    }

    /// Add a word watch at `addr` (recording `current` as its baseline),
    /// or remove it when already set. `filter` limits which writer stops
    /// it and `pc` limits which instruction does (None = any). Returns
    /// true when now set.
    pub fn toggle_watch(
        &mut self,
        addr: u32,
        current: u16,
        filter: Option<WatchSource>,
        pc: Option<u32>,
    ) -> bool {
        let added = match self.watches.iter().position(|w| w.addr == addr) {
            Some(pos) => {
                self.watches.remove(pos);
                false
            }
            None => {
                self.watches.push(UiWatch {
                    addr,
                    last: current,
                    filter,
                    pc,
                });
                true
            }
        };
        self.rearm();
        added
    }

    /// Add a custom-register write watch (the offset is normalized into
    /// $000-$1FE, so both `$DFF096` and `96` address DMACON), or remove it
    /// when already set. Returns true when now set.
    pub fn toggle_reg_watch(&mut self, off: u16) -> bool {
        let off = off & 0x1FE;
        let added = match self.reg_watches.iter().position(|&o| o == off) {
            Some(pos) => {
                self.reg_watches.remove(pos);
                false
            }
            None => {
                self.reg_watches.push(off);
                true
            }
        };
        self.rearm();
        added
    }

    /// Add an exception catchpoint for `vector`, or remove it when
    /// already set. Returns true when now set.
    pub fn toggle_catch(&mut self, vector: u16) -> bool {
        let added = match self.catches.iter().position(|&v| v == vector) {
            Some(pos) => {
                self.catches.remove(pos);
                false
            }
            None => {
                self.catches.push(vector);
                true
            }
        };
        self.rearm();
        added
    }

    /// Set or clear the scheduled-task catch. Returns the previous value.
    pub fn set_task_catch(&mut self, target: Option<String>) -> Option<String> {
        let previous = std::mem::replace(&mut self.task_catch, target);
        self.rearm();
        previous
    }

    pub fn clear(&mut self) {
        self.breakpoints.clear();
        self.watches.clear();
        self.reg_watches.clear();
        self.catches.clear();
        self.task_catch = None;
        self.armed = false;
    }
}

/// A one-shot memory dump request (`COPPERLINE_DBG_RAMDUMP=ADDR:LEN:FILE`,
/// hex ADDR/LEN), written the first time the debugger is active.
#[derive(Clone)]
pub struct RamDumpReq {
    pub addr: u32,
    pub len: u32,
    pub path: String,
}

/// A one-shot Copper-list disassembly request (`COPPERLINE_DBG_COPPER`).
#[derive(Clone)]
pub struct CopperDumpReq {
    /// List start address. `None` means "use the live COP1LC pointer".
    pub addr: Option<u32>,
    /// Maximum number of Copper instructions to disassemble.
    pub count: u32,
}

pub struct Debugger {
    pub breakpoints: Vec<u32>,
    pub watches: Vec<Watch>,
    /// `(addr, words)` regions hexdumped (as 16-bit words) on each hit.
    pub dumps: Vec<(u32, u32)>,
    pub trace: bool,
    /// COPPERLINE_DBG_TRACE_FULL: emit every CPU register (D0-D7/A0-A7) plus the
    /// CCR flags on each traced instruction, for differential comparison against
    /// a reference 68000 (vAmiga). Implies `trace`.
    pub trace_full: bool,
    /// COPPERLINE_DBG_TRACE_LO/HI: when set, only trace instructions whose PC is
    /// in `[lo, hi]`. Keeps a focused routine (e.g. a depacker loop) out of the
    /// noise of the rest of the system. `lo`=0/`hi`=u32::MAX means no filter.
    pub trace_lo: u32,
    pub trace_hi: u32,
    /// COPPERLINE_DBG_CATCH: exception vector numbers to report when the CPU
    /// enters them. Decimal by default; `irq N` and `trap N` are accepted.
    pub catches: Vec<u16>,
    /// COPPERLINE_DBG_CATCHALERT: once ExecBase is valid, derive the exec
    /// Alert() jump-table entry and report when execution reaches it.
    pub catch_alert: bool,
    /// Resolved Alert() jump-table PC. None until ExecBase becomes valid.
    pub alert_break: Option<u32>,
    pub after_secs: f64,
    pub until_secs: f64,
    pub max_hits: u64,
    pub hits: u64,
    pub shot_prefix: Option<String>,
    pub shot_seq: u32,
    pub trace_lines: u64,
    /// One-shot Copper-list disassembly request, performed the first time
    /// the debugger is active.
    pub copper_dump: Option<CopperDumpReq>,
    pub copper_dumped: bool,
    /// One-shot memory-to-file dump request, performed the first time the
    /// debugger is active.
    pub ram_dump: Option<RamDumpReq>,
    pub ram_dumped: bool,
}

impl Debugger {
    /// Build a debugger from the `COPPERLINE_DBG_*` environment, or `None` when no
    /// breakpoint, watchpoint, or trace is configured.
    pub fn from_env() -> Option<Self> {
        let breakpoints = parse_addr_list("COPPERLINE_DBG_BREAK");
        let watches = parse_watch_list("COPPERLINE_DBG_WATCH");
        let trace_full = crate::envcfg::flag("COPPERLINE_DBG_TRACE_FULL");
        let trace = trace_full || crate::envcfg::flag("COPPERLINE_DBG_TRACE");
        let trace_lo = parse_hex_var("COPPERLINE_DBG_TRACE_LO").unwrap_or(0);
        let trace_hi = parse_hex_var("COPPERLINE_DBG_TRACE_HI").unwrap_or(u32::MAX);
        let catches = parse_exception_catches("COPPERLINE_DBG_CATCH");
        let catch_alert = crate::envcfg::flag("COPPERLINE_DBG_CATCHALERT");
        let copper_dump = parse_copper_dump("COPPERLINE_DBG_COPPER");
        let ram_dump = parse_ram_dump("COPPERLINE_DBG_RAMDUMP");
        if breakpoints.is_empty()
            && watches.is_empty()
            && !trace
            && catches.is_empty()
            && !catch_alert
            && copper_dump.is_none()
            && ram_dump.is_none()
        {
            return None;
        }
        let dumps = parse_watch_list("COPPERLINE_DBG_DUMP")
            .into_iter()
            .map(|w| (w.addr, w.len))
            .collect();
        let dbg = Self {
            breakpoints,
            watches,
            dumps,
            trace,
            trace_full,
            trace_lo,
            trace_hi,
            catches,
            catch_alert,
            alert_break: None,
            after_secs: parse_f64("COPPERLINE_DBG_AFTER").unwrap_or(0.0),
            until_secs: parse_f64("COPPERLINE_DBG_UNTIL").unwrap_or(f64::INFINITY),
            max_hits: parse_u64("COPPERLINE_DBG_MAXHITS").unwrap_or(200),
            hits: 0,
            shot_prefix: crate::envcfg::var("COPPERLINE_DBG_SHOT"),
            shot_seq: 0,
            trace_lines: 0,
            copper_dump,
            copper_dumped: false,
            ram_dump,
            ram_dumped: false,
        };
        log::info!(
            "debugger armed: breaks={:?} catches={:?} catch_alert={} watches={} dumps={} trace={} window=[{},{}) max_hits={}",
            dbg.breakpoints
                .iter()
                .map(|pc| format!("{pc:#X}"))
                .collect::<Vec<_>>(),
            dbg.catches,
            dbg.catch_alert,
            dbg.watches.len(),
            dbg.dumps.len(),
            dbg.trace,
            dbg.after_secs,
            dbg.until_secs,
            dbg.max_hits,
        );
        Some(dbg)
    }

    /// Whether the debugger should act at the given emulated time. False once
    /// the hit budget is exhausted, keeping long runs from flooding the log.
    pub fn enabled_at(&self, secs: f64) -> bool {
        self.hits < self.max_hits && secs >= self.after_secs && secs < self.until_secs
    }

    pub fn is_breakpoint(&self, pc: u32) -> bool {
        self.breakpoints.contains(&pc) || self.alert_break == Some(pc)
    }

    pub fn catches_vector(&self, vector: u16) -> bool {
        self.catches.contains(&vector)
    }

    /// The next screenshot path, advancing the sequence counter.
    pub fn next_shot_path(&mut self) -> Option<String> {
        let prefix = self.shot_prefix.clone()?;
        let path = format!("{prefix}-{:04}.png", self.shot_seq);
        self.shot_seq += 1;
        Some(path)
    }
}

fn parse_ram_dump(var: &str) -> Option<RamDumpReq> {
    let v = crate::envcfg::var(var)?;
    let mut parts = v.splitn(3, ':');
    let addr = parse_hex(parts.next()?)?;
    let len = parse_hex(parts.next()?)?;
    let path = parts.next()?.to_string();
    Some(RamDumpReq { addr, len, path })
}

fn parse_hex(s: &str) -> Option<u32> {
    let s = s.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u32::from_str_radix(s, 16).ok()
}

fn parse_addr_list(var: &str) -> Vec<u32> {
    crate::envcfg::var(var)
        .map(|v| v.split(',').filter_map(parse_hex).collect())
        .unwrap_or_default()
}

fn parse_hex_var(var: &str) -> Option<u32> {
    crate::envcfg::var(var).and_then(|v| parse_hex(v.trim()))
}

fn parse_exception_catches(var: &str) -> Vec<u16> {
    crate::envcfg::var(var)
        .map(|v| v.split(',').filter_map(parse_exception_catch).collect())
        .unwrap_or_default()
}

fn parse_exception_catch(item: &str) -> Option<u16> {
    let item = item.trim();
    if item.is_empty() {
        return None;
    }
    let lower = item.to_ascii_lowercase();
    let (kind, rest) = lower
        .split_once(char::is_whitespace)
        .map(|(kind, rest)| (kind, rest.trim()))
        .unwrap_or_else(|| {
            let split = lower
                .find(|c: char| c.is_ascii_digit())
                .unwrap_or(lower.len());
            lower.split_at(split)
        });
    match kind {
        "irq" => parse_u16_auto(rest)
            .map(|level| 24 + level)
            .filter(|v| (25..=31).contains(v)),
        "trap" => parse_u16_auto(rest)
            .map(|trap| 32 + trap)
            .filter(|v| (32..=47).contains(v)),
        "vec" | "vector" => parse_u16_auto(rest),
        "" => parse_u16_auto(rest),
        _ => parse_u16_auto(item),
    }
}

fn parse_u16_auto(s: &str) -> Option<u16> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u16>().ok()
    }
}

fn parse_watch_list(var: &str) -> Vec<Watch> {
    crate::envcfg::var(var)
        .map(|v| {
            v.split(',')
                .filter_map(|item| {
                    let mut parts = item.split(':');
                    let addr = parse_hex(parts.next()?)?;
                    let len = parts
                        .next()
                        .and_then(|s| s.trim().parse::<u32>().ok())
                        .unwrap_or(2)
                        .max(1);
                    Some(Watch { addr, len })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse `COPPERLINE_DBG_COPPER`. Accepted forms (LEN optional, default 256):
/// `auto`/`1`/`on` (use the live COP1LC), `ADDR`, `ADDR:LEN`, `auto:LEN`.
fn parse_copper_dump(var: &str) -> Option<CopperDumpReq> {
    let raw = crate::envcfg::var(var)?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let mut parts = raw.split(':');
    let addr_s = parts.next().unwrap_or("").trim();
    let count = parts
        .next()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(256)
        .max(1);
    let addr = match addr_s.to_ascii_lowercase().as_str() {
        "" | "auto" | "1" | "on" | "yes" | "true" => None,
        _ => Some(parse_hex(addr_s)?),
    };
    Some(CopperDumpReq { addr, count })
}

fn parse_f64(var: &str) -> Option<f64> {
    crate::envcfg::var(var).and_then(|s| s.trim().parse().ok())
}

fn parse_u64(var: &str) -> Option<u64> {
    crate::envcfg::var(var).and_then(|s| s.trim().parse().ok())
}

/// Default reverse-debug snapshot memory budget, in MiB.
pub const RR_DEFAULT_BUDGET_MB: usize = 512;
/// Default emulated-frame gap between reverse-debug snapshots.
pub const RR_DEFAULT_INTERVAL_FRAMES: u64 = 5;

/// Reverse-debugging configuration parsed from the `COPPERLINE_DBG_*`
/// environment, used to arm the emulator's snapshot ring and (optionally) a
/// one-shot "last writer" reverse watchpoint. See `docs/debugger`.
#[derive(Clone, Debug, PartialEq)]
pub struct ReverseConfig {
    /// Snapshot-ring memory budget, MiB (`COPPERLINE_DBG_RR_BUDGET_MB`).
    pub budget_mb: usize,
    /// Frames between snapshots (`COPPERLINE_DBG_RR_INTERVAL`).
    pub interval_frames: u64,
    /// Reverse-watchpoint address (`COPPERLINE_DBG_RWATCH=ADDR[:LEN]`); the
    /// last instruction to write this word before `target_secs` is reported.
    pub watch_addr: Option<u32>,
    /// Emulated time at which the reverse watchpoint is evaluated. Reuses
    /// `COPPERLINE_DBG_UNTIL`; `None` (unset) means evaluate at run end.
    pub target_secs: Option<f64>,
}

/// Parse the reverse-debug knobs. Returns `None` unless reverse mode is armed
/// (`COPPERLINE_DBG_RWATCH` set, or `COPPERLINE_DBG_RR=1` to enable the ring
/// for reverse-step navigation without a watchpoint).
pub fn reverse_config_from_env() -> Option<ReverseConfig> {
    let watch_addr = parse_watch_list("COPPERLINE_DBG_RWATCH")
        .first()
        .map(|w| w.addr);
    let ring_only = crate::envcfg::flag("COPPERLINE_DBG_RR");
    if watch_addr.is_none() && !ring_only {
        return None;
    }
    let target = parse_f64("COPPERLINE_DBG_UNTIL").filter(|s| s.is_finite());
    let config = ReverseConfig {
        budget_mb: parse_u64("COPPERLINE_DBG_RR_BUDGET_MB")
            .map(|v| v as usize)
            .unwrap_or(RR_DEFAULT_BUDGET_MB),
        interval_frames: parse_u64("COPPERLINE_DBG_RR_INTERVAL")
            .unwrap_or(RR_DEFAULT_INTERVAL_FRAMES),
        watch_addr,
        target_secs: target,
    };
    log::info!(
        "reverse debug armed: budget={}MB interval={} frames{}",
        config.budget_mb,
        config.interval_frames,
        match (config.watch_addr, config.target_secs) {
            (Some(a), Some(t)) => format!(", rwatch ${a:06X} at {t}s"),
            (Some(a), None) => format!(", rwatch ${a:06X} at run end"),
            _ => String::new(),
        }
    );
    Some(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_breakpoints_toggle_mask_and_arm() {
        let mut breaks = InteractiveBreaks::new(UI_ADDR_MASK);
        assert!(!breaks.armed());

        // Adding masks the address to the 68000 bus width.
        assert!(breaks.toggle_breakpoint_full(0xFFC0_33C2, None, 0));
        assert!(breaks.armed());
        assert!(breaks.is_breakpoint(0x00C0_33C2));

        // Toggling the same (masked) address removes it and disarms.
        assert!(!breaks.toggle_breakpoint_full(0x00C0_33C2, None, 0));
        assert!(!breaks.armed());
        assert!(!breaks.is_breakpoint(0x00C0_33C2));
    }

    #[test]
    fn full_mask_keeps_z3_breakpoints_distinct_from_chip_aliases() {
        // On a 32-bit CPU a Zorro III breakpoint must not fire at the
        // chip-RAM address it would alias through a 24-bit mask.
        let mut breaks = InteractiveBreaks::new(0xFFFF_FFFF);
        assert!(breaks.toggle_breakpoint_full(0x4000_1000, None, 0));
        assert!(breaks.is_breakpoint(0x4000_1000));
        assert!(!breaks.is_breakpoint(0x0000_1000));
    }

    /// Fixed register/memory snapshot for exercising condition evaluation.
    #[derive(Default)]
    struct FakeCtx {
        data: [u32; 8],
        addr: [u32; 8],
        pc: u32,
        sr: u32,
        mem: std::collections::HashMap<u32, u16>,
    }

    impl BreakContext for FakeCtx {
        fn data(&self, n: usize) -> u32 {
            self.data[n]
        }
        fn addr_reg(&self, n: usize) -> u32 {
            self.addr[n]
        }
        fn pc(&self) -> u32 {
            self.pc
        }
        fn sr(&self) -> u32 {
            self.sr
        }
        fn mem_word(&self, addr: u32) -> u16 {
            self.mem.get(&addr).copied().unwrap_or(0)
        }
    }

    #[test]
    fn conditional_breakpoint_stops_only_when_condition_holds() {
        let mut breaks = InteractiveBreaks::new(UI_ADDR_MASK);
        breaks.toggle_breakpoint_full(
            0x1000,
            Some(BreakCond {
                lhs: CondOperand::Data(0),
                op: CondOp::Eq,
                rhs: CondOperand::Imm(5),
            }),
            0,
        );
        let mut ctx = FakeCtx::default();

        // Address matches but the condition is false: no stop.
        ctx.data[0] = 4;
        assert!(!breaks.breakpoint_stops(0x1000, &ctx));
        // A different address never stops regardless of the condition.
        ctx.data[0] = 5;
        assert!(!breaks.breakpoint_stops(0x2000, &ctx));
        // Address matches and the condition holds: stop.
        assert!(breaks.breakpoint_stops(0x1000, &ctx));
    }

    #[test]
    fn ignore_count_skips_the_first_qualifying_hits() {
        let mut breaks = InteractiveBreaks::new(UI_ADDR_MASK);
        // Stop on the 4th qualifying hit (ignore the first 3).
        breaks.toggle_breakpoint_full(0x1000, None, 3);
        let ctx = FakeCtx::default();
        assert!(!breaks.breakpoint_stops(0x1000, &ctx));
        assert!(!breaks.breakpoint_stops(0x1000, &ctx));
        assert!(!breaks.breakpoint_stops(0x1000, &ctx));
        assert!(breaks.breakpoint_stops(0x1000, &ctx));
        // And it keeps stopping afterwards.
        assert!(breaks.breakpoint_stops(0x1000, &ctx));
    }

    #[test]
    fn bit_test_condition_uses_memory_word() {
        let mut breaks = InteractiveBreaks::new(UI_ADDR_MASK);
        breaks.toggle_breakpoint_full(
            0x40,
            Some(BreakCond {
                lhs: CondOperand::Mem(0xDFF002),
                op: CondOp::And,
                rhs: CondOperand::Imm(0x4000),
            }),
            0,
        );
        let mut ctx = FakeCtx::default();
        ctx.mem.insert(0xDFF002, 0x0000);
        assert!(!breaks.breakpoint_stops(0x40, &ctx));
        ctx.mem.insert(0xDFF002, 0x4000);
        assert!(breaks.breakpoint_stops(0x40, &ctx));
    }

    #[test]
    fn interactive_watches_record_baselines_and_clear() {
        let mut breaks = InteractiveBreaks::new(UI_ADDR_MASK);
        assert!(breaks.toggle_watch(0x1000, 0xABCD, None, None));
        assert_eq!(breaks.watches[0].last, 0xABCD);
        // The register watch normalizes a full $DFFxxx address to the
        // word offset.
        assert!(breaks.toggle_reg_watch(0xF096 & 0x1FE));
        assert_eq!(breaks.reg_watches, [0x096]);
        assert!(breaks.armed());

        // Toggling off, then clearing a re-added set, disarms.
        assert!(!breaks.toggle_reg_watch(0x097));
        assert!(breaks.reg_watches.is_empty());
        assert!(breaks.armed()); // memory watch still set
        breaks.toggle_breakpoint_full(0x100, None, 0);
        breaks.clear();
        assert!(!breaks.armed());
        assert!(breaks.breakpoints.is_empty());
        assert!(breaks.watches.is_empty());
        assert!(breaks.reg_watches.is_empty());
    }

    #[test]
    fn guru_decode_names_subsystems_causes_and_cpu_traps() {
        assert_eq!(
            guru_decode(0x8000_0003),
            "DEADEND CPU exception: Address error"
        );
        assert_eq!(
            guru_decode(0x0000_0004),
            "CPU exception: Illegal instruction"
        );
        let text = guru_decode(0x8100_0005);
        assert!(text.starts_with("DEADEND exec.library"), "{text}");
        let text = guru_decode(0x0701_0002);
        assert!(
            text.contains("dos.library") && text.contains("no memory"),
            "{text}"
        );
        assert!(text.starts_with("recoverable"), "{text}");
    }

    #[test]
    fn headless_exception_catch_parser_accepts_vector_irq_and_trap_forms() {
        assert_eq!(parse_exception_catch("3"), Some(3));
        assert_eq!(parse_exception_catch("0x0b"), Some(11));
        assert_eq!(parse_exception_catch("vec 4"), Some(4));
        assert_eq!(parse_exception_catch("vector 10"), Some(10));
        assert_eq!(parse_exception_catch("irq 3"), Some(27));
        assert_eq!(parse_exception_catch("irq3"), Some(27));
        assert_eq!(parse_exception_catch("trap 0"), Some(32));
        assert_eq!(parse_exception_catch("trap15"), Some(47));
        assert_eq!(parse_exception_catch("irq 0"), None);
        assert_eq!(parse_exception_catch("irq 8"), None);
        assert_eq!(parse_exception_catch("trap 16"), None);
        assert_eq!(parse_exception_catch("not-a-vector"), None);
    }

    #[test]
    fn custom_reg_bit_decode_names_set_bits_and_fields() {
        let lines = custom_reg_bit_decode(0x096, 0x0240);
        assert_eq!(lines, vec!["DMAEN BLTEN".to_string()]);
        let lines = custom_reg_bit_decode(0x100, 0x5800);
        assert_eq!(lines[0], "HAM");
        assert_eq!(lines[1], "BPU=5");
        let lines = custom_reg_bit_decode(0x102, 0x0021);
        assert_eq!(lines, vec!["PF1H=1 PF2H=2".to_string()]);
        // Unknown registers decode to nothing (hex is always shown).
        assert!(custom_reg_bit_decode(0x1F0, 0xFFFF).is_empty());
    }

    #[test]
    fn watch_source_parses_engine_names_and_dma_channels() {
        assert_eq!(WatchSource::parse("cpu"), Some(WatchSource::Cpu));
        assert_eq!(WatchSource::parse("COPPER"), Some(WatchSource::Copper));
        // BPL channels are named from 1 on the hardware and from 0 in the
        // register file; the console speaks the hardware's names.
        assert_eq!(WatchSource::parse("bpl1"), Some(WatchSource::Bitplane(0)));
        assert_eq!(WatchSource::parse("BPL8"), Some(WatchSource::Bitplane(7)));
        assert_eq!(WatchSource::parse("spr0"), Some(WatchSource::Sprite(0)));
        assert_eq!(WatchSource::parse("aud3"), Some(WatchSource::Audio(3)));
        assert_eq!(WatchSource::parse("bpl0"), None);
        assert_eq!(WatchSource::parse("bpl9"), None);
        assert_eq!(WatchSource::parse("spr8"), None);
        // Paula has four audio channels, not eight: a filter naming a
        // channel that does not exist could never match.
        assert_eq!(WatchSource::parse("aud3"), Some(WatchSource::Audio(3)));
        assert_eq!(WatchSource::parse("aud4"), None);
        assert_eq!(WatchSource::parse("nonsense"), None);
    }

    #[test]
    fn only_cpu_accesses_carry_an_instruction_to_qualify_on() {
        assert!(WatchSource::Cpu.takes_pc_qualifier());
        for source in [
            WatchSource::Blitter,
            WatchSource::Disk,
            WatchSource::Copper,
            WatchSource::Bitplane(0),
            WatchSource::Sprite(3),
            WatchSource::Audio(1),
        ] {
            assert!(
                !source.takes_pc_qualifier(),
                "{source:?} has no PC behind its accesses"
            );
        }
    }

    #[test]
    fn a_channel_filter_accepts_only_its_own_channel() {
        assert!(WatchSource::Sprite(3).accepts(WatchSource::Sprite(3)));
        assert!(!WatchSource::Sprite(3).accepts(WatchSource::Sprite(4)));
        assert!(!WatchSource::Sprite(3).accepts(WatchSource::Bitplane(3)));
        assert!(WatchSource::Cpu.accepts(WatchSource::Cpu));
        assert!(!WatchSource::Cpu.accepts(WatchSource::Blitter));
    }

    #[test]
    fn a_dma_read_stop_says_read_rather_than_inventing_a_change() {
        // A read leaves the word alone, so old == new is the tell.
        let stop = DebugStop::Watch {
            addr: 0x21000,
            old: 0xBEEF,
            new: 0xBEEF,
            writer_pc: 0xF80010,
            source: WatchSource::Bitplane(2),
            vpos: 100,
            hpos: 40,
        };
        assert_eq!(
            stop.describe(),
            "Watch $021000: BEEF read by bpl3 (v100 h40)"
        );
    }

    #[test]
    fn debug_stop_describes_each_reason() {
        assert_eq!(
            DebugStop::Breakpoint { pc: 0xC033C2 }.describe(),
            "Breakpoint at $C033C2"
        );
        assert_eq!(
            DebugStop::Watch {
                addr: 0xC09580,
                old: 0x12,
                new: 0x13,
                writer_pc: 0xC03374,
                source: WatchSource::Cpu,
                vpos: 44,
                hpos: 100,
            }
            .describe(),
            "Watch $C09580: 0012->0013 (pc $C03374)"
        );
        assert_eq!(
            DebugStop::Watch {
                addr: 0xC09580,
                old: 0x12,
                new: 0x13,
                writer_pc: 0xC03374,
                source: WatchSource::Blitter,
                vpos: 44,
                hpos: 100,
            }
            .describe(),
            "Watch $C09580: 0012->0013 (blitter write, v44 h100)"
        );
        assert_eq!(
            DebugStop::ChipReg {
                off: 0x096,
                value: 0x8020,
                source: "copper",
                vpos: 44,
                hpos: 120,
            }
            .describe(),
            "DMACON = 8020 (copper write, v44 h120)"
        );
    }

    #[test]
    fn custom_reg_names_cover_fixed_and_banked_registers() {
        assert_eq!(custom_reg_name(0x096), "DMACON");
        assert_eq!(custom_reg_name(0x097), "DMACON"); // odd byte -> word
        assert_eq!(custom_reg_name(0x180), "COLOR00");
        assert_eq!(custom_reg_name(0x1BE), "COLOR31");
        assert_eq!(custom_reg_name(0x0A4), "AUD0LEN");
        assert_eq!(custom_reg_name(0x0DA), "AUD3DAT");
        assert_eq!(custom_reg_name(0x0E0), "BPL1PTH");
        assert_eq!(custom_reg_name(0x0FE), "BPL8PTL");
        assert_eq!(custom_reg_name(0x110), "BPL1DAT");
        assert_eq!(custom_reg_name(0x120), "SPR0PTH");
        assert_eq!(custom_reg_name(0x146), "SPR0DATB");
        assert_eq!(custom_reg_name(0x178), "SPR7POS");
        assert_eq!(custom_reg_name(0x1FC), "FMODE");
        // Unassigned offsets fall back to hex.
        assert_eq!(custom_reg_name(0x068), "$068");
        assert_eq!(custom_reg_name(0x0AC), "$0AC");
    }
}
