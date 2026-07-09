// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared Amiga bus state used by the CPU core, chipset, and host I/O.
//! It owns chip RAM, ROM, CIA state, and custom-chip state, and exposes
//! typed read/write methods for memory-mapped devices.

use crate::chipset::agnus::{
    sprite_dma_disabled_by_bitplane_ddf, Agnus, AgnusRevision, AgnusTick, VideoStandard,
    BEAMCON0_DUAL, BEAMCON0_HARDDIS, COLORCLOCKS_PER_LINE, NTSC_LINES,
    NTSC_LONG_COLORCLOCKS_PER_LINE, PAL_LINES,
};
use crate::chipset::blitter::Blitter;
use crate::chipset::cia::{
    reg_from_addr, Cia, CiaSideEffect, Which, REG_DDRA, REG_DDRB, REG_PRA, REG_PRB, REG_TODLO,
};
use crate::chipset::copper::{Copper, CopperSlotAction, CopperWait, DMACON_COPEN};
use crate::chipset::denise::{
    color_register_value, BitplaneMode, Denise, DeniseRevision, DiwHigh, Palette,
};
use crate::chipset::keyboard::KeyboardMcu;
use crate::chipset::paula::{
    Paula, PotPins, DMACON_DMAEN, INT_BLIT, INT_COPER, INT_DSKBLK, INT_DSKSYNC, INT_EXTER,
    INT_PORTS, INT_VERTB, PAULA_CLOCK_HZ,
};
use crate::floppy::FloppyController;
use crate::gayle::Gayle;
use crate::memory::Memory;
use crate::rtc::Msm6242Rtc;
use crate::video::{beam::BeamEventIndex, FrameGeometry, FB_HEIGHT, FB_WIDTH, MAX_VISIBLE_LINES};
use log::trace;
use std::io::Write;
use std::time::{Duration, Instant};

const CHIP_BUS_SLOT_CCK: u32 = 1;
const BLITTER_DEADLINE_SLOT_SCAN_LIMIT: u32 = 64;

// Number of consecutive CPU bus-miss color clocks a busy "nice" (BLTPRI=0)
// blitter holds the chip bus before yielding one slot to the waiting CPU.
//
// Grounded in the Minimig RTL (agnus.v): its `bls_cnt` increments on the !cck
// phase of EACH color clock (clk7 has two ticks per color clock, cck and !cck),
// i.e. once per color clock, up to BLS_CNT_MAX=3. So the blitter is blocked
// after the CPU has missed 3 color clocks -- which also matches the HRM rule
// that a waiting 68000 gets one bus cycle in four when BLTPRI=0.
//
// History: this was 2, which over-starved the blitter, then 6 after a misread
// of the RTL doubled the threshold by treating !cck as every-other color clock.
// That over-starved the CPU instead. Cross-emulator DMA accounting for a
// blitter-heavy frame (blitter 34892 cck, CPU 17882 cck) confirms 3.
pub(crate) const BLITTER_SLOWDOWN_CPU_MISS_LIMIT: u8 = 3;

#[cfg(feature = "internal-diagnostics")]
fn exp_miss_limit() -> u8 {
    use std::sync::OnceLock;
    static V: OnceLock<u8> = OnceLock::new();
    *V.get_or_init(|| {
        crate::envcfg::var("COPPERLINE_EXP_MISS_LIMIT")
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(BLITTER_SLOWDOWN_CPU_MISS_LIMIT)
    })
}

#[cfg(not(feature = "internal-diagnostics"))]
fn exp_miss_limit() -> u8 {
    BLITTER_SLOWDOWN_CPU_MISS_LIMIT
}

/// Cached COPPERLINE_DBG_CIA gate (read once). Consulted on the per-device-tick
/// path, so it must not do a live env lookup.
fn dbg_cia_on() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| crate::envcfg::flag("COPPERLINE_DBG_CIA"))
}

#[cfg(feature = "internal-diagnostics")]
fn no_bus_arb() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| crate::envcfg::flag("COPPERLINE_NO_BUS_ARB"))
}

#[cfg(not(feature = "internal-diagnostics"))]
fn no_bus_arb() -> bool {
    false
}

/// Cached COPPERLINE_DIAG_BLT_SLOTS gate (read once). Enables the blitter
/// slot-trace probe: one stderr line per blitter pipeline cycle with its
/// beam position, for cross-emulator slot-sequence comparison against the
/// vAmiga VAMIGA_BLT_PROBE hooks. Logging only; never alters timing.
pub(crate) fn diag_blt_slots() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| crate::envcfg::flag("COPPERLINE_DIAG_BLT_SLOTS"))
}

/// One-shot latch for the COPPERLINE_DIAG_COPLEN coplist dump (it used to clear its
/// own env var to log once; with cached env that no longer works).
static COPLEN_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "internal-diagnostics")]
fn no_disk_stall() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| crate::envcfg::flag("COPPERLINE_NO_DISK_STALL"))
}

#[cfg(not(feature = "internal-diagnostics"))]
fn no_disk_stall() -> bool {
    false
}

fn external_access_cck_x100_setting() -> u32 {
    #[cfg(feature = "internal-diagnostics")]
    {
        crate::envcfg::var("COPPERLINE_DBG_EXTCCK")
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(200)
    }
    #[cfg(not(feature = "internal-diagnostics"))]
    {
        200
    }
}

/// Paula INTREQ/INTENA -> CPU IPL-pin propagation delay in color clocks
/// (DEFAULT ON). A change to the enabled-pending interrupt set does not reach
/// the 68000's IPL pins combinationally: Paula pipelines the encoded level to
/// the pins over a few chip clocks (vAmiga models the same pipe as iplPipe,
/// with the pin taking the new value ~4 DMA cycles after the level change).
/// Together with the IPL sampling model in cpu.rs (the interrupt decision at
/// an instruction boundary uses the level sampled at the PREVIOUS instruction's
/// last bus access, as on the real 68000), this reproduces the raise-to-entry
/// latency measured against vAmiga and the vAmigaTS real-hardware photos.
///
/// History: this was 65 for a while, calibrated against timing-test row 19
/// with a mis-decoded VHPOSR (the low byte is the cck position, not cck/2);
/// that made every IRQ delivery ~50 cck late and dominated the vAmigaTS
/// cputim/irqtim/inttim divergence. Set COPPERLINE_IRQ_LATENCY_CCK to override
/// (0 disables the pipe AND the boundary-sampling delay = raw model).
///
/// 5 = the ~4-cycle pin pipe plus the one-cck register-change commit a poked
/// INTREQ takes before the level encoder sees it (folded into one constant;
/// an IRQ-latency probe against vAmiga across seven source/loop geometries
/// lands within 0..+7 cck, the residual being per-instruction IPL poll-point
/// detail the vendored core does not model).
const DEFAULT_IRQ_LATENCY_CCK: u32 = 5;

/// Read the COPPERLINE_IRQ_LATENCY_CCK setting once, at bus construction (stored in
/// `irq_latency_setting`). Unset uses DEFAULT_IRQ_LATENCY_CCK; 0 disables.
fn irq_latency_setting_from_env() -> u32 {
    crate::envcfg::var("COPPERLINE_IRQ_LATENCY_CCK")
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_IRQ_LATENCY_CCK)
}

/// Interrupt source bits (0..13); excludes INT_MASTER (bit 14) and the
/// set/clear bit (15). Used to detect a newly-raised interrupt.
const IRQ_SOURCE_BITS: u16 = 0x3FFF;

/// Cached COPPERLINE_DIAG_VBI gate (read once): logs the beam position when the
/// VERTB request is asserted. Checked per device tick, so it must not do a live
/// env lookup on that path.
fn diag_vbi() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| crate::envcfg::flag("COPPERLINE_DIAG_VBI"))
}

#[derive(Clone, Copy)]
struct CaprowDiag {
    first_vpos: u32,
    last_vpos: u32,
}

impl CaprowDiag {
    fn contains(self, vpos: u32) -> bool {
        (self.first_vpos..=self.last_vpos).contains(&vpos)
    }
}

/// Cached COPPERLINE_DIAG_CAPROW setting (read once). Accepted forms:
/// presence/`all` logs every captured row, `V` logs one beam line, and
/// `START:END` logs an inclusive beam-line range. Checked on every bitplane
/// DMA-word capture call (per beam advance), so it must not do a map lookup.
fn diag_caprow() -> Option<CaprowDiag> {
    use std::sync::OnceLock;
    static V: OnceLock<Option<CaprowDiag>> = OnceLock::new();
    *V.get_or_init(|| {
        let raw = crate::envcfg::var("COPPERLINE_DIAG_CAPROW")?;
        let raw = raw.trim();
        if raw.is_empty() || raw == "1" || raw.eq_ignore_ascii_case("all") {
            return Some(CaprowDiag {
                first_vpos: 0,
                last_vpos: u32::MAX,
            });
        }
        if let Some((first, last)) = raw.split_once(':') {
            let first_vpos = crate::envcfg::parse_u32(first).unwrap_or(0);
            let last_vpos = crate::envcfg::parse_u32(last).unwrap_or(u32::MAX);
            return Some(CaprowDiag {
                first_vpos: first_vpos.min(last_vpos),
                last_vpos: first_vpos.max(last_vpos),
            });
        }
        crate::envcfg::parse_u32(raw).map(|vpos| CaprowDiag {
            first_vpos: vpos,
            last_vpos: vpos,
        })
    })
}

/// Cached COPPERLINE_DIAG_SPRCAP setting (read once). Checked per captured
/// sprite line on the beam-advance path, so it must not do a map lookup.
fn diag_sprcap() -> Option<&'static str> {
    use std::sync::OnceLock;
    static V: OnceLock<Option<String>> = OnceLock::new();
    V.get_or_init(|| crate::envcfg::var("COPPERLINE_DIAG_SPRCAP"))
        .as_deref()
}

fn diag_sprcap_matches(want: &str, beam_y: i32) -> bool {
    let want = want.trim();
    want == "all" || want.parse::<i32>().ok() == Some(beam_y)
}
const CPU_COPPER_BOTTOM_PALETTE_MIN_VPOS: u32 = 0xC0;
const DMACON_DSKEN: u16 = 1 << 4;
const DMACON_SPREN: u16 = 1 << 5;
const DMACON_BLTEN: u16 = 1 << 6;
const DMACON_BPLEN: u16 = 1 << 8;
const DMACON_BLTPRI: u16 = 1 << 10;
const BLTCON0_USE_C: u16 = 1 << 9;
const BLTCON0_USE_D: u16 = 1 << 8;
const BLTCON1_LINE: u16 = 1 << 0;
const BLTCON1_DOFF: u16 = 1 << 7;
// The Copper cannot take (or hold) a bus slot on the last-but-two color
// clock of the line: $E0 on short lines, $E1 on NTSC long lines (vAmiga
// busIsFree<COPPER>, keyed on the long-line flag). A fetch chain that hits
// the lockout resumes past the line wrap, which is what a line-end SKIP's
// deferred decision and the cycleE0 vAmigaTS case observe.
const COPPER_BUS_LOCKOUT_HPOS_SHORT_LINE: u32 = 0x00E0;
const COPPER_BUS_LOCKOUT_HPOS_LONG_LINE: u32 = 0x00E1;
const COPER_CPU_IRQ_DELAY_CCK: u32 = 2;
const RENDER_VISIBLE_START_VPOS: u32 = 0x2C;
const RENDER_MIN_OVERSCAN_START_VPOS: u32 = 0x1C;
const PAL_SPRITE_DMA_FIRST_ACTIVE_VPOS: u32 = 0x19;
const NTSC_SPRITE_DMA_FIRST_ACTIVE_VPOS: u32 = 0x14;
const RENDER_VISIBLE_LINES: usize = FB_HEIGHT;
const RENDER_FRAMEBUFFER_WIDTH: i32 = FB_WIDTH as i32;
// Capture-side twin of `bitplane::DIW_HSTART_FB0`; held deep left of the
// standard display start so the captured window matches vAmiga's 716-wide
// cutout and includes the deep-left overscan. Like the renderer twin, this
// puts the hardware-verified standard $81 window edge (and the sprite
// comparator positions, which share Denise's counter) at framebuffer x = 62.
const RENDER_DIW_HSTART_FB0: i32 = 0x62;
// Standard DIWSTRT $81 is the visible window edge. The first standard
// bitplane sample at DDFSTRT $38 is already one lowres native sample into the
// fetched word, so the fetch/output phase is referenced one color clock earlier.
//
// Capture-side twins of `bitplane::DIW_HSTART_FETCH_REFERENCE_*`. The hi-res
// fetch/display phase sits 3 colour clocks earlier than lo-res, so the
// reference differs by resolution (lo-res $81, hi-res $84). Moved +1 in
// lockstep with RENDER_DIW_HSTART_FB0 so captured bitmap positions stay at
// their hardware-calibrated framebuffer columns. See the bitplane constant
// docs for the vAmiga-verified rationale.
const RENDER_DIW_HSTART_FETCH_REFERENCE_LORES: i32 = 0x81;
const RENDER_DIW_HSTART_FETCH_REFERENCE_HIRES: i32 = 0x84;
// Capture-side twin of `bitplane::COPPER_WAIT_HPOS_FB0`; moved left by 8 colour
// clocks in lockstep with RENDER_DIW_HSTART_FB0.
const RENDER_COPPER_WAIT_HPOS_FB0: u32 = 0x28;
// Agnus DMA scheduling runs four color clocks ahead of Denise's pixel counter.
const DENISE_HPOS_LAG_CCK: u32 = 4;
// Denise applies a register write to its pixel pipeline about four colour
// clocks after the chip-bus slot that carried it, regardless of whether the
// Copper or the CPU drove the bus (see `record_render_write`).
const DENISE_WRITE_EFFECT_DELAY_CCK: u32 = 4;
// Agnus applies its two-cycle register class (DMACON, BPLxPT, BPLxMOD,
// SPRxPT; vAmiga `DMA_CYCLES(2)` in `recordRegisterChange`) two colour
// clocks after the chip-bus slot that carried the write.
const AGNUS_WRITE_EFFECT_DELAY_CCK: u32 = 2;
const BPLCON0_ECSENA: u16 = 1 << 0;
const BPLCON0_SHRES: u16 = 1 << 6;
const BPLCON3_BRDSPRT: u16 = 1 << 1;
const BPLCON3_BRDRBLNK: u16 = 1 << 5;
const BPLCON3_SPRES_MASK: u16 = 0x00C0;
const BPLCON3_SPRES_LORES: u16 = 0x0040;
const BPLCON3_SPRES_HIRES: u16 = 0x0080;
const BPLCON3_SPRES_SHRES: u16 = 0x00C0;
const CLXDAT_SPRITE_PLAYFIELD_MASK: u16 = 0x01FE;
const CLXDAT_SPRITE_SPRITE_MASK: u16 = 0x7E00;
const BITPLANE_DDF_HARD_START: u16 = 0x0018;
const BITPLANE_DDF_HARD_STOP: u16 = 0x00D8;
/// First DMA slot colour clock of each sprite channel (the POS/DATA word;
/// the CTL/DATB word follows two clocks later). Hardware slot chart (and
/// vAmiga's DAS table): sprite N fetches at $15+4N / $17+4N.
const SPRITE_DMA_SLOT1_HPOS: [u32; 8] = [0x15, 0x19, 0x1D, 0x21, 0x25, 0x29, 0x2D, 0x31];
const NANOS_PER_SECOND: u128 = 1_000_000_000;
const VIDEO_FETCH_TIMING_SAMPLE_RATE: u128 = 128;
const VIDEO_COLLISION_TIMING_SAMPLE_RATE: u128 = 16;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CapturedBitplaneRow {
    pub nplanes: usize,
    pub words_per_row: usize,
    pub planes: [Vec<u16>; 8],
    /// Colour clock of the row's first fetch-unit boundary when the DDF
    /// sequencer's run diverges from the register-derived window (missed
    /// stops draining to the hardware stop, late starts). None when the
    /// register-derived geometry already matches (and for wide-FMODE rows).
    pub fetch_origin_cck: Option<u16>,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct CapturedSpriteLine {
    pub sprite: usize,
    pub hstart: i32,
    pub hsub_70ns: bool,
    pub beam_y: i32,
    /// First (or only) data/mask word pair; AGA wide fetches carry the
    /// remaining words in the `_ext` arrays.
    pub data: u16,
    pub datb: u16,
    pub data_ext: [u16; 3],
    pub datb_ext: [u16; 3],
    /// Words per channel per line: 1 (16 px), 2 (32 px), or 4 (64 px),
    /// from FMODE SPR32/SPAGEM.
    pub width_words: u8,
    pub attached: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct HeldSpriteLine {
    pub line: CapturedSpriteLine,
    pub vstart: i32,
    pub vstop: i32,
}

/// Agnus's per-channel sprite DMA state, modelled at the register level the
/// way the chip works: POS/CTL control words are fetched on the channel's
/// vstop line through its two DMA slots, the vertical comparators run off
/// these register copies at each line start (and immediately on CPU/Copper
/// pokes), the DMA flip-flop is cleared on the vstop line even when SPREN
/// is off, and SPRxPT advances only on words actually fetched.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
struct DisplaySpriteDmaState {
    /// Channel copies of the SPRxPOS/SPRxCTL register words (from DMA
    /// control-word fetches or CPU/Copper writes); the horizontal position
    /// and attach decode for emitted lines comes from these.
    pos: u16,
    ctl: u16,
    /// Vertical comparator values. Mostly derived from pos/ctl, but held
    /// separately because the vertical-blank reset writes vstop without
    /// touching the CTL word.
    vstrt: i32,
    vstop: i32,
    /// The per-sprite DMA flip-flop: set when the line counter passes
    /// vstrt, cleared when it passes vstop (even with SPREN off) and in
    /// the frame's last line.
    dma_enabled: bool,
    /// Line whose start-of-line comparator update has already run (the
    /// update is applied lazily, catching up over skipped lines).
    #[serde(default = "unset_sprite_line_marker")]
    comparator_vpos: i32,
    /// Display latches from the last assembled line: a skipped slot reuses
    /// the stale side, and with DMA off an armed channel keeps displaying
    /// this line until a CTL fetch or poke disarms it.
    last_line: Option<DisplaySpriteLineData>,
    /// Display line of the last DATA fetch: with SSCAN2 the hardware
    /// fetches on every other line and redisplays in between.
    last_data_fetch_vpos: i32,
    /// DATA word(s) fetched by the sprite's first DMA slot, sampled at that
    /// slot's beam time, awaiting the second slot to assemble the line.
    pending_data: Option<(u16, [u16; 3])>,
    /// Line the first slot's DATA fetch ran for.
    #[serde(default = "unset_sprite_line_marker")]
    pending_line_vpos: i32,
    /// Line the first slot's evaluation ran for, so the second slot
    /// completes the line rather than re-running the entry logic.
    #[serde(default = "unset_sprite_line_marker")]
    entry_line_vpos: i32,
}

fn unset_sprite_line_marker() -> i32 {
    i32::MIN
}

impl Default for DisplaySpriteDmaState {
    fn default() -> Self {
        // The line-marker fields must start unset, not at line 0: a derived
        // zero default would make a fresh state claim its per-line work
        // already ran when the beam is on line 0.
        Self {
            pos: 0,
            ctl: 0,
            vstrt: 0,
            vstop: 0,
            dma_enabled: false,
            comparator_vpos: unset_sprite_line_marker(),
            last_line: None,
            last_data_fetch_vpos: unset_sprite_line_marker(),
            pending_data: None,
            pending_line_vpos: unset_sprite_line_marker(),
            entry_line_vpos: unset_sprite_line_marker(),
        }
    }
}

impl DisplaySpriteDmaState {
    /// A POS word (fetch or poke) replaces the vertical start's low bits,
    /// keeping the CTL-supplied high bit.
    fn poke_pos(&mut self, value: u16) {
        self.pos = value;
        self.vstrt = (self.vstrt & 0x100) | i32::from(value >> 8);
    }

    /// A CTL word (fetch or poke) supplies vstart's high bit and the whole
    /// vstop, and disarms the channel's display latches.
    fn poke_ctl(&mut self, value: u16) {
        self.ctl = value;
        self.vstrt = (self.vstrt & 0x0FF) | (i32::from(value & 0x0004) << 6);
        self.vstop = (i32::from(value & 0x0002) << 7) | i32::from(value >> 8);
        self.last_line = None;
    }

    /// The comparators also fire immediately when a poke lands on the
    /// matching line (vstop wins over vstrt when both match).
    fn reevaluate_comparators_at(&mut self, beam_y: i32) {
        if self.vstrt == beam_y {
            self.dma_enabled = true;
        }
        if self.vstop == beam_y {
            self.dma_enabled = false;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpriteControlRegisterWrite {
    Pos,
    Ctl,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
struct BitplaneDmaconDelay {
    previous: u16,
    changed_at_cck: u64,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
struct BitplaneBplcon0Delay {
    previous: u16,
    changed_at_cck: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct BitplaneDdfStartMiss {
    vpos: u32,
    ddfstart: u32,
}

/// Cache key for the memoized bitplane slot plan: every register input that
/// feeds the Agnus fetch-ownership computation. The vpos-dependent gates
/// (vertical display window, DDFSTRT write miss) are evaluated live in
/// `bitplane_slot_active_at`, so DIW registers do not belong here. Likewise,
/// only BPLCON0's plane-count and resolution bits belong here: HAM, dual
/// playfield, lace, color, and genlock affect Denise interpretation of fetched
/// words, but they do not change which chip-bus slots Agnus reserves.
#[derive(Clone, Copy, PartialEq, Eq)]
struct BitplaneSlotKey {
    bplen: bool,
    bplcon0: u16,
    ddfstrt: u16,
    ddfstop: u16,
    fmode: u16,
    harddis: bool,
}

const BPLCON0_SLOT_PLAN_MASK_OCS_ECS: u16 = 0xF040;
const BPLCON0_SLOT_PLAN_MASK_AGA: u16 = BPLCON0_SLOT_PLAN_MASK_OCS_ECS | 0x0010;

fn bitplane_slot_plan_bplcon0_key(bplcon0: u16, aga: bool) -> u16 {
    let mask = if aga {
        BPLCON0_SLOT_PLAN_MASK_AGA
    } else {
        BPLCON0_SLOT_PLAN_MASK_OCS_ECS
    };
    bplcon0 & mask
}

/// Derived bitplane fetch cadence for the current `BitplaneSlotKey`: the
/// line-invariant parts of `bitplane_slot_active_at`.
#[derive(Clone, Copy)]
struct BitplaneSlotPlan {
    start: u32,
    last_fetch_hpos: u32,
    period: u32,
    unit: u32,
    quantum: u32,
    words_per_row: u32,
    hires_like: bool,
    /// Bit n set when a DMA-enabled plane fetches at within-unit order n
    /// (lores cadence only).
    order_mask: u8,
    /// Precomputed per-hpos slot pattern: bit `h` is set when `plan_slot_at`
    /// is true for `hpos == h`. The pattern is purely a function of the fields
    /// above (vpos-independent), so it is memoized once with the plan and the
    /// per-color-clock arbiter does a bit test instead of the div/mod math.
    /// Covers hpos 0..256, which spans every standard/long line; the rare
    /// programmable line with a slot at hpos >= 256 falls back to `plan_slot_at`.
    slot_mask: [u64; 4],
}

/// Width covered by `BitplaneSlotPlan::slot_mask` (hpos 0..SLOT_MASK_BITS).
const SLOT_MASK_BITS: u32 = 256;
const BITPLANE_SLOT_PLAN_CACHE_LEN: usize = 8;

type BitplaneSlotPlanCacheEntry = Option<(BitplaneSlotKey, Option<BitplaneSlotPlan>)>;

struct BitplaneSlotPlanCache {
    entries: [std::cell::Cell<BitplaneSlotPlanCacheEntry>; BITPLANE_SLOT_PLAN_CACHE_LEN],
    next_insert: std::cell::Cell<usize>,
    last_hit: std::cell::Cell<usize>,
}

impl BitplaneSlotPlanCache {
    fn new() -> Self {
        Self {
            entries: std::array::from_fn(|_| std::cell::Cell::new(None)),
            next_insert: std::cell::Cell::new(0),
            last_hit: std::cell::Cell::new(0),
        }
    }

    fn lookup(&self, key: BitplaneSlotKey) -> Option<Option<BitplaneSlotPlan>> {
        let last = self.last_hit.get().min(BITPLANE_SLOT_PLAN_CACHE_LEN - 1);
        if let Some((cached_key, plan)) = self.entries[last].get() {
            if cached_key == key {
                return Some(plan);
            }
        }

        for idx in 0..BITPLANE_SLOT_PLAN_CACHE_LEN {
            if idx == last {
                continue;
            }
            if let Some((cached_key, plan)) = self.entries[idx].get() {
                if cached_key == key {
                    self.last_hit.set(idx);
                    return Some(plan);
                }
            }
        }
        None
    }

    fn insert(&self, key: BitplaneSlotKey, plan: Option<BitplaneSlotPlan>) {
        let idx = self.next_insert.get() % BITPLANE_SLOT_PLAN_CACHE_LEN;
        self.entries[idx].set(Some((key, plan)));
        self.last_hit.set(idx);
        self.next_insert
            .set((idx + 1) % BITPLANE_SLOT_PLAN_CACHE_LEN);
    }

    #[cfg(test)]
    fn entries_snapshot(&self) -> [BitplaneSlotPlanCacheEntry; BITPLANE_SLOT_PLAN_CACHE_LEN] {
        std::array::from_fn(|idx| self.entries[idx].get())
    }

    #[cfg(test)]
    fn last_hit_entry(&self) -> BitplaneSlotPlanCacheEntry {
        self.entries[self.last_hit.get()].get()
    }
}

impl Default for BitplaneSlotPlanCache {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
struct DisplaySpriteLineData {
    hstart: i32,
    hsub_70ns: bool,
    data: u16,
    datb: u16,
    data_ext: [u16; 3],
    datb_ext: [u16; 3],
    width_words: u8,
    attached: bool,
}

fn empty_captured_bitplane_rows() -> Vec<Option<CapturedBitplaneRow>> {
    (0..MAX_VISIBLE_LINES).map(|_| None).collect()
}

fn empty_sprite_collision_sources() -> Vec<Option<Vec<LiveSpriteCollisionSource>>> {
    (0..MAX_VISIBLE_LINES).map(|_| None).collect()
}

fn empty_captured_sprite_lines_by_y() -> Vec<Vec<CapturedSpriteLine>> {
    (0..MAX_VISIBLE_LINES).map(|_| Vec::new()).collect()
}

fn clear_captured_sprite_lines_by_y(lines_by_y: &mut Vec<Vec<CapturedSpriteLine>>) {
    if lines_by_y.len() != MAX_VISIBLE_LINES {
        *lines_by_y = empty_captured_sprite_lines_by_y();
        return;
    }
    for lines in lines_by_y {
        lines.clear();
    }
}

fn empty_sprite_display_enable_x_by_y() -> [Option<usize>; MAX_VISIBLE_LINES] {
    [None; MAX_VISIBLE_LINES]
}

// Save-state note: Bus and everything it owns derive serde so a snapshot can
// be taken at an emulated-frame boundary (src/savestate.rs). Host-resource
// fields (open files, audio/serial sinks, wall-clock anchors, memo caches)
// are #[serde(skip)] and reattached by the savestate loader; everything else
// is emulated state and must round-trip. New fields are picked up by the
// derive automatically -- bump savestate::STATE_VERSION when the layout
// changes incompatibly.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Bus {
    pub mem: Memory,
    pub cia_a: Cia,
    pub cia_b: Cia,
    pub paula: Paula,
    pub agnus: Agnus,
    pub copper: Copper,
    pub denise: Denise,
    denise_revision: DeniseRevision,
    /// Effective chip-bus DMA address mask: installed chip RAM capped by the
    /// Agnus revision's address-bus reach. Single owner; refreshed by
    /// configure_chip_dma_masks().
    chip_dma_mask: u32,
    pub blitter: Blitter,
    pub floppy: FloppyController,
    pub rtc: Msm6242Rtc,
    /// Whether the $DC0000 RTC is fitted (machine-profile flag; the base
    /// A600 shipped without one). The CPU memory map consults this before
    /// decoding the RTC range.
    rtc_present: bool,
    /// Gayle gate array (A600/A1200 machine profiles); None on machines
    /// without one, which leaves $DA0000/$DE1000 floating as before.
    pub gayle: Option<Gayle>,
    /// Akiko gate array (CD32 machine profile): ID, the C2P port, and
    /// NVRAM/CD stubs at $B80000.
    pub akiko: Option<crate::akiko::Akiko>,
    /// CDTV DMAC/CD controller (CDTV machine profile): an autoconfig
    /// board with the 6525 TPI and Matshita drive.
    pub cdtv: Option<crate::cdtv::CdtvController>,
    /// Functional Zorro-chain boards (the A2091 SCSI controller, WASM plugin
    /// boards). The chain maps each board's window to a
    /// [`crate::zorro::BoardBacking::Device`] slot index into this vector, and
    /// accesses route here through the [`crate::zorro_device::ZorroDevice`]
    /// boundary. Serialized inline with the bus.
    pub devices: Vec<crate::zorro_device::BoardDevice>,
    /// Emulated-time deadline keeping the front-panel HDD LED lit after the
    /// most recent Gayle IDE activity, so short synchronous transfers stay
    /// visible for a human-perceptible stretch.
    hdd_led_until_cck: u64,
    /// CD32 joypad shift register position for port 2: starts at 8
    /// (Blue), decremented by /FIR1 falling edges the CPU drives while
    /// the pad is in serial mode, 1 returns the pad-present bit, 0
    /// returns zeros. Reset to 8 whenever serial mode is left.
    cd32_pad_shifter: i8,
    /// Previous CPU-driven /FIR1 output level (DDR & PRA & 0x80), for
    /// the shift-clock edge detector.
    cd32_pad_fire_oldstate: u8,
    /// One-time diagnostic when software writes BPLCON3 and the write is
    /// dropped (OCS Denise, or ECS with ENBPLCN3 clear).
    bplcon3_drop_warned: bool,
    /// AGA 32-bit fetch latch: the aligned chip longwords the CPU most
    /// recently fetched opcode words from (two entries, so a tight loop
    /// that straddles a longword boundary still hits). Fetch words from a
    /// latched longword cost no bus slot; chip writes to a latched
    /// longword invalidate it (self-modifying code refetches).
    cpu_fetch_latch: [Option<u32>; 2],

    /// Set by a CIA-A PRA write when /OVL transitions to 1. The
    /// emulator loop notices this between instruction slices, performs
    /// the actual mem_unmap + mem_map_ptr, and clears the flag.
    pub overlay_disable_pending: bool,

    /// The keyboard MCU (6500/1) model. Key transitions from the winit
    /// handler and scripted key events queue here, then clock into
    /// CIA-A bit by bit over the emulated KCLK/KDAT lines.
    pub keyboard: KeyboardMcu,

    /// Set when the keyboard MCU completed its 500 ms KCLK reset hold
    /// (Ctrl+Amiga+Amiga). The emulator loop performs the actual
    /// machine reset between instruction slices and clears the flag,
    /// like `overlay_disable_pending`.
    pub keyboard_system_reset_pending: bool,

    /// Live host input state (mouse counters, buttons, etc.). Mapped
    /// to CIA-A PRA, POTINP, and JOYxDAT on every read of those
    /// registers so Amiga-side software sees up-to-date values.
    pub input: InputState,

    pub poll_stats: PollStats,
    /// Host performance telemetry (call/probe/nanosecond counters), not
    /// machine state: call chunking legitimately differs across a
    /// save-state load, so it is excluded from the serialized layout.
    #[serde(skip)]
    video_pipeline_stats: VideoPipelineStats,

    /// Set by MMIO writes that should end the current CPU slice after
    /// the current instruction has retired.
    pub slice_preempted: bool,

    /// Diagnostic crash context: set when a blit is started whose D
    /// destination lands in the exception-vector / low-memory region, so
    /// the CPU wrapper can dump its instruction history at that moment.
    pub diag_lowmem_blit: bool,

    /// Latched agnus-frame-crossing that has not yet been ORed into
    /// Paula INTREQ. INTREQ is a bit latch, so multiple queued VBlanks
    /// collapse into one pending VERTB bit.
    pub pending_vbi: u32,
    pending_copper_frame_start: Option<u32>,
    /// Whether the Copper has been active at any point in the current field:
    /// true from the vertical-blank COP1LC strobe if Copper DMA was enabled
    /// there, or from any COPEN 0->1 write since. While false (a dormant
    /// Copper), a COPxLC write for the Copper's current list retargets the
    /// Copper PC directly instead of only loading the location latch --
    /// hardware behaviour the vAmigaTS Agnus/Copper/lc family photographs
    /// (vAmiga models it as activeInThisFrame in pokeCOPxLC).
    copper_active_in_frame: bool,
    /// Which location register the Copper PC was last strobed from (1 or 2):
    /// the vertical-blank restart selects COP1LC, COPJMPx selects its list.
    /// The dormant-retarget rule above only applies to the matching list.
    copper_current_list: u8,

    /// Paula interrupt bits that were pending when the current
    /// autovector was delivered. CPU INTREQ clears use this to tell
    /// ordinary palette setup apart from beam-timed Copper interrupt
    /// palette changes.
    pub delivered_irq_pending: u16,
    pub(crate) pending_copper_irq_beam: Option<(u32, u32)>,
    pub(crate) delivered_copper_irq_beam: Option<(u32, u32)>,
    coper_cpu_irq_delay_cck: u32,

    /// Paula INTREQ -> CPU IPL-pin propagation pipe (COPPERLINE_IRQ_LATENCY_CCK,
    /// see DEFAULT_IRQ_LATENCY_CCK). When the setting is non-zero, a newly
    /// raised maskable interrupt is held invisible to the CPU for that many cck
    /// (`irq_latency_mask` = the delayed bits, `irq_latency_cck` = countdown,
    /// `irq_latency_last_pending` = previous pending set for rising-edge detect).
    /// INTREQR reads are NOT delayed (the pipe sits between the level encoder
    /// and the pins, not on the register).
    irq_latency_cck: u32,
    irq_latency_mask: u16,
    irq_latency_last_pending: u16,
    /// Configured IPL-pipe length in cck (from COPPERLINE_IRQ_LATENCY_CCK or the
    /// default); 0 disables the pipe and the cpu.rs boundary-sampling delay. A
    /// field (not a global) so tests can set it per-instance -- mechanism tests
    /// run with 0 to deliver IRQs immediately.
    pub(crate) irq_latency_setting: u32,

    /// Palette snapshots written by CPU interrupt handlers. The top
    /// snapshot captures the display-start palette; the bottom
    /// snapshot tracks beam-timed Copper interrupt palettes used for
    /// buffer reuse decisions.
    pub beam_top_palette: Palette,
    pub beam_bottom_palette: Palette,
    pub beam_bottom_palette_valid: bool,
    cpu_palette_target: CpuPaletteTarget,
    cpu_palette_target_writes: u8,
    cpu_palette_target_beam: Option<(u32, u32)>,
    current_frame_render_base: RenderRegisterSnapshot,
    last_frame_render_base: Option<RenderRegisterSnapshot>,
    current_frame_render_events: Vec<BeamRegisterWrite>,
    current_frame_collision_events: Vec<BeamRegisterWrite>,
    current_frame_collision_control_events: Vec<BeamRegisterWrite>,
    current_frame_collision_bpldat_events: Vec<BeamRegisterWrite>,
    current_frame_collision_sprite_events: Vec<BeamRegisterWrite>,
    current_frame_collision_control_index: Option<BeamEventIndex>,
    current_frame_collision_bpldat_index: Option<BeamEventIndex>,
    current_frame_collision_sprite_index: Option<BeamEventIndex>,
    current_frame_collision_may_have_dual_playfield: bool,
    last_frame_render_events: Vec<BeamRegisterWrite>,
    beam_bottom_palette_events: Vec<BeamRegisterWrite>,
    pending_beam_bottom_palette_events: Vec<BeamRegisterWrite>,
    last_frame_beam_bottom_palette_events: Vec<BeamRegisterWrite>,
    current_frame_beam_top_palette: Palette,
    last_frame_beam_top_palette: Palette,
    last_frame_beam_top_palette_end: Palette,
    last_frame_beam_bottom_palette: Palette,
    last_frame_beam_bottom_palette_valid: bool,
    current_frame_chip_ram: Vec<u8>,
    last_frame_chip_ram: Vec<u8>,
    current_frame_chip_ram_writes: Vec<BeamChipRamWrite>,
    last_frame_chip_ram_writes: Vec<BeamChipRamWrite>,
    current_frame_bitplane_rows: Vec<Option<CapturedBitplaneRow>>,
    last_frame_bitplane_rows: Vec<Option<CapturedBitplaneRow>>,
    current_frame_sprite_lines: Vec<CapturedSpriteLine>,
    current_frame_sprite_lines_by_y: Vec<Vec<CapturedSpriteLine>>,
    current_frame_sprite_collision_sources: Vec<Option<Vec<LiveSpriteCollisionSource>>>,
    last_frame_sprite_lines: Vec<CapturedSpriteLine>,
    // Sprites whose data was DMA-fetched (off-screen) and then held with SPREN
    // cleared, to be repainted by Copper SPRxPOS repositioning during the
    // visible window. The renderer's manual-sprite path consumes these so it
    // can clip each repositioned segment to the reposition interval (a
    // CapturedSpriteLine cannot be clipped); the bus bar path is suppressed for
    // them. Carries the held pixel data and DMA-established control/window;
    // later SPRxPOS/CTL writes can still reposition the held line.
    #[serde(skip)]
    current_frame_held_sprites: [Option<HeldSpriteLine>; 8],
    #[serde(skip)]
    last_frame_held_sprites: [Option<HeldSpriteLine>; 8],
    #[serde(with = "serde_big_array::BigArray")]
    current_frame_sprite_display_enable_x_by_y: [Option<usize>; MAX_VISIBLE_LINES],
    #[serde(with = "serde_big_array::BigArray")]
    last_frame_sprite_display_enable_x_by_y: [Option<usize>; MAX_VISIBLE_LINES],
    current_frame_sprite_dma_observed: bool,
    last_frame_sprite_dma_observed: bool,
    current_frame_display_snapshot_taken: bool,
    #[serde(skip)]
    ocs_same_line_diw_start_blocked_vpos: Option<u32>,
    #[serde(skip)]
    current_frame_render_blocked: bool,
    current_frame_visible_start_vpos: u32,
    last_frame_visible_start_vpos: u32,
    /// Display geometry latched at the frame wrap (standard fixed canvas
    /// vs ECS/AGA VARBEAMEN programmable scan); see `FrameGeometry`.
    current_frame_geometry: FrameGeometry,
    last_frame_geometry: FrameGeometry,
    lazy_collision_vpos: u32,
    lazy_collision_hpos: u32,
    /// Sticky gate for the per-frame collision accumulation. Collision results
    /// are observable only through a CLXDAT read ($DFF00E); software that never
    /// reads CLXDAT cannot tell whether the latch was maintained. The full-frame
    /// collision scan in `begin_new_beam_frame` (`accumulate_live_collisions_to_
    /// frame_end`) is ~37% of emulation time on a busy demo, so skip it entirely
    /// until the first CLXDAT read. From that read onward this stays true and the
    /// per-frame flush runs exactly as before, so any software that actually uses
    /// collisions is bit-identical. Not serialized: a restored state restarts in
    /// the skipping state and re-arms on the next CLXDAT read (the latch carries
    /// the current frame's collisions, which is all real collision code relies
    /// on; cross-frame historical-OR of an unread latch is not meaningful).
    #[serde(skip)]
    collision_tracking_active: bool,
    cpu_bus_arbitration_enabled: bool,
    cpu_granted_chip_slots: u64,
    cpu_missed_chip_slots: u64,
    /// Sub-cck remainder from CPU-internal clock reporting (the core reports
    /// CPU clocks; the bus advances in `cpu_clocks_per_cck`-clock color
    /// clocks).
    cpu_clock_carry: u32,
    /// CPU clocks per color clock: 2 for a stock 7.09 MHz 68000, more for an
    /// accelerated CPU ([cpu] clock_mhz). Scales external-access and
    /// CPU-internal billing; the chip bus itself stays at one slot per cck.
    cpu_clocks_per_cck: u32,
    /// Sub-cck remainder from external (fast RAM/ROM) access billing, in
    /// hundredths of a CPU clock. At high clock ratios a single word access
    /// costs less than one cck; the carry accumulates so no time is lost.
    ext_clock_carry_x100: u32,
    /// True for the 68020+: its chip-bus cycle is 3 CPU clocks, not the
    /// 68000's 4 (2 cck) -- so after the granted slot it bills a shorter tail
    /// (write-posting and the faster 020 bus). Derived from the CPU model and
    /// re-set on construction / state load; not serialized.
    #[serde(skip)]
    cpu_short_bus_cycle: bool,
    /// Sub-cck remainder (in CPU clocks) for the 020+ short-bus-cycle tail:
    /// the 1-clock tail at the stock 2-clock ratio is half a cck, accumulated
    /// here so the fractional cck are not lost.
    #[serde(skip)]
    cpu_bus_tail_carry: u32,
    /// Chip-bus slot of the most recently granted CPU access to custom
    /// register space. A CPU register write is applied to chip state once
    /// its whole bus cycle has been billed (the beam sits past the granted
    /// slot by then), but its Denise/Agnus-effective position is referenced
    /// from the slot that carried the write: `record_render_write` places
    /// CPU-sourced render events at slot + `DENISE_WRITE_EFFECT_DELAY_CCK`,
    /// the same write-to-effect pipeline the Copper's slot-exact writes
    /// take. Also feeds the `COPPERLINE_DIAG_CPU_WRITES` landing trace.
    /// Transient bus-cycle state, rebuilt every access; not serialized.
    #[serde(skip)]
    cpu_custom_access_slot: Option<(u32, u32)>,
    /// Beam position where the CPU requested the most recent custom-register
    /// access, before waiting for fixed DMA, the Copper, or the blitter to
    /// release a chip-bus slot. This feeds the CPU read/write landing traces.
    /// Transient bus-cycle state, rebuilt every access; not serialized.
    #[serde(skip)]
    cpu_custom_request_slot: Option<(u32, u32)>,
    dbg_bpl_cck: Vec<u32>,
    dbg_slotmap: Vec<Vec<u8>>,
    dbg_slotmap_on: bool,
    dbg_slotmap_dumped: bool,
    #[serde(skip)]
    frame_analyzer_enabled: bool,
    #[serde(skip)]
    current_frame_bus_trace: FrameBusTrace,
    #[serde(skip)]
    last_frame_bus_trace: Option<FrameBusTrace>,
    /// Debugger-window custom-register watch offsets ($000-$1FE, word
    /// aligned), mirrored from the CPU machine's InteractiveBreaks, and
    /// the first pending hit since the debugger last polled. Recorded in
    /// the custom-register write path so every writer (CPU and Copper)
    /// is seen, including writes landing while the CPU is in STOP.
    ui_reg_watches: Vec<u16>,
    #[serde(skip)]
    ui_reg_hit: Option<UiRegHit>,
    /// Debugger beam traps and the first pending hit since the debugger
    /// last polled. Checked where the beam advances (`advance_beam`), so a
    /// hit lands at exact beam granularity and even while the CPU sits in
    /// STOP. Transient debug state, never serialized.
    #[serde(skip)]
    ui_beam_traps: Vec<BeamTrap>,
    #[serde(skip)]
    ui_beam_hit: Option<(u16, u16)>,
    /// Debugger Copper breakpoints (instruction addresses) and the first
    /// pending hit since last polled. A hit fires when the live Copper's
    /// PC first arrives at a breakpointed address -- before the
    /// instruction there executes -- whether it got there by sequential
    /// fetch, COPJMP, a CPU strobe write, or the vertical-blank restart.
    /// Transient debug state, never serialized.
    #[serde(skip)]
    ui_copper_breaks: Vec<u32>,
    #[serde(skip)]
    ui_copper_hit: Option<(u32, u16, u16)>,
    /// Debugger video-layer isolation (Video tab). Transient debug state:
    /// the Default of all-layers-visible is restored on state load.
    #[serde(skip)]
    ui_layer_masks: UiLayerMasks,
    /// Watched memory word addresses mirrored from the debugger (also
    /// forwarded into the blitter and floppy so their write sites can
    /// flag hits), plus the writers of recent watched-word writes for
    /// stop attribution. Transient debug state.
    #[serde(skip)]
    ui_mem_watch_addrs: Vec<u32>,
    #[serde(skip)]
    ui_mem_writers: Vec<UiMemWriter>,
    /// The Copper PC at the last breakpoint check, so arrival at an
    /// address fires once instead of on every eligible colour clock the
    /// PC rests there.
    #[serde(skip)]
    ui_copper_last_pc: u32,
    blitter_slowdown_cpu_misses: u8,
    /// Pending INTREQ.BLIT raise, in colour clocks. Real Agnus raises the
    /// blitter interrupt one clock after the sequencer's BLTDONE cycle
    /// (vAmiga scheduleIrqRel(BLIT, 1) at the terminal micro-cycle), so a
    /// scheduled completion arms this one-cck countdown instead of setting
    /// INTREQ in the final slot itself. Forced drains raise immediately.
    blit_irq_delay_cck: Option<u32>,
    slice_bus_advanced_cck: u32,
    slice_bus_tick: AgnusTick,
    // Deferred timed-device clock. Ticking CIA/serial/pots/audio/floppy/Akiko
    // once per CPU bus access dominated the host profile; instead accumulate the
    // color clocks here and `flush_timed_devices` them in one batch only when a
    // device is actually observed (a CIA/custom/peripheral access) or at an
    // instruction boundary (interrupt recognition). The CIA E-clock divider and
    // every device tick are exact under batching, so observable timing is
    // unchanged. Transient (always flushed to zero at frame boundaries, where
    // save states are taken), so skipped from serialization.
    #[serde(skip)]
    pending_device_cck: u32,
    #[serde(skip)]
    pending_device_tick: AgnusTick,
    audio_pending_cck: u32,
    last_chip_bus_owner: ChipBusOwner,
    /// Last 16-bit value driven on the chip data bus by a real access (display/
    /// audio/sprite DMA, or a mapped CPU read). CPU reads of unmapped addresses
    /// float to this, like the Agnus-arbitrated chip bus on real hardware, which
    /// is dominated by display DMA (often 0 on a blank screen) -- not a fixed
    /// all-ones pattern. Transient; re-established by DMA after a state load.
    #[serde(skip)]
    pub(crate) data_bus: u16,
    device_clock: DeviceClock,
    emulated_cck: u64,
    emulated_frames: u64,
    #[serde(skip)]
    blitter_trace: Option<std::fs::File>,
    display_dma_bplpt: [u32; 8],
    display_dma_sprpt: [u32; 8],
    /// Per-sprite SPRxPT value to seed the next frame's sprite-DMA replay with.
    /// Real Agnus advances SPRxPT through sprite DMA and does not snap it back to
    /// the last Copper/CPU write at the top of a field: once a channel has read
    /// its terminating descriptor its pointer sits past the consumed list. We
    /// model that by carrying the finished channel's DMA frontier across the
    /// frame boundary instead of re-seeding from `denise.sprpt` (the stale last
    /// write), so a sprite descriptor buffer that software rewrites every field
    /// is not re-armed from its previous, now-overwritten address before the
    /// Copper reloads SPRxPT. Captured at each frame start from the live
    /// (fetch-advanced) pointers, and serialized: the live pointers move on
    /// between the frame start and a mid-frame state save, so it cannot be
    /// re-derived at load time.
    #[serde(default)]
    sprite_dma_frame_start_ptr: [u32; 8],
    // The register-level sprite DMA state (comparator copies, DMA
    // flip-flops, display latches). Chip state: serialized and restored
    // exactly by save states.
    display_dma_sprite_state: [DisplaySpriteDmaState; 8],
    display_dma_clipped_rows_advanced: bool,
    bitplane_dmacon_delay: Option<BitplaneDmaconDelay>,
    bitplane_bplcon0_delay: Option<BitplaneBplcon0Delay>,
    bitplane_ddfstart_miss: Option<BitplaneDdfStartMiss>,
    /// Memoized bitplane fetch plans for `bitplane_slot_active_at`, keyed on
    /// the registers that feed it. The arbiter asks for bitplane ownership on
    /// every slot candidate, and mid-line fetch-shape changes can alternate
    /// between a few valid plans. Per-entry Cells avoid copying the whole cache
    /// on each hit while keeping the `&self` owner-selection call graph intact.
    #[serde(skip)]
    bitplane_slot_plan_cache: BitplaneSlotPlanCache,
    /// Bitplane DDF sequencer flop state at the start of the current line
    /// (see src/bus/ddf_line.rs); carried across lines by the flop walk.
    ddf_seq_line_initial: std::cell::Cell<crate::chipset::ddf_sequencer::DdfState>,
    /// DDFSTRT/DDFSTOP values as of the start of the current line (mid-line
    /// rewrites replay through `ddf_seq_writes`).
    ddf_seq_line_start_regs: std::cell::Cell<(u16, u16)>,
    /// (bmapen, bplcon0) as of the start of the current line; used only when
    /// mid-line DMACON/BPLCON0 writes are in the log.
    ddf_seq_line_start_ctl: std::cell::Cell<(bool, u16)>,
    /// Register writes that reached the sequencer during the current line.
    ddf_seq_writes: std::cell::RefCell<Vec<ddf_line::DdfSeqWrite>>,
    /// The current line's walked fetch table (rebuilt on demand).
    #[serde(skip)]
    ddf_seq_line: std::cell::RefCell<Option<ddf_line::DdfSeqLine>>,
    bus_accounting: BusAccounting,
    /// Latches once BEAMCON0.DUAL (A2024/UHRES) is first seen set, so the
    /// "not emulated" warning is logged a single time, not per write.
    uhres_dual_warned: bool,
    /// Stock-ratio cck-per-word for CPU external (fast RAM, ROM) accesses,
    /// in hundredths. Default 200 (= 2.00 cck/word, the real 68000 figure);
    /// diagnostic builds can override it for timing experiments.
    dbg_ext_cck_x100: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum CpuPaletteTarget {
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BeamWriteSource {
    Cpu,
    CpuCopperIrq,
    Copper,
}

/// One-shot env flag for the CPU write-landing trace
/// (`COPPERLINE_DIAG_CPU_WRITES=1`); see [`Bus::diag_cpu_write`].
fn diag_cpu_writes_on() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| crate::envcfg::flag("COPPERLINE_DIAG_CPU_WRITES"))
}

/// One-shot env flag for the CPU read-landing trace
/// (`COPPERLINE_DIAG_CPU_READS=1`); see [`Bus::diag_cpu_read`].
fn diag_cpu_reads_on() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| crate::envcfg::flag("COPPERLINE_DIAG_CPU_READS"))
}

fn diag_cpu_read_matches(off: u16, size: usize) -> bool {
    diag_cpu_reads_on()
        && diag_cpu_bus_addr_matches(Some(custom_register_cpu_addr(u64::from(off))), size)
}

/// One-shot env flag for the CPU chip-bus access trace
/// (`COPPERLINE_DIAG_CPU_BUS=1`); see [`Bus::diag_cpu_bus_access`].
fn diag_cpu_bus_on() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| crate::envcfg::flag("COPPERLINE_DIAG_CPU_BUS"))
}

fn cpu_bus_access_kind_name(kind: CpuBusAccessKind) -> &'static str {
    match kind {
        CpuBusAccessKind::Fetch => "fetch",
        CpuBusAccessKind::Read => "read",
        CpuBusAccessKind::Write => "write",
        CpuBusAccessKind::Custom => "custom",
    }
}

fn diag_cpu_bus_addr_ranges() -> &'static [(u32, u32)] {
    use std::sync::OnceLock;
    static V: OnceLock<Vec<(u32, u32)>> = OnceLock::new();
    V.get_or_init(|| {
        let Some(spec) = crate::envcfg::var("COPPERLINE_DIAG_CPU_BUS_ADDR") else {
            return Vec::new();
        };
        spec.split(',')
            .filter_map(|part| {
                let part = part.trim();
                if part.is_empty() {
                    return None;
                }
                if let Some((lo, hi)) = part.split_once(':') {
                    let lo = crate::envcfg::parse_u32(lo)?;
                    let hi = crate::envcfg::parse_u32(hi)?;
                    return Some((lo.min(hi), lo.max(hi)));
                }
                let addr = crate::envcfg::parse_u32(part)?;
                Some((addr, addr))
            })
            .collect()
    })
    .as_slice()
}

fn diag_cpu_bus_addr_matches(addr: Option<u32>, size: usize) -> bool {
    let ranges = diag_cpu_bus_addr_ranges();
    if ranges.is_empty() {
        return true;
    }
    let Some(addr) = addr else {
        return false;
    };
    let last = addr.saturating_add(size.max(1) as u32 - 1);
    ranges.iter().any(|&(lo, hi)| addr <= hi && last >= lo)
}

fn custom_register_cpu_addr(addr: u64) -> u32 {
    const CUSTOM_BASE: u32 = 0x00DF_F000;
    const CUSTOM_SIZE: u32 = 0x0000_1000;
    let addr = addr as u32;
    if addr < CUSTOM_SIZE {
        CUSTOM_BASE + addr
    } else {
        addr
    }
}

fn beam_write_source_name(source: BeamWriteSource) -> &'static str {
    match source {
        BeamWriteSource::Cpu => "cpu",
        BeamWriteSource::CpuCopperIrq => "cpu_copper_irq",
        BeamWriteSource::Copper => "copper",
    }
}

/// A debugger-window custom-register watch hit: the first watched write
/// since the debugger last polled, with its writer and beam position.
#[derive(Debug, Clone, Copy)]
pub struct UiRegHit {
    pub off: u16,
    pub value: u16,
    pub source: &'static str,
    pub vpos: u16,
    pub hpos: u16,
}

/// A debugger beam trap: halt when the Agnus beam reaches (or first passes)
/// a position. `hpos: None` fires at the first colour clock of the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeamTrap {
    pub vpos: u16,
    pub hpos: Option<u16>,
    /// Remove after the first hit (a run-to-position trap).
    pub once: bool,
}

/// Who last wrote a watched memory word, recorded at the write site.
#[derive(Debug, Clone, Copy)]
pub struct UiMemWriter {
    pub addr: u32,
    pub source: crate::debugger::WatchSource,
    pub vpos: u16,
    pub hpos: u16,
}

/// Debugger video-layer isolation masks: bit n set = bitplane n /
/// sprite n is drawn. Output-only filters applied where pixels resolve
/// to colours -- collision accumulation, playfield priority, and every
/// CPU-visible value always use the true data, so hiding a layer can
/// never perturb the emulation. All layers visible by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiLayerMasks {
    pub planes: u8,
    pub sprites: u8,
}

impl Default for UiLayerMasks {
    fn default() -> Self {
        Self {
            planes: 0xFF,
            sprites: 0xFF,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BeamRegisterWrite {
    pub vpos: u32,
    pub hpos: u32,
    pub offset: u16,
    pub value: u16,
    pub source: BeamWriteSource,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BeamChipRamWrite {
    pub vpos: u32,
    pub hpos: u32,
    pub offset: u32,
    bytes: [u8; 4],
    len: u8,
}

impl BeamChipRamWrite {
    fn from_cpu_write(vpos: u32, hpos: u32, offset: usize, size: usize, value: u32) -> Self {
        debug_assert!(matches!(size, 1 | 2 | 4));
        let mut bytes = [0; 4];
        let len = size.min(bytes.len());
        for (idx, byte) in bytes.iter_mut().enumerate().take(len) {
            let shift = (len - 1 - idx) * 8;
            *byte = ((value >> shift) & 0xFF) as u8;
        }
        Self {
            vpos,
            hpos,
            offset: offset as u32,
            bytes,
            len: len as u8,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_bytes(vpos: u32, hpos: u32, offset: u32, src: &[u8]) -> Self {
        let mut bytes = [0; 4];
        let len = src.len().min(bytes.len());
        bytes[..len].copy_from_slice(&src[..len]);
        Self {
            vpos,
            hpos,
            offset,
            bytes,
            len: len as u8,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RenderRegisterSnapshot {
    pub agnus_revision: AgnusRevision,
    /// BEAMCON0.HARDDIS active when this frame's registers were captured.
    pub harddis: bool,
    pub dmacon: u16,
    pub bplcon0: u16,
    pub bplcon1: u16,
    pub bplcon2: u16,
    pub bplcon3: u16,
    pub bplcon4: u16,
    pub fmode: u16,
    pub clxcon: u16,
    pub clxcon2: u16,
    pub bplpt: [u32; 8],
    pub bpldat: [u16; 8],
    pub sprpt: [u32; 8],
    pub sprpos: [u16; 8],
    pub sprctl: [u16; 8],
    pub sprdata: [u16; 8],
    pub sprdatb: [u16; 8],
    pub spr_armed: [bool; 8],
    pub bpl1mod: i16,
    pub bpl2mod: i16,
    pub palette: Palette,
    pub diwstrt: u16,
    pub diwstop: u16,
    pub diwhigh: DiwHigh,
    pub ddfstrt: u16,
    pub ddfstop: u16,
    /// Agnus LOF at this frame's start: with BPLCON0 LACE set, true for
    /// the long (upper) field of an interlaced pair. Set after the frame
    /// wrap toggles LOF (see `update_interlace_long_frame`); used by the
    /// presentation deinterlacer to route field lines by parity.
    pub long_field: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuBusAccessKind {
    Fetch,
    Read,
    Write,
    Custom,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChipBusOwner {
    Refresh,
    Bitplane,
    Sprite,
    Disk,
    Audio,
    Copper,
    Blitter,
    Cpu,
    #[default]
    Idle,
}

pub const CHIP_BUS_OWNER_NAMES: [&str; 9] = [
    "refresh", "bitplane", "sprite", "disk", "audio", "copper", "blitter", "cpu", "idle",
];

pub const FRAME_ANALYZER_MAX_VPOS: usize = MAX_VISIBLE_LINES;
pub const FRAME_ANALYZER_MAX_HPOS: usize = 512;

// Single-char codes for the COPPERLINE_DIAG_SLOTMAP per-color-clock owner map,
// chosen to line up with vAmiga's DMA-Debugger slot colours for visual diffing.
fn chip_bus_owner_code(owner: ChipBusOwner) -> u8 {
    match owner {
        ChipBusOwner::Refresh => b'R',
        ChipBusOwner::Bitplane => b'B',
        ChipBusOwner::Sprite => b'S',
        ChipBusOwner::Disk => b'D',
        ChipBusOwner::Audio => b'A',
        ChipBusOwner::Copper => b'C',
        ChipBusOwner::Blitter => b'L',
        ChipBusOwner::Cpu => b'P',
        ChipBusOwner::Idle => b'.',
    }
}

impl ChipBusOwner {
    fn accounting_index(self) -> usize {
        match self {
            ChipBusOwner::Refresh => 0,
            ChipBusOwner::Bitplane => 1,
            ChipBusOwner::Sprite => 2,
            ChipBusOwner::Disk => 3,
            ChipBusOwner::Audio => 4,
            ChipBusOwner::Copper => 5,
            ChipBusOwner::Blitter => 6,
            ChipBusOwner::Cpu => 7,
            ChipBusOwner::Idle => 8,
        }
    }
}

/// One blit started during the traced frame (Frame Analyzer / console
/// BLITS). `end` stays None while the blit is still running (or when it
/// finishes in a later frame).
#[derive(Clone, Debug)]
pub struct FrameBlitRecord {
    pub bltcon0: u16,
    pub bltcon1: u16,
    pub width_words: u32,
    pub height: u32,
    pub apt: u32,
    pub bpt: u32,
    pub cpt: u32,
    pub dpt: u32,
    pub start: (u16, u16),
    pub end: Option<(u16, u16)>,
}

/// Per-frame chip-bus ownership trace for the interactive frame analyzer.
///
/// One byte records the owner for one Agnus colour clock at `(vpos, hpos)`.
/// It is deliberately compact because it is filled from the bus-arbitration
/// hot path while the analyzer is open.
#[derive(Clone, Debug)]
pub struct FrameBusTrace {
    pub frame: u64,
    pub seconds: f64,
    pub rows: usize,
    pub cols: usize,
    pub line_cck: u32,
    pub visible_start_vpos: u32,
    pub visible_lines: usize,
    pub display_hpos_start: u32,
    pub display_hpos_end: u32,
    pub owner_cck: [u64; 9],
    pub blitter_busy_cck: u64,
    pub blitter_starve_cck: [u64; 9],
    pub partial: bool,
    /// Blits started this frame (capped; see FRAME_BLIT_RECORD_CAP).
    pub blits: Vec<FrameBlitRecord>,
    owners: Vec<u8>,
}

/// Blit records kept per traced frame.
pub const FRAME_BLIT_RECORD_CAP: usize = 64;

impl Default for FrameBusTrace {
    fn default() -> Self {
        Self {
            frame: 0,
            seconds: 0.0,
            rows: 0,
            cols: 0,
            line_cck: COLORCLOCKS_PER_LINE,
            visible_start_vpos: RENDER_VISIBLE_START_VPOS,
            visible_lines: RENDER_VISIBLE_LINES,
            display_hpos_start: RENDER_COPPER_WAIT_HPOS_FB0,
            display_hpos_end: RENDER_COPPER_WAIT_HPOS_FB0 + (RENDER_FRAMEBUFFER_WIDTH as u32 / 4),
            owner_cck: [0; 9],
            blitter_busy_cck: 0,
            blitter_starve_cck: [0; 9],
            partial: false,
            blits: Vec::new(),
            owners: Vec::new(),
        }
    }
}

impl FrameBusTrace {
    fn reset_for_frame(
        &mut self,
        frame: u64,
        seconds: f64,
        frame_lines: u32,
        line_cck: u32,
        visible_start_vpos: u32,
        visible_lines: usize,
        partial: bool,
    ) {
        self.frame = frame;
        self.seconds = seconds;
        self.rows = (frame_lines as usize).clamp(1, FRAME_ANALYZER_MAX_VPOS);
        self.cols = (line_cck as usize).clamp(1, FRAME_ANALYZER_MAX_HPOS);
        self.line_cck = line_cck;
        self.visible_start_vpos = visible_start_vpos;
        self.visible_lines = visible_lines.min(FRAME_ANALYZER_MAX_VPOS);
        self.display_hpos_start = RENDER_COPPER_WAIT_HPOS_FB0;
        self.display_hpos_end = (RENDER_COPPER_WAIT_HPOS_FB0
            + (RENDER_FRAMEBUFFER_WIDTH as u32 / 4))
            .min(self.cols as u32);
        self.owner_cck = [0; 9];
        self.blitter_busy_cck = 0;
        self.blitter_starve_cck = [0; 9];
        self.partial = partial;
        self.blits.clear();
        self.owners.resize(self.rows * self.cols, b'.');
        self.owners.fill(b'.');
    }

    fn finish_window(&mut self, visible_start_vpos: u32, visible_lines: usize) {
        self.visible_start_vpos = visible_start_vpos.min(self.rows.saturating_sub(1) as u32);
        self.visible_lines = visible_lines.min(self.rows);
    }

    fn clear(&mut self) {
        self.rows = 0;
        self.cols = 0;
        self.owners.clear();
        self.owner_cck = [0; 9];
        self.blitter_busy_cck = 0;
        self.blitter_starve_cck = [0; 9];
        self.partial = false;
    }

    fn record(&mut self, vpos: u32, hpos: u32, cck: u32, owner: ChipBusOwner, blitter_busy: bool) {
        if self.rows == 0 || self.cols == 0 {
            return;
        }
        let v = vpos as usize;
        let h = hpos as usize;
        if v >= self.rows || h >= self.cols {
            return;
        }
        let idx = owner.accounting_index();
        self.owner_cck[idx] = self.owner_cck[idx].saturating_add(u64::from(cck));
        if blitter_busy {
            self.blitter_busy_cck = self.blitter_busy_cck.saturating_add(u64::from(cck));
            if !matches!(owner, ChipBusOwner::Blitter) {
                self.blitter_starve_cck[idx] =
                    self.blitter_starve_cck[idx].saturating_add(u64::from(cck));
            }
        }
        let code = chip_bus_owner_code(owner);
        let row = &mut self.owners[v * self.cols..(v + 1) * self.cols];
        let end = (h + cck as usize).min(self.cols);
        for slot in row.iter_mut().take(end).skip(h) {
            *slot = code;
        }
    }

    pub fn owner_code_at(&self, vpos: usize, hpos: usize) -> u8 {
        if vpos >= self.rows || hpos >= self.cols {
            return b'.';
        }
        self.owners[vpos * self.cols + hpos]
    }

    pub fn owner_row(&self, vpos: usize) -> Option<&[u8]> {
        if vpos >= self.rows || self.cols == 0 {
            return None;
        }
        let start = vpos * self.cols;
        Some(&self.owners[start..start + self.cols])
    }

    pub fn has_samples(&self) -> bool {
        self.owner_cck.iter().any(|&v| v != 0)
    }
}

/// Per-display-frame chip-bus color-clock accounting. Gated behind
/// `COPPERLINE_DUMP_BUS_ACCOUNTING`; reports where the granted color clocks go
/// and how badly fixed DMA / Copper starve a busy blitter. See
/// docs/internals/timing.md.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct BusAccounting {
    enabled: bool,
    /// Color clocks attributed to each owner this display frame.
    owner_cck: [u64; 9],
    /// Color clocks during which the blitter was busy (whether or not it
    /// was granted the slot).
    blitter_busy_cck: u64,
    /// Of the busy color clocks, how many were taken by each non-blitter
    /// owner (the cycles that stretch a blit out past its granted time).
    blitter_starve_cck: [u64; 9],
    /// Blits started this frame, split line vs normal, with total scheduled
    /// slot (color-clock) cost. Measures whether the blit *workload* is
    /// inflated, independent of arbitration.
    blits_line: u64,
    blits_normal: u64,
    slots_line: u64,
    slots_normal: u64,
}

impl BusAccounting {
    fn from_env() -> Self {
        Self {
            enabled: crate::envcfg::flag("COPPERLINE_DUMP_BUS_ACCOUNTING"),
            ..Self::default()
        }
    }

    fn record_cck(&mut self, owner: ChipBusOwner, cck: u32, blitter_busy: bool) {
        let cck = cck as u64;
        let idx = owner.accounting_index();
        self.owner_cck[idx] += cck;
        if blitter_busy {
            self.blitter_busy_cck += cck;
            if !matches!(owner, ChipBusOwner::Blitter) {
                self.blitter_starve_cck[idx] += cck;
            }
        }
    }

    fn record_blit(&mut self, is_line: bool, slots: u32) {
        let slots = slots as u64;
        if is_line {
            self.blits_line += 1;
            self.slots_line += slots;
        } else {
            self.blits_normal += 1;
            self.slots_normal += slots;
        }
    }

    fn reset_frame(&mut self) {
        self.owner_cck = [0; 9];
        self.blitter_busy_cck = 0;
        self.blitter_starve_cck = [0; 9];
        self.blits_line = 0;
        self.blits_normal = 0;
        self.slots_line = 0;
        self.slots_normal = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrontPanelStatus {
    pub power_led_on: bool,
    pub fdd_led_on: bool,
    pub fdd_track: Option<u8>,
    /// HDD activity LED: None on machines without an IDE port (no Gayle),
    /// Some(on) when the machine has one.
    pub hdd_led: Option<bool>,
    /// CD activity LED: None on machines without a CD drive, Some(on)
    /// while the drive is reading data or playing audio.
    pub cd_led: Option<bool>,
    pub output_volume_percent: u8,
}

impl Default for FrontPanelStatus {
    fn default() -> Self {
        Self {
            power_led_on: false,
            fdd_led_on: false,
            fdd_track: None,
            hdd_led: None,
            cd_led: None,
            output_volume_percent: 100,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VideoPipelineStats {
    /// Sampling-gate evaluation counters. The Instant::now timing probes fire
    /// on 1 in SAMPLE_RATE evaluations of these counters, NOT of the *_calls
    /// counters below: *_calls only advance when a call did work, so gating on
    /// them parks the gate open across every no-op call in between (two clock
    /// reads per beam advance - a measurable share of host CPU).
    bitplane_fetch_probes: u64,
    sprite_fetch_probes: u64,
    collision_probes: u64,
    pub bitplane_fetch_calls: u64,
    pub bitplane_fetch_slots: u64,
    pub bitplane_fetch_rows_started: u64,
    pub bitplane_fetch_rows_completed: u64,
    pub bitplane_fetch_nanos: u128,
    pub sprite_fetch_calls: u64,
    pub sprite_fetch_pair_slots: u64,
    pub sprite_fetch_lines: u64,
    pub sprite_fetch_nanos: u128,
    pub collision_calls: u64,
    pub collision_pixels: u64,
    pub collision_control_segments: u64,
    pub collision_full_line_scans: u64,
    pub collision_nanos: u128,
    pub render_frames: u64,
    pub render_events: u64,
    pub render_control_segments: u64,
    pub render_playfield_pixels: u64,
    pub render_manual_bpl_segments: u64,
    pub render_sprite_lines: u64,
    pub render_total_nanos: u128,
    pub render_event_nanos: u128,
    pub render_background_nanos: u128,
    pub render_playfield_nanos: u128,
    pub render_manual_bpl_nanos: u128,
    pub render_sprite_nanos: u128,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VideoRenderFrameTiming {
    pub events: u64,
    pub control_segments: u64,
    pub playfield_pixels: u64,
    pub manual_bpl_segments: u64,
    pub sprite_lines: u64,
    pub total_nanos: u128,
    pub event_nanos: u128,
    pub background_nanos: u128,
    pub playfield_nanos: u128,
    pub manual_bpl_nanos: u128,
    pub sprite_nanos: u128,
}

impl VideoPipelineStats {
    /// Advance a probe counter and start a timing sample on 1 in `rate`
    /// evaluations. The recorded duration is later scaled back up by `rate`
    /// in `add_sampled_duration`, so the estimate stays unbiased as long as
    /// every potential sample point calls this exactly once.
    fn probe_timing_sample(probes: &mut u64, rate: u128) -> Option<Instant> {
        let due = probes.is_multiple_of(rate as u64);
        *probes = probes.wrapping_add(1);
        due.then(Instant::now)
    }

    fn add_sampled_duration(total: &mut u128, elapsed: Option<(Duration, u128)>) {
        if let Some((elapsed, sample_rate)) = elapsed {
            *total = total.saturating_add(elapsed.as_nanos().saturating_mul(sample_rate));
        }
    }

    fn millis(nanos: u128) -> f64 {
        nanos as f64 / 1_000_000.0
    }

    fn nanos_per_item(nanos: u128, items: u64) -> f64 {
        if items == 0 {
            0.0
        } else {
            nanos as f64 / items as f64
        }
    }

    pub fn dump(&self, label: &str) {
        if self.bitplane_fetch_calls
            + self.sprite_fetch_calls
            + self.collision_calls
            + self.render_frames
            == 0
        {
            return;
        }
        log::info!(
            "video pipeline stats ({label}): bitplane_fetch calls={} slots={} rows_started={} rows_completed={} time={:.3}ms, sprite_fetch calls={} pair_slots={} lines={} time={:.3}ms",
            self.bitplane_fetch_calls,
            self.bitplane_fetch_slots,
            self.bitplane_fetch_rows_started,
            self.bitplane_fetch_rows_completed,
            Self::millis(self.bitplane_fetch_nanos),
            self.sprite_fetch_calls,
            self.sprite_fetch_pair_slots,
            self.sprite_fetch_lines,
            Self::millis(self.sprite_fetch_nanos),
        );
        log::info!(
            "video pipeline stats ({label}): collisions calls={} pixels={} full_line_scans={} control_segments={} time={:.3}ms avg={:.1}ns/pixel",
            self.collision_calls,
            self.collision_pixels,
            self.collision_full_line_scans,
            self.collision_control_segments,
            Self::millis(self.collision_nanos),
            Self::nanos_per_item(self.collision_nanos, self.collision_pixels),
        );
        log::info!(
            "video pipeline stats ({label}): render frames={} events={} control_segments={} playfield_pixels={} manual_bpl_segments={} sprite_lines={} total={:.3}ms phases(events={:.3}, background={:.3}, playfield={:.3}, manual_bpl={:.3}, sprites={:.3})",
            self.render_frames,
            self.render_events,
            self.render_control_segments,
            self.render_playfield_pixels,
            self.render_manual_bpl_segments,
            self.render_sprite_lines,
            Self::millis(self.render_total_nanos),
            Self::millis(self.render_event_nanos),
            Self::millis(self.render_background_nanos),
            Self::millis(self.render_playfield_nanos),
            Self::millis(self.render_manual_bpl_nanos),
            Self::millis(self.render_sprite_nanos),
        );
    }
}

#[derive(Default, Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct InputState {
    pub mouse_x_port1: u8,
    pub mouse_y_port1: u8,
    pub mouse_x_port2: u8,
    pub mouse_y_port2: u8,
    pub lmb_port1: bool,
    pub lmb_port2: bool,
    pub rmb_port1: bool,
    pub rmb_port2: bool,
    /// A CD32 joypad is plugged into port 2: enables the pad's serial
    /// button protocol (POT1X low selects shift mode, the /FIR1 line
    /// clocks, POT1Y carries the active-low button bits). Red rides the
    /// normal fire line (`lmb_port2`), Blue the button-2 line
    /// (`rmb_port2`); the rest only exist serially.
    pub cd32_pad_port2: bool,
    pub cd32_play_port2: bool,
    pub cd32_rwd_port2: bool,
    pub cd32_ffw_port2: bool,
    pub cd32_green_port2: bool,
    pub cd32_yellow_port2: bool,
    pub mmb_port1: bool,
    pub mmb_port2: bool,
    /// Digital-joystick direction lines per controller port. When the
    /// matching `joystick_portN` flag is set, JOYxDAT reports these as the
    /// Gray-coded direction bits an Amiga game decodes, instead of the mouse
    /// quadrature counters. Fire reuses the FIR0/FIR1 lines (`lmb_portN`),
    /// which CIA-A PRA bits 6/7 already report.
    pub joy_up_port1: bool,
    pub joy_down_port1: bool,
    pub joy_left_port1: bool,
    pub joy_right_port1: bool,
    pub joy_up_port2: bool,
    pub joy_down_port2: bool,
    pub joy_left_port2: bool,
    pub joy_right_port2: bool,
    /// True when a digital joystick (not a mouse) is the active device on the
    /// port, so JOYxDAT returns the direction encoding.
    pub joystick_port1: bool,
    pub joystick_port2: bool,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct DeviceClock {
    realtime_enabled: bool,
    // Wall-clock instant corresponding to the current device timeline.
    // CPU chip-bus slots push this into the future; hardware barriers
    // wait for it so chip RAM/custom access cannot outrun real time.
    // Not part of a save state: the loader re-anchors to the host clock.
    #[serde(skip)]
    realtime_anchor: Option<Instant>,
    realtime_cck_remainder: u128,
    realtime_delay_remainder: u128,
    cia_tick_remainder_cck: u32,
}

impl DeviceClock {
    fn set_realtime_enabled(&mut self, enabled: bool) {
        self.realtime_enabled = enabled;
        self.reset();
    }

    fn reset(&mut self) {
        self.realtime_anchor = self.realtime_enabled.then(Instant::now);
        self.realtime_cck_remainder = 0;
        self.realtime_delay_remainder = 0;
        self.cia_tick_remainder_cck = 0;
    }

    fn wait_duration(&self, now: Instant) -> Option<Duration> {
        if !self.realtime_enabled {
            return None;
        }
        let anchor = self.realtime_anchor?;
        (anchor > now).then(|| anchor.duration_since(now))
    }

    fn realtime_cck_due(&mut self, now: Instant) -> u32 {
        if !self.realtime_enabled {
            return 0;
        }
        let Some(anchor) = self.realtime_anchor else {
            self.realtime_anchor = Some(now);
            return 0;
        };
        if now <= anchor {
            return 0;
        }

        let elapsed = now.duration_since(anchor);
        let total = self
            .realtime_cck_remainder
            .saturating_add(elapsed.as_nanos().saturating_mul(PAULA_CLOCK_HZ as u128));
        self.realtime_cck_remainder = total % NANOS_PER_SECOND;
        self.realtime_anchor = Some(now);
        (total / NANOS_PER_SECOND).min(u32::MAX as u128) as u32
    }

    fn note_realtime_device_advance(&mut self, cck: u32) {
        if !self.realtime_enabled || cck == 0 {
            return;
        }
        let anchor = self.realtime_anchor.unwrap_or_else(Instant::now);
        let total = self
            .realtime_delay_remainder
            .saturating_add(cck as u128 * NANOS_PER_SECOND);
        let nanos = total / PAULA_CLOCK_HZ as u128;
        self.realtime_delay_remainder = total % PAULA_CLOCK_HZ as u128;
        let delay = Duration::from_nanos(nanos.min(u64::MAX as u128) as u64);
        self.realtime_anchor = Some(anchor + delay);
    }

    fn cia_ticks_for_cck(&mut self, cck: u32) -> u32 {
        let total = self.cia_tick_remainder_cck + cck;
        self.cia_tick_remainder_cck = total % 5;
        total / 5
    }
}

impl InputState {
    pub fn add_mouse_delta_port1(&mut self, dx: i32, dy: i32) {
        self.mouse_x_port1 = self.mouse_x_port1.wrapping_add(dx as u8);
        self.mouse_y_port1 = self.mouse_y_port1.wrapping_add(dy as u8);
    }

    pub fn write_joytest(&mut self, val: u16) {
        self.mouse_y_port1 = (val >> 8) as u8;
        self.mouse_x_port1 = val as u8;
        self.mouse_y_port2 = (val >> 8) as u8;
        self.mouse_x_port2 = val as u8;
    }

    pub fn joy0dat(&self) -> u16 {
        if self.joystick_port1 {
            digital_joydat(
                self.joy_up_port1,
                self.joy_down_port1,
                self.joy_left_port1,
                self.joy_right_port1,
            )
        } else {
            mouse_joydat(self.mouse_x_port1, self.mouse_y_port1)
        }
    }

    pub fn joy1dat(&self) -> u16 {
        if self.joystick_port2 {
            digital_joydat(
                self.joy_up_port2,
                self.joy_down_port2,
                self.joy_left_port2,
                self.joy_right_port2,
            )
        } else {
            mouse_joydat(self.mouse_x_port2, self.mouse_y_port2)
        }
    }

    /// Set the port-2 digital joystick state from a host gamepad. Marks the
    /// port as a joystick so JOY1DAT reports directions. Fire (button 1)
    /// drives /FIR1 (CIA-A PRA bit 7) through the shared `lmb_port2` line;
    /// button 2 drives POT1Y, read back through POTGOR (the shared
    /// `rmb_port2` line), matching a real Amiga's second joystick button.
    pub fn set_joystick_port2(
        &mut self,
        up: bool,
        down: bool,
        left: bool,
        right: bool,
        fire: bool,
        button2: bool,
    ) {
        self.joystick_port2 = true;
        self.joy_up_port2 = up;
        self.joy_down_port2 = down;
        self.joy_left_port2 = left;
        self.joy_right_port2 = right;
        self.lmb_port2 = fire;
        self.rmb_port2 = button2;
    }

    /// Set the CD32 joypad's extra buttons (port 2). Red and Blue arrive
    /// through `set_joystick_port2` as fire/button2; these five only
    /// exist in the pad's serial report.
    pub fn set_cd32_buttons_port2(
        &mut self,
        play: bool,
        rwd: bool,
        ffw: bool,
        green: bool,
        yellow: bool,
    ) {
        self.cd32_play_port2 = play;
        self.cd32_rwd_port2 = rwd;
        self.cd32_ffw_port2 = ffw;
        self.cd32_green_port2 = green;
        self.cd32_yellow_port2 = yellow;
    }
}

fn mouse_joydat(x: u8, y: u8) -> u16 {
    ((y as u16) << 8) | x as u16
}

/// Encode digital-joystick directions into a JOYxDAT word so the Amiga's
/// documented decode recovers them. Per the Hardware Reference Manual:
///   right = bit1, down = bit1 ^ bit0, left = bit9, up = bit9 ^ bit8.
/// Note left/up live in the high (vertical-counter) byte and right/down in
/// the low (horizontal-counter) byte -- the axes are not split the obvious
/// way, which is a real hardware quirk of how the switches drive the
/// quadrature counters.
fn digital_joydat(up: bool, down: bool, left: bool, right: bool) -> u16 {
    let mut v = 0u16;
    if right {
        v |= 0x0002; // bit 1
    }
    if right ^ down {
        v |= 0x0001; // bit 0  -> down decodes as bit1 ^ bit0
    }
    if left {
        v |= 0x0200; // bit 9
    }
    if left ^ up {
        v |= 0x0100; // bit 8  -> up decodes as bit9 ^ bit8
    }
    v
}

/// Counts reads of each MMIO register so we can see what DiagROM is
/// busy-polling. Dumped on Bus::drop via the `poll-stats` log target.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct PollStats {
    pub cia_a: [u64; 16],
    pub cia_b: [u64; 16],
    /// Read counts per custom register, indexed by `offset >> 1` (registers
    /// are word-aligned, offsets $000..=$FFE). A flat table rather than a
    /// `HashMap`: `tick_read_custom` is hit on every $DFF000 register read,
    /// which Kickstart busy-polls hard, so the default-SipHash probe showed
    /// up on the hot path.
    pub custom: Vec<u64>,
}

impl Default for PollStats {
    fn default() -> Self {
        Self {
            cia_a: [0; 16],
            cia_b: [0; 16],
            custom: vec![0; 0x800],
        }
    }
}

impl PollStats {
    pub fn tick_read(&mut self, which: &str, reg: usize) {
        match which {
            "cia_a" => self.cia_a[reg & 0xF] += 1,
            "cia_b" => self.cia_b[reg & 0xF] += 1,
            _ => {}
        }
    }
    pub fn tick_read_custom(&mut self, off: u16) {
        if let Some(count) = self.custom.get_mut((off >> 1) as usize) {
            *count += 1;
        }
    }
    pub fn dump_top(&self, label: &str) {
        log::info!("== poll stats ({}) ==", label);
        for (i, &n) in self.cia_a.iter().enumerate() {
            if n > 0 {
                log::info!("  cia_a reg ${:X}: {}", i, n);
            }
        }
        for (i, &n) in self.cia_b.iter().enumerate() {
            if n > 0 {
                log::info!("  cia_b reg ${:X}: {}", i, n);
            }
        }
        let mut customs: Vec<(u16, u64)> = self
            .custom
            .iter()
            .enumerate()
            .filter(|(_, &n)| n > 0)
            .map(|(idx, &n)| ((idx as u16) << 1, n))
            .collect();
        customs.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
        for (off, n) in customs.iter().take(20) {
            log::info!("  custom ${:03X}: {}", off, n);
        }
    }
}

fn fixed_standard_frame_lines(video_standard: VideoStandard, lace: bool, long_field: bool) -> u32 {
    let long_lines = match video_standard {
        VideoStandard::Pal => PAL_LINES,
        VideoStandard::Ntsc => NTSC_LINES,
    };
    if lace && !long_field {
        long_lines.saturating_sub(1)
    } else {
        long_lines
    }
}

impl Bus {
    pub fn new(mem: Memory, paula: Paula, floppy: FloppyController) -> Self {
        let current_frame_chip_ram = mem.chip_ram.clone();
        let blitter_trace = crate::envcfg::var_os("COPPERLINE_TRACE_BLITTER").and_then(|path| {
            match std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&path)
            {
                Ok(file) => Some(file),
                Err(err) => {
                    log::warn!(
                        "could not open COPPERLINE_TRACE_BLITTER path {}: {err}",
                        std::path::Path::new(&path).display()
                    );
                    None
                }
            }
        });

        let mut bus = Self {
            mem,
            cia_a: Cia::new(Which::A),
            cia_b: Cia::new(Which::B),
            paula,
            agnus: Agnus::new(),
            copper: Copper::new(),
            denise: Denise::new(),
            denise_revision: DeniseRevision::Ocs,
            chip_dma_mask: 0x0007_FFFF,
            blitter: Blitter::new(),
            floppy,
            rtc: Msm6242Rtc::default(),
            rtc_present: true,
            gayle: None,
            akiko: None,
            cdtv: None,
            devices: Vec::new(),
            hdd_led_until_cck: 0,
            cd32_pad_shifter: 8,
            cd32_pad_fire_oldstate: 0,
            bplcon3_drop_warned: false,
            cpu_fetch_latch: [None; 2],
            overlay_disable_pending: false,
            keyboard: KeyboardMcu::new(),
            keyboard_system_reset_pending: false,
            input: InputState::default(),
            poll_stats: PollStats::default(),
            video_pipeline_stats: VideoPipelineStats::default(),
            slice_preempted: false,
            diag_lowmem_blit: false,
            pending_vbi: 0,
            pending_copper_frame_start: None,
            copper_active_in_frame: false,
            copper_current_list: 1,
            delivered_irq_pending: 0,
            pending_copper_irq_beam: None,
            delivered_copper_irq_beam: None,
            coper_cpu_irq_delay_cck: 0,
            irq_latency_cck: 0,
            irq_latency_mask: 0,
            irq_latency_last_pending: 0,
            irq_latency_setting: irq_latency_setting_from_env(),
            beam_top_palette: Palette::new(),
            beam_bottom_palette: Palette::new(),
            beam_bottom_palette_valid: false,
            cpu_palette_target: CpuPaletteTarget::Top,
            cpu_palette_target_writes: 0,
            cpu_palette_target_beam: None,
            current_frame_render_base: RenderRegisterSnapshot::default(),
            last_frame_render_base: None,
            current_frame_render_events: Vec::new(),
            current_frame_collision_events: Vec::new(),
            current_frame_collision_control_events: Vec::new(),
            current_frame_collision_bpldat_events: Vec::new(),
            current_frame_collision_sprite_events: Vec::new(),
            current_frame_collision_control_index: None,
            current_frame_collision_bpldat_index: None,
            current_frame_collision_sprite_index: None,
            current_frame_collision_may_have_dual_playfield: false,
            last_frame_render_events: Vec::new(),
            beam_bottom_palette_events: Vec::new(),
            pending_beam_bottom_palette_events: Vec::new(),
            last_frame_beam_bottom_palette_events: Vec::new(),
            current_frame_beam_top_palette: Palette::new(),
            last_frame_beam_top_palette: Palette::new(),
            last_frame_beam_top_palette_end: Palette::new(),
            last_frame_beam_bottom_palette: Palette::new(),
            last_frame_beam_bottom_palette_valid: false,
            current_frame_chip_ram,
            last_frame_chip_ram: Vec::new(),
            current_frame_chip_ram_writes: Vec::new(),
            last_frame_chip_ram_writes: Vec::new(),
            current_frame_bitplane_rows: empty_captured_bitplane_rows(),
            last_frame_bitplane_rows: empty_captured_bitplane_rows(),
            current_frame_sprite_lines: Vec::new(),
            current_frame_sprite_lines_by_y: empty_captured_sprite_lines_by_y(),
            current_frame_sprite_collision_sources: empty_sprite_collision_sources(),
            last_frame_sprite_lines: Vec::new(),
            current_frame_held_sprites: [None; 8],
            last_frame_held_sprites: [None; 8],
            current_frame_sprite_display_enable_x_by_y: empty_sprite_display_enable_x_by_y(),
            last_frame_sprite_display_enable_x_by_y: empty_sprite_display_enable_x_by_y(),
            current_frame_sprite_dma_observed: false,
            last_frame_sprite_dma_observed: false,
            current_frame_display_snapshot_taken: false,
            ocs_same_line_diw_start_blocked_vpos: None,
            current_frame_render_blocked: false,
            current_frame_visible_start_vpos: RENDER_VISIBLE_START_VPOS,
            last_frame_visible_start_vpos: RENDER_VISIBLE_START_VPOS,
            current_frame_geometry: FrameGeometry::standard(
                RENDER_VISIBLE_START_VPOS,
                PAL_LINES,
                false,
            ),
            last_frame_geometry: FrameGeometry::standard(
                RENDER_VISIBLE_START_VPOS,
                PAL_LINES,
                false,
            ),
            lazy_collision_vpos: RENDER_VISIBLE_START_VPOS,
            lazy_collision_hpos: RENDER_COPPER_WAIT_HPOS_FB0,
            collision_tracking_active: false,
            cpu_bus_arbitration_enabled: false,
            cpu_clock_carry: 0,
            cpu_clocks_per_cck: 2,
            ext_clock_carry_x100: 0,
            cpu_short_bus_cycle: false,
            cpu_bus_tail_carry: 0,
            cpu_custom_access_slot: None,
            cpu_custom_request_slot: None,
            cpu_granted_chip_slots: 0,
            cpu_missed_chip_slots: 0,
            dbg_bpl_cck: vec![0; 340],
            dbg_slotmap: Vec::new(),
            dbg_slotmap_on: crate::envcfg::flag("COPPERLINE_DIAG_SLOTMAP"),
            dbg_slotmap_dumped: false,
            frame_analyzer_enabled: false,
            current_frame_bus_trace: FrameBusTrace::default(),
            last_frame_bus_trace: None,
            ui_reg_watches: Vec::new(),
            ui_reg_hit: None,
            ui_beam_traps: Vec::new(),
            ui_beam_hit: None,
            ui_copper_breaks: Vec::new(),
            ui_copper_hit: None,
            ui_copper_last_pc: 0,
            ui_layer_masks: UiLayerMasks::default(),
            ui_mem_watch_addrs: Vec::new(),
            ui_mem_writers: Vec::new(),
            blitter_slowdown_cpu_misses: 0,
            blit_irq_delay_cck: None,
            slice_bus_advanced_cck: 0,
            slice_bus_tick: AgnusTick::default(),
            pending_device_cck: 0,
            pending_device_tick: AgnusTick::default(),
            audio_pending_cck: 0,
            last_chip_bus_owner: ChipBusOwner::Idle,
            data_bus: 0,
            device_clock: DeviceClock::default(),
            emulated_cck: 0,
            emulated_frames: 0,
            blitter_trace,
            display_dma_bplpt: [0; 8],
            display_dma_sprpt: [0; 8],
            sprite_dma_frame_start_ptr: [0; 8],
            display_dma_sprite_state: [DisplaySpriteDmaState::default(); 8],
            display_dma_clipped_rows_advanced: false,
            bitplane_dmacon_delay: None,
            bitplane_bplcon0_delay: None,
            bitplane_ddfstart_miss: None,
            bitplane_slot_plan_cache: BitplaneSlotPlanCache::new(),
            ddf_seq_line_initial: std::cell::Cell::new(Default::default()),
            ddf_seq_line_start_regs: std::cell::Cell::new((0, 0)),
            ddf_seq_line_start_ctl: std::cell::Cell::new((false, 0)),
            ddf_seq_writes: std::cell::RefCell::new(Vec::new()),
            ddf_seq_line: std::cell::RefCell::new(None),
            bus_accounting: BusAccounting::from_env(),
            uhres_dual_warned: false,
            dbg_ext_cck_x100: external_access_cck_x100_setting(),
        };
        bus.configure_chip_dma_masks();
        // Re-derive the per-frame capture buffers from the same helper the
        // reset paths use, rather than trusting the struct literal above to
        // stay in lockstep with it by hand.
        bus.reset_frame_capture_buffers();
        bus
    }

    pub fn rtc_present(&self) -> bool {
        self.rtc_present
    }

    /// Replace the debugger-window custom-register watch set (word
    /// offsets into $DFF000). A pending unpolled hit is dropped, so a
    /// stale hit cannot fire after its watch was removed.
    pub fn set_ui_reg_watches(&mut self, offsets: &[u16]) {
        self.ui_reg_watches = offsets.to_vec();
        self.ui_reg_hit = None;
    }

    /// Take the pending custom-register watch hit, if any.
    pub fn take_ui_reg_hit(&mut self) -> Option<UiRegHit> {
        self.ui_reg_hit.take()
    }

    /// The debugger's armed beam traps.
    pub fn ui_beam_traps(&self) -> &[BeamTrap] {
        &self.ui_beam_traps
    }

    /// Add a persistent beam trap at (`vpos`, `hpos`), or remove it when
    /// one is already set there. Returns true when the trap is now set.
    pub fn ui_toggle_beam_trap(&mut self, vpos: u16, hpos: Option<u16>) -> bool {
        match self
            .ui_beam_traps
            .iter()
            .position(|trap| trap.vpos == vpos && trap.hpos == hpos && !trap.once)
        {
            Some(pos) => {
                self.ui_beam_traps.remove(pos);
                false
            }
            None => {
                self.ui_beam_traps.push(BeamTrap {
                    vpos,
                    hpos,
                    once: false,
                });
                true
            }
        }
    }

    /// Arm a one-shot run-to-position beam trap, replacing any previous
    /// one-shot (only one run-to target makes sense at a time).
    pub fn ui_arm_beam_trap_once(&mut self, vpos: u16, hpos: Option<u16>) {
        self.ui_beam_traps.retain(|trap| !trap.once);
        self.ui_beam_traps.push(BeamTrap {
            vpos,
            hpos,
            once: true,
        });
    }

    /// Drop an armed one-shot run-to trap (a run-to that ended early).
    pub fn ui_disarm_beam_trap_once(&mut self) {
        self.ui_beam_traps.retain(|trap| !trap.once);
    }

    pub fn ui_clear_beam_traps(&mut self) {
        self.ui_beam_traps.clear();
        self.ui_beam_hit = None;
    }

    /// Take the pending beam-trap hit `(vpos, hpos)`, if any.
    pub fn take_ui_beam_hit(&mut self) -> Option<(u16, u16)> {
        self.ui_beam_hit.take()
    }

    /// Replace the debugger's watched-word mirror, forwarding it into
    /// the blitter and floppy write sites. Pending writer records are
    /// dropped (they described the previous watch set).
    pub fn set_ui_mem_watches(&mut self, addrs: &[u32]) {
        self.ui_mem_watch_addrs = addrs.to_vec();
        self.ui_mem_writers.clear();
        self.blitter.set_debug_watch_addrs(addrs);
        self.floppy.set_debug_watch_addrs(addrs);
    }

    /// Note a CPU write of `size` bytes at `addr` for watch attribution
    /// (called from the CPU chip/slow RAM write paths when watches are
    /// armed).
    pub(crate) fn ui_note_cpu_ram_write(&mut self, addr: u32, size: usize) {
        if self.ui_mem_watch_addrs.is_empty() {
            return;
        }
        let start = addr & 0x00FF_FFFE;
        let end = addr.wrapping_add(size.max(1) as u32);
        for &watch in &self.ui_mem_watch_addrs {
            if watch.wrapping_add(1) >= start && watch < end {
                Self::push_ui_mem_writer(
                    &mut self.ui_mem_writers,
                    UiMemWriter {
                        addr: watch,
                        source: crate::debugger::WatchSource::Cpu,
                        vpos: self.agnus.vpos.min(u32::from(u16::MAX)) as u16,
                        hpos: self.agnus.hpos.min(u32::from(u16::MAX)) as u16,
                    },
                );
            }
        }
    }

    fn push_ui_mem_writer(writers: &mut Vec<UiMemWriter>, writer: UiMemWriter) {
        // Latest write per address wins; the list stays tiny.
        writers.retain(|w| w.addr != writer.addr);
        if writers.len() >= 8 {
            writers.remove(0);
        }
        writers.push(writer);
    }

    /// The writer of the most recent write to watched word `addr`, if
    /// one was recorded since the last take. Drains the blitter/floppy
    /// latches lazily, so the hot DMA paths never touch the bus record.
    pub fn ui_take_mem_writer(&mut self, addr: u32) -> Option<UiMemWriter> {
        let vpos = self.agnus.vpos.min(u32::from(u16::MAX)) as u16;
        let hpos = self.agnus.hpos.min(u32::from(u16::MAX)) as u16;
        if let Some((waddr, _)) = self.blitter.take_debug_watched_write() {
            Self::push_ui_mem_writer(
                &mut self.ui_mem_writers,
                UiMemWriter {
                    addr: waddr & 0x00FF_FFFE,
                    source: crate::debugger::WatchSource::Blitter,
                    vpos,
                    hpos,
                },
            );
        }
        if let Some((waddr, _)) = self.floppy.take_debug_watched_write() {
            Self::push_ui_mem_writer(
                &mut self.ui_mem_writers,
                UiMemWriter {
                    addr: waddr & 0x00FF_FFFE,
                    source: crate::debugger::WatchSource::Disk,
                    vpos,
                    hpos,
                },
            );
        }
        let addr = addr & 0x00FF_FFFE;
        let found = self.ui_mem_writers.iter().position(|w| w.addr == addr)?;
        Some(self.ui_mem_writers.remove(found))
    }

    /// Carry the interactive debug state (beam traps, Copper
    /// breakpoints, layer-isolation masks) from the pre-restore bus into
    /// this freshly deserialized one. These fields are transient
    /// (serde-skipped), so without this a reverse step or state load
    /// would silently disarm the debugger. Pending unpolled hits stay
    /// dropped: they describe the abandoned timeline.
    pub(crate) fn adopt_ui_debug_state(&mut self, previous: &Bus) {
        self.ui_beam_traps = previous.ui_beam_traps.clone();
        self.ui_copper_breaks = previous.ui_copper_breaks.clone();
        self.ui_copper_last_pc = previous.ui_copper_last_pc;
        self.ui_layer_masks = previous.ui_layer_masks;
        let watches = previous.ui_mem_watch_addrs.clone();
        self.set_ui_mem_watches(&watches);
    }

    /// The debugger's armed Copper breakpoint addresses.
    pub fn ui_copper_breaks(&self) -> &[u32] {
        &self.ui_copper_breaks
    }

    /// Add a Copper breakpoint at a chip-RAM instruction address, or
    /// remove it when already set. Returns true when now set.
    pub fn ui_toggle_copper_break(&mut self, addr: u32) -> bool {
        let addr = addr & 0x00FF_FFFE;
        match self.ui_copper_breaks.iter().position(|&a| a == addr) {
            Some(pos) => {
                self.ui_copper_breaks.remove(pos);
                false
            }
            None => {
                self.ui_copper_breaks.push(addr);
                true
            }
        }
    }

    pub fn ui_clear_copper_breaks(&mut self) {
        self.ui_copper_breaks.clear();
        self.ui_copper_hit = None;
    }

    /// Take the pending Copper breakpoint hit `(pc, vpos, hpos)`, if any.
    pub fn take_ui_copper_hit(&mut self) -> Option<(u32, u16, u16)> {
        self.ui_copper_hit.take()
    }

    /// The Copper's completed-instruction count, for copper stepping.
    pub fn copper_instructions_retired(&self) -> u64 {
        self.copper.instructions_retired()
    }

    /// The debugger's video-layer isolation masks (Video tab).
    pub fn ui_layer_masks(&self) -> UiLayerMasks {
        self.ui_layer_masks
    }

    /// Toggle bitplane `plane` in the presented picture. Returns true
    /// when the plane is now shown.
    pub fn ui_toggle_layer_plane(&mut self, plane: usize) -> bool {
        self.ui_layer_masks.planes ^= 1 << (plane & 7);
        self.ui_layer_masks.planes & (1 << (plane & 7)) != 0
    }

    /// Toggle sprite `sprite` in the presented picture. Returns true
    /// when the sprite is now shown.
    pub fn ui_toggle_layer_sprite(&mut self, sprite: usize) -> bool {
        self.ui_layer_masks.sprites ^= 1 << (sprite & 7);
        self.ui_layer_masks.sprites & (1 << (sprite & 7)) != 0
    }

    /// Fire a Copper breakpoint when the live Copper's PC has newly
    /// arrived at a breakpointed address. Called from the live copper
    /// step path (never the blitter-deadline predictor's cloned
    /// simulation) and after CPU strobe writes, so every way the PC can
    /// move is observed.
    pub(super) fn check_ui_copper_breaks(&mut self) {
        let pc = self.copper.pc();
        if pc == self.ui_copper_last_pc {
            return;
        }
        self.ui_copper_last_pc = pc;
        if self.ui_copper_hit.is_none() && self.ui_copper_breaks.contains(&pc) {
            self.ui_copper_hit = Some((
                pc,
                self.agnus.vpos.min(u32::from(u16::MAX)) as u16,
                self.agnus.hpos.min(u32::from(u16::MAX)) as u16,
            ));
        }
    }

    /// Fire beam traps crossed by the last beam advance from `old`
    /// (exclusive) to the current position (inclusive), in beam order.
    /// `old_frame_lines` bounds targets within the frame the advance
    /// started in, so a trap on a line the scan never reaches does not
    /// fire spuriously at the frame wrap. Fired one-shot traps are
    /// removed; persistent traps stay armed and re-fire every frame.
    pub(super) fn check_ui_beam_traps(
        &mut self,
        old: (u32, u32),
        old_frame_lines: u32,
        new_frames: u32,
    ) {
        let cur = (self.agnus.vpos, self.agnus.hpos);
        let hit_slot = &mut self.ui_beam_hit;
        self.ui_beam_traps.retain(|trap| {
            let target = (u32::from(trap.vpos), u32::from(trap.hpos.unwrap_or(0)));
            let hit = match new_frames {
                0 => old < target && target <= cur,
                1 => (target > old && target.0 < old_frame_lines) || target <= cur,
                // The advance spanned whole frames: any reachable
                // position was crossed.
                _ => target.0 < old_frame_lines || target <= cur,
            };
            if hit && hit_slot.is_none() {
                *hit_slot = Some((trap.vpos, trap.hpos.unwrap_or(0)));
            }
            !(hit && trap.once)
        });
    }

    pub fn attach_gayle(&mut self, gayle: Gayle) {
        self.gayle = Some(gayle);
    }

    pub fn attach_akiko(&mut self, akiko: crate::akiko::Akiko) {
        self.akiko = Some(akiko);
    }

    /// Attach the functional Zorro-chain boards. Their slot indices must match
    /// the [`crate::zorro::BoardBacking::Device`] indices assigned when the
    /// boards were added to the chain.
    pub fn attach_devices(&mut self, devices: Vec<crate::zorro_device::BoardDevice>) {
        self.devices = devices;
    }

    pub fn attach_cdtv(&mut self, cdtv: crate::cdtv::CdtvController) {
        self.cdtv = Some(cdtv);
    }

    pub fn set_rtc_present(&mut self, present: bool) {
        self.rtc_present = present;
    }

    pub fn set_video_standard(&mut self, video_standard: VideoStandard) {
        self.agnus.set_video_standard(video_standard);
        self.current_frame_geometry.frame_lines = fixed_standard_frame_lines(
            video_standard,
            self.current_frame_geometry.lace,
            self.current_frame_render_base.long_field,
        );
        self.last_frame_geometry.frame_lines = fixed_standard_frame_lines(
            video_standard,
            self.last_frame_geometry.lace,
            self.frame_render_base().long_field,
        );
    }

    /// Preset entry point: an OCS Agnus pairs with an OCS Denise, an ECS
    /// Agnus with an ECS Denise. Mixed machines (e.g. late A500s with an ECS
    /// Agnus and OCS Denise) go through set_chipset_revisions.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_agnus_revision(&mut self, revision: AgnusRevision) {
        let denise = if revision.is_ecs() {
            DeniseRevision::Ecs8373
        } else {
            DeniseRevision::Ocs
        };
        self.set_chipset_revisions(revision, denise);
    }

    pub fn set_chipset_revisions(&mut self, agnus: AgnusRevision, denise: DeniseRevision) {
        self.agnus.set_revision(agnus);
        self.denise_revision = denise;
        if !denise.is_ecs() {
            self.denise.diwhigh = 0;
            self.denise.diwhigh_written = false;
        }
        self.configure_chip_dma_masks();
    }

    fn denise_ecs_registers(&self) -> bool {
        self.denise_revision.is_ecs()
    }

    /// ECS Denise ENBPLCN3: BPLCON0 bit 0 must be set for BPLCON3 writes to
    /// latch (8373 spec). OCS Denise has no BPLCON3 at all; AGA Lisa always
    /// accepts the write (the palette BANK/LOCT mechanics depend on it).
    fn bplcon3_write_enabled(&self) -> bool {
        self.denise_is_lisa()
            || (self.denise_ecs_registers() && self.denise.bplcon0 & BPLCON0_ECSENA != 0)
    }

    fn denise_is_lisa(&self) -> bool {
        matches!(self.denise_revision, DeniseRevision::AgaLisa)
    }

    /// AGA plane-count decode applies (Alice fetch + Lisa display; mixed
    /// AGA/non-AGA chip pairs never shipped).
    fn aga_enabled(&self) -> bool {
        matches!(self.agnus.revision(), AgnusRevision::AgaAlice)
    }

    pub fn reset_custom_chips_from_cpu_reset(&mut self) {
        // The CPU RESET instruction asserts the external reset line without
        // resetting the CPU core or clearing RAM. Reuse the warm device reset
        // so CIAs, custom chips, expansion boards, and /OVL all return to
        // reset state, then restore the emulator's monotonic time coordinate.
        let emulated_cck = self.emulated_cck;
        let emulated_frames = self.emulated_frames;
        self.reset_for_keyboard_reset();
        self.emulated_cck = emulated_cck;
        self.emulated_frames = emulated_frames;
    }

    fn effective_diwhigh(&self) -> DiwHigh {
        if self.denise_ecs_registers() && self.denise.diwhigh_written {
            DiwHigh::ecs_explicit(self.denise.diwhigh)
        } else {
            DiwHigh::ocs_implicit()
        }
    }

    fn configure_chip_dma_masks(&mut self) {
        // The writable DMA pointer high bits follow the Agnus revision's
        // address-bus reach as well as the installed chip RAM: an 8372A
        // drops bit 20 even with 2 MB fitted, and OCS stops at 512 KiB.
        let mask = chip_dma_addr_mask(self.mem.chip_ram.len())
            & self.agnus.revision().dma_addr_capability_mask();
        self.chip_dma_mask = mask;
        self.agnus.set_dma_addr_mask(mask);
        self.denise.set_dma_addr_mask(mask);
        self.blitter.set_dma_addr_mask(mask);
        self.paula.set_dma_addr_mask(mask);
        self.floppy.set_dma_addr_mask(mask);
        let ptr_mask = mask & !1;
        for ptr in &mut self.display_dma_bplpt {
            *ptr &= ptr_mask;
        }
        for ptr in &mut self.display_dma_sprpt {
            *ptr &= ptr_mask;
        }
    }

    fn blitter_ecs_registers_enabled(&self) -> bool {
        !matches!(self.agnus.revision(), AgnusRevision::Ocs)
    }

    /// Relaxed DDF stop ceiling for every effective-DDF-window computation:
    /// BEAMCON0.HARDDIS on ECS, and always on AGA Alice (the AGA fetch
    /// sequencer has no hardwired $D8 stop; the relaxed ceiling is still
    /// bounded by the canvas).
    fn harddis_active(&self) -> bool {
        self.aga_enabled()
            || (!matches!(self.agnus.revision(), AgnusRevision::Ocs)
                && self.agnus.beamcon0() & BEAMCON0_HARDDIS != 0)
    }

    /// External light-pen pulse at the current beam position. No input
    /// device is wired to this yet; tests (and future controller-port
    /// plumbing) call it directly.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn light_pen_pulse(&mut self) {
        self.agnus.trigger_light_pen();
    }

    /// Queue an Amiga raw key press (a 7-bit raw scan code).
    pub fn enqueue_key(&mut self, amiga_raw_keycode: u8) {
        self.enqueue_key_event(amiga_raw_keycode, true);
    }

    /// Queue an Amiga raw key transition with the keyboard MCU, which
    /// serializes it as `~((rawkey << 1) | up_down)` over KCLK/KDAT.
    pub fn enqueue_key_event(&mut self, amiga_raw_keycode: u8, pressed: bool) {
        self.keyboard.key_transition(amiga_raw_keycode, pressed);
    }

    /// Colour clocks until the keyboard MCU next needs to act (KCLK
    /// edge or protocol timeout); caps the emulator's idle fast-forward.
    pub fn next_keyboard_event_cck(&self) -> Option<u32> {
        self.keyboard.next_event_cck()
    }

    /// Reset the per-frame render/capture buffers to their empty starting
    /// state. Shared by a real hardware reset (`reset_for_keyboard_reset`)
    /// and the state-load path (`reset_transient_video_after_state_load`)
    /// for exactly the fields those two callers already reset to
    /// identical values, so a new capture-buffer field only has to be
    /// listed here once instead of drifting between the two copies.
    ///
    /// Deliberately NOT included: `current_frame_render_blocked` and
    /// `sprite_dma_frame_start_ptr` (the two callers legitimately disagree
    /// on their value -- a state load derives them from the just-restored
    /// beam/pointer state instead of zeroing them), and the fields only
    /// `reset_for_keyboard_reset` touches (palettes, collision tracking,
    /// frame geometry, DMA pointers/delays) because those are either
    /// power-on-only resets or are themselves part of the serialized save
    /// state and must not be clobbered after a state load.
    fn reset_frame_capture_buffers(&mut self) {
        self.last_frame_render_base = None;
        self.last_frame_render_events.clear();
        self.last_frame_beam_bottom_palette_events.clear();
        self.current_frame_chip_ram.clear();
        self.current_frame_chip_ram
            .extend_from_slice(&self.mem.chip_ram);
        self.last_frame_chip_ram.clear();
        self.current_frame_chip_ram_writes.clear();
        self.last_frame_chip_ram_writes.clear();
        self.current_frame_bitplane_rows = empty_captured_bitplane_rows();
        self.last_frame_bitplane_rows = empty_captured_bitplane_rows();
        self.current_frame_sprite_lines.clear();
        clear_captured_sprite_lines_by_y(&mut self.current_frame_sprite_lines_by_y);
        self.current_frame_sprite_collision_sources = empty_sprite_collision_sources();
        self.last_frame_sprite_lines.clear();
        self.current_frame_held_sprites = [None; 8];
        self.last_frame_held_sprites = [None; 8];
        self.current_frame_sprite_display_enable_x_by_y = empty_sprite_display_enable_x_by_y();
        self.last_frame_sprite_display_enable_x_by_y = empty_sprite_display_enable_x_by_y();
        self.current_frame_sprite_dma_observed = false;
        self.last_frame_sprite_dma_observed = false;
        self.current_frame_bus_trace.clear();
        self.last_frame_bus_trace = None;
    }

    pub fn reset_for_keyboard_reset(&mut self) {
        let video_standard = self.agnus.video_standard();
        let agnus_revision = self.agnus.revision();
        self.cia_a = Cia::new(Which::A);
        self.cia_b = Cia::new(Which::B);
        self.paula.reset_registers();
        self.agnus = Agnus::with_video_standard_and_revision(video_standard, agnus_revision);
        self.copper = Copper::new();
        self.denise = Denise::new();
        self.blitter = Blitter::new();
        self.configure_chip_dma_masks();
        self.rtc = Msm6242Rtc::default();
        if let Some(gayle) = self.gayle.as_mut() {
            gayle.reset();
        }
        if let Some(akiko) = self.akiko.as_mut() {
            akiko.reset();
        }
        if let Some(cdtv) = self.cdtv.as_mut() {
            cdtv.reset();
        }
        for dev in &mut self.devices {
            crate::zorro_device::ZorroDevice::reset(dev);
        }
        self.mem.zorro.warm_reset();
        self.hdd_led_until_cck = 0;
        self.overlay_disable_pending = false;
        // The MCU restarts its power-up flow; physically held keys stay
        // held and are reported in the upcoming $FD/$FE stream.
        self.keyboard.begin_power_up();
        self.keyboard_system_reset_pending = false;
        self.input = InputState::default();
        self.video_pipeline_stats = VideoPipelineStats::default();
        self.slice_preempted = false;
        self.pending_vbi = 0;
        self.pending_copper_frame_start = None;
        self.copper_active_in_frame = false;
        self.copper_current_list = 1;
        self.delivered_irq_pending = 0;
        self.pending_copper_irq_beam = None;
        self.delivered_copper_irq_beam = None;
        self.coper_cpu_irq_delay_cck = 0;
        self.irq_latency_cck = 0;
        self.irq_latency_mask = 0;
        self.irq_latency_last_pending = 0;
        self.beam_top_palette = Palette::new();
        self.beam_bottom_palette = Palette::new();
        self.beam_bottom_palette_valid = false;
        self.cpu_palette_target = CpuPaletteTarget::Top;
        self.cpu_palette_target_writes = 0;
        self.cpu_palette_target_beam = None;
        self.current_frame_render_base = RenderRegisterSnapshot::default();
        self.current_frame_render_events.clear();
        self.current_frame_collision_events.clear();
        self.current_frame_collision_control_events.clear();
        self.current_frame_collision_bpldat_events.clear();
        self.current_frame_collision_sprite_events.clear();
        self.current_frame_collision_control_index = None;
        self.current_frame_collision_bpldat_index = None;
        self.current_frame_collision_sprite_index = None;
        self.current_frame_collision_may_have_dual_playfield = false;
        self.beam_bottom_palette_events.clear();
        self.pending_beam_bottom_palette_events.clear();
        self.current_frame_beam_top_palette = self.beam_top_palette;
        self.last_frame_beam_top_palette = Palette::new();
        self.last_frame_beam_top_palette_end = Palette::new();
        self.last_frame_beam_bottom_palette = Palette::new();
        self.last_frame_beam_bottom_palette_valid = false;
        self.reset_frame_capture_buffers();
        self.display_dma_sprite_state = [DisplaySpriteDmaState::default(); 8];
        self.current_frame_display_snapshot_taken = false;
        self.ocs_same_line_diw_start_blocked_vpos = None;
        self.current_frame_render_blocked = false;
        self.current_frame_visible_start_vpos = RENDER_VISIBLE_START_VPOS;
        self.last_frame_visible_start_vpos = RENDER_VISIBLE_START_VPOS;
        let frame_lines = self.agnus.current_frame_lines();
        self.current_frame_geometry =
            FrameGeometry::standard(RENDER_VISIBLE_START_VPOS, frame_lines, false);
        self.last_frame_geometry =
            FrameGeometry::standard(RENDER_VISIBLE_START_VPOS, frame_lines, false);
        self.lazy_collision_vpos = RENDER_VISIBLE_START_VPOS;
        self.lazy_collision_hpos = RENDER_COPPER_WAIT_HPOS_FB0;
        self.collision_tracking_active = false;
        self.cpu_bus_arbitration_enabled = false;
        self.blitter_slowdown_cpu_misses = 0;
        self.slice_bus_advanced_cck = 0;
        self.slice_bus_tick = AgnusTick::default();
        self.last_chip_bus_owner = ChipBusOwner::Idle;
        self.device_clock.reset();
        self.emulated_cck = 0;
        self.emulated_frames = 0;
        self.display_dma_bplpt = [0; 8];
        self.display_dma_sprpt = [0; 8];
        self.sprite_dma_frame_start_ptr = [0; 8];
        self.display_dma_clipped_rows_advanced = false;
        self.bitplane_dmacon_delay = None;
        self.bitplane_bplcon0_delay = None;
        self.bitplane_ddfstart_miss = None;
        self.mem.overlay = true;
        // A 68000 RESET returns the A1000 WCS latch to boot mode (boot ROM at
        // $F80000, WCS writable) without clearing the WCS, so the boot ROM can
        // re-run the Kickstart it already loaded instead of reading the disk.
        self.mem.wcs_write_protected = false;
        self.floppy.reset_external_drives();
        self.floppy.write_prb(0xFF);
    }

    /// Full cold boot. Clears RAM to its power-on (zeroed) state and then
    /// runs the same chip/CIA reset as a keyboard reset. Unlike
    /// Ctrl-Amiga-Amiga, RAM is not preserved, so the machine comes up as
    /// if it had been power-cycled. Clear RAM first so the keyboard-reset
    /// path snapshots the zeroed chip RAM for the renderer.
    pub fn power_on_reset(&mut self) {
        self.mem.power_on_reset();
        self.reset_for_keyboard_reset();
    }

    pub fn front_panel_status(&self) -> FrontPanelStatus {
        let pra = self.cia_a.peek_register(REG_PRA);
        FrontPanelStatus {
            power_led_on: pra & 0x02 == 0,
            fdd_led_on: self.floppy.activity_led_on(),
            fdd_track: self.floppy.selected_track(),
            hdd_led: (self.gayle.is_some() || self.has_scsi_device())
                .then_some(self.emulated_cck < self.hdd_led_until_cck),
            cd_led: self
                .cdtv
                .as_ref()
                .map(|cdtv| cdtv.activity_led_on())
                .or_else(|| self.akiko.as_ref().map(|akiko| akiko.activity_led_on())),
            output_volume_percent: self.paula.output_volume_percent(),
        }
    }

    /// Whether the machine has a CD drive (CDTV DMAC or CD32 Akiko).
    pub fn cd_drive_present(&self) -> bool {
        self.cdtv.is_some() || self.akiko.is_some()
    }

    /// Whether a SCSI controller board is fitted (lights the HDD LED).
    fn has_scsi_device(&self) -> bool {
        self.devices
            .iter()
            .any(|d| matches!(d, crate::zorro_device::BoardDevice::A2091(_)))
    }

    /// Whether a disc is mounted (or, on CDTV, waiting in the tray).
    pub fn cd_disc_inserted(&self) -> bool {
        self.cdtv.as_ref().is_some_and(|cdtv| cdtv.has_disc())
            || self.akiko.as_ref().is_some_and(|akiko| akiko.has_disc())
    }

    /// Runtime disc insert with media-change notification. On CDTV the
    /// disc lands after a short tray delay (the same media-change STCH
    /// path as `[cd] insert_delay`); Akiko mounts immediately and
    /// volunteers a media-status packet.
    pub fn cd_insert_disc(&mut self, image: crate::cdrom::CdImage) {
        // A second or so of tray time keeps the eject and insert
        // media-change interrupts distinct for cdtv.device.
        const CDTV_TRAY_SECS: f64 = 1.0;
        if let Some(cdtv) = self.cdtv.as_mut() {
            cdtv.eject_disc();
            cdtv.insert_disc_after(image, CDTV_TRAY_SECS);
        } else if let Some(akiko) = self.akiko.as_mut() {
            // Model tray time on Akiko too: eject (media-absent) then mount
            // after a delay (media-present), so cd.device sees the
            // absent->present change instead of an instantaneous swap.
            akiko.insert_disc_after(image, CDTV_TRAY_SECS);
        }
    }

    /// Runtime disc eject with media-change notification.
    pub fn cd_eject_disc(&mut self) {
        if let Some(cdtv) = self.cdtv.as_mut() {
            cdtv.eject_disc();
        } else if let Some(akiko) = self.akiko.as_mut() {
            akiko.eject_disc();
        }
    }

    /// Record Gayle IDE activity: keep the HDD LED lit for a short stretch
    /// of emulated time (transfers complete synchronously, so without a
    /// hold the LED would never be visibly on).
    pub fn note_hdd_activity(&mut self) {
        // ~100 ms of emulated time per activity burst.
        const HDD_LED_HOLD_CCK: u64 = (PAULA_CLOCK_HZ / 10) as u64;
        self.hdd_led_until_cck = self.emulated_cck + HDD_LED_HOLD_CCK;
    }

    /// Move the host-side resources that a save state does not capture from
    /// the currently live Bus into this freshly deserialized one: the Paula
    /// audio/serial sinks and the open blitter trace file. The realtime
    /// device-clock anchor needs no carry-over -- it deserializes as None
    /// and `realtime_cck_due` re-anchors to the host clock on first use.
    pub(crate) fn adopt_host_resources(&mut self, live: &mut Bus) {
        std::mem::swap(&mut self.paula.serial, &mut live.paula.serial);
        std::mem::swap(&mut self.paula.audio, &mut live.paula.audio);
        self.blitter_trace = live.blitter_trace.take();
    }

    pub(crate) fn reset_transient_video_after_state_load(&mut self) {
        let current_frame_lines = self.frame_lines_for_geometry(
            self.current_frame_geometry,
            self.current_frame_render_base.long_field,
        );
        self.current_frame_geometry.frame_lines = current_frame_lines;
        self.last_frame_geometry.frame_lines = current_frame_lines;
        if self.current_frame_geometry.programmable {
            self.current_frame_visible_start_vpos = self.current_frame_geometry.visible_start_vpos;
        } else {
            self.current_frame_visible_start_vpos = RENDER_MIN_OVERSCAN_START_VPOS;
            self.current_frame_geometry.visible_start_vpos = RENDER_MIN_OVERSCAN_START_VPOS;
        }
        self.last_frame_visible_start_vpos = self.current_frame_visible_start_vpos;
        self.last_frame_geometry.visible_start_vpos = self.current_frame_visible_start_vpos;
        self.lazy_collision_vpos = self.current_frame_visible_start_vpos;
        self.ocs_same_line_diw_start_blocked_vpos = None;
        self.reset_frame_capture_buffers();
        self.current_frame_render_blocked = self.agnus.vpos != 0 || self.agnus.hpos != 0;
    }

    pub(crate) fn reset_transient_diagnostics_after_state_load(&mut self) {
        // The code-path ring that consumes this alarm is debugger-local state,
        // so a serialized pending alarm has no useful history after a restore.
        self.diag_lowmem_blit = false;
    }

    pub fn emulated_seconds(&self) -> f64 {
        self.emulated_cck as f64 / PAULA_CLOCK_HZ as f64
    }

    pub fn emulated_frames(&self) -> u64 {
        self.emulated_frames
    }

    /// Total colour clocks emulated since power-on. The monotonic timeline
    /// coordinate behind `emulated_seconds`; used by reverse debugging to
    /// label snapshots and report the beam time of a reconstructed event.
    pub fn emulated_cck(&self) -> u64 {
        self.emulated_cck
    }

    /// Publish the emulated-to-host time mapping to the serial sink so a
    /// timing-sensitive backend (MIDI) can schedule output. See
    /// [`crate::serial::SerialTimeAnchor`].
    pub fn set_serial_time_anchor(&mut self, anchor: crate::serial::SerialTimeAnchor) {
        self.paula.set_serial_time_anchor(anchor);
    }

    /// The live MIDI sink, when the serial port is in MIDI mode, for switching
    /// devices from the runtime menu.
    #[cfg(feature = "midi")]
    pub fn midi_serial_mut(&mut self) -> Option<&mut crate::midi::MidiSerialSink> {
        self.paula.serial.as_midi()
    }

    pub fn live_audio_output_lead_seconds(&self) -> f64 {
        self.paula.live_audio_output_lead_seconds()
    }

    pub fn live_audio_status(&self) -> crate::audio::AudioRuntimeStatus {
        self.paula.live_audio_status()
    }

    pub fn set_live_audio_suspended(&mut self, suspended: bool) {
        self.paula.set_live_audio_suspended(suspended);
    }

    pub fn reset_live_audio_after_timeline_jump(&mut self) {
        self.paula.reset_live_audio_after_timeline_jump();
    }

    pub fn output_volume_percent(&self) -> u8 {
        self.paula.output_volume_percent()
    }

    pub fn set_output_volume_percent(&mut self, percent: u8) {
        self.paula.set_output_volume_percent(percent);
    }

    pub fn adjust_output_volume_percent(&mut self, delta: i16) {
        let adjusted = i16::from(self.output_volume_percent()).saturating_add(delta);
        self.set_output_volume_percent(adjusted.clamp(0, 100) as u8);
    }

    pub fn dump_video_pipeline_stats(&self, label: &str) {
        self.video_pipeline_stats.dump(label);
    }

    pub fn reset_profile_stats(&mut self) {
        self.video_pipeline_stats = VideoPipelineStats::default();
        self.poll_stats = PollStats::default();
    }

    pub(crate) fn record_video_render_frame(&mut self, timing: VideoRenderFrameTiming) {
        let stats = &mut self.video_pipeline_stats;
        stats.render_frames = stats.render_frames.saturating_add(1);
        stats.render_events = stats.render_events.saturating_add(timing.events);
        stats.render_control_segments = stats
            .render_control_segments
            .saturating_add(timing.control_segments);
        stats.render_playfield_pixels = stats
            .render_playfield_pixels
            .saturating_add(timing.playfield_pixels);
        stats.render_manual_bpl_segments = stats
            .render_manual_bpl_segments
            .saturating_add(timing.manual_bpl_segments);
        stats.render_sprite_lines = stats
            .render_sprite_lines
            .saturating_add(timing.sprite_lines);
        stats.render_total_nanos = stats.render_total_nanos.saturating_add(timing.total_nanos);
        stats.render_event_nanos = stats.render_event_nanos.saturating_add(timing.event_nanos);
        stats.render_background_nanos = stats
            .render_background_nanos
            .saturating_add(timing.background_nanos);
        stats.render_playfield_nanos = stats
            .render_playfield_nanos
            .saturating_add(timing.playfield_nanos);
        stats.render_manual_bpl_nanos = stats
            .render_manual_bpl_nanos
            .saturating_add(timing.manual_bpl_nanos);
        stats.render_sprite_nanos = stats
            .render_sprite_nanos
            .saturating_add(timing.sprite_nanos);
    }

    fn record_bitplane_fetch_timing(
        &mut self,
        slots: usize,
        rows_started: usize,
        rows_completed: usize,
        elapsed: Option<(Duration, u128)>,
    ) {
        if slots == 0 && rows_started == 0 && rows_completed == 0 {
            return;
        }
        let stats = &mut self.video_pipeline_stats;
        stats.bitplane_fetch_calls = stats.bitplane_fetch_calls.saturating_add(1);
        stats.bitplane_fetch_slots = stats.bitplane_fetch_slots.saturating_add(slots as u64);
        stats.bitplane_fetch_rows_started = stats
            .bitplane_fetch_rows_started
            .saturating_add(rows_started as u64);
        stats.bitplane_fetch_rows_completed = stats
            .bitplane_fetch_rows_completed
            .saturating_add(rows_completed as u64);
        VideoPipelineStats::add_sampled_duration(&mut stats.bitplane_fetch_nanos, elapsed);
    }

    fn record_sprite_fetch_timing(
        &mut self,
        pair_slots: usize,
        lines: usize,
        elapsed: Option<(Duration, u128)>,
    ) {
        if pair_slots == 0 {
            return;
        }
        let stats = &mut self.video_pipeline_stats;
        stats.sprite_fetch_calls = stats.sprite_fetch_calls.saturating_add(1);
        stats.sprite_fetch_pair_slots = stats
            .sprite_fetch_pair_slots
            .saturating_add(pair_slots as u64);
        stats.sprite_fetch_lines = stats.sprite_fetch_lines.saturating_add(lines as u64);
        VideoPipelineStats::add_sampled_duration(&mut stats.sprite_fetch_nanos, elapsed);
    }

    fn record_live_collision_timing(
        &mut self,
        pixels: u64,
        control_segments: usize,
        full_line_scan: bool,
        elapsed: Option<(Duration, u128)>,
    ) {
        if pixels == 0 {
            return;
        }
        let stats = &mut self.video_pipeline_stats;
        stats.collision_calls = stats.collision_calls.saturating_add(1);
        stats.collision_pixels = stats.collision_pixels.saturating_add(pixels);
        stats.collision_control_segments = stats
            .collision_control_segments
            .saturating_add(control_segments as u64);
        if full_line_scan {
            stats.collision_full_line_scans = stats.collision_full_line_scans.saturating_add(1);
        }
        VideoPipelineStats::add_sampled_duration(&mut stats.collision_nanos, elapsed);
    }

    fn ensure_current_collision_control_index(&mut self) {
        if self.current_frame_collision_control_index.is_none() {
            self.current_frame_collision_control_index = Some(
                BeamEventIndex::from_register_writes(&self.current_frame_collision_control_events),
            );
        }
    }

    fn ensure_current_collision_bpldat_index(&mut self) {
        if self.current_frame_collision_bpldat_index.is_none() {
            self.current_frame_collision_bpldat_index = Some(BeamEventIndex::from_register_writes(
                &self.current_frame_collision_bpldat_events,
            ));
        }
    }

    fn ensure_current_collision_sprite_index(&mut self) {
        if self.current_frame_collision_sprite_index.is_none() {
            self.current_frame_collision_sprite_index = Some(BeamEventIndex::from_register_writes(
                &self.current_frame_collision_sprite_events,
            ));
        }
    }

    fn live_playfield_collision_may_have_dual_playfield(&self) -> bool {
        self.current_frame_collision_may_have_dual_playfield
            || self.denise.bplcon0 & 0x0400 != 0
            || self.current_frame_render_base.bplcon0 & 0x0400 != 0
    }

    fn trace_blitter_start(&mut self, bltsize: u16, source: BeamWriteSource) {
        let mut h = ((bltsize >> 6) & 0x03FF) as u32;
        if h == 0 {
            h = 1024;
        }
        let mut w = (bltsize & 0x003F) as u32;
        if w == 0 {
            w = 64;
        }
        self.trace_blitter_start_dims("bltsize", bltsize, h, w, source);
    }

    fn trace_blitter_start_ecs(&mut self, bltsizh: u16, source: BeamWriteSource) {
        let mut h = (self.blitter.bltsizv & 0x7FFF) as u32;
        if h == 0 {
            h = 32_768;
        }
        let mut w = (bltsizh & 0x07FF) as u32;
        if w == 0 {
            w = 2_048;
        }
        self.trace_blitter_start_dims("bltsizh", bltsizh, h, w, source);
    }

    fn trace_blitter_start_dims(
        &mut self,
        event_name: &str,
        size_register: u16,
        h: u32,
        w: u32,
        source: BeamWriteSource,
    ) {
        if self.blitter_trace.is_none() {
            return;
        }
        let con0 = self.blitter.bltcon0;
        let con1 = self.blitter.bltcon1;
        let line = con1 & 0x0001 != 0;
        let fill = !line && con1 & 0x0018 != 0;
        let source = beam_write_source_name(source);
        let entry = format!(
            "{{\"event\":\"{}\",\"source\":\"{}\",\"emu_secs\":{:.6},\"emu_frame\":{},\"vpos\":{},\"hpos\":{},\"bltsize\":{},\"h\":{},\"w\":{},\"line\":{},\"fill\":{},\"line_octant\":{},\"bltcon0\":{},\"bltcon1\":{},\"use_a\":{},\"use_b\":{},\"use_c\":{},\"use_d\":{},\"lf\":{},\"ash\":{},\"bsh\":{},\"sign\":{},\"sing\":{},\"desc\":{},\"ife\":{},\"efe\":{},\"fci\":{},\"bltafwm\":{},\"bltalwm\":{},\"bltapt\":{},\"bltbpt\":{},\"bltcpt\":{},\"bltdpt\":{},\"bltamod\":{},\"bltbmod\":{},\"bltcmod\":{},\"bltdmod\":{},\"bltadat\":{},\"bltbdat\":{},\"bltcdat\":{},\"dmacon\":{},\"fmode\":{},\"bplcon0\":{},\"bplcon1\":{},\"bplcon2\":{},\"bpl1mod\":{},\"bpl2mod\":{},\"ddfstrt\":{},\"ddfstop\":{},\"diwstrt\":{},\"diwstop\":{},\"bplpt\":[{},{},{},{},{},{},{},{}]}}",
            event_name,
            source,
            self.emulated_seconds(),
            self.emulated_frames,
            self.agnus.vpos,
            self.agnus.hpos,
            size_register,
            h,
            w,
            line,
            fill,
            (con1 >> 2) & 0x0007,
            con0,
            con1,
            con0 & 0x0800 != 0,
            con0 & 0x0400 != 0,
            con0 & 0x0200 != 0,
            con0 & 0x0100 != 0,
            con0 & 0x00FF,
            (con0 >> 12) & 0x000F,
            (con1 >> 12) & 0x000F,
            con1 & 0x0040 != 0,
            con1 & 0x0002 != 0,
            con1 & 0x0002 != 0 && !line,
            con1 & 0x0008 != 0,
            con1 & 0x0010 != 0,
            con1 & 0x0004 != 0,
            self.blitter.bltafwm,
            self.blitter.bltalwm,
            self.blitter.bltapt,
            self.blitter.bltbpt,
            self.blitter.bltcpt,
            self.blitter.bltdpt,
            self.blitter.bltamod,
            self.blitter.bltbmod,
            self.blitter.bltcmod,
            self.blitter.bltdmod,
            self.blitter.bltadat,
            self.blitter.bltbdat,
            self.blitter.bltcdat,
            self.agnus.dmacon,
            self.agnus.fmode(),
            self.denise.bplcon0,
            self.denise.bplcon1,
            self.denise.bplcon2,
            self.denise.bpl1mod,
            self.denise.bpl2mod,
            self.denise.ddfstrt,
            self.denise.ddfstop,
            self.denise.diwstrt,
            self.denise.diwstop,
            self.denise.bplpt[0],
            self.denise.bplpt[1],
            self.denise.bplpt[2],
            self.denise.bplpt[3],
            self.denise.bplpt[4],
            self.denise.bplpt[5],
            self.denise.bplpt[6],
            self.denise.bplpt[7],
        );
        if let Some(file) = self.blitter_trace.as_mut() {
            let _ = writeln!(file, "{entry}");
        }
    }

    fn trace_blitter_forced_finish(&mut self, was_busy: bool) {
        let secs = self.emulated_seconds();
        let frames = self.emulated_frames;
        let vpos = self.agnus.vpos;
        let hpos = self.agnus.hpos;
        let Some(file) = self.blitter_trace.as_mut() else {
            return;
        };
        let _ = writeln!(
            file,
            "{{\"event\":\"forced_finish\",\"emu_secs\":{secs:.6},\"emu_frame\":{frames},\"vpos\":{vpos},\"hpos\":{hpos},\"was_busy\":{was_busy}}}"
        );
    }

    fn trace_blitter_completion(&mut self, source: &'static str, intreq_before: u16) {
        let secs = self.emulated_seconds();
        let frames = self.emulated_frames;
        let vpos = self.agnus.vpos;
        let hpos = self.agnus.hpos;
        let intreq = self.paula.intreq;
        let intena = self.paula.intena;
        let dmacon = self.agnus.dmacon;
        let fmode = self.agnus.fmode();
        let busy = self.blitter.busy;
        let bzero = self.blitter.bzero;
        let bltcon0 = self.blitter.bltcon0;
        let bltcon1 = self.blitter.bltcon1;
        let bltdpt = self.blitter.bltdpt;
        let Some(file) = self.blitter_trace.as_mut() else {
            return;
        };
        let _ = writeln!(
            file,
            "{{\"event\":\"completion\",\"source\":\"{source}\",\"emu_secs\":{secs:.6},\"emu_frame\":{frames},\"vpos\":{vpos},\"hpos\":{hpos},\"intreq_before\":{intreq_before},\"intreq\":{intreq},\"intena\":{intena},\"dmacon\":{dmacon},\"fmode\":{fmode},\"busy\":{busy},\"bzero\":{bzero},\"bltcon0\":{bltcon0},\"bltcon1\":{bltcon1},\"bltdpt\":{bltdpt}}}"
        );
    }

    fn trace_dmaconr_read(&mut self, value: u16) {
        let secs = self.emulated_seconds();
        let frames = self.emulated_frames;
        let vpos = self.agnus.vpos;
        let hpos = self.agnus.hpos;
        let busy = self.blitter.busy;
        let bzero = self.blitter.bzero;
        let fmode = self.agnus.fmode();
        let Some(file) = self.blitter_trace.as_mut() else {
            return;
        };
        let _ = writeln!(
            file,
            "{{\"event\":\"dmaconr_read\",\"emu_secs\":{secs:.6},\"emu_frame\":{frames},\"vpos\":{vpos},\"hpos\":{hpos},\"value\":{value},\"fmode\":{fmode},\"busy\":{busy},\"bzero\":{bzero}}}"
        );
    }

    pub fn set_cpu_bus_arbitration_enabled(&mut self, enabled: bool) {
        // Diagnostic builds can force CPU chip-bus arbitration off for A/B
        // timing experiments. Normal builds always keep hardware contention on.
        let enabled = enabled && !no_bus_arb();
        self.cpu_bus_arbitration_enabled = enabled;
        if !enabled {
            self.blitter_slowdown_cpu_misses = 0;
        }
        self.cpu_clock_carry = 0;
        self.ext_clock_carry_x100 = 0;
    }

    /// Configure the CPU clock ratio ([cpu] clock_mhz) so external-access and
    /// CPU-internal billing scale with the configured CPU speed.
    pub fn set_cpu_clocks_per_cck(&mut self, clocks: u32) {
        self.cpu_clocks_per_cck = clocks.max(1);
    }

    /// Select the chip-bus cycle length: the 68020+ completes a word access in
    /// 3 CPU clocks where the 68000 takes 4, so its post-grant tail is shorter
    /// (write-posting; faster reads). Derived from the CPU model.
    /// Whether the CPU runs the shorter 020+ bus cycle (see the field doc);
    /// also distinguishes the 68000's shared chip data bus from the 020+
    /// local bus for the floating-bus latch.
    pub(crate) fn cpu_short_bus_cycle(&self) -> bool {
        self.cpu_short_bus_cycle
    }

    pub fn set_cpu_short_bus_cycle(&mut self, enabled: bool) {
        self.cpu_short_bus_cycle = enabled;
    }

    /// Advance the chipset for CPU-internal (non-bus) clocks reported by the
    /// cycle-exact core via `AddressBus::sync`. The chip bus stays free for
    /// DMA during this time; timed devices tick along. Sub-cck clock counts
    /// carry over to the next call so no time is lost to the
    /// clocks-per-cck conversion.
    pub fn sync_cpu_internal_clocks(&mut self, cpu_clocks: u32) {
        if !self.cpu_bus_arbitration_enabled {
            return;
        }
        let total = cpu_clocks + std::mem::take(&mut self.cpu_clock_carry);
        let cck = total / self.cpu_clocks_per_cck;
        self.cpu_clock_carry = total % self.cpu_clocks_per_cck;
        if cck == 0 {
            return;
        }
        let tick = self.advance_chipset(cck);
        self.record_slice_bus_advance(cck, tick);
    }

    /// Advance the chipset for a CPU access to memory that is NOT on the chip
    /// bus (ROM, fast RAM): the bus cycle takes 4 CPU clocks per word at the
    /// configured CPU clock, during which the chip bus is entirely free for
    /// DMA. At the stock 2-clocks-per-cck ratio that is the real 68000 figure
    /// of 2 cck per word; an accelerated CPU completes it proportionally
    /// faster, with sub-cck remainders carried so no time is lost.
    pub fn cpu_external_access(&mut self, words: u32) {
        if !self.cpu_bus_arbitration_enabled || words == 0 {
            return;
        }
        // dbg_ext_cck_x100 expresses the stock-ratio cost in hundredths of a
        // cck per word (default 200 = 4 CPU clocks/word); 2x converts it to
        // hundredths of a CPU clock.
        let clocks_x100 =
            words * 2 * self.dbg_ext_cck_x100 + std::mem::take(&mut self.ext_clock_carry_x100);
        let denom = self.cpu_clocks_per_cck * 100;
        let cck = clocks_x100 / denom;
        self.ext_clock_carry_x100 = clocks_x100 % denom;
        if cck == 0 {
            return;
        }
        let tick = self.advance_chipset(cck);
        self.record_slice_bus_advance(cck, tick);
    }

    /// Advance the chipset for a CPU access to a motherboard peripheral (CIA,
    /// RTC, autoconfig, undecoded space). These stay on the slow motherboard
    /// bus no matter how fast the CPU is clocked, so bill the stock 68000
    /// figure of 2 cck per word regardless of the configured CPU speed.
    /// A CPU access to a CIA ($BFxxxx). These are 6800-style VPA cycles: the
    /// 68000 synchronizes to the E clock (CPU/10, six clocks low, four high)
    /// before the data transfer, so the access costs 6..15 CPU cycles
    /// depending on the E phase it starts in, not a plain bus cycle. The
    /// delay table is vAmiga's Agnus::syncWithEClock at Copperline's colour
    /// clock resolution (one cck = two CPU clocks; the CIA PHI2 divider
    /// remainder is the live E phase). Software loops that poll or toggle a
    /// CIA lock to the E clock through this - the vAmigaTS CIA/cnt ramps
    /// count it directly.
    pub fn cpu_cia_access(&mut self, words: u32) {
        if !self.cpu_bus_arbitration_enabled || words == 0 {
            return;
        }
        self.flush_timed_devices();
        const E_SYNC_DELAY_CCK: [u32; 5] = [6, 5, 4, 3, 7];
        let phase = (self.device_clock.cia_tick_remainder_cck as usize).min(4);
        let delay = E_SYNC_DELAY_CCK[phase];
        let cck = delay + words * 2;
        let tick = self.advance_chipset(cck);
        self.record_slice_bus_advance(cck, tick);
        self.flush_timed_devices();
    }

    pub fn cpu_slow_external_access(&mut self, words: u32) {
        if !self.cpu_bus_arbitration_enabled || words == 0 {
            return;
        }
        let cck = words * 2;
        let tick = self.advance_chipset(cck);
        self.record_slice_bus_advance(cck, tick);
        // This access targets a motherboard peripheral (CIA, RTC, Akiko, Gayle,
        // A2091, autoconfig). The caller reads/writes the device immediately
        // after, so apply the deferred device clocks now -- including this
        // access -- so the device reflects time right up to the observation.
        self.flush_timed_devices();
    }

    /// The bus advance accumulated so far in the current CPU slice (color
    /// clocks). Used to measure how much of an instruction's time was spent on
    /// the bus, so the remaining CPU-internal cycles can be advanced separately.
    pub fn slice_bus_advanced_cck(&self) -> u32 {
        self.slice_bus_advanced_cck
    }

    /// Advance the chipset clock by `cck` color clocks of CPU-INTERNAL execution
    /// time -- cycles where the 68000 is computing, not driving the bus. On real
    /// hardware the beam and all DMA channels keep running during these cycles;
    /// modelling them keeps the chipset clock locked to the CPU's full
    /// instruction time (cpu_cck) rather than only its bus accesses (bus_cck), so
    /// DMA phase and frame timing track wall-clock like silicon. The CPU is not
    /// granted any slot here (owner is whatever DMA the chipset schedules).
    pub fn advance_cpu_internal_cycles(&mut self, cck: u32) {
        if !self.cpu_bus_arbitration_enabled || cck == 0 {
            return;
        }
        let tick = self.advance_chipset(cck);
        self.record_slice_bus_advance(cck, tick);
    }

    pub fn set_realtime_devices_enabled(&mut self, enabled: bool) {
        self.device_clock.set_realtime_enabled(enabled);
    }

    pub fn sync_realtime_devices(&mut self) -> u32 {
        if !self.device_clock.realtime_enabled {
            return 0;
        }
        if let Some(wait) = self.device_clock.wait_duration(Instant::now()) {
            std::thread::sleep(wait);
        }
        self.sync_realtime_devices_to(Instant::now())
    }

    pub(crate) fn sync_realtime_devices_to(&mut self, now: Instant) -> u32 {
        let cck = self.device_clock.realtime_cck_due(now);
        if cck != 0 {
            self.advance_devices(cck);
        }
        cck
    }

    pub fn take_slice_bus_advance(&mut self) -> (u32, AgnusTick) {
        let cck = std::mem::take(&mut self.slice_bus_advanced_cck);
        let tick = std::mem::take(&mut self.slice_bus_tick);
        (cck, tick)
    }

    pub fn grant_cpu_bus_access(&mut self, size: usize, kind: CpuBusAccessKind) {
        self.grant_cpu_bus_access_at(None, size, kind);
    }

    /// CPU access to the chip bus with the AGA 32-bit data path modelled:
    /// on Alice machines one bus slot moves a longword, and sequential
    /// opcode-word fetches from the same aligned longword ride a single
    /// access (the 020 fetches 32 bits at a time). Chip writes drop the
    /// fetch latch so self-modifying code refetches.
    pub fn grant_cpu_bus_access_at(
        &mut self,
        addr: Option<u32>,
        size: usize,
        kind: CpuBusAccessKind,
    ) {
        if !self.cpu_bus_arbitration_enabled {
            return;
        }

        if matches!(kind, CpuBusAccessKind::Custom) {
            self.sync_realtime_devices();
        }

        let wide_bus = self.aga_enabled();
        if wide_bus && matches!(kind, CpuBusAccessKind::Write) {
            if let Some(addr) = addr {
                let first = addr & !3;
                let last = addr.wrapping_add(size.max(1) as u32 - 1) & !3;
                for entry in &mut self.cpu_fetch_latch {
                    if *entry == Some(first) || *entry == Some(last) {
                        *entry = None;
                    }
                }
            } else {
                self.cpu_fetch_latch = [None; 2];
            }
        }
        let slots = if wide_bus {
            if let (CpuBusAccessKind::Fetch, Some(addr), 2) = (kind, addr, size) {
                let longword = addr & !3;
                if self.cpu_fetch_latch.contains(&Some(longword)) {
                    return;
                }
                self.cpu_fetch_latch[1] = self.cpu_fetch_latch[0];
                self.cpu_fetch_latch[0] = Some(longword);
                1
            } else {
                (size.max(1) as u32).div_ceil(4)
            }
        } else {
            bus_slots_for_cpu_access(size)
        };
        let trace_cpu_bus = diag_cpu_bus_on() && diag_cpu_bus_addr_matches(addr, size);
        for slot in 0..slots {
            self.flush_audio_before_audio_dma_slot();
            let request_slot = (self.agnus.vpos, self.agnus.hpos);
            if matches!(kind, CpuBusAccessKind::Custom) {
                self.cpu_custom_request_slot = Some(request_slot);
            }
            let mut wait_cck = 0;
            while !self.cpu_can_use_current_slot() {
                let (cck, tick) = self.advance_one_chip_bus_quantum(None);
                wait_cck += cck;
                self.note_cpu_missed_chip_bus_cycle();
                self.record_slice_bus_advance(cck, tick);
                self.flush_audio_before_audio_dma_slot();
            }
            let grant_slot = (self.agnus.vpos, self.agnus.hpos);
            if matches!(kind, CpuBusAccessKind::Custom) {
                // Remember the slot that carries a custom-register access:
                // a register write's Denise/Agnus-effective position is
                // referenced from this slot (see `record_render_write`),
                // not from the beam position after the bus cycle's tail.
                // A long-word access stores its second (low-word) slot.
                self.cpu_custom_access_slot = Some(grant_slot);
            }
            let (cck, tick) = self.advance_one_chip_bus_quantum(Some(ChipBusOwner::Cpu));
            self.note_cpu_granted_chip_bus_cycle();
            self.record_slice_bus_advance(cck, tick);
            // After the granted slot (one cck), the CPU's bus cycle runs out
            // its remaining clocks with the chip bus free for DMA. The 68000's
            // 4-clock cycle leaves one whole cck (2 clocks at the stock ratio);
            // the 68020+'s 3-clock cycle leaves only one clock -- half a cck at
            // the stock ratio, and none at all once a slot is >= 3 clocks
            // (14 MHz), which is the write-posting / faster-020-bus effect.
            // Bill the 020 tail in CPU clocks through a carry so the fractional
            // cck are not lost.
            if self.cpu_short_bus_cycle {
                self.cpu_bus_tail_carry += 3u32.saturating_sub(self.cpu_clocks_per_cck);
                while self.cpu_bus_tail_carry >= self.cpu_clocks_per_cck {
                    self.cpu_bus_tail_carry -= self.cpu_clocks_per_cck;
                    let (cck, tick) = self.advance_one_chip_bus_quantum(None);
                    self.record_slice_bus_advance(cck, tick);
                }
            } else {
                let (cck, tick) = self.advance_one_chip_bus_quantum(None);
                self.record_slice_bus_advance(cck, tick);
            }
            if trace_cpu_bus {
                self.diag_cpu_bus_access(
                    kind,
                    addr,
                    size,
                    slot + 1,
                    slots,
                    request_slot,
                    grant_slot,
                    wait_cck,
                );
            }
        }
        if self.cpu_short_bus_cycle && !wide_bus && matches!(kind, CpuBusAccessKind::Read) {
            self.bill_020_read_data_wait();
        }
    }

    /// CPU chip-bus access trace (`COPPERLINE_DIAG_CPU_BUS=1`): logs each
    /// requested CPU chip-bus slot, the slot granted by Agnus arbitration, and
    /// the beam position after the bus cycle tail has elapsed.
    fn diag_cpu_bus_access(
        &self,
        kind: CpuBusAccessKind,
        addr: Option<u32>,
        size: usize,
        slot: u32,
        slots: u32,
        request_slot: (u32, u32),
        grant_slot: (u32, u32),
        wait_cck: u32,
    ) {
        let (rv, rh) = request_slot;
        let (v, h) = grant_slot;
        match addr {
            Some(addr) => eprintln!(
                "CPUBUS kind={} addr={:08x} size={} slot={}/{} rv={:03x} rh={:02x} v={:03x} h={:02x} wait={} ev={:03x} eh={:02x}",
                cpu_bus_access_kind_name(kind),
                addr,
                size,
                slot,
                slots,
                rv,
                rh,
                v,
                h,
                wait_cck,
                self.agnus.vpos,
                self.agnus.hpos
            ),
            None => eprintln!(
                "CPUBUS kind={} addr=-------- size={} slot={}/{} rv={:03x} rh={:02x} v={:03x} h={:02x} wait={} ev={:03x} eh={:02x}",
                cpu_bus_access_kind_name(kind),
                size,
                slot,
                slots,
                rv,
                rh,
                v,
                h,
                wait_cck,
                self.agnus.vpos,
                self.agnus.hpos
            ),
        }
    }

    fn bill_020_read_data_wait(&mut self) {
        let (cck, tick) = self.advance_one_chip_bus_quantum(None);
        self.record_slice_bus_advance(cck, tick);
    }

    fn note_cpu_missed_chip_bus_cycle(&mut self) {
        self.cpu_missed_chip_slots = self.cpu_missed_chip_slots.wrapping_add(1);
        if self.blitter_slowdown_counter_enabled() && self.blitter.current_slot_counts_for_bls() {
            self.blitter_slowdown_cpu_misses = self
                .blitter_slowdown_cpu_misses
                .saturating_add(1)
                .min(exp_miss_limit());
        } else {
            self.blitter_slowdown_cpu_misses = 0;
        }
    }

    fn note_cpu_granted_chip_bus_cycle(&mut self) {
        self.blitter_slowdown_cpu_misses = 0;
        // Cumulative count of chip-bus slots the CPU is granted, used by the
        // real-time pacing profile.
        self.cpu_granted_chip_slots = self.cpu_granted_chip_slots.wrapping_add(1);
    }

    /// Cumulative chip-bus slots granted to the CPU.
    pub fn cpu_granted_chip_slots(&self) -> u64 {
        self.cpu_granted_chip_slots
    }

    fn finish_pending_blitter(&mut self) {
        let was_busy = self.blitter.busy;
        if self.blitter.finish_scheduled_now(&mut self.mem.chip_ram) {
            self.latch_blitter_completion("forced");
            self.trace_blitter_forced_finish(was_busy);
        }
    }

    fn latch_blitter_completion(&mut self, source: &'static str) {
        if self.frame_analyzer_enabled {
            if let Some(record) = self
                .current_frame_bus_trace
                .blits
                .iter_mut()
                .rev()
                .find(|record| record.end.is_none())
            {
                record.end = Some((
                    self.agnus.vpos.min(u32::from(u16::MAX)) as u16,
                    self.agnus.hpos.min(u32::from(u16::MAX)) as u16,
                ));
            }
        }
        if diag_blt_slots() {
            eprintln!(
                "BLTP {} {} {} END source={source}",
                self.emulated_frames, self.agnus.vpos, self.agnus.hpos
            );
        }
        if source == "forced" {
            // Drained synchronously (mid-blit register write): the whole
            // hardware timeline has already been collapsed, so raise now
            // and drop any raise already armed by the terminal cycle.
            self.blit_irq_delay_cck = None;
            self.raise_blit_irq(source);
        }
        // Scheduled completions do not raise here: INTREQ.BLIT is armed
        // when the sequencer ENTERS its terminal BLTDONE cycle (see
        // note_blitter_slot_ticked), one colour clock after that cycle's
        // first attempt -- which real Agnus asserts even while a contended
        // final D write is still blocked (vAmiga scheduleIrqRel(BLIT, 1)
        // runs before the bus allocation check).
    }

    /// Post-tick bookkeeping shared by the blitter's bus-slot and
    /// idle-cycle tick sites: arm the INTREQ.BLIT raise when the sequencer
    /// just entered its terminal BLTDONE cycle. Armed with 2 because the
    /// arming tick's own advance_beam decrements it once in the same
    /// colour clock; the raise then lands at the end of the terminal
    /// cycle's first attempt.
    pub(crate) fn note_blitter_slot_ticked(&mut self) {
        if self.blitter.take_irq_arm() {
            self.blit_irq_delay_cck = Some(2);
        }
    }

    fn raise_blit_irq(&mut self, source: &'static str) {
        let intreq_before = self.paula.intreq;
        self.paula.intreq |= INT_BLIT;
        self.note_irq_source_asserted();
        self.trace_blitter_completion(source, intreq_before);
    }

    #[cfg(test)]
    pub fn last_chip_bus_owner(&self) -> ChipBusOwner {
        self.last_chip_bus_owner
    }

    pub fn advance_chipset(&mut self, target_cck: u32) -> AgnusTick {
        let mut total = AgnusTick::default();

        let mut remaining = target_cck;
        while remaining > 0 {
            let (cck, tick) = self.advance_one_chip_bus_quantum_limited(None, remaining);
            remaining = remaining.saturating_sub(cck);
            add_agnus_tick(&mut total, tick);
        }

        total
    }

    pub fn advance_devices(&mut self, cck: u32) -> AgnusTick {
        // Apply any color clocks deferred by the CPU access path before ticking
        // this (idle/stopped-CPU or test-driven) span, so device time stays
        // ordered, then tick this span directly.
        self.flush_timed_devices();
        let tick = self.advance_chipset(cck);
        self.tick_timed_devices(cck, tick);
        tick
    }

    pub fn next_blitter_completion_cck(&self) -> Option<u32> {
        if !self.blitter_dma_enabled() {
            return None;
        }
        let slots = self.blitter.scheduled_slots_remaining()?;
        let prediction = self
            .blitter
            .scheduled_slot_access_pattern(BLITTER_DEADLINE_SLOT_SCAN_LIMIT)
            .filter(|&(_, count)| count == slots)
            .and_then(|(mask, count)| self.cck_until_blitter_completes(mask, count));
        Some(prediction.unwrap_or_else(|| slots.saturating_mul(CHIP_BUS_SLOT_CCK).max(1)))
    }

    pub fn next_serial_event_cck(&self) -> Option<u32> {
        self.paula.next_serial_event_cck()
    }

    pub fn next_pot_event_cck(&self) -> Option<u32> {
        // The pot counters step at H-sync, so while a scan is running cap the
        // advance at the next line boundary (the same deadline CIA-B TOD uses)
        // so a CPU read of POTxDAT lands on the up-to-date value. When no scan
        // is running the pots impose no deadline.
        if !self.paula.pot_running() {
            return None;
        }
        self.agnus.cck_until_line_ticks(1)
    }

    pub fn next_audio_irq_cck(&self) -> Option<u32> {
        let cck = self.paula.next_audio_irq_cck(self.agnus.dmacon)?;
        Some(cck.saturating_sub(self.audio_pending_cck).max(1))
    }

    pub fn cpu_visible_intreq(&self) -> u16 {
        let mut visible = self.paula.intreq;
        if self.coper_cpu_irq_delay_cck != 0 {
            visible &= !INT_COPER;
        }
        // Hold a newly-raised interrupt invisible during its recognition latency.
        if self.irq_latency_cck != 0 {
            visible &= !self.irq_latency_mask;
        }
        visible
    }

    /// Detect a newly-raised maskable interrupt and arm its recognition-latency
    /// countdown. Called per device tick, after intreq/intena have settled.
    fn arm_irq_recognition_latency(&mut self) {
        let setting = self.irq_latency_setting;
        if setting == 0 {
            return;
        }
        let pending = self.current_enabled_irq_sources();
        let newly = pending & !self.irq_latency_last_pending;
        if newly != 0 {
            self.irq_latency_mask |= newly;
            self.irq_latency_cck = setting;
        }
        // Drop any bits that are no longer pending (acked while still delayed).
        self.irq_latency_mask &= pending;
        self.irq_latency_last_pending = pending;
    }

    fn current_enabled_irq_sources(&self) -> u16 {
        if self.paula.intena & crate::chipset::paula::INT_MASTER != 0 {
            self.paula.intena & self.paula.intreq & IRQ_SOURCE_BITS
        } else {
            0
        }
    }

    /// CPU INTENA/INTREQ writes change Paula's mask/latch state, but they are
    /// usually not new asynchronous interrupt-source edges. Keep the delayed-bit
    /// state coherent without hiding an already-latched source again; real
    /// recognition latency is armed where Paula/CIA/blitter sources assert.
    ///
    /// PORTS is level-fed by CIA-A/Gayle-style INT2 sources and is left visible
    /// immediately when software unmasks an already-latched level. Other newly
    /// exposed sources still represent a freshly-present CPU IPL input and pass
    /// through interrupt recognition.
    fn note_irq_latches_changed(&mut self) {
        let pending = self.current_enabled_irq_sources();
        let newly = pending & !self.irq_latency_last_pending;
        let delayed = newly & !INT_PORTS;
        if delayed != 0 && self.irq_latency_setting != 0 {
            self.irq_latency_mask |= delayed;
            self.irq_latency_cck = self.irq_latency_setting;
        }
        self.irq_latency_mask &= pending;
        self.irq_latency_last_pending = pending;
    }

    fn note_irq_source_asserted(&mut self) {
        self.arm_irq_recognition_latency();
    }

    pub fn next_frame_event_cck(&self) -> u32 {
        self.agnus.cck_until_next_frame().max(1)
    }

    pub fn next_display_start_event_cck(&self) -> Option<u32> {
        let display_start = self.display_start_vpos_for_current_control();
        if self.current_frame_display_snapshot_taken || self.agnus.vpos >= display_start {
            return None;
        }
        self.agnus.cck_until_line_start(display_start)
    }

    fn display_start_vpos_for_current_control(&self) -> u32 {
        // Programmable scans anchor at the geometry's visible window (from
        // the programmable vertical blank); the PAL/NTSC DIW clamp below
        // only makes sense for the fixed 15 kHz field.
        if self.current_frame_geometry.programmable {
            return self.current_frame_geometry.visible_start_vpos;
        }
        visible_start_vpos_for_diw(
            self.denise.diwstrt,
            self.denise.diwstop,
            self.effective_diwhigh(),
        )
    }

    /// Display geometry for the frame that is just starting, computed once
    /// at the frame wrap. Mid-frame BEAMCON0/HTOTAL/VTOTAL writes affect
    /// the live beam immediately but take presentation effect at the next
    /// wrap, like the interlace long-field flag. Standard frames keep a fixed
    /// overscan render top; DIW controls the display-start snapshot separately.
    fn compute_frame_geometry(&self) -> FrameGeometry {
        let lace = self.denise.bplcon0 & 0x0004 != 0;
        if let Some((start, lines)) = self.agnus.programmable_visible_window() {
            return FrameGeometry {
                programmable: true,
                visible_start_vpos: start,
                visible_lines: (lines as usize).clamp(1, MAX_VISIBLE_LINES),
                line_cck: self.agnus.programmable_line_cck().unwrap_or(227),
                frame_lines: self.agnus.current_frame_lines(),
                lace,
            };
        }
        let frame_lines =
            fixed_standard_frame_lines(self.agnus.video_standard(), lace, self.agnus.lof);
        FrameGeometry::standard(self.current_frame_visible_start_vpos, frame_lines, lace)
    }

    fn frame_lines_for_geometry(&self, geometry: FrameGeometry, long_field: bool) -> u32 {
        if geometry.programmable {
            self.agnus.current_frame_lines()
        } else {
            fixed_standard_frame_lines(self.agnus.video_standard(), geometry.lace, long_field)
        }
    }

    /// Keep a standard frame's geometry in step with the framebuffer render
    /// origin (programmable geometry derives its window from the beam registers
    /// instead and is left alone).
    #[cfg(test)]
    fn refresh_frame_geometry_visible_start(&mut self) {
        if !self.current_frame_geometry.programmable {
            self.current_frame_geometry.visible_start_vpos = self.current_frame_visible_start_vpos;
        }
    }

    pub fn next_cia_b_tod_alarm_cck(&self) -> Option<u32> {
        self.agnus
            .cck_until_line_ticks(self.cia_b.next_tod_alarm_ticks()?)
    }

    pub fn next_copper_wakeup_cck(&self) -> Option<u32> {
        if !self.copper_dma_enabled() {
            return None;
        }
        if let Some(cck) = self.cck_until_pending_copper_frame_start() {
            return Some(cck);
        }
        let Some(wait) = self.copper.waiting() else {
            // A running Copper (or one in a WAIT/SKIP tail) fetches an
            // instruction word every other colour clock, and its next MOVE
            // can raise INTREQ: bound the stopped-CPU fast-forward to a
            // couple of colour clocks so a wake-up interrupt written by the
            // instruction stream is recognized on time. A halted Copper
            // (illegal register write) fetches nothing until restarted.
            if self.copper.is_stopped() {
                return None;
            }
            return Some(2);
        };
        let position_cck = self.cck_until_copper_wait_position(wait)?;
        if position_cck == 0 && wait.blitter_wait_enabled() && self.blitter.busy {
            return self.next_blitter_completion_cck();
        }
        Some(position_cck)
    }

    fn tick_timed_devices(&mut self, cck: u32, agnus_tick: AgnusTick) {
        if agnus_tick.new_frames > 0 {
            self.pending_vbi = 1;
        }

        // Gayle drives INT2 (PORTS) as a level; Paula's INTREQ latch keeps
        // getting set while the line is asserted.
        if self
            .gayle
            .as_ref()
            .is_some_and(crate::gayle::Gayle::int2_line)
        {
            self.paula.intreq |= INT_PORTS;
        }

        // Akiko: advance the CD controller (sector DMA pacing, command
        // and response rings) and level-feed its INT2 line like Gayle's.
        if let Some(akiko) = self.akiko.as_mut() {
            akiko.tick(cck, &mut self.mem.chip_ram, self.paula.cd_audio_mut());
            if akiko.int2_line() {
                self.paula.intreq |= INT_PORTS;
            }
        }

        // CDTV DMAC: advance through the ZorroDevice boundary (its tick streams
        // CD audio from Paula's ring) and level-feed its INT2 line like Gayle's.
        if let Some(cdtv) = self.cdtv.as_mut() {
            {
                let mut host = crate::zorro_device::DeviceHost::with_cd_audio(
                    &mut self.mem,
                    self.paula.cd_audio_mut(),
                );
                crate::zorro_device::ZorroDevice::tick(cdtv, cck, &mut host);
            }
            if crate::zorro_device::ZorroDevice::int2_line(cdtv) {
                self.paula.intreq |= INT_PORTS;
            }
        }

        // Functional Zorro-chain boards (A2091 SCSI, WASM plugins): advance each
        // -- delivering delayed interrupts and any DMA that became ready -- and
        // level-feed its INT2/INT6 lines like Gayle's.
        {
            let Self {
                devices,
                mem,
                paula,
                ..
            } = self;
            for (slot, dev) in devices.iter_mut().enumerate() {
                let mut host = crate::zorro_device::DeviceHost::for_slot(&mut *mem, slot);
                crate::zorro_device::ZorroDevice::tick(dev, cck, &mut host);
                if crate::zorro_device::ZorroDevice::int2_line(dev) {
                    paula.intreq |= INT_PORTS;
                }
                if crate::zorro_device::ZorroDevice::int6_line(dev) {
                    paula.intreq |= INT_EXTER;
                }
            }
        }

        let ticks = self.device_clock.cia_ticks_for_cck(cck);
        // Cached once: this runs on the per-device-tick path, so a live env
        // lookup here would cost real-time performance for a debug-only logger.
        let dbg_cia = dbg_cia_on();
        if self.cia_a.tick(ticks) {
            self.paula.intreq |= INT_PORTS;
            if dbg_cia {
                log::info!(
                    "cia A irq secs={:.5} f={} icr={:#04X}",
                    self.emulated_seconds(),
                    self.emulated_frames,
                    self.cia_a.debug_icr_data(),
                );
            }
        }
        if self.cia_b.tick(ticks) {
            self.paula.intreq |= INT_EXTER;
            if dbg_cia {
                log::info!(
                    "cia B irq secs={:.5} f={} icr={:#04X}",
                    self.emulated_seconds(),
                    self.emulated_frames,
                    self.cia_b.debug_icr_data(),
                );
            }
        }

        for _ in 0..agnus_tick.new_frames {
            if self.cia_a.tick_tod() {
                self.paula.intreq |= INT_PORTS;
            }
        }
        for _ in 0..agnus_tick.new_lines {
            // Denise clocks the POTxDAT counters at H-sync, the same per-line
            // clock as CIA-B's TOD, so advance the pot scan once per new line.
            self.paula.tick_pot_hsync();
            if self.cia_b.tick_tod() {
                self.paula.intreq |= INT_EXTER;
                if dbg_cia {
                    log::info!(
                        "cia B TOD alarm (tick) secs={:.5} f={}",
                        self.emulated_seconds(),
                        self.emulated_frames,
                    );
                }
            }
        }
        for _ in 0..agnus_tick.new_frames {
            if self
                .cia_b
                .sync_tod_to_frame(self.agnus.nominal_frame_lines())
            {
                self.paula.intreq |= INT_EXTER;
                if dbg_cia {
                    log::info!(
                        "cia B TOD alarm (frame sync) secs={:.5} f={}",
                        self.emulated_seconds(),
                        self.emulated_frames,
                    );
                }
            }
        }

        if !self.keyboard.is_idle() && self.keyboard.tick(cck, &mut self.cia_a) {
            self.paula.intreq |= INT_PORTS;
        }
        if self.keyboard.take_system_reset_request() {
            self.keyboard_system_reset_pending = true;
            self.slice_preempted = true;
        }

        // emulated_cck already covers this span, so it is the color clock at
        // the span's end. Paula uses it to stamp any serial byte that finishes
        // here; passing it avoids arithmetic on the common (no byte) path.
        self.paula.intreq |= self.paula.tick_serial(cck, self.emulated_cck);
        let dmacon = self.agnus.dmacon;
        self.flush_audio();
        // The floppy mechanism is quiescent for almost all of normal running
        // (no DMA, motor off, no drive selected). Skip the whole block -- an
        // `is_idle()` recompute plus the DSKBLK/DSKSYNC/index-pulse polling and
        // drive-sound feed -- on a single cached bool while that holds. The
        // cache is cleared by every activation write and recomputed in
        // `floppy.tick`, so a newly active drive is serviced from the next
        // access. Drive sounds need no feed once idle: the spin/read levels are
        // already zeroed and the tails decay in Paula's mixer.
        if !self.floppy.is_idle_cached() {
            self.floppy.set_adkcon(self.paula.adkcon);
            if self.floppy.tick(cck, dmacon, &mut self.mem.chip_ram) {
                self.paula.intreq |= INT_DSKBLK;
                // Companion to COPPERLINE_DBG_DSKLEN: the wall-time DSKBLK
                // raise, closing each arm -> completion interval.
                if crate::envcfg::flag("COPPERLINE_DBG_DSKLEN") {
                    log::info!(
                        "dskblk f={} secs={:.4} v={} h={}",
                        self.emulated_frames,
                        self.emulated_seconds(),
                        self.agnus.vpos,
                        self.agnus.hpos,
                    );
                }
            }
            if self.floppy.take_sync_irq() {
                self.paula.intreq |= INT_DSKSYNC;
            }
            if self.floppy.take_index_pulse() {
                let flag_irq = self.cia_b.assert_flag();
                self.cia_b.release_flag();
                if flag_irq {
                    self.paula.intreq |= INT_EXTER;
                }
            }
            self.feed_drive_sounds(dmacon);
        }
        self.refresh_cia_irq_lines();
        self.flush_pending_vbi();
        self.arm_irq_recognition_latency();
    }

    /// Forward floppy mechanism activity (head steps, motor spin
    /// levels, read/write DMA) to the synthesized drive sound effects
    /// mixed into Paula's host output. State updates here trail the
    /// already-flushed audio batch by at most one chipset tick, well
    /// under a single host sample.
    fn feed_drive_sounds(&mut self, dmacon: u16) {
        let steps = self.floppy.take_sound_steps();
        let spins = self.floppy.motor_spin_levels();
        let reading = self.floppy.dma_active(dmacon);
        let sounds = self.paula.drive_sounds_mut();
        if !sounds.enabled() {
            return;
        }
        for _ in 0..steps {
            sounds.step_pulse();
        }
        for (drive, spin) in spins.into_iter().enumerate() {
            sounds.set_motor_spin(drive, spin);
        }
        sounds.set_read_active(reading);
    }

    fn refresh_cia_irq_lines(&mut self) {
        if self.cia_a.irq_line_asserted() {
            self.paula.intreq |= INT_PORTS;
        }
        if self.cia_b.irq_line_asserted() {
            self.paula.intreq |= INT_EXTER;
        }
    }

    pub fn flush_pending_vbi(&mut self) {
        if self.pending_vbi == 0 {
            return;
        }
        // VERTB is asserted at the frame wrap (vpos 0, hpos 0), which the
        // timing-test row 22 confirmed matches real HW (FS-UAE/vAmiga both raise
        // it at vpos 0). The ~70 cck that real hardware adds before the handler
        // runs is interrupt RECOGNITION LATENCY, modelled separately (see
        // irq_latency_setting / arm_irq_recognition_latency), not a raise delay.
        if self.paula.intreq & INT_VERTB == 0 {
            self.paula.intreq |= INT_VERTB;
            if diag_vbi() {
                log::info!(
                    "vbi-assert secs={:.5} v={} h={}",
                    self.emulated_seconds(),
                    self.agnus.vpos,
                    self.agnus.hpos
                );
            }
        }
        self.pending_vbi = 0;
    }

    pub fn flush_audio(&mut self) -> u16 {
        let cck = std::mem::take(&mut self.audio_pending_cck);
        if cck == 0 {
            return 0;
        }
        let irq = self.paula.advance_audio(cck, self.agnus.dmacon);
        self.paula.latch_interrupt_sources(irq);
        irq
    }

    pub fn frame_render_events(&self) -> &[BeamRegisterWrite] {
        if self.last_frame_render_base.is_some() {
            &self.last_frame_render_events
        } else {
            &self.current_frame_render_events
        }
    }

    pub fn frame_render_base(&self) -> RenderRegisterSnapshot {
        self.last_frame_render_base
            .unwrap_or(self.current_frame_render_base)
    }

    pub fn frame_render_available(&self) -> bool {
        self.last_frame_render_base.is_some()
    }

    pub fn frame_visible_start_vpos(&self) -> u32 {
        if self.last_frame_render_base.is_some() {
            self.last_frame_visible_start_vpos
        } else {
            self.current_frame_visible_start_vpos
        }
    }

    /// Display geometry of the frame the renderer is about to draw
    /// (the completed frame once one exists, like `frame_render_base`).
    pub fn frame_geometry(&self) -> FrameGeometry {
        if self.last_frame_render_base.is_some() {
            self.last_frame_geometry
        } else {
            self.current_frame_geometry
        }
    }

    /// Beam-line count for the frame the renderer is about to draw. This is
    /// latched with `FrameGeometry`; the fallback covers old in-process
    /// snapshots whose transient geometry field predates `frame_lines`.
    pub fn frame_lines(&self) -> u32 {
        let frame_lines = self.frame_geometry().frame_lines;
        if frame_lines == 0 {
            self.agnus.current_frame_lines()
        } else {
            frame_lines
        }
    }

    pub fn set_frame_analyzer_enabled(&mut self, enabled: bool) {
        if self.frame_analyzer_enabled == enabled {
            return;
        }
        self.frame_analyzer_enabled = enabled;
        if enabled {
            self.reset_current_frame_bus_trace(true);
        } else {
            self.current_frame_bus_trace.clear();
            self.last_frame_bus_trace = None;
        }
    }

    pub fn frame_bus_trace(&self) -> Option<&FrameBusTrace> {
        self.last_frame_bus_trace
            .as_ref()
            .filter(|trace| trace.has_samples())
            .or_else(|| {
                self.current_frame_bus_trace
                    .has_samples()
                    .then_some(&self.current_frame_bus_trace)
            })
    }

    pub fn current_render_events(&self) -> &[BeamRegisterWrite] {
        &self.current_frame_render_events
    }

    pub fn frame_bottom_palette_events(&self) -> &[BeamRegisterWrite] {
        if self.last_frame_render_base.is_some() {
            &self.last_frame_beam_bottom_palette_events
        } else {
            &self.beam_bottom_palette_events
        }
    }

    pub fn current_render_base(&self) -> RenderRegisterSnapshot {
        self.current_frame_render_base
    }

    pub fn frame_top_palette_end(&self) -> Palette {
        if self.last_frame_render_base.is_some() {
            self.last_frame_beam_top_palette_end
        } else {
            self.beam_top_palette
        }
    }

    pub fn frame_palette_split(&self) -> (Palette, Palette, bool) {
        if self.last_frame_render_base.is_some() {
            (
                self.last_frame_beam_top_palette,
                self.last_frame_beam_bottom_palette,
                self.last_frame_beam_bottom_palette_valid,
            )
        } else {
            (
                self.beam_top_palette,
                self.beam_bottom_palette,
                self.beam_bottom_palette_valid,
            )
        }
    }

    pub fn frame_chip_ram(&self) -> &[u8] {
        if crate::envcfg::flag("COPPERLINE_RENDER_LIVE_CHIP_RAM") {
            return &self.mem.chip_ram;
        }
        if self.last_frame_render_base.is_some()
            && self.last_frame_chip_ram.len() == self.mem.chip_ram.len()
        {
            &self.last_frame_chip_ram
        } else {
            &self.mem.chip_ram
        }
    }

    pub fn frame_chip_ram_writes(&self) -> &[BeamChipRamWrite] {
        if crate::envcfg::flag("COPPERLINE_RENDER_LIVE_CHIP_RAM") {
            return &[];
        }
        if self.last_frame_render_base.is_some() {
            &self.last_frame_chip_ram_writes
        } else {
            &self.current_frame_chip_ram_writes
        }
    }

    pub fn frame_captured_bitplane_rows(&self) -> &[Option<CapturedBitplaneRow>] {
        if crate::envcfg::flag("COPPERLINE_RENDER_LIVE_CHIP_RAM") {
            return &[];
        }
        if self.last_frame_render_base.is_some()
            && self.last_frame_bitplane_rows.len() == MAX_VISIBLE_LINES
        {
            &self.last_frame_bitplane_rows
        } else {
            &self.current_frame_bitplane_rows
        }
    }

    pub fn frame_captured_sprite_lines(&self) -> &[CapturedSpriteLine] {
        if crate::envcfg::flag("COPPERLINE_RENDER_LIVE_CHIP_RAM") {
            return &[];
        }
        if self.last_frame_render_base.is_some() {
            &self.last_frame_sprite_lines
        } else {
            &self.current_frame_sprite_lines
        }
    }

    pub fn frame_held_sprites(&self) -> [Option<HeldSpriteLine>; 8] {
        if crate::envcfg::flag("COPPERLINE_RENDER_LIVE_CHIP_RAM") {
            return [None; 8];
        }
        if self.last_frame_render_base.is_some() {
            self.last_frame_held_sprites
        } else {
            self.current_frame_held_sprites
        }
    }

    pub fn frame_sprite_display_enable_x_by_y(&self) -> &[Option<usize>] {
        if crate::envcfg::flag("COPPERLINE_RENDER_LIVE_CHIP_RAM") {
            return &[];
        }
        if self.last_frame_render_base.is_some() {
            &self.last_frame_sprite_display_enable_x_by_y
        } else {
            &self.current_frame_sprite_display_enable_x_by_y
        }
    }

    pub fn frame_sprite_dma_observed(&self) -> bool {
        if crate::envcfg::flag("COPPERLINE_RENDER_LIVE_CHIP_RAM") {
            return false;
        }
        if self.last_frame_render_base.is_some() {
            self.last_frame_sprite_dma_observed
        } else {
            self.current_frame_sprite_dma_observed
        }
    }

    pub fn begin_cpu_slice(&mut self) {
        self.blitter_slowdown_cpu_misses = 0;
        self.slice_bus_advanced_cck = 0;
        self.slice_bus_tick = AgnusTick::default();
    }

    // -----------------------------------------------------------------
    // CIA dispatch
    // -----------------------------------------------------------------

    pub fn cia_a_read(&mut self, addr: u64, size: usize) -> u64 {
        self.sync_realtime_devices();
        let reg = reg_from_addr(addr);
        let mut v = self.cia_a.read(reg);
        if reg == REG_PRA {
            // PRA bits 6 and 7 (/FIR0, /FIR1) report the live port-1/port-2
            // primary button: left-mouse button or joystick fire (they share
            // the FIR line). Active-low: 0 = pressed, 1 = released.
            if self.input.lmb_port1 {
                v &= !0x40;
            } else {
                v |= 0x40;
            }
            if self.input.lmb_port2 {
                v &= !0x80;
            } else {
                v |= 0x80;
            }
            // PRA bits 2-5 are the selected floppy drive's active-low
            // status lines: /CHNG, /WPRO, /TK0, /RDY.
            v = (v & !0x3C) | self.floppy.cia_a_status_bits();
        }
        trace!("cia_a R reg={:X} sz={} val={:02X}", reg, size, v);
        self.poll_stats.tick_read("cia_a", reg);
        v as u64
    }

    pub fn cia_a_write(&mut self, addr: u64, size: usize, val: u64) -> CiaSideEffect {
        self.sync_realtime_devices();
        let byte = (val & 0xFF) as u8;
        let reg = reg_from_addr(addr);
        trace!("cia_a W reg={:X} sz={} val={:02X}", reg, size, byte);
        let eff = self.cia_a.write(reg, byte);
        if self.cia_a.irq_line_asserted() {
            self.paula.intreq |= INT_PORTS;
        }
        match eff.keyboard_handshake {
            Some(true) => self.keyboard.amiga_kdat_edge(true),
            Some(false) => self.keyboard.amiga_kdat_edge(false),
            None => {}
        }
        if reg == REG_PRA || reg == REG_DDRA {
            // CIA-A PRA bit 1 is /LED. On post-A1000 Amigas this line also
            // switches the analogue audio low-pass filter: low = LED bright +
            // filter engaged, high = LED dim/off + filter bypassed. The A1000
            // does not have that circuit -- its filter is fixed and the LED is
            // not on this line -- so the LED never switches the filter there.
            // The A1000 is the machine with a WCS fitted at $FC0000.
            let is_a1000 = !self.mem.wcs.is_empty();
            if !is_a1000 {
                let pra = self.cia_a.read(REG_PRA);
                self.paula.set_led_filter_enabled(pra & 0x02 == 0);
            }
            self.cd32_pad_cia_clock();
        }
        eff
    }

    /// Whether the port-2 CD32 pad is in serial (shift) mode: the CPU
    /// drives POT1X (P5) low through POTGO (OUTRX set, DATRX clear).
    fn cd32_pad_serial_mode(&self) -> bool {
        self.input.cd32_pad_port2 && self.paula.potgo & 0x3000 == 0x2000
    }

    /// CD32 pad shift clock: with the pad in serial mode, a falling edge
    /// the CPU drives on /FIR1 (CIA-A PRA bit 7 with DDR output) steps
    /// the shift register. Outside serial mode the register reloads.
    fn cd32_pad_cia_clock(&mut self) {
        if !self.input.cd32_pad_port2 {
            return;
        }
        const FIR1: u8 = 0x80;
        let pra = self.cia_a.peek_register(REG_PRA);
        let ddra = self.cia_a.peek_register(REG_DDRA);
        if self.cd32_pad_serial_mode() {
            if ddra & FIR1 != 0 && (pra & FIR1) != self.cd32_pad_fire_oldstate && pra & FIR1 == 0 {
                self.cd32_pad_shifter = (self.cd32_pad_shifter - 1).max(0);
            }
        } else {
            self.cd32_pad_shifter = 8;
        }
        self.cd32_pad_fire_oldstate = ddra & pra & FIR1;
    }

    /// The serial button bit for the current shift position, active-low
    /// on POT1Y. Order from 8 down: Blue, Red, Yellow, Green, FFW, RWD,
    /// Play; 1 is the always-high pad-present bit; 0 reads zero.
    fn cd32_pad_serial_bit(&self) -> bool {
        let input = &self.input;
        match self.cd32_pad_shifter {
            0 => false,
            1 => true,
            2 => !input.cd32_play_port2,
            3 => !input.cd32_rwd_port2,
            4 => !input.cd32_ffw_port2,
            5 => !input.cd32_green_port2,
            6 => !input.cd32_yellow_port2,
            7 => !input.lmb_port2, // Red
            _ => !input.rmb_port2, // Blue
        }
    }

    pub fn cia_b_read(&mut self, addr: u64, size: usize) -> u64 {
        self.sync_realtime_devices();
        let reg = reg_from_addr(addr);
        let v = self.cia_b.read(reg);
        trace!("cia_b R reg={:X} sz={} val={:02X}", reg, size, v);
        self.poll_stats.tick_read("cia_b", reg);
        if size == 2 {
            (v as u64) << 8
        } else {
            v as u64
        }
    }

    pub fn cia_b_write(&mut self, addr: u64, size: usize, val: u64) -> CiaSideEffect {
        self.sync_realtime_devices();
        let byte = if size == 2 {
            ((val >> 8) & 0xFF) as u8
        } else {
            (val & 0xFF) as u8
        };
        let reg = reg_from_addr(addr);
        trace!("cia_b W reg={:X} sz={} val={:02X}", reg, size, byte);
        let anchor_tod = reg == REG_TODLO && !self.cia_b.tod_writes_alarm();
        let eff = self.cia_b.write(reg, byte);
        if self.cia_b.irq_line_asserted() {
            self.paula.intreq |= INT_EXTER;
        }
        if anchor_tod {
            self.cia_b.anchor_tod_to_frame(0);
        }
        if reg == REG_PRB || reg == REG_DDRB {
            let prb = self.cia_b.read(REG_PRB);
            self.floppy.write_prb(prb);
        }
        eff
    }

    // -----------------------------------------------------------------
    // Custom chip ($DFF000) dispatch
    // -----------------------------------------------------------------

    pub fn custom_read(&mut self, addr: u64, size: usize) -> u64 {
        self.grant_cpu_bus_access_at(
            Some(custom_register_cpu_addr(addr)),
            size,
            CpuBusAccessKind::Custom,
        );
        if self.cpu_short_bus_cycle && !self.aga_enabled() {
            self.bill_020_read_data_wait();
        }
        // Read-only custom registers (INTREQR, DSKBYTR, SERDATR, POTxDAT, ...)
        // reflect timed-device state, so apply the deferred device clocks before
        // reading.
        self.flush_timed_devices();
        let off = (addr & 0xFFF) as u16;
        self.poll_stats.tick_read_custom(off & 0xFFE);
        match size {
            1 => {
                let val = self.read_custom_word(off & 0xFFE);
                trace!("custom R8  off={:03X} val_word={:04X}", off, val);
                if diag_cpu_read_matches(off & 0xFFE, size) {
                    self.diag_cpu_read(off & 0xFFE, size, val as u64);
                }
                if addr & 1 == 0 {
                    (val >> 8) as u64
                } else {
                    (val & 0xFF) as u64
                }
            }
            4 => {
                // MOVE.L from $DFFxxx reads two consecutive register
                // words: high word at addr, low word at addr+2. Each
                // register is 16 bits wide on the custom chip bus.
                let hi = self.read_custom_word(off);
                let lo = self.read_custom_word(off.wrapping_add(2));
                let v = ((hi as u64) << 16) | (lo as u64);
                trace!("custom R32 off={:03X} val={:08X}", off, v);
                if diag_cpu_read_matches(off, size) {
                    self.diag_cpu_read(off, size, hi as u64);
                }
                if diag_cpu_read_matches(off.wrapping_add(2), size) {
                    self.diag_cpu_read(off.wrapping_add(2), size, lo as u64);
                }
                v
            }
            _ => {
                let val = self.read_custom_word(off);
                trace!("custom R16 off={:03X} val={:04X}", off, val);
                if diag_cpu_read_matches(off, size) {
                    self.diag_cpu_read(off, size, val as u64);
                }
                val as u64
            }
        }
    }

    /// CPU custom-register read trace
    /// (`COPPERLINE_DIAG_CPU_READS=1`): logs every CPU custom-register read's
    /// granted chip-bus slot and the beam position after timed devices were
    /// flushed for the value being returned.
    fn diag_cpu_read(&self, off: u16, size: usize, value: u64) {
        let (rv, rh) = self
            .cpu_custom_request_slot
            .unwrap_or((self.agnus.vpos, self.agnus.hpos));
        let (v, h) = self
            .cpu_custom_access_slot
            .unwrap_or((self.agnus.vpos, self.agnus.hpos));
        eprintln!(
            "CPUPROBE PEEK rv={:03x} rh={:02x} v={:03x} h={:02x} reg={:03x} size={} val={:04x} ev={:03x} eh={:02x}",
            rv,
            rh,
            v,
            h,
            off & 0x1FE,
            size,
            value,
            self.agnus.vpos,
            self.agnus.hpos
        );
    }

    /// One-shot env flag for the CPU write-landing trace
    /// (`COPPERLINE_DIAG_CPU_WRITES=1`): logs every CPU custom-register
    /// write's granted chip-bus slot (and the beam position the write's
    /// effect applies at) to stderr, for cross-emulator comparison against
    /// vAmiga's `VAMIGA_CPU_PROBE` trace.
    fn diag_cpu_write(&self, off: u16, word: u16) {
        let (rv, rh) = self
            .cpu_custom_request_slot
            .unwrap_or((self.agnus.vpos, self.agnus.hpos));
        let (v, h) = self
            .cpu_custom_access_slot
            .unwrap_or((self.agnus.vpos, self.agnus.hpos));
        eprintln!(
            "CPUPROBE POKE rv={:03x} rh={:02x} v={:03x} h={:02x} reg={:03x} val={:04x} ev={:03x} eh={:02x}",
            rv,
            rh,
            v,
            h,
            off & 0x1FE,
            word,
            self.agnus.vpos,
            self.agnus.hpos
        );
    }

    /// Returns true if the write set a new INTREQ bit and the caller
    /// should preempt the slice so the freshly-asserted IRQ can be
    /// delivered before agnus has a chance to OR in VERTB.
    pub fn custom_write(&mut self, addr: u64, size: usize, val: u64) -> bool {
        self.grant_cpu_bus_access_at(
            Some(custom_register_cpu_addr(addr)),
            size,
            CpuBusAccessKind::Custom,
        );
        // Apply deferred device clocks before the write lands: registers such as
        // INTREQ/INTENA/ADKCON/DSKLEN/AUDxxx/SERDAT change timed-device state, so
        // the device must first be advanced to this color clock (e.g. so a
        // pending IRQ is latched before an INTREQ clear).
        self.flush_timed_devices();
        let off = (addr & 0xFFF) as u16;
        match size {
            1 => {
                // A 68000 byte write drives the byte onto BOTH halves of
                // the data bus, and the custom chips have no byte lanes:
                // they latch the full 16-bit word regardless of which
                // strobe (/UDS or /LDS) the CPU asserted. `move.b
                // v,COLOR00+1` therefore lands $vvvv in the register, not
                // an addressed-lane merge (vAmigaTS CIA/oldcnt cnt1/cnt3/
                // cnt5 ramps show the mirrored high byte on hardware;
                // vAmiga's CPU poke8 to custom space doubles the byte the
                // same way).
                let b = (val & 0xFF) as u16;
                let word = (b << 8) | b;
                trace!("custom W8  off={:03X} val={:02X}", off, b);
                if diag_cpu_writes_on() {
                    self.diag_cpu_write(off & 0xFFE, word);
                }
                self.write_custom_word_from(off & 0xFFE, word, BeamWriteSource::Cpu)
            }
            4 => {
                // MOVE.L to $DFFxxx writes two consecutive register
                // words. DiagROM relies on this for `move.l #copper,
                // COP1LCH` setting both halves of the pointer in one
                // instruction.
                let hi = ((val >> 16) & 0xFFFF) as u16;
                let lo = (val & 0xFFFF) as u16;
                trace!("custom W32 off={:03X} val={:08X}", off, val);
                if diag_cpu_writes_on() {
                    self.diag_cpu_write(off, hi);
                    self.diag_cpu_write(off.wrapping_add(2), lo);
                }
                let p1 = self.write_custom_word_from(off, hi, BeamWriteSource::Cpu);
                let p2 = self.write_custom_word_from(off.wrapping_add(2), lo, BeamWriteSource::Cpu);
                p1 || p2
            }
            _ => {
                let word = (val & 0xFFFF) as u16;
                trace!("custom W16 off={:03X} val={:04X}", off, word);
                if diag_cpu_writes_on() {
                    self.diag_cpu_write(off, word);
                }
                self.write_custom_word_from(off, word, BeamWriteSource::Cpu)
            }
        }
    }

    fn blitter_start_may_write_lowmem(&self) -> bool {
        if self.blitter.bltdpt >= 0x1000 {
            return false;
        }
        let con0 = self.blitter.bltcon0;
        let con1 = self.blitter.bltcon1;
        if con1 & BLTCON1_LINE != 0 {
            // In line mode the store is gated by channel C, and the first
            // pixel is written through BLTDPT before the D pointer follows C.
            con0 & BLTCON0_USE_C != 0
        } else {
            con0 & BLTCON0_USE_D != 0 && con1 & BLTCON1_DOFF == 0
        }
    }

    /// COPPERLINE_DIAG_BLITREGS=START:END plus COPPERLINE_DUMP_BLITMEM
    /// diagnostics shared by the classic (BLTSIZE) and ECS (BLTSIZH) blit
    /// start paths. `h`/`w` are the decoded blit dimensions.
    fn diag_blit_start(&self, h: u32, w: u32) {
        if let Some(spec) = crate::envcfg::var("COPPERLINE_DIAG_BLITREGS") {
            let mut parts = spec.split(':');
            let lo_t: f64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
            let hi_t: f64 = parts
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(f64::INFINITY);
            let secs = self.emulated_seconds();
            if (lo_t..hi_t).contains(&secs) {
                let b = &self.blitter;
                log::info!(
                    "blitregs t={secs:.6} f={} v={} h={} con0={:04X} con1={:04X} fwm={:04X} lwm={:04X} \
                     apt={:06X} bpt={:06X} cpt={:06X} dpt={:06X} amod={} bmod={} cmod={} dmod={} \
                     adat={:04X} bdat={:04X} cdat={:04X} size h={h} w={w}",
                    self.emulated_frames,
                    self.agnus.vpos,
                    self.agnus.hpos,
                    b.bltcon0,
                    b.bltcon1,
                    b.bltafwm,
                    b.bltalwm,
                    b.bltapt,
                    b.bltbpt,
                    b.bltcpt,
                    b.bltdpt,
                    b.bltamod,
                    b.bltbmod,
                    b.bltcmod,
                    b.bltdmod,
                    b.bltadat,
                    b.bltbdat,
                    b.bltcdat,
                );
            }
        }
        if let Some(spec) = crate::envcfg::var("COPPERLINE_DUMP_BLITMEM") {
            let parts: Vec<&str> = spec.split(':').collect();
            if parts.len() == 4 {
                let secs = self.emulated_seconds();
                let lo_t: f64 = parts[0].parse().unwrap_or(0.0);
                let hi_t: f64 = parts[1].parse().unwrap_or(0.0);
                let lo = usize::from_str_radix(parts[2], 16).unwrap_or(0);
                let hi = usize::from_str_radix(parts[3], 16).unwrap_or(0);
                if (lo_t..hi_t).contains(&secs) && hi > lo && hi <= self.mem.chip_ram.len() {
                    let dir = crate::envcfg::var("COPPERLINE_DUMP_BLITMEM_DIR")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| std::env::temp_dir().join("copperline-blitdump"));
                    match std::fs::create_dir_all(&dir) {
                        Ok(()) => {
                            let path = dir.join(format!("{:.6}.bin", secs));
                            if let Err(err) = std::fs::write(&path, &self.mem.chip_ram[lo..hi]) {
                                log::warn!("writing blit memory dump {}: {err}", path.display());
                            }
                        }
                        Err(err) => {
                            log::warn!("creating blit memory dump dir {}: {err}", dir.display())
                        }
                    }
                }
            }
        }
    }

    /// Record a started blit into the analyzer's frame trace (no-op
    /// while the analyzer is closed).
    pub(crate) fn record_frame_blit_start(&mut self, height: u32, width_words: u32) {
        if !self.frame_analyzer_enabled
            || self.current_frame_bus_trace.blits.len() >= FRAME_BLIT_RECORD_CAP
        {
            return;
        }
        self.current_frame_bus_trace.blits.push(FrameBlitRecord {
            bltcon0: self.blitter.bltcon0,
            bltcon1: self.blitter.bltcon1,
            width_words,
            height,
            apt: self.blitter.bltapt,
            bpt: self.blitter.bltbpt,
            cpt: self.blitter.bltcpt,
            dpt: self.blitter.bltdpt,
            start: (
                self.agnus.vpos.min(u32::from(u16::MAX)) as u16,
                self.agnus.hpos.min(u32::from(u16::MAX)) as u16,
            ),
            end: None,
        });
    }

    fn record_blit_accounting(&mut self) {
        if !self.bus_accounting.enabled {
            return;
        }
        if let (Some(is_line), Some(slots)) = (
            self.blitter.pending_is_line(),
            self.blitter.scheduled_slots_remaining(),
        ) {
            self.bus_accounting.record_blit(is_line, slots);
        }
    }

    /// Read a big-endian word straight out of chip RAM (debug probes only).
    /// One-line summary of the display/DMA chipset state, for the debugger.
    /// Shows the live bitplane pointers, the render-captured base pointers, and
    /// the key display registers (DMACON, DIW/DDF window, BPLCONx).
    pub fn debug_dmacon(&self) -> u16 {
        self.agnus.dmacon
    }

    pub fn debug_display_state(&self) -> String {
        format!(
            "dmacon={:#06X} diwstrt={:#06X} diwstop={:#06X} ddfstrt={:#06X} ddfstop={:#06X} \
             bplcon0={:#06X} bplcon1={:#06X} bplcon2={:#06X} bpl1mod={} bpl2mod={} \
             bplpt={:08X?} dispbplpt={:08X?}",
            self.agnus.dmacon,
            self.denise.diwstrt,
            self.denise.diwstop,
            self.denise.ddfstrt,
            self.denise.ddfstop,
            self.denise.bplcon0,
            self.denise.bplcon1,
            self.denise.bplcon2,
            self.denise.bpl1mod,
            self.denise.bpl2mod,
            self.denise.bplpt,
            self.display_dma_bplpt,
        )
    }

    /// Side-effect-free debugger view of a custom-register word.
    ///
    /// This is intentionally not the CPU-visible custom bus read path:
    /// inspecting state from GDB or the in-window debugger must not acknowledge
    /// interrupts, advance collision latches, flush audio, or return an
    /// undriven-bus value for write-only registers whose internal latch is
    /// useful to inspect.
    pub fn debug_custom_word(&self, off: u16) -> Option<u16> {
        let off = off & 0x1FE;
        let value = match off {
            0x002 => {
                let mut r = self.agnus.dmacon & 0x07FF;
                if self.blitter.busy {
                    r |= 1 << 14;
                }
                if self.blitter.bzero {
                    r |= 1 << 13;
                }
                r
            }
            0x004 => self.agnus.read_vposr(),
            0x006 => self.agnus.read_vhposr(),
            0x010 => self.paula.adkcon,
            0x01C => self.paula.intena,
            0x01E => self.cpu_visible_intreq(),
            0x080 => ((self.agnus.cop1lc >> 16) & 0x001F) as u16,
            0x082 => (self.agnus.cop1lc & 0xFFFE) as u16,
            0x084 => ((self.agnus.cop2lc >> 16) & 0x001F) as u16,
            0x086 => (self.agnus.cop2lc & 0xFFFE) as u16,
            0x096 => self.agnus.dmacon,
            0x09A => self.paula.intena,
            0x09C => self.cpu_visible_intreq(),
            0x09E => self.paula.adkcon,
            audio @ 0x0A0..=0x0DF => self.paula.peek_audio_reg_latch(audio - 0x0A0)?,
            other => self.custom_reg_latch(other)?,
        };
        Some(value)
    }

    /// The writable RAM regions a memory hunt should scan: chip RAM,
    /// slow/ranger RAM, and configured Zorro RAM boards, as
    /// (base, length) pairs.
    pub fn writable_ram_regions(&self) -> Vec<(u32, u32)> {
        let mut regions = Vec::new();
        if !self.mem.chip_ram.is_empty() {
            regions.push((
                crate::memory::CHIP_RAM_BASE as u32,
                self.mem.chip_ram.len() as u32,
            ));
        }
        if !self.mem.slow_ram.is_empty() {
            regions.push((
                crate::memory::SLOW_RAM_BASE as u32,
                self.mem.slow_ram.len() as u32,
            ));
        }
        regions.extend(self.mem.zorro.ram_regions());
        regions
    }

    /// Read a 16-bit big-endian word from whichever RAM/ROM region maps
    /// `addr` (chip, fast, slow, or ROM), for the debugger's memory dumps.
    /// Returns 0 for unmapped addresses.
    pub fn peek_word_any(&self, addr: u32) -> u16 {
        use crate::memory::{CHIP_RAM_BASE, ROM_BASE, SLOW_RAM_BASE};
        if let Some((board, off)) = self.mem.zorro.region_at(addr, 2) {
            let ram = self.mem.zorro.board_ram(board);
            return ((ram[off] as u16) << 8) | ram[off + 1] as u16;
        }
        // Device-board windows: serve what the device can answer without
        // side effects (e.g. the A2091 boot ROM, where scsi.device task
        // and node names live), 0 for live registers.
        if let Some((crate::zorro::BoardBacking::Device(dev), off)) =
            self.mem.zorro.device_region_at(addr, 2)
        {
            return crate::zorro_device::ZorroDevice::peek_word(&self.devices[dev], off)
                .unwrap_or(0);
        }
        let a = addr as usize;
        let regions: [(usize, &[u8]); 4] = [
            (CHIP_RAM_BASE as usize, &self.mem.chip_ram),
            (SLOW_RAM_BASE as usize, &self.mem.slow_ram),
            (ROM_BASE as usize, &self.mem.rom),
            (self.mem.extended_rom_base as usize, &self.mem.extended_rom),
        ];
        for (base, mem) in regions {
            if a >= base && a.wrapping_sub(base) + 1 < mem.len() {
                let off = a - base;
                return ((mem[off] as u16) << 8) | mem[off + 1] as u16;
            }
        }
        0
    }

    pub fn peek_chip_word(&self, addr: usize) -> u16 {
        let ram = &self.mem.chip_ram;
        if addr + 1 >= ram.len() {
            return 0;
        }
        ((ram[addr] as u16) << 8) | ram[addr + 1] as u16
    }

    /// Emit the per-display-frame chip-bus color-clock accounting and reset
    /// the accumulators. Called once per beam frame from begin_new_beam_frame.
    fn log_bus_accounting_frame(&mut self) {
        if !self.bus_accounting.enabled {
            return;
        }
        let total: u64 = self.bus_accounting.owner_cck.iter().sum();
        if total == 0 {
            return;
        }
        let blit_idx = ChipBusOwner::Blitter.accounting_index();
        let blit_grant = self.bus_accounting.owner_cck[blit_idx];
        let blit_busy = self.bus_accounting.blitter_busy_cck;
        let grant_pct = if blit_busy > 0 {
            blit_grant as f64 / blit_busy as f64 * 100.0
        } else {
            0.0
        };
        let mut owners = String::new();
        let mut starve = String::new();
        for i in 0..9 {
            if self.bus_accounting.owner_cck[i] > 0 {
                owners.push_str(&format!(
                    " {}={}",
                    CHIP_BUS_OWNER_NAMES[i], self.bus_accounting.owner_cck[i]
                ));
            }
            if self.bus_accounting.blitter_starve_cck[i] > 0 {
                starve.push_str(&format!(
                    " {}={}",
                    CHIP_BUS_OWNER_NAMES[i], self.bus_accounting.blitter_starve_cck[i]
                ));
            }
        }
        // Optional diagnostic: sample a chip-RAM word so frame windows can be
        // grouped by a software counter. Meaningless unless the watched address
        // is known for the workload under inspection.
        let diag_ctr = self.peek_chip_word(0x4_C4EE);
        log::info!(
            "bus-acct frame={} t={:.3}s diag_ctr={} total_cck={} blit_busy={} blit_grant={} grant_pct={:.1} blits(line={}/{}cck normal={}/{}cck) |{} | blit_starve:{}",
            self.emulated_frames,
            self.emulated_seconds(),
            diag_ctr,
            total,
            blit_busy,
            blit_grant,
            grant_pct,
            self.bus_accounting.blits_line,
            self.bus_accounting.slots_line,
            self.bus_accounting.blits_normal,
            self.bus_accounting.slots_normal,
            owners,
            if starve.is_empty() { " none" } else { &starve },
        );
        log::info!(
            "bus-acct-cpu frame={} cpu_granted_slots={} cpu_missed_slots={}",
            self.emulated_frames,
            self.cpu_granted_chip_slots,
            self.cpu_missed_chip_slots,
        );
        self.cpu_granted_chip_slots = 0;
        self.cpu_missed_chip_slots = 0;
        self.bus_accounting.reset_frame();
    }

    fn reset_current_frame_bus_trace(&mut self, partial: bool) {
        if !self.frame_analyzer_enabled {
            return;
        }
        self.current_frame_bus_trace.reset_for_frame(
            self.emulated_frames,
            self.emulated_seconds(),
            self.agnus.current_frame_lines(),
            self.agnus.current_line_cck(),
            self.current_frame_visible_start_vpos,
            self.current_frame_geometry.visible_lines,
            partial,
        );
    }

    fn finish_frame_bus_trace(&mut self) {
        if !self.frame_analyzer_enabled {
            self.current_frame_bus_trace.clear();
            self.last_frame_bus_trace = None;
            return;
        }
        self.current_frame_bus_trace.finish_window(
            self.current_frame_visible_start_vpos,
            self.current_frame_geometry.visible_lines,
        );
        if self.current_frame_bus_trace.has_samples() {
            self.last_frame_bus_trace = Some(self.current_frame_bus_trace.clone());
        }
    }

    /// The `COPPERLINE_DIAG_*` frame-start dumps (bpl-dma summary, slotmap
    /// dump, copper-list length walk, BUF0/sprite snapshots), split out of
    /// `begin_new_beam_frame` so that function's real frame-boundary
    /// bookkeeping is reviewable on its own instead of interleaved with
    /// ~200 lines of opt-in diagnostic logging.
    fn diag_log_frame_start(&mut self) {
        if crate::envcfg::flag("COPPERLINE_DIAG_DISPLAY") {
            let lines: Vec<usize> = self
                .dbg_bpl_cck
                .iter()
                .enumerate()
                .filter(|(_, &c)| c > 0)
                .map(|(v, _)| v)
                .collect();
            if !lines.is_empty() {
                let total: u32 = self.dbg_bpl_cck.iter().sum();
                let first = *lines.first().unwrap();
                let last = *lines.last().unwrap();
                let per_line: Vec<u32> = lines.iter().map(|&v| self.dbg_bpl_cck[v]).collect();
                let min = *per_line.iter().min().unwrap();
                let max = *per_line.iter().max().unwrap();
                let anomalous: Vec<usize> = lines
                    .iter()
                    .copied()
                    .filter(|&v| self.dbg_bpl_cck[v] != 44)
                    .collect();
                log::info!(
                    "bpl-dma frame={} lines={} (v{}..v{}) total_cck={} per_line_min={} max={} anomalous_lines({})={:?}",
                    self.emulated_frames,
                    lines.len(),
                    first,
                    last,
                    total,
                    min,
                    max,
                    anomalous.len(),
                    &anomalous[..anomalous.len().min(50)],
                );
            }
            for c in self.dbg_bpl_cck.iter_mut() {
                *c = 0;
            }
        }
        if self.dbg_slotmap_on && !self.dbg_slotmap_dumped && !self.dbg_slotmap.is_empty() {
            // Dump the per-color-clock slot-owner map once, for the first frame
            // that contains a 4-plane (loading-screen gear) band. Covers the
            // 2->4->2 plane transition so it can be diffed against vAmiga's
            // DMA Debugger line by line. Codes: R refresh, B bitplane, S sprite,
            // D disk, A audio, C copper, L blitter, P cpu, . idle.
            let bcount = |v: usize| self.dbg_slotmap[v].iter().filter(|&&b| b == b'B').count();
            // Default: dump the first 4-plane (gear) frame. COPPERLINE_DIAG_SLOTMAP_AT
            // (seconds) instead dumps the first frame at/after that time -- used to
            // capture the 3D-vector-scene BLITWAIT contention.
            let trigger = match crate::envcfg::var("COPPERLINE_DIAG_SLOTMAP_AT")
                .and_then(|s| s.parse::<f64>().ok())
            {
                Some(at) => self.emulated_seconds() >= at,
                None => (140..200).any(|v: usize| bcount(v) > 50),
            };
            if trigger {
                self.dbg_slotmap_dumped = true;
                let (range_start, range_end) = crate::envcfg::var("COPPERLINE_DIAG_SLOTMAP_RANGE")
                    .and_then(|raw| {
                        let (start, end) = raw.split_once(':')?;
                        Some((
                            start.trim().parse::<usize>().ok()?,
                            end.trim().parse::<usize>().ok()?,
                        ))
                    })
                    .filter(|&(start, end)| start <= end && end < self.dbg_slotmap.len())
                    .unwrap_or((138, 198));
                log::info!(
                    "slotmap frame={} band v{}..{} (hpos 0..=0xE2); codes R/B/S/D/A/C/L/P/.",
                    self.emulated_frames,
                    range_start,
                    range_end
                );
                for v in range_start..=range_end {
                    let row = &self.dbg_slotmap[v];
                    let line: String = row[..227.min(row.len())]
                        .iter()
                        .map(|&b| b as char)
                        .collect();
                    let nb = bcount(v);
                    let ncpu = row.iter().filter(|&&b| b == b'P').count();
                    log::info!("slotmap v={v:3} B={nb:3} P={ncpu:3} |{line}");
                }
            }
        }
        if self.dbg_slotmap_on && !self.dbg_slotmap.is_empty() {
            for row in self.dbg_slotmap.iter_mut() {
                for slot in row.iter_mut() {
                    *slot = b'.';
                }
            }
        }
        if crate::envcfg::flag("COPPERLINE_DIAG_COPLEN")
            && !COPLEN_LOGGED.load(std::sync::atomic::Ordering::Relaxed)
        {
            let at = crate::envcfg::var("COPPERLINE_DIAG_COPLEN")
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(52.0);
            let secs = self.emulated_seconds();
            if secs >= at && self.last_chip_bus_owner != ChipBusOwner::Refresh {
                // walk the copper-1 list and count instructions to the terminating
                // WAIT($FF,$FE), to compare against how many the copper actually ran.
                let base = self.agnus.cop1lc as usize;
                let mut n = 0usize;
                let mut addr = base;
                let mut ended = false;
                while n < 4000 {
                    let w0 = self.peek_chip_word(addr);
                    let w1 = self.peek_chip_word(addr + 2);
                    n += 1;
                    addr += 4;
                    if w0 & 1 != 0 && w0 == 0xFFFF && w1 == 0xFFFE {
                        ended = true;
                        break;
                    }
                }
                // scan again, logging control-flow instructions (WAIT/SKIP and any
                // MOVE to COPJMP1/2 = reg 0x088/0x08A) that could cause looping.
                let mut a2 = base;
                let mut cf = String::new();
                for _ in 0..n.min(400) {
                    let w0 = self.peek_chip_word(a2);
                    let w1 = self.peek_chip_word(a2 + 2);
                    if w0 & 1 == 0 {
                        let reg = w0 & 0x01FE;
                        if reg == 0x088 || reg == 0x08A {
                            cf.push_str(&format!(" COPJMP@{a2:#08X}(reg{reg:#05X})"));
                        }
                    } else if w1 & 1 != 0 {
                        cf.push_str(&format!(" SKIP@{a2:#08X}"));
                    }
                    a2 += 4;
                }
                log::info!(
                    "coplen f={} cop1lc={:#08X} list_instructions={} ended={} ctrlflow:{}",
                    self.emulated_frames,
                    self.agnus.cop1lc,
                    n,
                    ended,
                    if cf.is_empty() {
                        " (none -- pure MOVE/WAIT list)".into()
                    } else {
                        cf
                    },
                );
                log::info!(
                    "bplsetup f={} bplpt={:08X?} bpl1mod={} bpl2mod={} bplcon0={:#06X} ddf={:#06X}..{:#06X}",
                    self.emulated_frames,
                    self.denise.bplpt,
                    self.denise.bpl1mod,
                    self.denise.bpl2mod,
                    self.denise.bplcon0,
                    self.denise.ddfstrt,
                    self.denise.ddfstop,
                );
                // dump all 4 planes at line 90 (each plane base + 24*352) to see if
                // every plane has structured data or only plane 0 is filled.
                for (p, base) in [0x03E606usize, 0x03E65E, 0x03E6B6, 0x03E70E]
                    .iter()
                    .enumerate()
                {
                    let a = base + (90 - 66) * 352;
                    let mut s = String::new();
                    for k in 0..14 {
                        s.push_str(&format!("{:04X} ", self.peek_chip_word(a + k * 2)));
                    }
                    log::info!("memdump logo-pl{}-line90@{a:#08X}: {s}", p + 1);
                }
                // walk the list and log every MOVE to a COLOR register (0x180-0x1BE)
                // with its immediate value -- the source-of-truth palette in the
                // copper list itself, to compare against what gets applied.
                let mut a3 = base;
                let mut cols = String::new();
                for _ in 0..n {
                    let w0 = self.peek_chip_word(a3);
                    let w1 = self.peek_chip_word(a3 + 2);
                    if w0 & 1 == 0 {
                        let reg = w0 & 0x01FE;
                        if (0x180..=0x1BE).contains(&reg) {
                            let idx = (reg - 0x180) / 2;
                            cols.push_str(&format!(" c{idx:02}@{a3:#08X}={:04X}", w1 & 0x0FFF));
                        }
                    } else if w0 == 0xFFFF && w1 == 0xFFFE {
                        break;
                    }
                    a3 += 4;
                }
                log::info!("coplist-colors:{cols}");
                COPLEN_LOGGED.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        if crate::envcfg::flag("COPPERLINE_DIAG_BUF0") {
            let secs = self.emulated_seconds();
            // log once per ~second across the run
            if secs.fract() < 0.02 {
                let mut nz = 0;
                for row in 0..272usize {
                    for w in 0..22usize {
                        if self.peek_chip_word(0x042EC0 + row * 44 + w * 2) != 0 {
                            nz += 1;
                        }
                    }
                }
                log::info!("buf0 nonzero-words={nz} t={secs:.4}");
            }
        }
        if crate::envcfg::flag("COPPERLINE_DIAG_SPRITES") {
            let secs = self.emulated_seconds();
            if (44.44..44.52).contains(&secs) {
                let spren = (self.agnus.dmacon >> 5) & 1;
                let aud = self.agnus.dmacon & 0x000F;
                let mut s = String::new();
                for i in 0..8 {
                    let st = &self.display_dma_sprite_state[i];
                    let ena = st.dma_enabled as u8;
                    let armed = st.last_line.is_some() as u8;
                    s.push_str(&format!(
                        " s{i}[ena={ena} armed={armed} vstrt={} vstop={} ptr={:06X}]",
                        st.vstrt, st.vstop, self.display_dma_sprpt[i]
                    ));
                }
                log::info!(
                    "sprites f={} SPREN={} AUD={:X}{}",
                    self.emulated_frames,
                    spren,
                    aud,
                    s
                );
            }
        }
    }
}

fn bus_slots_for_cpu_access(size: usize) -> u32 {
    (size.max(1) as u32).div_ceil(2)
}

fn add_agnus_tick(total: &mut AgnusTick, tick: AgnusTick) {
    total.new_lines = total.new_lines.saturating_add(tick.new_lines);
    total.new_frames = total.new_frames.saturating_add(tick.new_frames);
}

fn diw_v_start(diwstrt: u16, diwhigh: DiwHigh) -> u16 {
    diwhigh.v_start(diwstrt)
}

fn diw_v_stop(diwstop: u16, diwhigh: DiwHigh) -> u16 {
    diwhigh.v_stop(diwstop)
}

fn display_window_unprogrammed(diwstrt: u16, diwstop: u16) -> bool {
    diwstrt == 0 && diwstop == 0
}

fn visible_start_vpos_for_diw(diwstrt: u16, diwstop: u16, diwhigh: DiwHigh) -> u32 {
    if display_window_unprogrammed(diwstrt, diwstop) {
        return RENDER_VISIBLE_START_VPOS;
    }
    u32::from(diw_v_start(diwstrt, diwhigh))
        .clamp(RENDER_MIN_OVERSCAN_START_VPOS, RENDER_VISIBLE_START_VPOS)
}

fn clipped_display_rows_before_visible(
    diwstrt: u16,
    diwstop: u16,
    diwhigh: DiwHigh,
    visible_start_vpos: u32,
) -> usize {
    if display_window_unprogrammed(diwstrt, diwstop) {
        return 0;
    }
    (visible_start_vpos as i32 - diw_v_start(diwstrt, diwhigh) as i32).max(0) as usize
}

fn visible_framebuffer_y(
    vpos: u32,
    visible_start_vpos: u32,
    visible_lines: usize,
) -> Option<usize> {
    vpos.checked_sub(visible_start_vpos)
        .map(|y| y as usize)
        .filter(|&y| y < visible_lines)
}

fn display_window_contains_vpos(diwstrt: u16, diwstop: u16, diwhigh: DiwHigh, vpos: u32) -> bool {
    if display_window_unprogrammed(diwstrt, diwstop) {
        return (RENDER_VISIBLE_START_VPOS
            ..RENDER_VISIBLE_START_VPOS + RENDER_VISIBLE_LINES as u32)
            .contains(&vpos);
    }
    let start = diw_v_start(diwstrt, diwhigh) as u32;
    let mut stop = diw_v_stop(diwstop, diwhigh) as u32;
    let mut v = vpos;
    if stop <= start {
        stop += 0x100;
        if v < start {
            v += 0x100;
        }
    }
    v >= start && v < stop
}

fn bitplane_words_per_row(
    revision: AgnusRevision,
    bplcon0: u16,
    fmode: u16,
    ddfstrt: u16,
    ddfstop: u16,
    harddis: bool,
) -> usize {
    let fallback = if bitplane_shres(bplcon0) {
        RENDER_FRAMEBUFFER_WIDTH as usize * 2
    } else if bitplane_hires(bplcon0) {
        RENDER_FRAMEBUFFER_WIDTH as usize
    } else {
        RENDER_FRAMEBUFFER_WIDTH as usize / 2
    } / 16;
    let Some((start, stop)) = effective_ddf_window(revision, bplcon0, ddfstrt, ddfstop, harddis)
    else {
        return fallback;
    };
    let unit = bitplane_fetch_unit(bplcon0, fmode);
    let start = crate::chipset::agnus::anchor_bitplane_fetch_start(start, unit);
    let blocks = crate::chipset::agnus::bitplane_fetch_blocks(u32::from(stop - start), unit);
    let words = blocks * (unit / bitplane_fetch_cck_per_word(bplcon0)) as usize;
    words.max(1)
}

// These fetch-mode helpers used to be independently-maintained twins of
// the agnus.rs functions of the same name/shape; they now delegate to the
// single agnus.rs implementation (kept as thin wrappers here so none of
// this file's many call sites had to change).
fn bitplane_shres(bplcon0: u16) -> bool {
    crate::chipset::agnus::bitplane_shres(bplcon0)
}

fn bitplane_hires(bplcon0: u16) -> bool {
    crate::chipset::agnus::bitplane_hires(bplcon0)
}

fn bitplane_fetch_quantum(fmode: u16) -> u32 {
    crate::chipset::agnus::bitplane_fetch_quantum(fmode)
}

fn bitplane_fetch_period(bplcon0: u16, fmode: u16) -> u32 {
    crate::chipset::agnus::bitplane_fetch_period(bplcon0, fmode)
}

fn bitplane_fetch_unit(bplcon0: u16, fmode: u16) -> u32 {
    crate::chipset::agnus::bitplane_fetch_unit(bplcon0, fmode)
}

fn plane_mask_for_count(nplanes: usize) -> u16 {
    if nplanes >= 8 {
        0x00FF
    } else {
        (1u16 << nplanes) - 1
    }
}

/// FMODE SSCAN2 (bit 15, Alice only): sprite scan doubling. Sprite data DMA
/// fetches a new line only on every second display line of the sprite; the
/// in-between line redisplays the previous data, so each fetched line covers
/// two display lines of a double-scan mode.
fn sprite_scan_doubled(fmode: u16) -> bool {
    fmode & 0x8000 != 0
}

/// AGA FMODE sprite fetch width (SPR32/SPAGEM, bits 2-3): 16-bit words per
/// sprite channel fetch.
fn sprite_fetch_quantum(fmode: u16) -> u32 {
    match (fmode >> 2) & 0x0003 {
        0 => 1,
        3 => 4,
        _ => 2,
    }
}

fn bitplane_fetch_cck_per_word(bplcon0: u16) -> u32 {
    crate::chipset::agnus::bitplane_fetch_cck_per_word(bplcon0)
}

fn chip_dma_addr_mask(chip_ram_len: usize) -> u32 {
    let bytes = chip_ram_len.next_power_of_two().clamp(2, 0x0020_0000usize);
    (bytes - 1) as u32
}

// The vertical-blank COP1LC strobe wakes the Copper early on the restart
// line: its first instruction-word fetch lands on the hpos $02 access cycle
// and a leading MOVE's write on $04, matching the vAmiga copper trace
// (jumpbpu image-list upload; first MOVE write at v=0 h=$04). The value was
// 6 while copper WAIT releases ran four colour clocks late; it moved in
// lockstep with the WAIT comparator lookahead fix so un-waited frame-start
// streams keep their calibrated screen positions.
pub(crate) const COPPER_FRAME_START_HPOS: u32 = 2;

fn copper_frame_start_vpos(_video_standard: VideoStandard) -> u32 {
    // The Copper is restarted (COP1LC reloaded into the Copper PC) at the very
    // top of every frame and runs through the vertical-blank lines, not just
    // the displayed region. Demos rely on this: their copper lists do
    // frame-top setup -- and crucially trigger the per-frame CPU work via a
    // copper MOVE to INTREQ (a SOFT/copper interrupt) -- during vblank. Holding
    // the Copper idle until the end of vblank delayed that trigger by ~25
    // lines, collapsing the CPU's pre-display work margin. Restarting at line 0
    // restores the margin real hardware gives before early display DMA fetches.
    0
}

fn sprite_dma_first_active_vpos(video_standard: VideoStandard) -> u32 {
    // Hard vertical blank inhibits sprite DMA near the top of a standard
    // field. The first line after that blank is one line earlier than bitplane
    // DMA: PAL line $19 and NTSC line $14.
    match video_standard {
        VideoStandard::Pal => PAL_SPRITE_DMA_FIRST_ACTIVE_VPOS,
        VideoStandard::Ntsc => NTSC_SPRITE_DMA_FIRST_ACTIVE_VPOS,
    }
}

fn next_chip_bus_quantum_at(hpos: u32, line_cck: u32) -> u32 {
    CHIP_BUS_SLOT_CCK.min(line_cck.saturating_sub(hpos).max(1))
}

fn effective_ddf_hpos(revision: AgnusRevision, bplcon0: u16, raw: u16) -> u16 {
    effective_ddf_start_hpos_raw(revision, bplcon0, raw)
}

fn effective_ddf_start_hpos_raw(revision: AgnusRevision, bplcon0: u16, raw: u16) -> u16 {
    crate::chipset::agnus::effective_bitplane_ddf_start_hpos(revision, bplcon0, raw)
}

fn effective_ddf_start_hpos(revision: AgnusRevision, bplcon0: u16, raw: u16) -> u16 {
    let start = effective_ddf_start_hpos_raw(revision, bplcon0, raw);
    if start == 0 {
        0
    } else {
        start.clamp(BITPLANE_DDF_HARD_START, BITPLANE_DDF_HARD_STOP)
    }
}

fn effective_ddf_window(
    revision: AgnusRevision,
    bplcon0: u16,
    ddfstrt: u16,
    ddfstop: u16,
    harddis: bool,
) -> Option<(u16, u16)> {
    crate::chipset::agnus::effective_bitplane_ddf_window(
        revision, bplcon0, ddfstrt, ddfstop, harddis,
    )
}

fn bitplane_fetch_order(bplcon0: u16, plane: usize) -> u32 {
    crate::chipset::agnus::bitplane_fetch_order(bplcon0, plane)
}

fn read_chip_word_wrapping(ram: &[u8], addr: u32) -> u16 {
    let len = ram.len();
    let a = addr as usize % len;
    u16::from_be_bytes([ram[a], ram[(a + 1) % len]])
}

fn sprite_vstart_from_words(pos: u16, ctl: u16) -> i32 {
    (((pos >> 8) & 0x00FF) | ((ctl & 0x0004) << 6)) as i32
}

fn sprite_vstop_from_ctl(ctl: u16) -> i32 {
    (((ctl >> 8) & 0x00FF) | ((ctl & 0x0002) << 7)) as i32
}

fn sprite_hstart_from_words(pos: u16, ctl: u16) -> i32 {
    (((pos & 0x00FF) << 1) | (ctl & 0x0001)) as i32
}

fn sprite_hsub_70ns_from_ctl(ctl: u16) -> bool {
    ctl & 0x0010 != 0
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct LiveSpriteCollisionSource {
    group: usize,
    hstart: i32,
    hsub_70ns: bool,
    words: [u16; 4],
    requires_odd_enable: bool,
}

#[derive(Clone, Copy)]
struct LiveManualSpriteCollisionSource {
    #[cfg_attr(not(test), allow(dead_code))]
    sprite: usize,
    source: LiveSpriteCollisionSource,
    x_start: i32,
    x_stop: i32,
}

fn live_sprite_playfield_collision_sources(
    lines: &[CapturedSpriteLine],
    beam_y: i32,
) -> Vec<LiveSpriteCollisionSource> {
    live_sprite_collision_sources_with_beam_gated_odd(lines, beam_y)
}

fn live_sprite_collision_sources_with_beam_gated_odd(
    lines: &[CapturedSpriteLine],
    beam_y: i32,
) -> Vec<LiveSpriteCollisionSource> {
    live_sprite_collision_sources_with_odd_policy(lines, beam_y, 0, true)
}

fn live_sprite_collision_sources_with_odd_policy(
    lines: &[CapturedSpriteLine],
    beam_y: i32,
    clxcon: u16,
    include_disabled_odd: bool,
) -> Vec<LiveSpriteCollisionSource> {
    let mut sources = Vec::new();

    for sprite in 0..8 {
        let Some(line) = lines
            .iter()
            .find(|line| line.sprite == sprite && line.beam_y == beam_y)
        else {
            continue;
        };
        let group = sprite / 2;
        let requires_odd_enable = sprite & 1 != 0;
        if requires_odd_enable && !include_disabled_odd && clxcon & (1 << (12 + group)) == 0 {
            continue;
        }
        push_live_sprite_collision_source_if_visible(
            &mut sources,
            LiveSpriteCollisionSource {
                group,
                hstart: line.hstart,
                hsub_70ns: line.hsub_70ns,
                words: [line.data, line.datb, 0, 0],
                requires_odd_enable,
            },
        );
    }

    sources
}

fn push_live_sprite_collision_source_if_visible(
    sources: &mut Vec<LiveSpriteCollisionSource>,
    source: LiveSpriteCollisionSource,
) {
    if live_sprite_source_has_pixels(&source) {
        sources.push(source);
    }
}

fn sprite_pixel_repeat_for_control(bplcon0: u16, bplcon3: u16) -> i32 {
    match bplcon3 & BPLCON3_SPRES_MASK {
        0 => {
            if bplcon0 & BPLCON0_SHRES != 0 {
                1
            } else {
                2
            }
        }
        BPLCON3_SPRES_LORES => 2,
        BPLCON3_SPRES_HIRES | BPLCON3_SPRES_SHRES => 1,
        _ => unreachable!(),
    }
}

fn live_sprite_sprite_collision_bits(
    sources: &[LiveSpriteCollisionSource],
    control_replay: &LiveCollisionLineReplay,
    beam_y: i32,
    x_start: i32,
    x_stop: i32,
    display_enable_x: Option<i32>,
    latched_clxdat: u16,
) -> u16 {
    if sources.len() < 2 {
        return 0;
    }
    let x_start = x_start.max(0);
    let x_stop = x_stop.min(RENDER_FRAMEBUFFER_WIDTH);
    if x_start >= x_stop {
        return 0;
    }
    let target_mask = live_sprite_sprite_possible_clx_mask(sources, x_start, x_stop);
    let needed_mask = target_mask & !latched_clxdat;
    if needed_mask == 0 {
        return 0;
    }

    let mut clxdat = 0u16;
    let constant_control = control_replay.constant_control();
    for (idx, source) in sources.iter().enumerate() {
        for other in &sources[idx + 1..] {
            if source.group == other.group {
                continue;
            }
            let clx_bit = sprite_sprite_clx_bit(source.group, other.group);
            if clx_bit == 0 || needed_mask & clx_bit == 0 || clxdat & clx_bit != 0 {
                continue;
            }
            let Some((pair_x_start, pair_x_stop)) =
                live_sprite_source_pair_x_range(source, other, x_start, x_stop)
            else {
                continue;
            };
            if let Some(control) = constant_control {
                let Some((visible_x_start, visible_x_stop)) =
                    live_sprite_visible_x_range_for_control(
                        control,
                        beam_y,
                        pair_x_start,
                        pair_x_stop,
                        display_enable_x,
                    )
                else {
                    continue;
                };
                for x in visible_x_start..visible_x_stop {
                    if !live_sprite_source_collision_matches_with_control(
                        source,
                        control,
                        control.clxcon,
                        x,
                    ) {
                        continue;
                    }
                    if !live_sprite_source_collision_matches_with_control(
                        other,
                        control,
                        control.clxcon,
                        x,
                    ) {
                        continue;
                    }
                    clxdat |= clx_bit;
                    break;
                }
                if clxdat & needed_mask == needed_mask {
                    return clxdat;
                }
                continue;
            }
            for x in pair_x_start..pair_x_stop {
                let control = control_replay.control_for_x(x);
                if !live_sprite_pixel_inside_display_window(control, beam_y, x, display_enable_x) {
                    continue;
                }
                if !live_sprite_source_collision_matches(source, control_replay, control.clxcon, x)
                {
                    continue;
                }
                if !live_sprite_source_collision_matches(other, control_replay, control.clxcon, x) {
                    continue;
                }
                clxdat |= clx_bit;
                break;
            }
            if clxdat & needed_mask == needed_mask {
                return clxdat;
            }
        }
    }

    clxdat
}

fn live_manual_sprite_sprite_collision_bits_in_range(
    frame_base: RenderRegisterSnapshot,
    source_index: &BeamEventIndex,
    control_replay: &LiveCollisionLineReplay,
    beam_y: i32,
    x_start: i32,
    x_stop: i32,
    display_enable_x: Option<i32>,
    latched_clxdat: u16,
) -> u16 {
    if beam_y < 0 {
        return 0;
    }
    let x_start = x_start.max(0);
    let x_stop = x_stop.min(RENDER_FRAMEBUFFER_WIDTH);
    if x_start >= x_stop {
        return 0;
    }
    let sources =
        live_manual_sprite_collision_sources(frame_base, source_index, beam_y, x_start, x_stop);
    if sources.len() < 2 {
        return 0;
    }
    let target_mask = live_manual_sprite_sprite_possible_clx_mask(&sources, x_start, x_stop);
    let needed_mask = target_mask & !latched_clxdat;
    if needed_mask == 0 {
        return 0;
    }

    let mut clxdat = 0u16;
    if let Some(control) = control_replay.constant_control() {
        let Some((visible_x_start, visible_x_stop)) = live_sprite_visible_x_range_for_control(
            control,
            beam_y,
            x_start,
            x_stop,
            display_enable_x,
        ) else {
            return 0;
        };
        for x in visible_x_start..visible_x_stop {
            let mut occupied_groups = 0u8;
            for source in &sources {
                if x < source.x_start || x >= source.x_stop {
                    continue;
                }
                if !live_sprite_source_collision_matches_with_control(
                    &source.source,
                    control,
                    control.clxcon,
                    x,
                ) {
                    continue;
                }
                for other_group in 0..4 {
                    if occupied_groups & (1 << other_group) != 0
                        && other_group != source.source.group
                    {
                        let clx_bit = sprite_sprite_clx_bit(source.source.group, other_group);
                        if needed_mask & clx_bit != 0 {
                            clxdat |= clx_bit;
                        }
                    }
                }
                if clxdat & needed_mask == needed_mask {
                    return clxdat;
                }
                occupied_groups |= 1 << source.source.group;
            }
        }
        return clxdat;
    }

    for x in x_start..x_stop {
        let control = control_replay.control_for_x(x);
        if !live_sprite_pixel_inside_display_window(control, beam_y, x, display_enable_x) {
            continue;
        }
        let mut occupied_groups = 0u8;
        for source in &sources {
            if x < source.x_start || x >= source.x_stop {
                continue;
            }
            if !live_sprite_source_collision_matches(
                &source.source,
                control_replay,
                control.clxcon,
                x,
            ) {
                continue;
            }
            for other_group in 0..4 {
                if occupied_groups & (1 << other_group) != 0 && other_group != source.source.group {
                    let clx_bit = sprite_sprite_clx_bit(source.source.group, other_group);
                    if needed_mask & clx_bit != 0 {
                        clxdat |= clx_bit;
                    }
                }
            }
            if clxdat & needed_mask == needed_mask {
                return clxdat;
            }
            occupied_groups |= 1 << source.source.group;
        }
    }
    clxdat
}

fn live_manual_sprite_playfield_collision_bits_in_range(
    row: &CapturedBitplaneRow,
    frame_base: RenderRegisterSnapshot,
    source_index: &BeamEventIndex,
    playfield_control: &LiveCollisionLineReplay,
    sprite_control: &LiveCollisionLineReplay,
    beam_y: i32,
    x_start: i32,
    x_stop: i32,
    display_enable_x: Option<i32>,
    latched_clxdat: u16,
) -> u16 {
    if beam_y < 0 {
        return 0;
    }
    let x_start = x_start.max(0);
    let x_stop = x_stop.min(RENDER_FRAMEBUFFER_WIDTH);
    if x_start >= x_stop {
        return 0;
    }
    let sources =
        live_manual_sprite_collision_sources(frame_base, source_index, beam_y, x_start, x_stop);
    if sources.is_empty() {
        return 0;
    }
    let target_mask = live_manual_sprite_playfield_possible_clx_mask(&sources, x_start, x_stop);
    let needed_mask = target_mask & !latched_clxdat;
    if needed_mask == 0 {
        return 0;
    }

    let mut clxdat = 0u16;
    let sprite_constant_control = sprite_control.constant_control();
    let (x_start, x_stop) = if let Some(control) = sprite_constant_control {
        let Some(range) = live_sprite_visible_x_range_for_control(
            control,
            beam_y,
            x_start,
            x_stop,
            display_enable_x,
        ) else {
            return 0;
        };
        range
    } else {
        (x_start, x_stop)
    };
    for x in x_start..x_stop {
        let control = playfield_control.control_for_x(x);
        let Some(collision) = live_bitplane_collision_pixel_at(
            row,
            control.agnus_revision,
            control.bplcon0,
            control.bplcon1,
            control.clxcon,
            control.diwstrt,
            control.diwstop,
            control.diwhigh,
            control.ddfstrt,
            control.bpldat,
            x,
        ) else {
            continue;
        };
        for source in &sources {
            if x < source.x_start || x >= source.x_stop {
                continue;
            }
            let sprite_matches = if let Some(sprite_control_at_x) = sprite_constant_control {
                live_sprite_source_collision_matches_with_control(
                    &source.source,
                    sprite_control_at_x,
                    control.clxcon,
                    x,
                )
            } else {
                let sprite_control_at_x = sprite_control.control_for_x(x);
                live_sprite_pixel_inside_display_window(
                    sprite_control_at_x,
                    beam_y,
                    x,
                    display_enable_x,
                ) && live_sprite_source_collision_matches(
                    &source.source,
                    sprite_control,
                    control.clxcon,
                    x,
                )
            };
            if !sprite_matches {
                continue;
            }
            if collision.pf1_match {
                let clx_bit = 1 << (source.source.group + 1);
                if needed_mask & clx_bit != 0 {
                    clxdat |= clx_bit;
                }
            }
            if collision.pf2_match {
                let clx_bit = 1 << (source.source.group + 5);
                if needed_mask & clx_bit != 0 {
                    clxdat |= clx_bit;
                }
            }
            if clxdat & needed_mask == needed_mask {
                return clxdat;
            }
        }
    }
    clxdat
}

fn live_manual_sprite_collision_sources(
    frame_base: RenderRegisterSnapshot,
    event_index: &BeamEventIndex,
    beam_y: i32,
    x_start: i32,
    x_stop: i32,
) -> Vec<LiveManualSpriteCollisionSource> {
    let mut sprpos = frame_base.sprpos;
    let mut sprctl = frame_base.sprctl;
    let mut sprdata = frame_base.sprdata;
    let mut sprdatb = frame_base.sprdatb;
    let mut spr_armed = frame_base.spr_armed;
    let mut interval_start = [0i32; 8];
    let mut sources = Vec::new();

    let Some(line) = beam_y
        .checked_sub(RENDER_VISIBLE_START_VPOS as i32)
        .map(|line| line as usize)
        .filter(|&line| line < RENDER_VISIBLE_LINES)
    else {
        return sources;
    };

    for event in event_index.sprite_register_writes_before_visible_line(line) {
        apply_live_manual_sprite_event(
            &mut sprpos,
            &mut sprctl,
            &mut sprdata,
            &mut sprdatb,
            &mut spr_armed,
            *event,
        );
    }

    let line_events = event_index
        .line(line)
        .map(|line| line.sprite_register_writes())
        .unwrap_or(&[]);

    for event in line_events {
        let off = event.offset & 0x01FE;
        let sprite = ((off - 0x140) / 8) as usize;
        if sprite >= 8 {
            continue;
        }
        let event_x = if event.vpos < beam_y as u32 {
            0
        } else {
            live_manual_sprite_event_x(*event)
        };
        let source_stop = live_manual_sprite_preserved_source_stop(
            *event,
            sprpos[sprite],
            sprctl[sprite],
            bitplane_shres(frame_base.bplcon0),
            event_x,
            x_stop,
        );
        push_live_manual_sprite_source(
            &mut sources,
            sprite,
            sprpos[sprite],
            sprctl[sprite],
            sprdata[sprite],
            sprdatb[sprite],
            spr_armed[sprite],
            bitplane_shres(frame_base.bplcon0),
            beam_y,
            interval_start[sprite].max(x_start),
            source_stop.min(x_stop),
        );
        apply_live_manual_sprite_event(
            &mut sprpos,
            &mut sprctl,
            &mut sprdata,
            &mut sprdatb,
            &mut spr_armed,
            *event,
        );
        interval_start[sprite] = event_x;
    }

    for sprite in 0..8 {
        push_live_manual_sprite_source(
            &mut sources,
            sprite,
            sprpos[sprite],
            sprctl[sprite],
            sprdata[sprite],
            sprdatb[sprite],
            spr_armed[sprite],
            bitplane_shres(frame_base.bplcon0),
            beam_y,
            interval_start[sprite].max(x_start),
            x_stop,
        );
    }

    combine_live_manual_sprite_collision_sources(sources)
}

fn live_manual_sprite_preserved_source_stop(
    event: BeamRegisterWrite,
    sprpos: u16,
    sprctl: u16,
    shres: bool,
    event_x: i32,
    query_x_stop: i32,
) -> i32 {
    let off = event.offset & 0x01FE;
    if !(0x140..=0x17F).contains(&off) || (off - 0x140) & 0x0006 != 0 {
        return event_x;
    }
    let base_x = (sprite_hstart_from_words(sprpos, sprctl) - RENDER_DIW_HSTART_FB0) * 2
        + i32::from(shres && sprite_hsub_70ns_from_ctl(sprctl));
    if event_x >= base_x {
        query_x_stop
    } else {
        event_x
    }
}

fn live_manual_sprite_event_x(event: BeamRegisterWrite) -> i32 {
    let off = event.offset & 0x01FE;
    if (0x140..=0x17F).contains(&off) && (off - 0x140) & 0x0006 == 0 {
        let hpos = event.hpos.saturating_sub(DENISE_HPOS_LAG_CCK);
        return ((hpos as i32 * 2 - RENDER_DIW_HSTART_FB0) * 2).clamp(0, RENDER_FRAMEBUFFER_WIDTH);
    }
    ((event.hpos.saturating_sub(RENDER_COPPER_WAIT_HPOS_FB0)).saturating_mul(4))
        .min(RENDER_FRAMEBUFFER_WIDTH as u32) as i32
}

fn apply_live_manual_sprite_event(
    sprpos: &mut [u16; 8],
    sprctl: &mut [u16; 8],
    sprdata: &mut [u16; 8],
    sprdatb: &mut [u16; 8],
    spr_armed: &mut [bool; 8],
    event: BeamRegisterWrite,
) {
    let off = event.offset & 0x01FE;
    if !(0x140..=0x17F).contains(&off) {
        return;
    }
    let sprite = ((off - 0x140) / 8) as usize;
    if sprite >= 8 {
        return;
    }
    match (off - 0x140) & 0x0006 {
        0x0 => sprpos[sprite] = event.value,
        0x2 => {
            sprctl[sprite] = event.value;
            spr_armed[sprite] = false;
        }
        0x4 => {
            sprdata[sprite] = event.value;
            spr_armed[sprite] = true;
        }
        0x6 => sprdatb[sprite] = event.value,
        _ => {}
    }
}

fn combine_live_manual_sprite_collision_sources(
    sources: Vec<LiveManualSpriteCollisionSource>,
) -> Vec<LiveManualSpriteCollisionSource> {
    let mut combined = Vec::new();
    for source in sources {
        push_live_manual_collision_source_if_visible(
            &mut combined,
            source,
            source.x_start,
            source.x_stop,
        );
    }

    combined
}

fn push_live_manual_collision_source_if_visible(
    combined: &mut Vec<LiveManualSpriteCollisionSource>,
    mut source: LiveManualSpriteCollisionSource,
    x_start: i32,
    x_stop: i32,
) {
    if x_start >= x_stop || !live_sprite_source_has_pixels(&source.source) {
        return;
    }
    source.x_start = x_start;
    source.x_stop = x_stop;
    combined.push(source);
}

fn live_sprite_source_has_pixels(source: &LiveSpriteCollisionSource) -> bool {
    source.words.iter().any(|&word| word != 0)
}

fn live_sprite_source_framebuffer_bounds(source: &LiveSpriteCollisionSource) -> (i32, i32) {
    let x_start = (source.hstart - RENDER_DIW_HSTART_FB0) * 2 + i32::from(source.hsub_70ns);
    (x_start, x_start + 32)
}

fn live_sprite_source_may_overlap_x_range(
    source: &LiveSpriteCollisionSource,
    x_start: i32,
    x_stop: i32,
) -> bool {
    let (source_start, source_stop) = live_sprite_source_framebuffer_bounds(source);
    x_start < source_stop && x_stop > source_start
}

fn live_sprite_source_pair_x_range(
    a: &LiveSpriteCollisionSource,
    b: &LiveSpriteCollisionSource,
    x_start: i32,
    x_stop: i32,
) -> Option<(i32, i32)> {
    let (a_start, a_stop) = live_sprite_source_framebuffer_bounds(a);
    let (b_start, b_stop) = live_sprite_source_framebuffer_bounds(b);
    let start = x_start.max(a_start).max(b_start);
    let stop = x_stop.min(a_stop).min(b_stop);
    (start < stop).then_some((start, stop))
}

fn live_sprite_sprite_possible_clx_mask(
    sources: &[LiveSpriteCollisionSource],
    x_start: i32,
    x_stop: i32,
) -> u16 {
    let mut mask = 0;
    for (idx, source) in sources.iter().enumerate() {
        for other in &sources[idx + 1..] {
            if source.group == other.group
                || live_sprite_source_pair_x_range(source, other, x_start, x_stop).is_none()
            {
                continue;
            }
            mask |= sprite_sprite_clx_bit(source.group, other.group);
        }
    }
    mask & CLXDAT_SPRITE_SPRITE_MASK
}

fn live_manual_sprite_sprite_possible_clx_mask(
    sources: &[LiveManualSpriteCollisionSource],
    x_start: i32,
    x_stop: i32,
) -> u16 {
    let mut mask = 0;
    for (idx, source) in sources.iter().enumerate() {
        for other in &sources[idx + 1..] {
            if source.source.group == other.source.group
                || source.x_start.max(other.x_start).max(x_start)
                    >= source.x_stop.min(other.x_stop).min(x_stop)
            {
                continue;
            }
            mask |= sprite_sprite_clx_bit(source.source.group, other.source.group);
        }
    }
    mask & CLXDAT_SPRITE_SPRITE_MASK
}

fn sprite_playfield_clx_mask_for_group(group: usize) -> u16 {
    (1 << (group + 1)) | (1 << (group + 5))
}

fn live_sprite_playfield_possible_clx_mask(
    sources: &[LiveSpriteCollisionSource],
    x_start: i32,
    x_stop: i32,
) -> u16 {
    let mut mask = 0;
    for source in sources {
        if live_sprite_source_may_overlap_x_range(source, x_start, x_stop) {
            mask |= sprite_playfield_clx_mask_for_group(source.group);
        }
    }
    mask & CLXDAT_SPRITE_PLAYFIELD_MASK
}

fn live_manual_sprite_playfield_possible_clx_mask(
    sources: &[LiveManualSpriteCollisionSource],
    x_start: i32,
    x_stop: i32,
) -> u16 {
    let mut mask = 0;
    for source in sources {
        if source.x_start < x_stop && source.x_stop > x_start {
            mask |= sprite_playfield_clx_mask_for_group(source.source.group);
        }
    }
    mask & CLXDAT_SPRITE_PLAYFIELD_MASK
}

fn live_sprite_sources_have_group_pair_overlap(
    sources: &[LiveSpriteCollisionSource],
    x_start: i32,
    x_stop: i32,
) -> bool {
    let x_start = x_start.max(0);
    let x_stop = x_stop.min(RENDER_FRAMEBUFFER_WIDTH);
    if x_start >= x_stop {
        return false;
    }

    for (idx, source) in sources.iter().enumerate() {
        for other in &sources[idx + 1..] {
            if source.group == other.group {
                continue;
            }
            if live_sprite_source_pair_x_range(source, other, x_start, x_stop).is_some() {
                return true;
            }
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn push_live_manual_sprite_source(
    sources: &mut Vec<LiveManualSpriteCollisionSource>,
    sprite: usize,
    sprpos: u16,
    sprctl: u16,
    sprdata: u16,
    sprdatb: u16,
    spr_armed: bool,
    shres: bool,
    beam_y: i32,
    x_start: i32,
    x_stop: i32,
) {
    if x_start >= x_stop || !spr_armed {
        return;
    }
    let vstart = sprite_vstart_from_words(sprpos, sprctl);
    let vstop = sprite_vstop_from_ctl(sprctl);
    if beam_y < vstart || beam_y >= vstop {
        return;
    }
    sources.push(LiveManualSpriteCollisionSource {
        sprite,
        source: LiveSpriteCollisionSource {
            group: sprite / 2,
            hstart: sprite_hstart_from_words(sprpos, sprctl),
            hsub_70ns: shres && sprite_hsub_70ns_from_ctl(sprctl),
            words: [sprdata, sprdatb, 0, 0],
            requires_odd_enable: sprite & 1 != 0,
        },
        x_start,
        x_stop,
    });
}

#[derive(Clone, Copy, Default)]
struct LiveSpritePixelPresence {
    even: bool,
    odd: bool,
}

fn live_sprite_source_pixel_presence(
    source: &LiveSpriteCollisionSource,
    control_replay: &LiveCollisionLineReplay,
    x: i32,
) -> LiveSpritePixelPresence {
    if let Some(control) = control_replay.constant_control() {
        return live_sprite_source_pixel_presence_with_control(source, control, x);
    }

    let (sprite_base_x, sprite_stop_x) = live_sprite_source_framebuffer_bounds(source);
    if x < sprite_base_x || x >= sprite_stop_x {
        return LiveSpritePixelPresence::default();
    }
    let mut x_cursor = sprite_base_x;
    for bit in (0..16).rev() {
        let sprite_control = control_replay.control_for_x(x_cursor);
        let sprite_pixel_repeat =
            sprite_pixel_repeat_for_control(sprite_control.bplcon0, sprite_control.bplcon3);
        let x_stop = x_cursor + sprite_pixel_repeat;
        if x >= x_cursor && x < x_stop {
            let low = source.words[0] & (1 << bit) != 0 || source.words[1] & (1 << bit) != 0;
            let high = source.words[2] & (1 << bit) != 0 || source.words[3] & (1 << bit) != 0;
            return if source.requires_odd_enable {
                LiveSpritePixelPresence {
                    even: false,
                    odd: low,
                }
            } else {
                LiveSpritePixelPresence {
                    even: low,
                    odd: high,
                }
            };
        }
        x_cursor = x_stop;
    }
    LiveSpritePixelPresence::default()
}

fn live_sprite_source_pixel_presence_with_control(
    source: &LiveSpriteCollisionSource,
    control: LiveCollisionControl,
    x: i32,
) -> LiveSpritePixelPresence {
    let sprite_base_x = (source.hstart - RENDER_DIW_HSTART_FB0) * 2 + i32::from(source.hsub_70ns);
    let sprite_pixel_repeat = sprite_pixel_repeat_for_control(control.bplcon0, control.bplcon3);
    let offset = x - sprite_base_x;
    if offset < 0 {
        return LiveSpritePixelPresence::default();
    }
    let bit_offset = offset / sprite_pixel_repeat;
    if !(0..16).contains(&bit_offset) {
        return LiveSpritePixelPresence::default();
    }
    let bit = 15 - bit_offset;
    let mask = 1 << bit;
    let low = source.words[0] & mask != 0 || source.words[1] & mask != 0;
    let high = source.words[2] & mask != 0 || source.words[3] & mask != 0;
    if source.requires_odd_enable {
        LiveSpritePixelPresence {
            even: false,
            odd: low,
        }
    } else {
        LiveSpritePixelPresence {
            even: low,
            odd: high,
        }
    }
}

fn live_sprite_source_collision_matches(
    source: &LiveSpriteCollisionSource,
    control_replay: &LiveCollisionLineReplay,
    clxcon: u16,
    x: i32,
) -> bool {
    let presence = live_sprite_source_pixel_presence(source, control_replay, x);
    presence.even || (presence.odd && clxcon & (1 << (12 + source.group)) != 0)
}

fn live_sprite_source_collision_matches_with_control(
    source: &LiveSpriteCollisionSource,
    control: LiveCollisionControl,
    clxcon: u16,
    x: i32,
) -> bool {
    let presence = live_sprite_source_pixel_presence_with_control(source, control, x);
    presence.even || (presence.odd && clxcon & (1 << (12 + source.group)) != 0)
}

fn live_sprite_visible_x_range_for_control(
    control: LiveCollisionControl,
    beam_y: i32,
    x_start: i32,
    x_stop: i32,
    display_enable_x: Option<i32>,
) -> Option<(i32, i32)> {
    let x_start = x_start.max(0);
    let x_stop = x_stop.min(RENDER_FRAMEBUFFER_WIDTH);
    if x_start >= x_stop {
        return None;
    }
    if live_border_sprite_enabled(control) {
        return Some((x_start, x_stop));
    }
    let display_enable_x = display_enable_x?;
    if beam_y < 0 {
        return None;
    }
    // Bitplane DMA records DIW's left edge here; a manual BPL1DAT write records
    // its beam position. Either path can make OCS/ECS sprites active vertically,
    // while DIW still clips the horizontal span.
    let (window_x_start, window_x_stop) =
        live_display_window_x(control.diwstrt, control.diwstop, control.diwhigh);
    let x_start = x_start.max(display_enable_x).max(window_x_start);
    let x_stop = if window_x_stop <= window_x_start {
        x_stop
    } else {
        x_stop.min(window_x_stop)
    };
    (x_start < x_stop).then_some((x_start, x_stop))
}

fn live_sprite_pixel_inside_display_window(
    control: LiveCollisionControl,
    beam_y: i32,
    framebuffer_x: i32,
    display_enable_x: Option<i32>,
) -> bool {
    if live_border_sprite_enabled(control) {
        return true;
    }
    if beam_y < 0 {
        return false;
    }
    if display_enable_x.is_none_or(|enable_x| framebuffer_x < enable_x) {
        return false;
    }
    // See `live_sprite_visible_x_range_for_control`: display_enable_x carries
    // the per-line manual-BPL1DAT/DMA gate, so do not apply a separate DIW
    // vertical test here.
    live_display_window_contains_x(
        control.diwstrt,
        control.diwstop,
        control.diwhigh,
        framebuffer_x,
    )
}

fn live_border_sprite_enabled(control: LiveCollisionControl) -> bool {
    control.bplcon0 & BPLCON0_ECSENA != 0
        && control.bplcon3 & BPLCON3_BRDSPRT != 0
        && control.bplcon3 & BPLCON3_BRDRBLNK == 0
}

fn sprite_sprite_clx_bit(a: usize, b: usize) -> u16 {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    match (lo, hi) {
        (0, 1) => 1 << 9,
        (0, 2) => 1 << 10,
        (0, 3) => 1 << 11,
        (1, 2) => 1 << 12,
        (1, 3) => 1 << 13,
        (2, 3) => 1 << 14,
        _ => 0,
    }
}

#[cfg(test)]
fn live_bitplane_collision_bits(
    row: &CapturedBitplaneRow,
    control_replay: &LiveCollisionLineReplay,
    beam_y: i32,
) -> u16 {
    live_bitplane_collision_bits_in_range(row, control_replay, beam_y, 0, RENDER_FRAMEBUFFER_WIDTH)
}

fn live_bitplane_collision_bits_in_range(
    row: &CapturedBitplaneRow,
    control_replay: &LiveCollisionLineReplay,
    _beam_y: i32,
    x_start: i32,
    x_stop: i32,
) -> u16 {
    if row.nplanes < 2 {
        return 0;
    }

    let x_start = x_start.max(0);
    let x_stop = x_stop.min(RENDER_FRAMEBUFFER_WIDTH);
    if x_start >= x_stop {
        return 0;
    }

    for x in x_start..x_stop {
        let control = control_replay.control_for_x(x);
        if control.bplcon0 & 0x0400 == 0 {
            continue;
        }
        let Some(collision) = live_bitplane_collision_pixel_at(
            row,
            control.agnus_revision,
            control.bplcon0,
            control.bplcon1,
            control.clxcon,
            control.diwstrt,
            control.diwstop,
            control.diwhigh,
            control.ddfstrt,
            control.bpldat,
            x,
        ) else {
            continue;
        };
        if collision.pf1 && collision.pf2 {
            return 1;
        }
    }
    0
}

#[derive(Clone, Copy, Default)]
struct LivePlayfieldCollisionPixel {
    pf1: bool,
    pf2: bool,
    pf1_match: bool,
    pf2_match: bool,
}

#[derive(Clone, Copy)]
struct LiveCollisionControl {
    agnus_revision: AgnusRevision,
    bplcon0: u16,
    bplcon1: u16,
    bplcon3: u16,
    clxcon: u16,
    diwstrt: u16,
    diwstop: u16,
    diwhigh: DiwHigh,
    ddfstrt: u16,
    bpldat: [u16; 8],
}

impl LiveCollisionControl {
    fn from_current(
        agnus_revision: AgnusRevision,
        bplcon0: u16,
        bplcon1: u16,
        bplcon3: u16,
        clxcon: u16,
        diwstrt: u16,
        diwstop: u16,
        diwhigh: DiwHigh,
        ddfstrt: u16,
        bpldat: [u16; 8],
    ) -> Self {
        Self {
            agnus_revision,
            bplcon0,
            bplcon1,
            bplcon3,
            clxcon,
            diwstrt,
            diwstop,
            diwhigh,
            ddfstrt,
            bpldat,
        }
    }

    fn from_snapshot(snapshot: RenderRegisterSnapshot) -> Self {
        Self {
            agnus_revision: snapshot.agnus_revision,
            bplcon0: snapshot.bplcon0,
            bplcon1: snapshot.bplcon1,
            bplcon3: snapshot.bplcon3,
            clxcon: snapshot.clxcon,
            diwstrt: snapshot.diwstrt,
            diwstop: snapshot.diwstop,
            diwhigh: snapshot.diwhigh,
            ddfstrt: snapshot.ddfstrt,
            bpldat: snapshot.bpldat,
        }
    }

    fn apply_write(&mut self, offset: u16, value: u16) {
        match offset & 0x01FE {
            0x08E => self.diwstrt = value,
            0x090 => self.diwstop = value,
            0x092 => self.ddfstrt = value,
            0x098 => self.clxcon = value,
            0x100 => self.bplcon0 = value,
            0x102 => self.bplcon1 = value,
            0x106 => self.bplcon3 = value,
            0x1E4 => self.diwhigh = DiwHigh::ecs_explicit(value),
            off @ 0x110..=0x11A => {
                let plane = ((off - 0x110) / 2) as usize;
                if plane < self.bpldat.len() {
                    self.bpldat[plane] = value;
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
struct LiveCollisionControlSegment {
    x: i32,
    control: LiveCollisionControl,
}

struct LiveCollisionLineReplay {
    line_start: LiveCollisionControl,
    segments: Vec<LiveCollisionControlSegment>,
}

impl LiveCollisionLineReplay {
    fn from_index(
        current_control: LiveCollisionControl,
        frame_base: RenderRegisterSnapshot,
        index: &BeamEventIndex,
        beam_y: i32,
    ) -> Self {
        let Some(line) = beam_y
            .checked_sub(RENDER_VISIBLE_START_VPOS as i32)
            .map(|line| line as usize)
            .filter(|&line| line < RENDER_VISIBLE_LINES)
        else {
            return Self {
                line_start: current_control,
                segments: Vec::new(),
            };
        };

        let line_events = index
            .line(line)
            .map(|events| events.video_control_writes())
            .unwrap_or(&[]);
        let has_control_events = !line_events.is_empty()
            || index.video_control_writes_before_visible_line(line).count() != 0;
        if !has_control_events {
            return Self {
                line_start: current_control,
                segments: Vec::new(),
            };
        }

        let mut line_start = LiveCollisionControl::from_snapshot(frame_base);
        for event in index.video_control_writes_before_visible_line(line) {
            line_start.apply_write(event.offset, event.value);
        }
        let mut control = line_start;
        let mut segments: Vec<LiveCollisionControlSegment> = Vec::with_capacity(line_events.len());
        for event in line_events {
            control.apply_write(event.offset, event.value);
            let x = framebuffer_x_for_live_collision_hpos(event.hpos);
            if let Some(last) = segments.last_mut() {
                if last.x == x {
                    last.control = control;
                    continue;
                }
            }
            segments.push(LiveCollisionControlSegment { x, control });
        }
        Self {
            line_start,
            segments,
        }
    }

    fn control_for_x(&self, framebuffer_x: i32) -> LiveCollisionControl {
        if self.segments.is_empty() {
            return self.line_start;
        }
        let framebuffer_x = framebuffer_x.max(0);
        match self
            .segments
            .binary_search_by(|segment| segment.x.cmp(&framebuffer_x))
        {
            Ok(idx) => self.segments[idx].control,
            Err(0) => self.line_start,
            Err(idx) => self.segments[idx - 1].control,
        }
    }

    fn constant_control(&self) -> Option<LiveCollisionControl> {
        self.segments.is_empty().then_some(self.line_start)
    }

    fn segment_count(&self) -> usize {
        self.segments.len()
    }

    fn dual_playfield_in_range(&self, x_start: i32, x_stop: i32) -> bool {
        let x_start = x_start.max(0);
        let x_stop = x_stop.min(RENDER_FRAMEBUFFER_WIDTH);
        if x_start >= x_stop {
            return false;
        }
        if self.control_for_x(x_start).bplcon0 & 0x0400 != 0 {
            return true;
        }
        self.segments.iter().any(|segment| {
            segment.x >= x_start && segment.x < x_stop && segment.control.bplcon0 & 0x0400 != 0
        })
    }
}

fn framebuffer_x_for_live_collision_hpos(hpos: u32) -> i32 {
    hpos.saturating_sub(RENDER_COPPER_WAIT_HPOS_FB0)
        .saturating_mul(4)
        .min(RENDER_FRAMEBUFFER_WIDTH as u32) as i32
}

fn live_sprite_playfield_collision_bits_in_range(
    row: &CapturedBitplaneRow,
    sources: &[LiveSpriteCollisionSource],
    playfield_control: &LiveCollisionLineReplay,
    sprite_control: &LiveCollisionLineReplay,
    beam_y: i32,
    x_start: i32,
    x_stop: i32,
    display_enable_x: Option<i32>,
    latched_clxdat: u16,
) -> u16 {
    if sources.is_empty() {
        return 0;
    }
    let x_start = x_start.max(0);
    let x_stop = x_stop.min(RENDER_FRAMEBUFFER_WIDTH);
    if x_start >= x_stop {
        return 0;
    }
    let target_mask = live_sprite_playfield_possible_clx_mask(sources, x_start, x_stop);
    let needed_mask = target_mask & !latched_clxdat;
    if needed_mask == 0 {
        return 0;
    }

    let mut clxdat = 0u16;
    let sprite_constant_control = sprite_control.constant_control();
    for source in sources {
        let source_mask = sprite_playfield_clx_mask_for_group(source.group) & needed_mask;
        if source_mask == 0 || clxdat & source_mask == source_mask {
            continue;
        }
        let (source_start, source_stop) = live_sprite_source_framebuffer_bounds(source);
        let source_x_start = x_start.max(source_start);
        let source_x_stop = x_stop.min(source_stop);
        if source_x_start >= source_x_stop {
            continue;
        }
        let (source_x_start, source_x_stop) = if let Some(control) = sprite_constant_control {
            let Some(range) = live_sprite_visible_x_range_for_control(
                control,
                beam_y,
                source_x_start,
                source_x_stop,
                display_enable_x,
            ) else {
                continue;
            };
            range
        } else {
            (source_x_start, source_x_stop)
        };
        for x in source_x_start..source_x_stop {
            let control = playfield_control.control_for_x(x);
            let sprite_matches = if let Some(sprite_control_at_x) = sprite_constant_control {
                live_sprite_source_collision_matches_with_control(
                    source,
                    sprite_control_at_x,
                    control.clxcon,
                    x,
                )
            } else {
                let sprite_control_at_x = sprite_control.control_for_x(x);
                live_sprite_pixel_inside_display_window(
                    sprite_control_at_x,
                    beam_y,
                    x,
                    display_enable_x,
                ) && live_sprite_source_collision_matches(source, sprite_control, control.clxcon, x)
            };
            if !sprite_matches {
                continue;
            }
            let Some(collision) = live_bitplane_collision_pixel_at(
                row,
                control.agnus_revision,
                control.bplcon0,
                control.bplcon1,
                control.clxcon,
                control.diwstrt,
                control.diwstop,
                control.diwhigh,
                control.ddfstrt,
                control.bpldat,
                x,
            ) else {
                continue;
            };
            if collision.pf1_match {
                let clx_bit = 1 << (source.group + 1);
                if needed_mask & clx_bit != 0 {
                    clxdat |= clx_bit;
                }
            }
            if collision.pf2_match {
                let clx_bit = 1 << (source.group + 5);
                if needed_mask & clx_bit != 0 {
                    clxdat |= clx_bit;
                }
            }
            if clxdat & source_mask == source_mask {
                break;
            }
            if clxdat & needed_mask == needed_mask {
                return clxdat;
            }
        }
    }

    clxdat
}

fn live_manual_bpl_collision_bits_in_range(
    frame_base: RenderRegisterSnapshot,
    bpldat_index: &BeamEventIndex,
    sprite_index: &BeamEventIndex,
    control_replay: &LiveCollisionLineReplay,
    sprite_lines: &[CapturedSpriteLine],
    beam_y: i32,
    x_start: i32,
    x_stop: i32,
    display_enable_x: Option<i32>,
) -> u16 {
    const MANUAL_BPL_WORD_BITS: usize = 16;
    const MAX_BPLCON1_DELAY: usize = 15;
    const MAX_MANUAL_BPL_NATIVE_SAMPLES: usize = MANUAL_BPL_WORD_BITS + MAX_BPLCON1_DELAY;

    if beam_y < 0 {
        return 0;
    }
    let x_start = x_start.max(0);
    let x_stop = x_stop.min(RENDER_FRAMEBUFFER_WIDTH);
    if x_start >= x_stop {
        return 0;
    }
    let Some(line) = beam_y
        .checked_sub(RENDER_VISIBLE_START_VPOS as i32)
        .map(|line| line as usize)
        .filter(|&line| line < RENDER_VISIBLE_LINES)
    else {
        return 0;
    };

    let mut bpldat = frame_base.bpldat;
    for event in bpldat_index.bitplane_data_writes_before_visible_line(line) {
        apply_live_bpldat_event(&mut bpldat, event.offset, event.value);
    }

    let mut clxdat = 0u16;
    let line_events = bpldat_index
        .line(line)
        .map(|line| line.bitplane_data_writes())
        .unwrap_or(&[]);
    for event in line_events {
        let off = event.offset & 0x01FE;
        apply_live_bpldat_event(&mut bpldat, event.offset, event.value);
        if off == 0x110 {
            let segment_x = (event.hpos as i32 - RENDER_COPPER_WAIT_HPOS_FB0 as i32) * 4;
            clxdat |= live_manual_bpl_word_collision_bits(
                bpldat,
                frame_base,
                sprite_index,
                control_replay,
                sprite_lines,
                beam_y,
                segment_x,
                x_start,
                x_stop,
                MAX_MANUAL_BPL_NATIVE_SAMPLES,
                display_enable_x,
            );
        }
    }
    clxdat
}

fn apply_live_bpldat_event(bpldat: &mut [u16; 8], offset: u16, value: u16) {
    let off = offset & 0x01FE;
    if !matches!(off, 0x110..=0x11A) {
        return;
    }
    let plane = ((off - 0x110) / 2) as usize;
    if plane < bpldat.len() {
        bpldat[plane] = value;
    }
}

/// Live collisions evaluate at most the classic 6 bitplanes until the AGA
/// CLXCON2 collision extensions are interpreted (plan 3.4).
const COLLISIONS_AGA_DECODE: bool = false;

fn live_manual_bpl_word_collision_bits(
    planes: [u16; 8],
    frame_base: RenderRegisterSnapshot,
    sprite_index: &BeamEventIndex,
    control_replay: &LiveCollisionLineReplay,
    sprite_lines: &[CapturedSpriteLine],
    beam_y: i32,
    segment_x: i32,
    x_start: i32,
    x_stop: i32,
    max_native_samples: usize,
    display_enable_x: Option<i32>,
) -> u16 {
    const MANUAL_BPL_WORD_BITS: usize = 16;

    let sources = live_sprite_playfield_collision_sources(sprite_lines, beam_y);
    let manual_sources =
        live_manual_sprite_collision_sources(frame_base, sprite_index, beam_y, x_start, x_stop);
    let mut clxdat = 0u16;
    let mut x_cursor = segment_x;
    let mut native_idx = 0usize;
    while native_idx < max_native_samples {
        let source_control = control_replay.control_for_x(x_cursor);
        let shres = bitplane_shres(source_control.bplcon0);
        let hires = bitplane_hires(source_control.bplcon0);
        let pixel_repeat = if hires || shres { 1 } else { 2 };
        let native_step = if shres { 2 } else { 1 };
        // Collision sampling stays on the pre-AGA 6-plane decode until
        // CLXCON2 / 8-plane collisions land (plan 3.4).
        let mode = BitplaneMode::from_bplcon0(source_control.bplcon0, COLLISIONS_AGA_DECODE);
        let nplanes = mode.display_planes().min(planes.len());
        let dual_playfield = source_control.bplcon0 & 0x0400 != 0;
        let mut idx = 0u8;
        let mut word_active = false;
        for plane in 0..nplanes {
            let delay = live_scroll_for_plane(source_control.bplcon1, plane);
            if native_idx < delay {
                word_active = true;
                continue;
            }
            let source_bit = native_idx - delay;
            if source_bit >= MANUAL_BPL_WORD_BITS {
                continue;
            }
            word_active = true;
            let bit = 15 - source_bit;
            if planes[plane] & (1 << bit) != 0 {
                idx |= 1 << plane;
            }
        }
        if shres {
            let right_native_idx = native_idx + 1;
            for plane in 0..nplanes {
                let delay = live_scroll_for_plane(source_control.bplcon1, plane);
                if right_native_idx < delay {
                    word_active = true;
                    continue;
                }
                let source_bit = right_native_idx - delay;
                if source_bit >= MANUAL_BPL_WORD_BITS {
                    continue;
                }
                word_active = true;
                let bit = 15 - source_bit;
                if planes[plane] & (1 << bit) != 0 {
                    idx |= 1 << plane;
                }
            }
        }
        if word_active {
            let collision =
                live_playfield_collision_pixel(idx, source_control.clxcon, dual_playfield);
            for dx in 0..pixel_repeat {
                let x = x_cursor + dx;
                if x < x_start || x >= x_stop {
                    continue;
                }
                let pixel_control = control_replay.control_for_x(x);
                if !display_window_contains_vpos(
                    pixel_control.diwstrt,
                    pixel_control.diwstop,
                    pixel_control.diwhigh,
                    beam_y as u32,
                ) || !live_display_window_contains_x(
                    pixel_control.diwstrt,
                    pixel_control.diwstop,
                    pixel_control.diwhigh,
                    x,
                ) {
                    continue;
                }
                if collision.pf1_match && collision.pf2_match {
                    clxdat |= 1;
                }
                let sprite_visible = live_sprite_pixel_inside_display_window(
                    pixel_control,
                    beam_y,
                    x,
                    display_enable_x,
                );
                if !sprite_visible {
                    continue;
                }
                for source in &sources {
                    if !live_sprite_source_collision_matches(
                        source,
                        control_replay,
                        pixel_control.clxcon,
                        x,
                    ) {
                        continue;
                    }
                    if collision.pf1_match {
                        clxdat |= 1 << (source.group + 1);
                    }
                    if collision.pf2_match {
                        clxdat |= 1 << (source.group + 5);
                    }
                }
                for source in &manual_sources {
                    if x < source.x_start || x >= source.x_stop {
                        continue;
                    }
                    if !live_sprite_source_collision_matches(
                        &source.source,
                        control_replay,
                        pixel_control.clxcon,
                        x,
                    ) {
                        continue;
                    }
                    if collision.pf1_match {
                        clxdat |= 1 << (source.source.group + 1);
                    }
                    if collision.pf2_match {
                        clxdat |= 1 << (source.source.group + 5);
                    }
                }
            }
        }
        x_cursor += pixel_repeat;
        native_idx += native_step;
    }
    clxdat
}

fn live_bitplane_collision_pixel_at(
    row: &CapturedBitplaneRow,
    agnus_revision: AgnusRevision,
    bplcon0: u16,
    bplcon1: u16,
    clxcon: u16,
    diwstrt: u16,
    diwstop: u16,
    diwhigh: DiwHigh,
    ddfstrt: u16,
    bpldat: [u16; 8],
    framebuffer_x: i32,
) -> Option<LivePlayfieldCollisionPixel> {
    if framebuffer_x < 0 {
        return None;
    }
    let (window_x_start, window_x_stop) = live_display_window_x(diwstrt, diwstop, diwhigh);
    if framebuffer_x < window_x_start
        || (window_x_stop > window_x_start && framebuffer_x >= window_x_stop)
    {
        return None;
    }
    let shres = bitplane_shres(bplcon0);
    let hires = bitplane_hires(bplcon0);
    let pixel_repeat = if hires || shres { 1 } else { 2 };
    let native_samples_per_pixel = if shres { 2 } else { 1 };
    let output_native_x =
        ((framebuffer_x - window_x_start) as usize / pixel_repeat) * native_samples_per_pixel;
    let fetch_start_native_x =
        live_fetch_start_native_x(agnus_revision, bplcon0, diwstrt, diwhigh, ddfstrt);
    let relative_native_x = output_native_x.checked_sub(fetch_start_native_x)?;
    let native_x = relative_native_x
        + live_fetch_origin_native_offset(agnus_revision, bplcon0, diwstrt, diwhigh, ddfstrt);
    let fetched_pixels = row.words_per_row * 16;
    // Collision sampling stays on the pre-AGA 6-plane decode until CLXCON2 /
    // 8-plane collisions land (plan 3.4).
    let mode = BitplaneMode::from_bplcon0(bplcon0, COLLISIONS_AGA_DECODE);
    let nplanes = mode.display_planes().min(row.nplanes).min(6);
    let dma_planes = mode.dma_planes().min(nplanes);
    let mut idx = 0u8;
    for plane in 0..nplanes {
        let delay = live_scroll_for_plane(bplcon1, plane);
        if native_x < delay {
            continue;
        }
        let fetch_x = native_x - delay;
        if fetch_x >= fetched_pixels {
            continue;
        }
        let word = if plane < dma_planes {
            row.planes[plane][fetch_x / 16]
        } else {
            bpldat[plane]
        };
        let bit = 15 - (fetch_x & 0x0F);
        if word & (1 << bit) != 0 {
            idx |= 1 << plane;
        }
    }
    if shres {
        let mut right_idx = 0u8;
        let right_native_x = native_x + 1;
        for plane in 0..nplanes {
            let delay = live_scroll_for_plane(bplcon1, plane);
            if right_native_x < delay {
                continue;
            }
            let fetch_x = right_native_x - delay;
            if fetch_x >= fetched_pixels {
                continue;
            }
            let word = if plane < dma_planes {
                row.planes[plane][fetch_x / 16]
            } else {
                bpldat[plane]
            };
            let bit = 15 - (fetch_x & 0x0F);
            if word & (1 << bit) != 0 {
                right_idx |= 1 << plane;
            }
        }
        idx |= right_idx;
    }
    Some(live_playfield_collision_pixel(
        idx,
        clxcon,
        bplcon0 & 0x0400 != 0,
    ))
}

fn live_playfield_collision_pixel(
    idx: u8,
    clxcon: u16,
    dual_playfield: bool,
) -> LivePlayfieldCollisionPixel {
    let even_match = live_clxcon_planes_match(idx, clxcon, 1);
    let odd_match_raw = live_clxcon_planes_match(idx, clxcon, 0);
    let odd_match = odd_match_raw && (dual_playfield || even_match);
    LivePlayfieldCollisionPixel {
        pf1: dual_playfield && idx & 0b010101 != 0,
        pf2: if dual_playfield {
            idx & 0b101010 != 0
        } else {
            idx != 0
        },
        pf1_match: odd_match,
        pf2_match: even_match,
    }
}

fn live_clxcon_planes_match(idx: u8, clxcon: u16, first_plane: usize) -> bool {
    let mut matches = true;
    // Every CLXCON-enabled plane participates in the match, not just the planes
    // the display currently fetches: a plane enabled beyond the BPU count reads
    // as 0 and still gates the collision (vAmiga checkS2PCollisions compares
    // `(dBuffer & enbp) == (mvbp & enbp)` over all six planes). Regression:
    // Denise/Sprites/collision/sprcoll* set CLXCON match bits for absent planes
    // over a low-plane-count playfield.
    for plane in (first_plane..6).step_by(2) {
        if clxcon & (1 << (6 + plane)) == 0 {
            continue;
        }
        let desired = clxcon & (1 << plane) != 0;
        let actual = idx & (1 << plane) != 0;
        matches &= desired == actual;
    }
    matches
}

fn live_scroll_for_plane(bplcon1: u16, plane: usize) -> usize {
    if plane & 1 != 0 {
        ((bplcon1 >> 4) & 0x000F) as usize
    } else {
        (bplcon1 & 0x000F) as usize
    }
}

fn live_fetch_start_native_x(
    agnus_revision: AgnusRevision,
    bplcon0: u16,
    diwstrt: u16,
    diwhigh: DiwHigh,
    ddfstrt: u16,
) -> usize {
    (-live_fetch_origin_native_shift(agnus_revision, bplcon0, diwstrt, diwhigh, ddfstrt)).max(0)
        as usize
}

fn live_fetch_origin_native_offset(
    agnus_revision: AgnusRevision,
    bplcon0: u16,
    diwstrt: u16,
    diwhigh: DiwHigh,
    ddfstrt: u16,
) -> usize {
    live_fetch_origin_native_shift(agnus_revision, bplcon0, diwstrt, diwhigh, ddfstrt).max(0)
        as usize
}

fn live_fetch_origin_native_shift(
    agnus_revision: AgnusRevision,
    bplcon0: u16,
    diwstrt: u16,
    diwhigh: DiwHigh,
    ddfstrt: u16,
) -> i32 {
    let shres = bitplane_shres(bplcon0);
    let hires = bitplane_hires(bplcon0);
    let pixel_repeat = if hires || shres { 1 } else { 2 };
    let native_samples_per_pixel = if shres { 2 } else { 1 };
    let fetch_reference = if hires {
        RENDER_DIW_HSTART_FETCH_REFERENCE_HIRES
    } else {
        RENDER_DIW_HSTART_FETCH_REFERENCE_LORES
    };
    let display_native_shift =
        ((diw_h_start(diwstrt, diwhigh) as i32 - fetch_reference) * 2) / pixel_repeat;
    let display_native_shift = display_native_shift * native_samples_per_pixel;
    let standard_ddf = if hires || shres { 0x003C } else { 0x0038 };
    let ddf_native_scale = if shres {
        8
    } else if hires {
        4
    } else {
        2
    };
    let ddf_native_shift = (effective_ddf_start_hpos(agnus_revision, bplcon0, ddfstrt) as i32
        - standard_ddf)
        * ddf_native_scale;
    display_native_shift - ddf_native_shift
}

fn live_display_window_contains_x(
    diwstrt: u16,
    diwstop: u16,
    diwhigh: DiwHigh,
    framebuffer_x: i32,
) -> bool {
    let (left, right) = live_display_window_x(diwstrt, diwstop, diwhigh);
    framebuffer_x >= left && (right <= left || framebuffer_x < right)
}

fn live_display_window_x(diwstrt: u16, diwstop: u16, diwhigh: DiwHigh) -> (i32, i32) {
    if display_window_unprogrammed(diwstrt, diwstop) {
        return (0, RENDER_FRAMEBUFFER_WIDTH);
    }
    let start = diw_h_start(diwstrt, diwhigh) as i32;
    let mut stop = diw_h_stop(diwstop, diwhigh) as i32;
    if stop <= start {
        stop += 0x100;
    }
    let left = ((start - RENDER_DIW_HSTART_FB0).max(0) * 2).min(RENDER_FRAMEBUFFER_WIDTH);
    let mut right = ((stop - RENDER_DIW_HSTART_FB0).max(0) * 2).min(RENDER_FRAMEBUFFER_WIDTH);
    if RENDER_FRAMEBUFFER_WIDTH.saturating_sub(right) <= 2 {
        right = RENDER_FRAMEBUFFER_WIDTH;
    }
    (left, right)
}

fn diw_h_start(diwstrt: u16, diwhigh: DiwHigh) -> u16 {
    diwhigh.h_start(diwstrt)
}

fn diw_h_stop(diwstop: u16, diwhigh: DiwHigh) -> u16 {
    diwhigh.h_stop(diwstop)
}

fn is_render_relevant_custom_write(off: u16) -> bool {
    matches!(
        off,
        0x08E
            | 0x090
            | 0x092
            | 0x094
            | 0x096
            | 0x098
            | 0x100
            | 0x102
            | 0x104
            | 0x106
            | 0x108
            | 0x10A
            | 0x10C
            | 0x10E
            | 0x110..=0x11E
            | 0x0E0..=0x0FF
            | 0x120..=0x13F
            | 0x140..=0x17F
            | 0x1E4
            | 0x1FC
            | 0x180..=0x1BE
    )
}

fn is_live_collision_relevant_custom_write(off: u16) -> bool {
    matches!(
        off & 0x01FE,
        0x08E | 0x090 | 0x092 | 0x098 | 0x100 | 0x102 | 0x106 | 0x110..=0x11A
            | 0x140..=0x17F
            | 0x1E4
    )
}

fn is_live_collision_control_custom_write(off: u16) -> bool {
    matches!(
        off & 0x01FE,
        0x08E | 0x090 | 0x092 | 0x098 | 0x100 | 0x102 | 0x106 | 0x110..=0x11A | 0x1E4
    )
}

fn is_live_collision_bpldat_custom_write(off: u16) -> bool {
    matches!(off & 0x01FE, 0x110..=0x11A)
}

fn is_live_collision_sprite_custom_write(off: u16) -> bool {
    matches!(off & 0x01FE, 0x140..=0x17F)
}

fn is_audio_timing_custom_write(off: u16) -> bool {
    // DMACON, INTREQ (audio-interrupt acks gate the state machine's
    // AUDxIP tests), ADKCON, the audio registers, and the beam-rate
    // registers the mixer clock derives from.
    matches!(off, 0x096 | 0x09C | 0x09E | 0x0A0..=0x0DF | 0x1C0 | 0x1DC)
}

fn palette_event_sequences_equivalent(a: &[BeamRegisterWrite], b: &[BeamRegisterWrite]) -> bool {
    !a.is_empty()
        && a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            (a.offset & 0x01FE) == (b.offset & 0x01FE)
                && color_register_value(a.value) == color_register_value(b.value)
                && matches!(a.offset & 0x01FE, 0x180..=0x1BE)
        })
}

mod collisions;
mod custom_regs;
mod ddf_line;
mod dma_slots;
mod frame_capture;

#[cfg(test)]
mod tests;
