// SPDX-License-Identifier: GPL-3.0-or-later

//! Decode the Amiga's planar bitmap into a packed RGBA8 framebuffer.
//!
//! The Copper is executed by the bus as beam time advances. Rendering
//! replays the recorded custom-register writes for the most recently
//! completed frame so scanline and horizontal palette/control changes
//! are based on scheduled execution rather than reparsing COP1LC.

use super::FrameGeometry;
#[cfg(test)]
use super::FB_PIXELS;
use super::{FB_HEIGHT, FB_WIDTH, MAX_VISIBLE_LINES};
use crate::bus::{
    BeamChipRamWrite, BeamRegisterWrite, BeamWriteSource, Bus, CapturedBitplaneRow,
    CapturedSpriteLine, HeldSpriteLine, RenderRegisterSnapshot, VideoRenderFrameTiming,
};
use crate::chipset::agnus::{
    ddf_hard_bounds, sprite_dma_disabled_by_bitplane_ddf, AgnusRevision, COLORCLOCKS_PER_LINE,
};
#[cfg(test)]
use crate::chipset::denise::BPLCON3_PF2OF_DEFAULT;
use crate::chipset::denise::{
    color_register_value, rgb12_to_rgb24, rgb12_to_rgba8, rgb24_to_rgba8, BitplaneMode, DiwHigh,
    Palette, COLOR_RGB_MASK, COLOR_TRANSPARENCY_BIT,
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

// Beam-to-framebuffer conversion anchors for the pragmatic renderer.
// They are derived from the OCS PAL display window/fetch positions
// used by DiagROM and the boot ROM screen, not from a cycle-exact
// Denise/Agnus scheduler.
#[cfg_attr(not(test), allow(dead_code))]
const PAL_VISIBLE_LINE0: i32 = 0x2C;
// Framebuffer x=0 anchor. Held deep left of the standard display start so the
// framebuffer captures the deep-left overscan a real Denise can display,
// matching vAmiga's 716-wide regression cutout. The value pins the DIW
// comparator mapping: a DIWSTRT hstart H opens the window at framebuffer
// x = (H - this) * 2, and real hardware puts the standard $81 edge at x = 62
// (2H - 196; measured on the sblit0_A500_ECS.jpeg partial swatch columns,
// which show the bitmap's first lo-res pixel fully visible at the edge).
const DIW_HSTART_FB0: i32 = 0x62;
const STANDARD_DIW_HSTART: i32 = 0x81;
// Standard DIWSTRT $81 is the visible window edge. The first standard
// bitplane sample at DDFSTRT $38 is already one lowres native sample into the
// fetched word, so the fetch/output phase is referenced one color clock earlier.
//
// The lo-res fetch/display phase sits 3 colour clocks later than the hi-res
// phase: a hi-res fetch slot delivers its word to Denise's shifter on a
// different beam edge than a lo-res slot, so the reference the renderer uses to
// place the first fetched pixel differs by resolution. The references position
// bitplane sample 0 at framebuffer x = (reference - DIW_HSTART_FB0) * 2, so
// they moved +1 in lockstep with the DIW_HSTART_FB0 comparator fix to keep the
// hardware-calibrated bitmap positions (lo-res sample 0 at x = 62, hi-res
// standard $81/$3C flush at x = 64). The standard $81 window edge now sits at
// x = 62 too, exposing the lo-res bitmap's first sample as on real hardware.
// See `fetch_reference` below.
const DIW_HSTART_FETCH_REFERENCE_LORES: i32 = 0x81;
const DIW_HSTART_FETCH_REFERENCE_HIRES: i32 = 0x82;
// Register/copper-write x=0 anchor, in colour clocks. Moved left by 8 colour
// clocks in lockstep with DIW_HSTART_FB0 (16 lo-res pixels) so register writes
// and bitplane pixels still register against each other after widening.
//
// This anchor is the beam-position -> framebuffer-x mapping of the write
// domain. It maps events recorded at their Denise-effective position (the
// carrying chip-bus slot plus DENISE_WRITE_EFFECT_DELAY_CCK, source
// independent -- see `Bus::record_render_write`), so it did NOT move when
// the Copper WAIT comparator lookahead moved copper write landings 4 colour
// clocks earlier on the bus, nor when CPU-sourced events switched from the
// post-bus-cycle beam position to the same slot-referenced delay: a write
// carried by a given bus slot produces pixels at the same place regardless
// of who performed it. Copper-vs-fetch races compare both sides through
// this same anchor, so they follow the corrected bus landings automatically.
const COPPER_WAIT_HPOS_FB0: i32 = 0x28;
/// COLORxx writes feed Denise's final colour-selection/output path. Denise
/// applies copper/CPU colour-register changes in the palette/output phase,
/// ahead of register writes that feed the bitplane shifter. This anchor keeps
/// COLORxx changes one lores pixel ahead of the generic beam/write domain,
/// matching vAmiga's model where COLORxx is recorded at the current output
/// pixel while BPLCON/DDF/sprite data paths carry explicit pixel delays. OCS
/// Denise (8362) and ECS Denise (8373) share this timing exactly -- the only
/// OCS/ECS colour-path difference is the OCS 12-bit value mask -- so this
/// anchor is revision-independent across OCS/ECS.
///
/// TODO: AGA Lisa delays colour changes by one hires pixel relative to
/// OCS/ECS (WinUAE: "AGA color changes are 1 hires pixel delayed"). That
/// sub-colour-clock offset is not yet modelled here.
///
/// STOP before retuning this. If a scene's colours or copper-driven picture
/// look horizontally shifted, the cause is usually bitplane fetch/DDF
/// alignment, sprite arming, or a missed write-domain delay, not this final
/// colour-output anchor. The anchor maps events recorded at their
/// Denise-effective position (bus landing plus the write-effect delay, see
/// `Bus::record_render_write`), so it did not move when the Copper WAIT
/// comparator lookahead fix moved copper bus landings four colour clocks
/// earlier.
const COLOR_WRITE_HPOS_FB0: i32 = 0x35;
/// Denise's texture/output line starts at the hblank-start counter value. Beam
/// positions before this are the wrapped tail of the previous output line, so
/// a colour write there must draw at the far right of the previous row while
/// still updating the palette seen by the following row.
const DENISE_HBLANK_START_HPOS: u32 = 0x12;
/// AGA BPLCON4's low sprite-palette byte follows Lisa's sprite colour lookup
/// path, which reaches sprite output earlier than ordinary COLORxx palette
/// writes. Keep it separate from COLOR replay so copper palette gradients stay
/// in the Denise palette-output phase on OCS/ECS.
const SPRITE_PALETTE_CONTROL_HPOS_FB0: i32 = 0x36;
/// SPRxPOS and SPRxDATx writes feed Denise's sprite comparator/latches seven
/// CCK ahead of the normal register/output beam domain. Manual sprite replays
/// use this earlier domain so adjacent position writes can abut at their
/// programmed HSTARTs, and data writes that beat the comparator load the same
/// scanline.
const SPRITE_REGISTER_WRITE_PIPELINE_CCK: u32 = 7;
/// Framebuffer-x offset between the copper/register coordinate
/// ([`COPPER_WAIT_HPOS_FB0`], used to place beam-timed register writes) and the
/// bitplane/DIW coordinate ([`DIW_HSTART_FB0`], used to place fetched bitplane
/// pixels, which bakes in the Agnus-fetch -> Denise-display pipeline delay).
///
/// A register write at copper-x maps to bitplane-x `copper_x - this`. Bitplane
/// control writes (BPLCON scroll/mode) feed the bitplane shifter, so the scroll
/// they set must be applied to the pixels they actually control. Without this
/// correction a per-line scroll write lands
/// to the right of the first fetched word it governs, leaving that word's left
/// edge using the previous line's stale scroll -- a duplicate ("E-clone") of the
/// playfield's left edge in the deep left overscan at maximum scroll.
const BITPLANE_CONTROL_PIPELINE_FB: usize =
    ((DIW_HSTART_FB0 - COPPER_WAIT_HPOS_FB0 * 2) * 2) as usize;
/// Framebuffer x of the left edge of the standard (non-overscan) display. The
/// columns to its left are overscan border that a real PAL display crops.
pub(crate) const STANDARD_VISIBLE_X0: usize = ((STANDARD_DIW_HSTART - DIW_HSTART_FB0) * 2) as usize;
/// Standard PAL display right edge (DIWSTOP H), in colour clocks. Columns to
/// its right are overscan a real PAL display crops. OCS forces DIWSTOP bit 8,
/// so the stock $2CC1 DIWSTOP yields $1C1.
const STANDARD_DIW_HSTOP: i32 = 0x1C1;

/// Horizontal presentation shift, in framebuffer pixels, that recentres a
/// standard (non-overscan) display inside the overscan field buffer.
///
/// The framebuffer anchors a deep slab of left overscan ([`DIW_HSTART_FB0`] is
/// 0x20 colour clocks left of the standard display start = 64 hi-res px) but
/// only a few px of right overscan, so a stock display sits right-of-centre
/// compared with vAmiga/FS-UAE, which crop overscan roughly symmetrically.
/// Shifting the picture left by half the border asymmetry centres it.
///
/// Returns 0 (leave the frame untouched) whenever the window uses left *or*
/// right overscan, so overscan demos -- which deliberately fetch into the
/// border -- are presented exactly as rendered and never clipped.
pub fn present_h_shift(diw_h_start: u16, diw_h_stop: u16) -> usize {
    let h_start = diw_h_start as i32;
    let h_stop = diw_h_stop as i32;
    if h_start < STANDARD_DIW_HSTART || h_stop > STANDARD_DIW_HSTOP {
        return 0;
    }
    let left_border = ((h_start - DIW_HSTART_FB0).max(0) * 2) as usize;
    let right_x = ((h_stop - DIW_HSTART_FB0).max(0) * 2) as usize;
    let right_border = FB_WIDTH.saturating_sub(right_x);
    left_border.saturating_sub(right_border) / 2
}

/// `present_h_shift`, but able to recentre a display that opens its DIW window
/// into the overscan around a picture it only fetches at standard width.
///
/// A demo can open DIWSTRT/DIWSTOP much wider than the playfield it draws --
/// Virtual Dreams' "Absolute Inebriation" opens DIW $02..$1FF around a standard
/// 320-px lo-res picture (DDF $38..$D0) -- where the extra window only reveals
/// COLOR0 border the TV crops, not content. The bare `present_h_shift` keys off
/// DIW alone and so declines to recentre any window that reaches into the
/// overscan, leaving such a picture sitting right-of-centre.
///
/// While the DIW window stays inside the standard window, behaviour is
/// identical to `present_h_shift(diw_h_start, diw_h_stop)` -- the window
/// (including any COLOR0 border it legitimately shows) is centred as before.
/// Only when the window reaches into the overscan do we fall back to centring
/// on the *fetched content* (DDF) clamped to the window: a picture whose fetch
/// stays within the standard window is recentred like the standard display it
/// really is, while one that genuinely fetches bitplane data into the border is
/// still left exactly as rendered (`present_h_shift` returns 0 for it).
pub fn present_h_shift_for(snapshot: &RenderRegisterSnapshot) -> usize {
    let control = ControlState::from_render_state(&RenderState::from_snapshot(*snapshot));
    let diw_start = control.diw_h_start() as i32;
    let mut diw_stop = control.diw_h_stop() as i32;
    if diw_stop <= diw_start {
        diw_stop += 0x100;
    }
    // DIW within the standard window: centre on the window itself, exactly as
    // the bare helper does, so stock and sub-standard displays are unchanged.
    if diw_start >= STANDARD_DIW_HSTART && diw_stop <= STANDARD_DIW_HSTOP {
        return present_h_shift(diw_start as u16, diw_stop as u16);
    }
    // DIW reaches into the overscan: recentre on the fetched content if it
    // stays standard, otherwise leave a true overscan display untouched.
    let Some((content_start, content_stop)) = control.bitplane_content_window_h() else {
        return 0;
    };
    let eff_start = diw_start.max(content_start);
    let eff_stop = diw_stop.min(content_stop);
    if eff_stop <= eff_start {
        return 0;
    }
    present_h_shift(eff_start as u16, eff_stop as u16)
}

/// Whether a TV-style standard PAL aperture can crop the horizontal borders
/// without cutting fetched playfield content. Wide DIW alone is not enough to
/// reject the aperture: some software opens DIW into the border around a
/// standard-width fetch and only exposes COLOR0 there. A fetch that reaches
/// outside the standard window is true horizontal overscan and must stay on
/// the full framebuffer.
pub(crate) fn uses_standard_horizontal_content(snapshot: &RenderRegisterSnapshot) -> bool {
    let control = ControlState::from_render_state(&RenderState::from_snapshot(*snapshot));
    let diw_start = control.diw_h_start() as i32;
    let mut diw_stop = control.diw_h_stop() as i32;
    if diw_stop <= diw_start {
        diw_stop += 0x100;
    }
    if diw_start >= STANDARD_DIW_HSTART && diw_stop <= STANDARD_DIW_HSTOP {
        return true;
    }
    let Some((content_start, content_stop)) = control.bitplane_content_window_h() else {
        return false;
    };
    let eff_start = diw_start.max(content_start);
    let eff_stop = diw_stop.min(content_stop);
    eff_start >= STANDARD_DIW_HSTART && eff_stop <= STANDARD_DIW_HSTOP && eff_stop > eff_start
}
const BPLCON0_ECSENA: u16 = 1 << 0;
const BPLCON0_SHRES: u16 = 1 << 6;
const BPLCON2_ZDBPSEL_SHIFT: u16 = 12;
const BPLCON2_ZDBPSEL_MASK: u16 = 0x7000;
const BPLCON2_ZDBPEN: u16 = 1 << 11;
const BPLCON2_ZDCTEN: u16 = 1 << 10;
const BPLCON2_KILLEHB: u16 = 1 << 9;
const BPLCON3_BRDSPRT: u16 = 1 << 1;
const BPLCON3_ZDCLKEN: u16 = 1 << 2;
const BPLCON3_BRDNTRAN: u16 = 1 << 4;
const BPLCON3_BRDRBLNK: u16 = 1 << 5;
const BPLCON3_SPRES_MASK: u16 = 0x00C0;
const BPLCON3_SPRES_LORES: u16 = 0x0040;
const BPLCON3_SPRES_HIRES: u16 = 0x0080;
const BPLCON3_SPRES_SHRES: u16 = 0x00C0;
const BPLCON3_PF2OF_MASK: u16 = 0x1C00;
const DMACON_SPREN: u16 = 1 << 5;
const DMACON_BPLEN: u16 = 1 << 8;
const DMACON_DMAEN: u16 = 1 << 9;

#[cfg(test)]
fn sprite_display_enabled_from_line_start() -> [Option<usize>; FB_HEIGHT] {
    [Some(0); FB_HEIGHT]
}
const BITPLANE_DDF_HARD_START: u16 = 0x0018;
const BITPLANE_DDF_HARD_STOP: u16 = 0x00D8;
const BITPLANE_FETCH_HARD_END: u32 = BITPLANE_DDF_HARD_STOP as u32 + 7;
const OCS_LORES_BPL_SEQUENCE: [usize; 8] = [8, 4, 6, 2, 7, 3, 5, 1];
#[cfg_attr(
    not(any(test, debug_assertions, feature = "display-plan-trace")),
    allow(dead_code)
)]
const SPRITE_DMA_PAIR_CAPTURE_HPOS: [u32; 4] = [0x018, 0x020, 0x028, 0x030];

#[derive(Clone, Copy)]
/// One COLORxx write replayed mid-line: at framebuffer x, the absolute
/// palette entry (bank * 32 + index on AGA, plain index otherwise) takes
/// `value` on the high-nibble plane (and low, unless `loct`). Stored as a
/// diff (not a full palette snapshot) so copper-palette-heavy frames stay
/// cheap with the 256-entry store.
struct PaletteSegment {
    x: usize,
    entry: u8,
    loct: bool,
    value: u16,
}

impl PaletteSegment {
    fn apply(&self, palette: &mut Palette) {
        palette.write_entry(usize::from(self.entry), self.loct, self.value);
    }
}

#[derive(Clone, Copy)]
struct PaletteRowDiag {
    first_vpos: u32,
    last_vpos: u32,
}

impl PaletteRowDiag {
    fn contains(self, vpos: u32) -> bool {
        (self.first_vpos..=self.last_vpos).contains(&vpos)
    }
}

fn parse_diag_u32(raw: &str) -> Option<u32> {
    let raw = raw.trim();
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        raw.parse::<u32>().ok()
    }
}

/// Cached COPPERLINE_DIAG_PALETTE_ROW setting (read once). Accepted forms:
/// presence/`all` logs every COLOR write, `V` logs one beam line, and
/// `START:END` logs an inclusive beam-line range.
fn palette_row_diag() -> Option<PaletteRowDiag> {
    static SPEC: OnceLock<Option<PaletteRowDiag>> = OnceLock::new();
    *SPEC.get_or_init(|| {
        let raw = crate::envcfg::var("COPPERLINE_DIAG_PALETTE_ROW")?;
        let raw = raw.trim();
        if raw.is_empty() || raw == "1" || raw.eq_ignore_ascii_case("all") {
            return Some(PaletteRowDiag {
                first_vpos: 0,
                last_vpos: u32::MAX,
            });
        }
        if let Some((first, last)) = raw.split_once(':') {
            let first_vpos = parse_diag_u32(first).unwrap_or(0);
            let last_vpos = parse_diag_u32(last).unwrap_or(u32::MAX);
            return Some(PaletteRowDiag {
                first_vpos: first_vpos.min(last_vpos),
                last_vpos: first_vpos.max(last_vpos),
            });
        }
        parse_diag_u32(raw).map(|vpos| PaletteRowDiag {
            first_vpos: vpos,
            last_vpos: vpos,
        })
    })
}

fn beam_write_source_label(source: BeamWriteSource) -> &'static str {
    match source {
        BeamWriteSource::Cpu => "cpu",
        BeamWriteSource::CpuCopperIrq => "cpu_copper_irq",
        BeamWriteSource::Copper => "copper",
    }
}

/// Cached COPPERLINE_CLAMP_PLANES setting (read once). `bitplane_mode` runs per
/// pixel in the playfield decode loop, so it must not do a map lookup (hashing
/// the name dominated the whole renderer in profiles).
fn clamp_planes_setting() -> Option<u16> {
    use std::sync::OnceLock;
    static V: OnceLock<Option<u16>> = OnceLock::new();
    *V.get_or_init(|| {
        crate::envcfg::var("COPPERLINE_CLAMP_PLANES").and_then(|v| v.trim().parse::<u16>().ok())
    })
}

/// Resolve where a COLORxx write lands in the palette store: the BPLCON3
/// BANK/LOCT mechanics on AGA, the plain entry otherwise.
fn palette_entry_for_write(bplcon3: u16, aga: bool, idx: usize) -> (u8, bool) {
    if aga {
        (
            (Palette::bank_from_bplcon3(bplcon3) * 32 + (idx & 31)) as u8,
            Palette::loct_from_bplcon3(bplcon3),
        )
    } else {
        ((idx & 31) as u8, false)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ControlState {
    agnus_revision: AgnusRevision,
    harddis: bool,
    dmacon: u16,
    bplcon0: u16,
    bplcon1: u16,
    bplcon2: u16,
    bplcon3: u16,
    bplcon4: u16,
    /// AGA FMODE latch; 0 on OCS/ECS.
    fmode: u16,
    clxcon: u16,
    clxcon2: u16,
    diwstrt: u16,
    diwstop: u16,
    diwhigh: DiwHigh,
    ddfstrt: u16,
    ddfstop: u16,
    bpl1mod: i16,
    bpl2mod: i16,
}

fn display_window_unprogrammed(diwstrt: u16, diwstop: u16) -> bool {
    diwstrt == 0 && diwstop == 0
}

/// Horizontal display-window state for one framebuffer row, from the
/// hardware comparator model: Denise's window flip-flop opens on an exact
/// HSTART match of its horizontal counter and closes on an exact HSTOP
/// match, evaluated per lores position with the register values current
/// at that position (vAmiga's hardware-verified model). No match means no
/// change, so degenerate windows never close (or never open) and the
/// state carries across lines. `open_runs` lists the [start, end) spans
/// in framebuffer hires x where the window is open on this row.
#[derive(Clone, Debug, Default)]
pub(super) struct HWindowRow {
    open_runs: Vec<(usize, usize)>,
    /// Framebuffer x of the first HSTART comparator match on this row
    /// (None when the row's openness is only carried in from a previous
    /// line). Feeds the playfield painter's shifter-origin anchor.
    comparator_anchor: Option<usize>,
}

impl HWindowRow {
    fn full() -> Self {
        Self {
            open_runs: vec![(0, FB_WIDTH)],
            comparator_anchor: None,
        }
    }

    /// Envelope for consumers that only handle a single span (sprite
    /// windows, scroll bookkeeping): first open edge to last close edge.
    pub(super) fn span(&self) -> (usize, usize) {
        match (self.open_runs.first(), self.open_runs.last()) {
            (Some(&(start, _)), Some(&(_, end))) => (start, end),
            _ => (0, 0),
        }
    }

    pub(super) fn open_runs(&self) -> &[(usize, usize)] {
        &self.open_runs
    }

    /// Whether the window flip-flop is open at framebuffer x.
    fn open_at(&self, x: usize) -> bool {
        self.open_runs.iter().any(|&(s, e)| x >= s && x < e)
    }

    /// First open/close boundary right of x (for run chunking).
    fn next_boundary_after(&self, x: usize) -> usize {
        self.open_runs
            .iter()
            .flat_map(|&(s, e)| [s, e])
            .filter(|&b| b > x)
            .min()
            .unwrap_or(FB_WIDTH)
    }
}

/// Denise's horizontal comparator counter starts each line at the hblank
/// edge (lores position 0x24 = HBLANK_MIN colour clocks), runs 454 lores
/// positions and wraps 0x1C8 -> 2 near the line end, so positions 2..0x23
/// are a line's tail, not its head. OCS Denise does not reset the counter
/// on lines 0-8: it free-runs modulo 0x200 there, which lets otherwise
/// unreachable comparator values (0x1C8..0x1FF) fire during vertical
/// blank. Framebuffer hires x maps as x = (counter - DIW_HSTART_FB0) * 2.
/// (vAmiga's hardware-verified model, Denise::updateBorderBuffer.)
const H_COUNTER_LINE_ORIGIN: i32 = 0x24;
const H_COUNTER_TICKS_PER_LINE: i32 = 454;
const H_COUNTER_WRAP: i32 = 0x1C8;
const H_COUNTER_WRAP_TARGET: i32 = 2;
/// Scan tick (lores steps from the line origin) at which the framebuffer
/// begins.
const H_COUNTER_FB_START_TICK: i32 = DIW_HSTART_FB0 - H_COUNTER_LINE_ORIGIN;
/// A DIWSTRT/DIWSTOP write reaches Denise's window comparators one colour
/// clock after the write cycle (vAmiga applies the Denise-side register
/// change with a DMA_CYCLES(1) delay).
const DIW_COMPARATOR_WRITE_DELAY_FB: i32 = 4;

/// Scan tick at which a control segment's DIW values reach the window
/// comparators. Segment x is in the copper/register coordinate; the
/// comparators sit with the bitplane controls on the other side of the
/// fetch -> display pipeline, plus the one-colour-clock write delay.
fn diw_segment_effect_tick(seg_x: usize) -> i32 {
    (seg_x as i32 - BITPLANE_CONTROL_PIPELINE_FB as i32 + DIW_COMPARATOR_WRITE_DELAY_FB) / 2
        + H_COUNTER_FB_START_TICK
}

/// Run the window flip-flop over one beam line, updating `flop` in place.
/// When `record` is given, [start, end) open spans clipped to the
/// framebuffer are appended to it.
fn scan_h_window_line(
    flop: &mut bool,
    beam_line: i32,
    is_ecs: bool,
    mut control: ControlState,
    segs: &[ControlSegment],
    mut record: Option<&mut HWindowRow>,
) {
    let free_run = beam_line < 9 && !is_ecs;
    let mut counter = if free_run {
        (H_COUNTER_LINE_ORIGIN + beam_line * 0x1C6) & 0x1FF
    } else {
        H_COUNTER_LINE_ORIGIN
    };
    let mut hstrt = control.diw_h_start() as i32;
    let mut hstop = control.diw_h_stop() as i32;
    let mut seg_idx = 0usize;
    let mut open_from: Option<usize> = None;
    for tick in 0..H_COUNTER_TICKS_PER_LINE {
        while seg_idx < segs.len() && diw_segment_effect_tick(segs[seg_idx].x) <= tick {
            control = segs[seg_idx].control;
            hstrt = control.diw_h_start() as i32;
            hstop = control.diw_h_stop() as i32;
            seg_idx += 1;
        }
        if record.is_some() && tick == H_COUNTER_FB_START_TICK && *flop {
            open_from = Some(0);
        }
        let was_open = *flop;
        if counter == hstrt {
            *flop = true;
        }
        if counter == hstop {
            *flop = false;
        }
        if *flop != was_open {
            if let Some(row) = record.as_deref_mut() {
                let fb_tick = tick - H_COUNTER_FB_START_TICK;
                if (0..FB_WIDTH as i32 / 2).contains(&fb_tick) {
                    let x = (fb_tick * 2) as usize;
                    if *flop {
                        open_from = Some(x);
                        if row.comparator_anchor.is_none() {
                            row.comparator_anchor = Some(x);
                        }
                    } else if let Some(start) = open_from.take() {
                        if x > start {
                            row.open_runs.push((start, x));
                        }
                    }
                } else if fb_tick >= FB_WIDTH as i32 / 2 && !*flop {
                    // Closed right of the framebuffer (or in the wrapped
                    // tail): the visible part of the run reaches the edge.
                    // A reopening out here only matters as carry into the
                    // next line.
                    if let Some(start) = open_from.take() {
                        row.open_runs.push((start, FB_WIDTH));
                    }
                }
            }
        }
        counter = (counter + 1) & 0x1FF;
        if counter == H_COUNTER_WRAP && !free_run {
            counter = H_COUNTER_WRAP_TARGET;
        }
    }
    if let Some(row) = record {
        if let Some(start) = open_from.take() {
            if FB_WIDTH > start {
                row.open_runs.push((start, FB_WIDTH));
            }
        }
    }
}

fn compute_h_window_rows(
    base_controls: &[ControlState],
    control_segments: &[Vec<ControlSegment>],
    visible_line0: i32,
) -> Vec<HWindowRow> {
    let rows = base_controls.len();
    let mut out = vec![HWindowRow::default(); rows];
    if rows == 0 {
        return out;
    }
    let is_ecs = base_controls[0].agnus_revision.is_ecs();
    // Vertical sync leaves the flip-flop set (vAmiga vsyncHandler). The
    // pre-visible lines then run with the frame-start values: register
    // writes before the visible window are already folded into row 0's
    // base control.
    let mut flop = true;
    for beam_line in 0..visible_line0.max(0) {
        scan_h_window_line(&mut flop, beam_line, is_ecs, base_controls[0], &[], None);
    }
    for y in 0..rows {
        let beam_line = visible_line0 + y as i32;
        let mut row = HWindowRow::default();
        scan_h_window_line(
            &mut flop,
            beam_line,
            is_ecs,
            base_controls[y],
            &control_segments[y],
            Some(&mut row),
        );
        // Software that never programs DIW keeps the pragmatic whole-
        // framebuffer window (matching display_window_x).
        if display_window_unprogrammed(base_controls[y].diwstrt, base_controls[y].diwstop)
            && control_segments[y].is_empty()
        {
            out[y] = HWindowRow::full();
        } else {
            out[y] = row;
        }
    }
    out
}

/// Repaint the horizontal window's closed intervals with border pixels
/// after compositing (see the call site). Only rows inside the vertical
/// window need it: rows outside are already all border.
#[allow(clippy::too_many_arguments)]
fn enforce_h_window_closed_intervals(
    fb: &mut [u32],
    base_palettes: &[Palette],
    palette_segments: &[Vec<PaletteSegment>],
    base_controls: &[ControlState],
    control_segments: &[Vec<ControlSegment>],
    h_window_rows: &[HWindowRow],
    visible_line0: i32,
    rows: usize,
) {
    for y in 0..rows {
        let open_runs = h_window_rows[y].open_runs();
        if open_runs.len() == 1 && open_runs[0] == (0, FB_WIDTH) {
            continue;
        }
        let row = &mut fb[y * FB_WIDTH..(y + 1) * FB_WIDTH];
        let mut x = 0usize;
        while x < FB_WIDTH {
            let next_open = open_runs
                .iter()
                .map(|&(s, _)| s)
                .filter(|&s| s > x)
                .min()
                .unwrap_or(FB_WIDTH);
            if open_runs.iter().any(|&(s, e)| x >= s && x < e) {
                // Inside an open run: skip to its end.
                let end = open_runs
                    .iter()
                    .find(|&&(s, e)| x >= s && x < e)
                    .map(|&(_, e)| e)
                    .unwrap_or(FB_WIDTH);
                x = end;
                continue;
            }
            let closed_end = next_open.min(FB_WIDTH);
            // Repaint [x, closed_end) as border, walking the control and
            // palette segments for the correct border colour, and skipping
            // border-sprite segments.
            let mut sx = x;
            while sx < closed_end {
                let control = control_at_x(base_controls[y], &control_segments[y], sx);
                let next_ctl = control_segments[y]
                    .iter()
                    .map(|seg| seg.x)
                    .filter(|&b| b > sx)
                    .min()
                    .unwrap_or(FB_WIDTH)
                    .min(closed_end);
                if !control.display_window_contains_line(y, visible_line0) {
                    // Row is outside the vertical window here: already border.
                    sx = next_ctl;
                    continue;
                }
                if control.border_sprite_enabled() {
                    sx = next_ctl;
                    continue;
                }
                let palette = palette_at_x(base_palettes[y], &palette_segments[y], sx);
                let pixel = background_pixel(&control, palette[0], true);
                row[sx..next_ctl].fill(pixel);
                sx = next_ctl;
            }
            x = closed_end;
        }
    }
}

impl ControlState {
    fn from_render_state(state: &RenderState) -> Self {
        Self {
            agnus_revision: state.agnus_revision,
            harddis: state.harddis,
            dmacon: state.dmacon,
            bplcon0: state.bplcon0,
            bplcon1: state.bplcon1,
            bplcon2: state.bplcon2,
            bplcon3: state.bplcon3,
            bplcon4: state.bplcon4,
            fmode: state.fmode,
            clxcon: state.clxcon,
            clxcon2: state.clxcon2,
            diwstrt: state.diwstrt,
            diwstop: state.diwstop,
            diwhigh: state.diwhigh,
            ddfstrt: state.ddfstrt,
            ddfstop: state.ddfstop,
            bpl1mod: state.bpl1mod,
            bpl2mod: state.bpl2mod,
        }
    }

    fn bitplane_mode(&self) -> BitplaneMode {
        // Debug aid: COPPERLINE_CLAMP_PLANES=N clamps the displayed bitplane count
        // to N by masking the BPLCON0 BPU field, for A/B testing which plane is
        // responsible for a rendering artifact.
        if let Some(n) = clamp_planes_setting() {
            let bpu = (self.bplcon0 >> 12) & 0x7;
            if bpu > n {
                let clamped = (self.bplcon0 & !0x7000) | (n << 12);
                return BitplaneMode::from_bplcon0(clamped, self.aga());
            }
        }
        BitplaneMode::from_bplcon0(self.bplcon0, self.aga())
    }

    fn aga(&self) -> bool {
        matches!(self.agnus_revision, AgnusRevision::AgaAlice)
    }

    fn nplanes(&self) -> usize {
        self.bitplane_mode().display_planes()
    }

    fn dma_planes(&self) -> usize {
        self.bitplane_mode().dma_planes()
    }

    fn bitplane_dma_enabled(&self) -> bool {
        self.dmacon & (DMACON_DMAEN | DMACON_BPLEN) == (DMACON_DMAEN | DMACON_BPLEN)
    }

    fn ecsena(&self) -> bool {
        self.bplcon0 & BPLCON0_ECSENA != 0
    }

    fn border_blank_enabled(&self) -> bool {
        self.ecsena() && self.bplcon3 & BPLCON3_BRDRBLNK != 0
    }

    fn border_non_transparent_enabled(&self) -> bool {
        self.ecsena() && self.bplcon3 & BPLCON3_BRDNTRAN != 0
    }

    fn border_sprite_enabled(&self) -> bool {
        self.ecsena() && self.bplcon3 & BPLCON3_BRDSPRT != 0
    }

    fn zd_clock_enabled(&self) -> bool {
        self.ecsena() && self.bplcon3 & BPLCON3_ZDCLKEN != 0
    }

    fn color_key_enabled(&self) -> bool {
        !self.zd_clock_enabled() && self.bplcon2 & BPLCON2_ZDCTEN != 0
    }

    fn bitplane_key_enabled(&self) -> bool {
        !self.zd_clock_enabled() && self.bplcon2 & BPLCON2_ZDBPEN != 0
    }

    fn bitplane_key_plane(&self) -> usize {
        ((self.bplcon2 & BPLCON2_ZDBPSEL_MASK) >> BPLCON2_ZDBPSEL_SHIFT) as usize
    }

    fn genlock_transparent(
        &self,
        color_latch: u16,
        sample: Option<DeniseBitplaneSample>,
        border: bool,
    ) -> bool {
        if self.zd_clock_enabled() || (border && self.border_non_transparent_enabled()) {
            return false;
        }
        let color_key = self.color_key_enabled() && color_latch & COLOR_TRANSPARENCY_BIT != 0;
        let bitplane_key = self.bitplane_key_enabled()
            && sample.is_some_and(|sample| {
                let plane = self.bitplane_key_plane();
                plane < sample.nplanes && sample.idx & (1 << plane) != 0
            });
        (border && self.border_blank_enabled()) || color_key || bitplane_key
    }

    fn sprite_pixel_repeat(&self) -> i32 {
        match self.bplcon3 & BPLCON3_SPRES_MASK {
            0 => {
                if self.bplcon0 & BPLCON0_SHRES != 0 {
                    1
                } else {
                    2
                }
            }
            BPLCON3_SPRES_LORES => 2,
            BPLCON3_SPRES_HIRES | BPLCON3_SPRES_SHRES => {
                // TODO: A true SHRES output path should emit 35 ns sprite
                // samples; the current framebuffer resolves 70 ns.
                1
            }
            _ => unreachable!(),
        }
    }

    fn display_window_contains_line(&self, line: usize, visible_line0: i32) -> bool {
        if display_window_unprogrammed(self.diwstrt, self.diwstop) {
            return true;
        }
        let start = self.diw_v_start() as i32;
        let mut stop = self.diw_v_stop() as i32;
        let mut v = visible_line0 + line as i32;
        if stop <= start {
            stop += 0x100;
            if v < start {
                v += 0x100;
            }
        }
        v >= start && v < stop
    }

    fn display_window_x(&self) -> (usize, usize) {
        if display_window_unprogrammed(self.diwstrt, self.diwstop) {
            return (0, FB_WIDTH);
        }
        let start = self.diw_h_start() as i32;
        let mut stop = self.diw_h_stop() as i32;
        if stop <= start {
            stop += 0x100;
        }
        let left = ((start - DIW_HSTART_FB0).max(0) as usize * 2).min(FB_WIDTH);
        let mut right = ((stop - DIW_HSTART_FB0).max(0) as usize * 2).min(FB_WIDTH);
        if FB_WIDTH.saturating_sub(right) <= 2 {
            right = FB_WIDTH;
        }
        if right > left {
            (left, right)
        } else {
            (left, FB_WIDTH)
        }
    }

    fn clipped_display_rows_before_frame(&self, visible_line0: i32) -> usize {
        if display_window_unprogrammed(self.diwstrt, self.diwstop) {
            return 0;
        }
        (visible_line0 - self.diw_v_start() as i32).max(0) as usize
    }

    fn diw_v_start(&self) -> u16 {
        self.diwhigh.v_start(self.diwstrt)
    }

    fn diw_v_stop(&self) -> u16 {
        self.diwhigh.v_stop(self.diwstop)
    }

    fn diw_h_start(&self) -> u16 {
        self.diwhigh.h_start(self.diwstrt)
    }

    fn diw_h_stop(&self) -> u16 {
        self.diwhigh.h_stop(self.diwstop)
    }

    fn hires(&self) -> bool {
        self.bplcon0 & 0x8000 != 0 && !self.shres()
    }

    fn shres(&self) -> bool {
        self.bplcon0 & BPLCON0_SHRES != 0
    }

    /// Beam hpos the renderer treats as the origin for the first fetched
    /// bitplane pixel. Hi-res and lo-res use separate references because their
    /// fetch slots load Denise's shifter on different beam phases. Super-hi-res
    /// (ECS, effectively unused under OCS) keeps the lo-res anchor.
    fn fetch_reference(&self) -> i32 {
        if self.hires() {
            DIW_HSTART_FETCH_REFERENCE_HIRES
        } else {
            DIW_HSTART_FETCH_REFERENCE_LORES
        }
    }

    fn framebuffer_pixel_repeat(&self) -> usize {
        if self.hires() || self.shres() {
            1
        } else {
            2
        }
    }

    fn native_samples_per_framebuffer_pixel(&self) -> usize {
        if self.shres() {
            2
        } else {
            1
        }
    }

    fn fetch_cck_per_word(&self) -> u32 {
        if self.shres() {
            2
        } else if self.hires() {
            4
        } else {
            8
        }
    }

    /// AGA FMODE: 16-bit words per bitplane fetch slot.
    fn fetch_quantum(&self) -> u32 {
        if !self.aga() {
            return 1;
        }
        match self.fmode & 0x0003 {
            0 => 1,
            3 => 4,
            _ => 2,
        }
    }

    /// Colour clocks between successive fetches of one plane.
    fn fetch_period(&self) -> u32 {
        self.fetch_cck_per_word() * self.fetch_quantum()
    }

    /// DDF block quantum in colour clocks (8 at FMODE=0).
    fn fetch_unit(&self) -> u32 {
        self.fetch_period().max(8)
    }

    /// AGA BPLCON4 BPLAM: XOR mask applied to the bitplane pixel index.
    fn bplam(&self) -> u8 {
        (self.bplcon4 >> 8) as u8
    }

    fn extra_half_brite(&self) -> bool {
        // OCS EHB is selected by six bitplanes with HAM and dual-playfield
        // disabled. Bitplane 6 halves the intensity of colors 0..31.
        self.nplanes() == 6 && self.bplcon0 & 0x0C00 == 0 && self.bplcon2 & BPLCON2_KILLEHB == 0
    }

    fn hold_and_modify(&self) -> bool {
        !self.shres() && matches!(self.nplanes(), 5 | 6) && self.bplcon0 & 0x0800 != 0
    }

    fn dual_playfield(&self) -> bool {
        self.bplcon0 & 0x0400 != 0
    }

    fn pf2_priority(&self) -> bool {
        self.bplcon2 & 0x0040 != 0
    }

    fn pf2_palette_offset(&self) -> usize {
        match (self.bplcon3 & BPLCON3_PF2OF_MASK) >> 10 {
            0 => 0,
            1 => 2,
            2 => 4,
            3 => 8,
            4 => 16,
            5 => 32,
            6 => 64,
            7 => 128,
            _ => unreachable!(),
        }
    }

    fn pf1_scroll(&self) -> usize {
        if self.aga() {
            return self.aga_bplcon1_scroll_samples(false);
        }
        (self.bplcon1 & 0x000F) as usize
    }

    fn pf2_scroll(&self) -> usize {
        if self.aga() {
            return self.aga_bplcon1_scroll_samples(true);
        }
        ((self.bplcon1 >> 4) & 0x000F) as usize
    }

    /// AGA Lisa expands BPLCON1 to two 8-bit scroll counters in 35 ns
    /// super-hires units. The old OCS/ECS nibble bits are renamed to H2..H5,
    /// preserving old lo-res scroll values, while the new bits provide
    /// sub-lores positioning and the extra range needed by wide FMODE fetches.
    fn aga_bplcon1_scroll_samples(&self, pf2: bool) -> usize {
        let shres_scroll = if pf2 {
            ((self.bplcon1 >> 12) & 0x0001)
                | ((self.bplcon1 >> 12) & 0x0002)
                | ((self.bplcon1 >> 2) & 0x0004)
                | ((self.bplcon1 >> 2) & 0x0008)
                | ((self.bplcon1 >> 2) & 0x0010)
                | ((self.bplcon1 >> 2) & 0x0020)
                | ((self.bplcon1 >> 8) & 0x0040)
                | ((self.bplcon1 >> 8) & 0x0080)
        } else {
            ((self.bplcon1 >> 8) & 0x0001)
                | ((self.bplcon1 >> 8) & 0x0002)
                | ((self.bplcon1 << 2) & 0x0004)
                | ((self.bplcon1 << 2) & 0x0008)
                | ((self.bplcon1 << 2) & 0x0010)
                | ((self.bplcon1 << 2) & 0x0020)
                | ((self.bplcon1 >> 4) & 0x0040)
                | ((self.bplcon1 >> 4) & 0x0080)
        } as usize;
        let fetch_mask = match self.fetch_quantum() {
            1 => 0x3F,
            2 => 0x7F,
            _ => 0xFF,
        };
        let shres_scroll = shres_scroll & fetch_mask;
        let samples_per_native = if self.shres() {
            1
        } else if self.hires() {
            2
        } else {
            4
        };
        shres_scroll / samples_per_native
    }

    fn scroll_for_plane(&self, plane: usize) -> usize {
        if plane & 1 != 0 {
            self.pf2_scroll()
        } else {
            self.pf1_scroll()
        }
    }

    fn playfield_priority_code(&self, playfield: u8) -> u8 {
        if playfield == 1 {
            (self.bplcon2 & 0x0007) as u8
        } else {
            ((self.bplcon2 >> 3) & 0x0007) as u8
        }
    }

    /// End-of-line modulo for a plane. FMODE BSCAN2 (bit 14, Alice only)
    /// scan-doubles bitplanes: both plane groups share one modulo, selected
    /// by the line parity relative to DIWSTRT's vertical start - the
    /// matching-parity line adds BPL1MOD, the doubled line BPL2MOD (WinUAE
    /// model). Software doubles each row by rewinding with
    /// BPL1MOD = -(row bytes) and advancing with BPL2MOD.
    fn modulo_for_plane(&self, plane: usize, vpos: i32) -> i32 {
        if self.aga() && self.fmode & 0x4000 != 0 {
            return if (i32::from(self.diwstrt >> 8) ^ vpos) & 1 != 0 {
                self.bpl2mod as i32
            } else {
                self.bpl1mod as i32
            };
        }
        if plane & 1 == 0 {
            self.bpl1mod as i32
        } else {
            self.bpl2mod as i32
        }
    }

    fn words_per_row(&self, native_w: usize) -> usize {
        let fallback = native_w / 16;
        let Some((start, stop)) = effective_ddf_window(
            self.agnus_revision,
            self.hires() || self.shres(),
            self.ddfstrt,
            self.ddfstop,
            self.harddis,
        ) else {
            return fallback;
        };
        let unit = self.fetch_unit();
        let start = crate::chipset::agnus::anchor_bitplane_fetch_start(start, unit);
        let blocks = crate::chipset::agnus::bitplane_fetch_blocks(u32::from(stop - start), unit);
        let words = blocks * (unit / self.fetch_cck_per_word()) as usize;
        words.max(1)
    }

    fn has_valid_ddf_window(&self) -> bool {
        let hires_like = self.hires() || self.shres();
        let start = effective_ddf_start_hpos_raw(self.agnus_revision, hires_like, self.ddfstrt);
        let stop = effective_ddf_stop_hpos(self.agnus_revision, hires_like, self.ddfstop);
        (start == 0 && stop == 0)
            || effective_ddf_window(
                self.agnus_revision,
                hires_like,
                self.ddfstrt,
                self.ddfstop,
                self.harddis,
            )
            .is_some()
    }

    /// Horizontal extent, in DIWSTRT/DIWSTOP H coordinates, of the bitplane
    /// data this control actually fetches: the display-data-fetch window
    /// (DDFSTRT/DDFSTOP, rounded to completed fetch units) widened by the
    /// fetched word count at the current resolution. Calibrated so a standard DDF
    /// window ($38/$D0 lo-res, $3C/$D4 hi-res) yields exactly the standard
    /// DIW edges (`STANDARD_DIW_HSTART`..`STANDARD_DIW_HSTOP`): the picture a
    /// stock display fetches lands on the same beam positions as its DIW
    /// window, so presentation centring of a stock display is unchanged.
    ///
    /// Returns `None` when no valid DDF window is programmed, so the caller
    /// falls back to the raw DIW window.
    fn bitplane_content_window_h(&self) -> Option<(i32, i32)> {
        let hires_like = self.hires() || self.shres();
        // Reuse `words_per_row`'s own validity check (same arguments): a
        // valid window guarantees a non-fallback word count below.
        effective_ddf_window(
            self.agnus_revision,
            hires_like,
            self.ddfstrt,
            self.ddfstop,
            self.harddis,
        )?;
        let words = self.words_per_row(0) as i32;
        if words == 0 {
            return None;
        }
        // The displayed shifter origin moves in whole fetch gulps, matching
        // the renderer's placement (see `fetch_origin_native_shift`). This is
        // separate from the DMA slot positions, which start at the
        // revision-masked DDFSTRT comparator value. Each colour clock of DDF
        // shift moves the picture two lo-res H units.
        let gulp = self.fetch_period() as i32;
        let align = |hpos: i32| -> i32 {
            (hpos.div_euclid(gulp) * gulp).max(BITPLANE_DDF_HARD_START as i32)
        };
        let standard_ddf = if hires_like { 0x003C } else { 0x0038 };
        let aligned_start =
            align(effective_ddf_start_hpos(self.agnus_revision, hires_like, self.ddfstrt) as i32);
        let start_h = STANDARD_DIW_HSTART + (aligned_start - align(standard_ddf)) * 2;
        // Fetched H width: one word spans 16 lo-res, 8 hi-res, or 4 super-hi-res
        // H units, so the standard 20/40/80-word row is 320 H units wide.
        let h_units_per_word = if self.shres() {
            4
        } else if self.hires() {
            8
        } else {
            16
        };
        Some((start_h, start_h + words * h_units_per_word))
    }

    fn fetch_start_native_x(&self, diw_h_start: u16, pixel_repeat: usize) -> usize {
        (-self.fetch_origin_native_shift(diw_h_start, pixel_repeat)).max(0) as usize
    }

    fn native_x_offset(&self, diw_h_start: u16, pixel_repeat: usize) -> usize {
        self.fetch_origin_native_shift(diw_h_start, pixel_repeat)
            .max(0) as usize
    }

    fn ham_history_start_native_x(
        &self,
        diw_h_start: u16,
        pixel_repeat: usize,
        native_x_offset: usize,
    ) -> usize {
        let display_phase_native =
            ((diw_h_start as i32 - self.fetch_reference()) * 2) / pixel_repeat as i32;
        let visible_phase = display_phase_native.max(0) as usize;
        native_x_offset.saturating_sub(visible_phase.min(native_x_offset))
    }

    fn holds_final_lowres_fetch_sample_at_diwstop(&self) -> bool {
        if self.hires() || self.shres() || self.fetch_quantum() != 1 {
            return false;
        }
        // DDFSTOP requests bitplane DMA shutdown; the sequencer drops BPRUN
        // at the fetch-unit boundary. A late FMODE=0 low-res row whose
        // completed fetch content ends exactly at DIWSTOP still presents the
        // final latched word for the last DIW sample, without moving the
        // row's fetch origin.
        let ddf_start = effective_ddf_start_hpos(self.agnus_revision, false, self.ddfstrt);
        if ddf_start <= 0x0038 {
            return false;
        }
        self.bitplane_content_window_h()
            .is_some_and(|(_content_start, content_stop)| content_stop == self.diw_h_stop() as i32)
    }

    fn fetch_origin_native_shift(&self, diw_h_start: u16, pixel_repeat: usize) -> i32 {
        let display_native_shift =
            ((diw_h_start as i32 - self.fetch_reference()) * 2) / pixel_repeat as i32;
        let standard_ddf = if self.hires() || self.shres() {
            0x003C
        } else {
            0x0038
        };
        let ddf_native_scale = if self.shres() {
            8
        } else if self.hires() {
            4
        } else {
            2
        };
        // The displayed picture position is quantized to the fetch-period
        // grid (one FMODE gulp per plane). The DMA sequencer itself starts at
        // the revision-masked DDFSTRT comparator value, but Denise's shifter
        // reloads on its own fixed grid, so data fetched off-grid waits for
        // the NEXT reload slot: FMODE=0 placement rounds UP to the gulp grid.
        // Hardware-verified on the arosddf1 A500 ECS photo: the DDFSTRT $3C
        // lo-res picture sits at the $40 reload slot relative to the
        // copper-anchored ruler dashes (both ruler ends agree), one full
        // 8-cck unit right of a floor-aligned placement. On-grid starts
        // ($30/$38 lo-res, $3C hi-res - every previously calibrated case,
        // including the boot-screen insert-disk art) are unchanged by
        // rounding direction. With wide FMODE fetches system software
        // programs DDFSTRT $38 or $3C interchangeably (same 16-cck gulp
        // slot, BPLCON1=0), and its interleaved-bitmap modulos expect
        // exactly the visible row width in the window; the wide grids keep
        // their calibrated floor alignment. The placement grid anchor is the
        // colour-clock origin, not the hard DDF start $18.
        let align = |hpos: i32| -> i32 {
            let gulp = self.fetch_period() as i32;
            if self.fetch_quantum() == 1 {
                // FMODE=0 placement rounds UP to the gulp grid and stays
                // linear below the hardwired start window: a run armed from
                // a DDFSTRT below $18 (surviving SHW latch) places its
                // picture at the raw grid position, left of the standard
                // slots (vAmigaTS Agnus/DDF oldhwstop3/4 A500 photos).
                hpos.div_euclid(gulp) * gulp + if hpos.rem_euclid(gulp) != 0 { gulp } else { 0 }
            } else {
                // Wide-FMODE gulps clamp to the DDF hard start: placement
                // before the first usable fetch position is not visible.
                (hpos.div_euclid(gulp) * gulp).max(BITPLANE_DDF_HARD_START as i32)
            }
        };
        let ddf_native_shift = (align(effective_ddf_start_hpos(
            self.agnus_revision,
            self.hires() || self.shres(),
            self.ddfstrt,
        ) as i32)
            - align(standard_ddf))
            * ddf_native_scale;
        // The render loop measures output pixels from the CLAMPED display
        // window start: the framebuffer cannot show anything left of
        // DIW_HSTART_FB0, so a window programmed further left (extreme
        // left-overscan DIWSTRT) has its off-screen
        // part clipped. The fetched content's position is fixed by DDFSTRT on
        // the beam, not by the window, so the clipped-away window pixels must
        // not push the content to the right. Without this correction the
        // content shifted right by (DIW_HSTART_FB0 - diw_h_start) lores pixels
        // and lost its right edge off the framebuffer.
        let clamped_window_native =
            ((DIW_HSTART_FB0 - diw_h_start as i32).max(0) * 2) / pixel_repeat as i32;
        // Lo-res FMODE=0 placement is linear in DDFSTRT: moving DDFSTRT one
        // 8-cck fetch period earlier moves the picture exactly 16 lo-res
        // pixels left. Real hardware confirms this (vAmigaTS
        // Agnus/DIW/OLDDIW/diw1 A500 photos: the DDF-$30 stripe grid sits
        // exactly 16 lo-res pixels left of the standard $38 grid, and the
        // window-edge staircase steps pair on that grid). An earlier
        // "-1 sample" phase correction here compensated for the display
        // window edge sitting one lo-res pixel too far right, which made a
        // standard DIW overrun the fetched row at the right edge on
        // early-DDF screens; the picture phase itself was never non-linear.
        let mut origin_shift = display_native_shift - ddf_native_shift + clamped_window_native;
        // A hi-res/SHRES FMODE=0 screen that starts DDFSTRT earlier than the
        // standard $3C slot pre-fetches whole word(s) before the display window
        // opens. On real hardware those words are shifted into the left border
        // ahead of DIWSTRT and never appear inside the window; the negative
        // `ddf_native_shift` is exactly that early pre-fetch width in pixels.
        // Snap the picture origin up to the first fully-fetched word so the
        // pre-fetch word is clipped on the left AND the real right edge (the
        // last visible word) stays inside the window instead of being cropped.
        //
        // Confirmed against vAmiga for XSysInfo's hardware panel (hi-res,
        // DDFSTRT=$38, DIWSTRT=$81, BPL1MOD/BPL2MOD=-4): a clean left edge with
        // the >OVERVIEW box keeping its right border. With the negative modulo
        // that pre-fetch word equals the previous row's right-edge word, so
        // without this the right edge both bled into the left column one
        // scanline down and was cropped off the right. Late DDFSTRT (e.g. the
        // Kickstart insert-disk screen's $40, `ddf_native_shift >= 0`), lo-res,
        // and wide-FMODE fetches are untouched.
        if (self.hires() || self.shres()) && self.fetch_quantum() == 1 && ddf_native_shift < 0 {
            origin_shift = origin_shift.max(-ddf_native_shift);
        }
        origin_shift
    }
}

#[derive(Clone, Copy)]
struct ControlSegment {
    x: usize,
    control: ControlState,
}

#[derive(Clone, Copy)]
struct ManualBplSegment {
    line: usize,
    hpos: u32,
    x: i32,
    planes: [u16; 8],
    palette: Palette,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DeniseBitplaneSample {
    idx: u8,
    nplanes: usize,
    active: bool,
}

struct DenisePlannedPlayfieldLine<'a> {
    y: usize,
    x_start: usize,
    x_stop: usize,
    plane_words: &'a [Vec<u16>],
    fetched_pixels: usize,
    carry_words: [Option<u16>; 8],
}

impl<'a> DenisePlannedPlayfieldLine<'a> {
    fn new(
        y: usize,
        x_start: usize,
        x_stop: usize,
        plane_words: &'a [Vec<u16>],
        fetched_pixels: usize,
    ) -> Self {
        Self {
            y,
            x_start,
            x_stop,
            plane_words,
            fetched_pixels,
            carry_words: [None; 8],
        }
    }

    fn with_carry_words(mut self, carry_words: [Option<u16>; 8]) -> Self {
        self.carry_words = carry_words;
        self
    }

    #[cfg(test)]
    fn sample(&self, control: ControlState, native_x: usize) -> DeniseBitplaneSample {
        let nplanes = control.nplanes().min(self.plane_words.len());
        let delays = std::array::from_fn(|plane| control.scroll_for_plane(plane));
        self.sample_prepared(nplanes, &delays, 0, native_x)
    }

    /// `sample` with the control-derived inputs hoisted out: the playfield
    /// pixel loop runs this per output pixel, so the plane count and the
    /// per-plane scroll delays are computed once per control run instead.
    ///
    /// `min_fetch_x` is the first fetched-pixel index that the display window
    /// actually shows (the renderer's `native_x_offset`). When the display
    /// window opens to the right of the DDF-derived fetch origin
    /// (`native_x_offset > 0`, e.g. a narrow DIWSTRT), the bitplane shifter has
    /// already clocked those leading pixels out into the left border by the
    /// time the window opens. BPLCON1 scroll must not pull that shifted-out
    /// pre-fetch back into view: the scrolled-in region at the window's left
    /// edge is background, matching the standard `native_x_offset == 0` case
    /// where `native_x < delay` already yields background. (Kickstart 3.1's
    /// insert-disk screen leaves an uninitialised word at the bitplane base,
    /// which would otherwise scroll a stray fleck into the top-left corner.)
    fn sample_prepared(
        &self,
        nplanes: usize,
        delays: &[usize; 8],
        min_fetch_x: usize,
        native_x: usize,
    ) -> DeniseBitplaneSample {
        self.sample_prepared_with_final_fetch_hold(nplanes, delays, min_fetch_x, native_x, false)
    }

    fn sample_prepared_with_final_fetch_hold(
        &self,
        nplanes: usize,
        delays: &[usize; 8],
        min_fetch_x: usize,
        native_x: usize,
        hold_final_fetch_sample: bool,
    ) -> DeniseBitplaneSample {
        let mut idx = 0u8;
        let mut active = false;
        for (plane, words) in self.plane_words.iter().enumerate().take(nplanes) {
            let delay = delays[plane];
            if native_x < delay {
                active = true;
                if delay <= 16 {
                    let carry_offset = delay - native_x;
                    if let Some(word) = self.carry_words.get(plane).copied().flatten() {
                        let bit = carry_offset - 1;
                        if word & (1 << bit) != 0 {
                            idx |= 1 << plane;
                        }
                    }
                }
                continue;
            }
            let fetch_x = native_x - delay;
            if fetch_x < min_fetch_x {
                active = true;
                continue;
            }
            // The DMA fetch slots decide which word reaches Denise, but the
            // display shifter sees that word as a complete latched sample.
            // Do not expose the first word plane-by-plane at a late DDF edge.
            let fetch_x = if fetch_x >= self.fetched_pixels {
                // A DDFSTOP-delayed final fetch keeps the last latched sample
                // visible for the last DIW position. Apply this per plane so
                // BPLCON1-delayed planes keep their own tap positions.
                if hold_final_fetch_sample
                    && self.fetched_pixels > 0
                    && fetch_x == self.fetched_pixels
                {
                    self.fetched_pixels - 1
                } else {
                    continue;
                }
            } else {
                fetch_x
            };
            active = true;
            let word = words[fetch_x / 16];
            let bit = 15 - (fetch_x & 0x0F);
            if word & (1 << bit) != 0 {
                idx |= 1 << plane;
            }
        }
        DeniseBitplaneSample {
            idx,
            nplanes,
            active,
        }
    }
}

#[derive(Clone, Copy)]
struct DeniseManualBitplaneShifter {
    planes: [u16; 8],
    word_bits: usize,
}

impl DeniseManualBitplaneShifter {
    fn new(planes: [u16; 8], word_bits: usize) -> Self {
        Self { planes, word_bits }
    }

    fn sample(&self, control: ControlState, native_idx: usize) -> Option<DeniseBitplaneSample> {
        let nplanes = control.nplanes().min(self.planes.len());
        let mut idx = 0u8;
        let mut word_active = false;
        for plane in 0..nplanes {
            let delay = control.scroll_for_plane(plane);
            if native_idx < delay {
                word_active = true;
                continue;
            }
            let source_bit = native_idx - delay;
            if source_bit >= self.word_bits {
                continue;
            }
            word_active = true;
            let bit = 15 - source_bit;
            if self.planes[plane] & (1 << bit) != 0 {
                idx |= 1 << plane;
            }
        }
        word_active.then_some(DeniseBitplaneSample {
            idx,
            nplanes,
            active: true,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DenisePlayfieldOutput {
    /// 24-bit 0x00RRGGBB. OCS/ECS resolution keeps its exact 12-bit maths
    /// and expands by nibble duplication at this boundary; the AGA path
    /// resolves natively in 24-bit.
    color: u32,
    color_latch: u16,
    pf_mask: u8,
}

#[cfg_attr(
    not(any(test, debug_assertions, feature = "display-plan-trace")),
    allow(dead_code)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
enum DisplayLinePlanEvent {
    BitplaneDmaFetch {
        hpos: u32,
        word_idx: usize,
        plane: usize,
        word: u16,
    },
    LatchedBitplaneWord {
        hpos: u32,
        word_idx: usize,
        plane: usize,
        word: u16,
    },
    BpldatWrite {
        hpos: u32,
        x: i32,
        plane: usize,
        value: u16,
    },
    ControlChange {
        hpos: u32,
        x: usize,
        control: ControlState,
    },
    PaletteChange {
        hpos: u32,
        x: usize,
        palette: Box<Palette>,
    },
    SpriteSlot {
        hpos: u32,
        sprite: usize,
        hstart: i32,
        data: u16,
        datb: u16,
        attached: bool,
    },
}

#[cfg_attr(
    not(any(test, debug_assertions, feature = "display-plan-trace")),
    allow(dead_code)
)]
impl DisplayLinePlanEvent {
    fn hpos(&self) -> u32 {
        match self {
            Self::BitplaneDmaFetch { hpos, .. }
            | Self::LatchedBitplaneWord { hpos, .. }
            | Self::BpldatWrite { hpos, .. }
            | Self::ControlChange { hpos, .. }
            | Self::PaletteChange { hpos, .. }
            | Self::SpriteSlot { hpos, .. } => *hpos,
        }
    }

    fn beam_order(&self) -> u8 {
        match self {
            Self::SpriteSlot { .. } => 0,
            Self::ControlChange { .. } => 1,
            Self::PaletteChange { .. } => 2,
            Self::BpldatWrite { .. } => 3,
            Self::BitplaneDmaFetch { .. } => 4,
            Self::LatchedBitplaneWord { .. } => 5,
        }
    }
}

// In-progress display-plan trace machinery; not every field has a consumer
// yet.
#[allow(dead_code)]
struct DisplayLinePlan<'a> {
    line: usize,
    beam_y: u32,
    x_start: usize,
    x_stop: usize,
    nplanes: usize,
    dma_planes: usize,
    words_per_row: usize,
    fetch_plans: &'a [LineFetchPlan],
    row_words: &'a [Vec<u16>; 8],
    register_events: &'a [DisplayLinePlanEvent],
    captured_sprite_lines: &'a [CapturedSpriteLine],
    fallback_control: ControlState,
}

#[cfg_attr(
    not(any(test, debug_assertions, feature = "display-plan-trace")),
    allow(dead_code)
)]
impl<'a> DisplayLinePlan<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        line: usize,
        beam_y: u32,
        x_start: usize,
        x_stop: usize,
        nplanes: usize,
        dma_planes: usize,
        words_per_row: usize,
        fetch_plans: &'a [LineFetchPlan],
        row_words: &'a [Vec<u16>; 8],
        register_events: &'a [DisplayLinePlanEvent],
        captured_sprite_lines: &'a [CapturedSpriteLine],
        fallback_control: ControlState,
    ) -> Self {
        Self {
            line,
            beam_y,
            x_start,
            x_stop,
            nplanes,
            dma_planes,
            words_per_row,
            fetch_plans,
            row_words,
            register_events,
            captured_sprite_lines,
            fallback_control,
        }
    }

    fn collect_events(&self) -> Vec<DisplayLinePlanEvent> {
        let mut events = Vec::new();
        for (word_idx, fetch_plan) in self.fetch_plans.iter().enumerate().take(self.words_per_row) {
            let mut recorded_dma_planes = [false; 6];
            for (hpos, plane) in fetch_plan.iter() {
                if plane < self.nplanes
                    && plane < self.row_words.len()
                    && word_idx < self.row_words[plane].len()
                {
                    recorded_dma_planes[plane] = true;
                    events.push(DisplayLinePlanEvent::BitplaneDmaFetch {
                        hpos,
                        word_idx,
                        plane,
                        word: self.row_words[plane][word_idx],
                    });
                }
            }
            for plane in 0..self.dma_planes.min(self.nplanes).min(self.row_words.len()) {
                if !recorded_dma_planes[plane] && word_idx < self.row_words[plane].len() {
                    events.push(DisplayLinePlanEvent::BitplaneDmaFetch {
                        hpos: fetch_plan.word_fetch_hpos.unwrap_or_else(|| {
                            bitplane_fetch_hpos_for_plane(self.fallback_control, word_idx, plane)
                        }),
                        word_idx,
                        plane,
                        word: self.row_words[plane][word_idx],
                    });
                }
            }
            let latched_hpos = fetch_plan
                .latched_plane_sample_hpos()
                .unwrap_or_else(|| bitplane_fetch_hpos(self.fallback_control, word_idx));
            for plane in self.dma_planes..self.nplanes.min(self.row_words.len()) {
                if word_idx < self.row_words[plane].len() {
                    events.push(DisplayLinePlanEvent::LatchedBitplaneWord {
                        hpos: latched_hpos,
                        word_idx,
                        plane,
                        word: self.row_words[plane][word_idx],
                    });
                }
            }
        }
        events.extend_from_slice(self.register_events);
        for line in self.captured_sprite_lines {
            if line.beam_y != self.beam_y as i32 || line.sprite >= 8 {
                continue;
            }
            events.push(DisplayLinePlanEvent::SpriteSlot {
                hpos: SPRITE_DMA_PAIR_CAPTURE_HPOS[line.sprite / 2],
                sprite: line.sprite,
                hstart: line.hstart,
                data: line.data,
                datb: line.datb,
                attached: line.attached,
            });
        }
        events.sort_by_key(|event| (event.hpos(), event.beam_order()));
        events
    }
}

#[cfg_attr(
    not(any(test, debug_assertions, feature = "display-plan-trace")),
    allow(dead_code)
)]
struct DisplayFramePlan {
    register_events_by_line: Vec<Vec<DisplayLinePlanEvent>>,
    line_events: Vec<Vec<DisplayLinePlanEvent>>,
    recorded_lines: [bool; MAX_VISIBLE_LINES],
}

#[cfg_attr(
    not(any(test, debug_assertions, feature = "display-plan-trace")),
    allow(dead_code)
)]
impl DisplayFramePlan {
    fn new() -> Self {
        Self {
            register_events_by_line: vec![Vec::new(); MAX_VISIBLE_LINES],
            line_events: vec![Vec::new(); MAX_VISIBLE_LINES],
            recorded_lines: [false; MAX_VISIBLE_LINES],
        }
    }

    fn register_events_mut(&mut self) -> &mut [Vec<DisplayLinePlanEvent>] {
        &mut self.register_events_by_line
    }

    #[allow(clippy::too_many_arguments)]
    fn record_line(
        &mut self,
        line: usize,
        beam_y: u32,
        x_start: usize,
        x_stop: usize,
        nplanes: usize,
        dma_planes: usize,
        words_per_row: usize,
        fetch_plans: &[LineFetchPlan],
        row_words: &[Vec<u16>; 8],
        captured_sprite_lines: &[CapturedSpriteLine],
        fallback_control: ControlState,
    ) {
        if line >= self.line_events.len() {
            return;
        }
        let events = {
            let plan = DisplayLinePlan::new(
                line,
                beam_y,
                x_start,
                x_stop,
                nplanes,
                dma_planes,
                words_per_row,
                fetch_plans,
                row_words,
                &self.register_events_by_line[line],
                captured_sprite_lines,
                fallback_control,
            );
            plan.collect_events()
        };
        self.line_events[line] = events;
        self.recorded_lines[line] = true;
    }

    fn finish_register_and_sprite_only_lines(
        &mut self,
        captured_sprite_lines: &[CapturedSpriteLine],
        visible_line0: i32,
    ) {
        for line in 0..self.recorded_lines.len() {
            if self.recorded_lines[line] {
                continue;
            }
            self.line_events[line].extend_from_slice(&self.register_events_by_line[line]);
            let beam_y = visible_line0 + line as i32;
            for sprite_line in captured_sprite_lines {
                if sprite_line.beam_y != beam_y || sprite_line.sprite >= 8 {
                    continue;
                }
                self.line_events[line].push(DisplayLinePlanEvent::SpriteSlot {
                    hpos: SPRITE_DMA_PAIR_CAPTURE_HPOS[sprite_line.sprite / 2],
                    sprite: sprite_line.sprite,
                    hstart: sprite_line.hstart,
                    data: sprite_line.data,
                    datb: sprite_line.datb,
                    attached: sprite_line.attached,
                });
            }
            self.line_events[line].sort_by_key(|event| (event.hpos(), event.beam_order()));
        }
    }

    fn log_summary(&self) {
        let mut lines = 0usize;
        let mut dma_fetches = 0usize;
        let mut latched_words = 0usize;
        let mut bpldat_writes = 0usize;
        let mut control_changes = 0usize;
        let mut palette_changes = 0usize;
        let mut sprite_slots = 0usize;
        for events in &self.line_events {
            if !events.is_empty() {
                lines += 1;
            }
            for event in events {
                match event {
                    DisplayLinePlanEvent::BitplaneDmaFetch { .. } => dma_fetches += 1,
                    DisplayLinePlanEvent::LatchedBitplaneWord { .. } => latched_words += 1,
                    DisplayLinePlanEvent::BpldatWrite { .. } => bpldat_writes += 1,
                    DisplayLinePlanEvent::ControlChange { .. } => control_changes += 1,
                    DisplayLinePlanEvent::PaletteChange { .. } => palette_changes += 1,
                    DisplayLinePlanEvent::SpriteSlot { .. } => sprite_slots += 1,
                }
            }
        }
        log::info!(
            "display-plan lines={} dma_fetches={} latched_words={} bpldat_writes={} control_changes={} palette_changes={} sprite_slots={}",
            lines,
            dma_fetches,
            latched_words,
            bpldat_writes,
            control_changes,
            palette_changes,
            sprite_slots
        );
    }
}

/// Snapshot of all chipset register values relevant to rendering this
/// frame. Initialized from direct register writes (Denise/Agnus state)
/// and then optionally overridden by recorded beam events.
struct RenderState {
    agnus_revision: AgnusRevision,
    harddis: bool,
    dmacon: u16,
    bplcon0: u16,
    bplcon1: u16,
    bplcon2: u16,
    bplcon3: u16,
    bplcon4: u16,
    fmode: u16,
    clxcon: u16,
    clxcon2: u16,
    bplpt: [u32; 8],
    bpldat: [u16; 8],
    sprpt: [u32; 8],
    sprpos: [u16; 8],
    sprctl: [u16; 8],
    sprdata: [u16; 8],
    sprdatb: [u16; 8],
    spr_armed: [bool; 8],
    bpl1mod: i16,
    bpl2mod: i16,
    palette: Palette,
    diwstrt: u16,
    diwstop: u16,
    diwhigh: DiwHigh,
    ddfstrt: u16,
    ddfstop: u16,
}

impl RenderState {
    fn from_snapshot(snapshot: RenderRegisterSnapshot) -> Self {
        Self {
            agnus_revision: snapshot.agnus_revision,
            harddis: snapshot.harddis,
            dmacon: snapshot.dmacon,
            bplcon0: snapshot.bplcon0,
            bplcon1: snapshot.bplcon1,
            bplcon2: snapshot.bplcon2,
            bplcon3: snapshot.bplcon3,
            bplcon4: snapshot.bplcon4,
            fmode: snapshot.fmode,
            clxcon: snapshot.clxcon,
            clxcon2: snapshot.clxcon2,
            bplpt: snapshot.bplpt,
            bpldat: snapshot.bpldat,
            sprpt: snapshot.sprpt,
            sprpos: snapshot.sprpos,
            sprctl: snapshot.sprctl,
            sprdata: snapshot.sprdata,
            sprdatb: snapshot.sprdatb,
            spr_armed: snapshot.spr_armed,
            bpl1mod: snapshot.bpl1mod,
            bpl2mod: snapshot.bpl2mod,
            palette: snapshot.palette,
            diwstrt: snapshot.diwstrt,
            diwstop: snapshot.diwstop,
            diwhigh: snapshot.diwhigh,
            ddfstrt: snapshot.ddfstrt,
            ddfstop: snapshot.ddfstop,
        }
    }

    #[cfg(test)]
    fn display_window_y(&self) -> (usize, usize) {
        if display_window_unprogrammed(self.diwstrt, self.diwstop) {
            return (0, FB_HEIGHT);
        }
        let start = self.diw_v_start() as i32;
        let mut stop = self.diw_v_stop() as i32;
        if stop <= start {
            stop += 0x100;
        }
        let top = (start - PAL_VISIBLE_LINE0).max(0) as usize;
        let bottom = (stop - PAL_VISIBLE_LINE0).max(top as i32) as usize;
        (top.min(FB_HEIGHT), bottom.min(FB_HEIGHT))
    }

    #[cfg(test)]
    fn clipped_display_rows_before_frame(&self) -> usize {
        if display_window_unprogrammed(self.diwstrt, self.diwstop) {
            return 0;
        }
        (PAL_VISIBLE_LINE0 - self.diw_v_start() as i32).max(0) as usize
    }

    #[cfg(test)]
    fn display_window_x(&self) -> (usize, usize) {
        if display_window_unprogrammed(self.diwstrt, self.diwstop) {
            return (0, FB_WIDTH);
        }
        let start = self.diw_h_start() as i32;
        let mut stop = self.diw_h_stop() as i32;
        if stop <= start {
            stop += 0x100;
        }
        let left = ((start - DIW_HSTART_FB0).max(0) as usize * 2).min(FB_WIDTH);
        let mut right = ((stop - DIW_HSTART_FB0).max(0) as usize * 2).min(FB_WIDTH);
        if FB_WIDTH.saturating_sub(right) <= 2 {
            right = FB_WIDTH;
        }
        if right > left {
            (left, right)
        } else {
            (left, FB_WIDTH)
        }
    }

    #[cfg(test)]
    fn clipped_display_pixels_before_frame(&self) -> usize {
        if display_window_unprogrammed(self.diwstrt, self.diwstop) {
            return 0;
        }
        ((DIW_HSTART_FB0 - self.diw_h_start() as i32).max(0) as usize * 2).min(FB_WIDTH)
    }

    #[cfg(test)]
    fn diw_v_start(&self) -> u16 {
        self.diwhigh.v_start(self.diwstrt)
    }

    #[cfg(test)]
    fn diw_v_stop(&self) -> u16 {
        self.diwhigh.v_stop(self.diwstop)
    }

    #[cfg(test)]
    fn diw_h_start(&self) -> u16 {
        self.diwhigh.h_start(self.diwstrt)
    }

    #[cfg(test)]
    fn diw_h_stop(&self) -> u16 {
        self.diwhigh.h_stop(self.diwstop)
    }

    #[cfg(test)]
    fn words_per_row(&self, hires: bool, native_w: usize) -> usize {
        let mut control = ControlState::from_render_state(self);
        if hires {
            control.bplcon0 |= 0x8000;
        } else {
            control.bplcon0 &= !0x8000;
        }
        control.words_per_row(native_w)
    }

    #[cfg(test)]
    fn fetch_origin_native_offset(&self, hires: bool, pixel_repeat: usize) -> usize {
        self.fetch_origin_native_shift(hires, pixel_repeat).max(0) as usize
    }

    #[cfg(test)]
    fn fetch_start_native_x(&self, hires: bool, pixel_repeat: usize) -> usize {
        (-self.fetch_origin_native_shift(hires, pixel_repeat)).max(0) as usize
    }

    #[cfg(test)]
    fn fetch_origin_native_shift(&self, hires: bool, pixel_repeat: usize) -> i32 {
        let mut control = ControlState::from_render_state(self);
        if hires {
            control.bplcon0 |= 0x8000;
        } else {
            control.bplcon0 &= !0x8000;
        }
        control.fetch_origin_native_shift(self.diw_h_start(), pixel_repeat)
    }

    #[cfg(test)]
    fn native_x_offset(&self, hires: bool, pixel_repeat: usize) -> usize {
        self.fetch_origin_native_offset(hires, pixel_repeat)
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn apply_render_events(
    state: &mut RenderState,
    events: &[BeamRegisterWrite],
    base_palettes: &mut [Palette],
    palette_segments: &mut [Vec<PaletteSegment>],
    base_controls: &mut [ControlState],
    control_segments: &mut [Vec<ControlSegment>],
    manual_bpl_segments: &mut Vec<ManualBplSegment>,
) {
    apply_render_events_with_visible_line0(
        state,
        events,
        base_palettes,
        palette_segments,
        base_controls,
        control_segments,
        manual_bpl_segments,
        PAL_VISIBLE_LINE0,
    );
}

// Render-event path for builds without display-plan tracing; trace-capable
// builds call the _and_collect_display_plan_events variant directly.
#[cfg_attr(
    any(test, debug_assertions, feature = "display-plan-trace"),
    allow(dead_code)
)]
fn apply_render_events_with_visible_line0(
    state: &mut RenderState,
    events: &[BeamRegisterWrite],
    base_palettes: &mut [Palette],
    palette_segments: &mut [Vec<PaletteSegment>],
    base_controls: &mut [ControlState],
    control_segments: &mut [Vec<ControlSegment>],
    manual_bpl_segments: &mut Vec<ManualBplSegment>,
    visible_line0: i32,
) {
    apply_render_events_and_collect_display_plan_events_with_visible_line0(
        state,
        events,
        base_palettes,
        palette_segments,
        base_controls,
        control_segments,
        manual_bpl_segments,
        visible_line0,
        None,
    );
}

#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(test), allow(dead_code))]
fn apply_render_events_and_collect_display_plan_events(
    state: &mut RenderState,
    events: &[BeamRegisterWrite],
    base_palettes: &mut [Palette],
    palette_segments: &mut [Vec<PaletteSegment>],
    base_controls: &mut [ControlState],
    control_segments: &mut [Vec<ControlSegment>],
    manual_bpl_segments: &mut Vec<ManualBplSegment>,
    display_line_events: Option<&mut [Vec<DisplayLinePlanEvent>]>,
) {
    apply_render_events_and_collect_display_plan_events_with_visible_line0(
        state,
        events,
        base_palettes,
        palette_segments,
        base_controls,
        control_segments,
        manual_bpl_segments,
        PAL_VISIBLE_LINE0,
        display_line_events,
    );
}

#[allow(clippy::too_many_arguments)]
fn apply_render_events_and_collect_display_plan_events_with_visible_line0(
    state: &mut RenderState,
    events: &[BeamRegisterWrite],
    base_palettes: &mut [Palette],
    palette_segments: &mut [Vec<PaletteSegment>],
    base_controls: &mut [ControlState],
    control_segments: &mut [Vec<ControlSegment>],
    manual_bpl_segments: &mut Vec<ManualBplSegment>,
    visible_line0: i32,
    mut display_line_events: Option<&mut [Vec<DisplayLinePlanEvent>]>,
) {
    let mut palette = state.palette;
    let mut control = ControlState::from_render_state(state);
    let mut next_base_line = 0usize;
    let mut next_control_line = 0usize;
    let cpu_palette_beam_timed = cpu_palette_writes_are_beam_timed(events, visible_line0);

    if !cpu_palette_beam_timed {
        for event in events {
            let off = event.offset & 0x01FE;
            if matches!(event.source, BeamWriteSource::Cpu) && matches!(off, 0x180..=0x1BE) {
                let idx = ((off - 0x180) / 2) as usize;
                if idx < 32 {
                    let value = color_register_value(event.value);
                    let (entry, loct) = palette_entry_for_write(state.bplcon3, control.aga(), idx);
                    palette.write_entry(usize::from(entry), loct, value);
                    state.palette.write_entry(usize::from(entry), loct, value);
                }
            }
        }
    }

    for event in events {
        let off = event.offset & 0x01FE;
        if matches!(
            event.source,
            BeamWriteSource::Cpu | BeamWriteSource::CpuCopperIrq
        ) && matches!(off, 0x180..=0x1BE)
            && (event.vpos as i32) <= visible_line0
        {
            let idx = ((off - 0x180) / 2) as usize;
            if idx < 32 {
                let value = color_register_value(event.value);
                let (entry, loct) = palette_entry_for_write(state.bplcon3, control.aga(), idx);
                palette.write_entry(usize::from(entry), loct, value);
                state.palette.write_entry(usize::from(entry), loct, value);
            }
            continue;
        }

        let color_wraps_to_previous_line = matches!(off, 0x180..=0x1BE)
            && color_write_wraps_to_previous_output_line(event.hpos)
            && (event.vpos as i32) > visible_line0;
        let (line, mut beam_x) = if color_wraps_to_previous_line {
            let line = (event.vpos as i32 - visible_line0 - 1) as usize;
            (
                line.min(base_palettes.len().saturating_sub(1)),
                color_write_wrapped_framebuffer_x(event.hpos),
            )
        } else {
            beam_to_framebuffer_pos_with_visible_line0(
                event.vpos,
                event.hpos,
                visible_line0,
                base_palettes.len(),
            )
        };
        // An event from a line above the visible area happened before the
        // first framebuffer line started: it contributes to line 0's start
        // state, not a mid-line change at its horizontal position (which
        // would, e.g., split the first display line of a screen whose
        // copper list programs the display on the line before the window
        // opens, as the boot ROM does).
        let before_visible_lines = (event.vpos as i32) < visible_line0;
        if before_visible_lines {
            beam_x = 0;
        }
        fill_base_palettes(base_palettes, &mut next_base_line, line, palette);
        fill_base_controls(base_controls, &mut next_control_line, line, control);

        if matches!(event.source, BeamWriteSource::Cpu)
            && matches!(off, 0x180..=0x1BE)
            && !cpu_palette_beam_timed
        {
            continue;
        }

        if let 0x180..=0x1BE = off {
            let idx = ((off - 0x180) / 2) as usize;
            let (entry, loct) = palette_entry_for_write(control.bplcon3, control.aga(), idx);
            if idx < 32 {
                palette.write_entry(usize::from(entry), loct, color_register_value(event.value));
            }
            let x = if color_wraps_to_previous_line {
                beam_x
            } else if before_visible_lines {
                0
            } else {
                color_write_framebuffer_x(event.hpos)
            };
            if palette_row_diag().is_some_and(|spec| spec.contains(event.vpos)) {
                log::info!(
                    "palrow v={} h={} x={} line={} source={} color{:02} entry={} loct={} value={:#06X} bplcon3={:#06X}",
                    event.vpos,
                    event.hpos,
                    x,
                    line,
                    beam_write_source_label(event.source),
                    idx,
                    entry,
                    loct,
                    color_register_value(event.value),
                    control.bplcon3,
                );
            }
            push_palette_segment(
                palette_segments,
                line,
                x,
                entry,
                loct,
                color_register_value(event.value),
            );
            // A colour write in the horizontal-blank tail attributes to the
            // previous output row, behind an event of the same beam line
            // that already seeded the next row's base palette (line
            // attribution is not monotonic across write kinds). Patch the
            // written entry into that base so the change still reaches the
            // row it precedes.
            if color_wraps_to_previous_line && next_base_line > line + 1 {
                if let Some(base) = base_palettes.get_mut(line + 1) {
                    base.write_entry(usize::from(entry), loct, color_register_value(event.value));
                }
            }
            if let Some(events_by_line) = display_line_events.as_deref_mut() {
                if line < events_by_line.len() {
                    events_by_line[line].push(DisplayLinePlanEvent::PaletteChange {
                        hpos: event.hpos,
                        x,
                        palette: Box::new(palette),
                    });
                }
            }
        }

        let previous_control = control;
        apply_move(state, off, event.value);
        if matches!(
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
                | 0x1E4
                | 0x1FC
        ) {
            let next_control = ControlState::from_render_state(state);
            if off == 0x10C && previous_control.aga() && !before_visible_lines {
                // Lisa applies BPLCON4's sprite palette-base byte earlier
                // than its bitplane XOR byte. Keep BPLAM on the normal
                // control timeline while letting sprite colour lookup see the
                // new ESPRM/OSPRM byte in its earlier sprite-palette domain.
                let sprite_x = sprite_palette_control_framebuffer_x(event.hpos);
                if sprite_x < beam_x {
                    let mut sprite_control = previous_control;
                    sprite_control.bplcon4 =
                        (previous_control.bplcon4 & 0xFF00) | (next_control.bplcon4 & 0x00FF);
                    push_control_segment(
                        control_segments,
                        line,
                        sprite_x,
                        base_controls[line],
                        sprite_control,
                    );
                }
            }
            control = next_control;
            push_control_segment(control_segments, line, beam_x, base_controls[line], control);
            if matches!(off, 0x102 | 0x108 | 0x10A) {
                if let Some(events_by_line) = display_line_events.as_deref_mut() {
                    if line < events_by_line.len() {
                        events_by_line[line].push(DisplayLinePlanEvent::ControlChange {
                            hpos: event.hpos,
                            x: beam_x,
                            control,
                        });
                    }
                }
            }
        }
        if let 0x110..=0x11A = off {
            if let Some(events_by_line) = display_line_events.as_deref_mut() {
                if line < events_by_line.len() {
                    events_by_line[line].push(DisplayLinePlanEvent::BpldatWrite {
                        hpos: event.hpos,
                        x: if before_visible_lines {
                            0
                        } else {
                            beam_to_framebuffer_x_unclamped(event.hpos)
                        },
                        plane: ((off - 0x110) / 2) as usize,
                        value: event.value,
                    });
                }
            }
        }
        let visible_line = event.vpos as i32 - visible_line0;
        if off == 0x110 && (0..base_palettes.len() as i32).contains(&visible_line) {
            manual_bpl_segments.push(ManualBplSegment {
                line,
                hpos: event.hpos,
                x: beam_to_framebuffer_x_unclamped(event.hpos),
                planes: state.bpldat,
                palette,
            });
        }
    }

    fill_base_palettes(
        base_palettes,
        &mut next_base_line,
        base_palettes.len().saturating_sub(1),
        palette,
    );
    fill_base_controls(
        base_controls,
        &mut next_control_line,
        base_controls.len().saturating_sub(1),
        control,
    );
}

fn cpu_palette_writes_are_beam_timed(events: &[BeamRegisterWrite], visible_line0: i32) -> bool {
    events.iter().any(|event| {
        matches!(event.source, BeamWriteSource::Cpu)
            && matches!(event.offset & 0x01FE, 0x180..=0x1BE)
            && (event.vpos as i32) > visible_line0
    })
}

#[cfg_attr(not(test), allow(dead_code))]
fn beam_to_framebuffer_pos(vpos: u32, hpos: u32) -> (usize, usize) {
    beam_to_framebuffer_pos_with_visible_line0(vpos, hpos, PAL_VISIBLE_LINE0, FB_HEIGHT)
}

fn beam_to_framebuffer_pos_with_visible_line0(
    vpos: u32,
    hpos: u32,
    visible_line0: i32,
    rows: usize,
) -> (usize, usize) {
    let line = (vpos as i32 - visible_line0).max(0) as usize;
    let x = beam_to_framebuffer_x_unclamped(hpos).clamp(0, FB_WIDTH as i32) as usize;
    (line.min(rows.saturating_sub(1)), x)
}

fn beam_to_framebuffer_x_unclamped(hpos: u32) -> i32 {
    (hpos as i32 - COPPER_WAIT_HPOS_FB0) * 4
}

fn color_write_framebuffer_x(hpos: u32) -> usize {
    ((hpos as i32 - COLOR_WRITE_HPOS_FB0) * 4).clamp(0, FB_WIDTH as i32) as usize
}

fn color_write_wraps_to_previous_output_line(hpos: u32) -> bool {
    hpos < DENISE_HBLANK_START_HPOS
}

fn color_write_wrapped_framebuffer_x(hpos: u32) -> usize {
    ((hpos as i32 + COLORCLOCKS_PER_LINE as i32 - COLOR_WRITE_HPOS_FB0) * 4)
        .clamp(0, FB_WIDTH as i32) as usize
}

fn sprite_palette_control_framebuffer_x(hpos: u32) -> usize {
    ((hpos as i32 - SPRITE_PALETTE_CONTROL_HPOS_FB0) * 4).clamp(0, FB_WIDTH as i32) as usize
}

fn fill_base_palettes(
    base_palettes: &mut [Palette],
    next_line: &mut usize,
    end_inclusive: usize,
    palette: Palette,
) {
    let end = end_inclusive.saturating_add(1).min(base_palettes.len());
    if end <= *next_line {
        return;
    }
    for dst in &mut base_palettes[*next_line..end] {
        *dst = palette;
    }
    *next_line = end;
}

fn push_palette_segment(
    palette_segments: &mut [Vec<PaletteSegment>],
    line: usize,
    x: usize,
    entry: u8,
    loct: bool,
    value: u16,
) {
    if line >= palette_segments.len() {
        return;
    }
    let x = x.min(FB_WIDTH);
    // A rewrite of the same entry at the same x collapses to the last value;
    // writes to other entries at the same x are kept as separate diffs.
    if let Some(last) = palette_segments[line].last_mut() {
        if last.x == x && last.entry == entry && last.loct == loct {
            last.value = value;
            return;
        }
    }
    palette_segments[line].push(PaletteSegment {
        x,
        entry,
        loct,
        value,
    });
}

fn fill_base_controls(
    base_controls: &mut [ControlState],
    next_line: &mut usize,
    end_inclusive: usize,
    control: ControlState,
) {
    let end = end_inclusive.saturating_add(1).min(base_controls.len());
    if end <= *next_line {
        return;
    }
    for dst in &mut base_controls[*next_line..end] {
        *dst = control;
    }
    *next_line = end;
}

fn push_control_segment(
    control_segments: &mut [Vec<ControlSegment>],
    line: usize,
    x: usize,
    base_control: ControlState,
    control: ControlState,
) {
    if line >= control_segments.len() {
        return;
    }
    let x = x.min(FB_WIDTH);
    if let Some(last) = control_segments[line].last_mut() {
        if last.control == control {
            return;
        }
        if last.x == x {
            last.control = control;
            return;
        }
    } else if base_control == control {
        return;
    }
    control_segments[line].push(ControlSegment { x, control });
}

fn bitplane_scroll_effect_x(segment_x: usize, visible_x_stop: usize) -> usize {
    if segment_x >= visible_x_stop {
        segment_x
    } else {
        segment_x.saturating_sub(BITPLANE_CONTROL_PIPELINE_FB)
    }
}

fn line_control_at_x(
    base_controls: &[ControlState],
    control_segments: &[Vec<ControlSegment>],
    line: usize,
    x: usize,
) -> ControlState {
    let mut control = base_controls[line];
    for seg in &control_segments[line] {
        if seg.x <= x {
            control = seg.control;
        }
    }
    control
}

/// Rewrite a row's DDFSTRT/DDFSTOP so the register-derived fetch geometry
/// matches the captured sequencer run (origin colour clock + word count).
/// FMODE=0 only. A run origin below the hardware start window ($18) is kept
/// as-is: a DDFSTRT comparator match below $18 starts the run at its raw
/// position when the sequencer's SHW latch survived from the previous line,
/// and the fetched picture sits linearly left of the standard grid
/// (hardware-verified by the vAmigaTS Agnus/DDF oldhwstop3/4 A500 photos).
fn apply_captured_fetch_geometry(control: &mut ControlState, origin: u16, words: usize) {
    if control.fetch_quantum() != 1 || words == 0 {
        return;
    }
    // DDFSTRT 0 doubles as the "no window programmed" sentinel throughout
    // the register-derived paths, so a run genuinely armed at colour clock
    // 0 (DDFSTRT=$00 with a surviving SHW latch) cannot be expressed as a
    // synthesized window; keep the register view. TODO: replace the zero
    // sentinel with an explicit window-valid flag so origin-0 runs can be
    // placed exactly.
    if origin == 0 {
        return;
    }
    let words_per_unit = (8 / control.fetch_cck_per_word() as usize).max(1);
    let units = words.div_ceil(words_per_unit);
    let synth_stop = origin + ((units.saturating_sub(1)) as u16) * 8;
    if control.ddfstrt == origin && control.ddfstop == synth_stop {
        return;
    }
    let native_w = native_frame_width_for_control(*control);
    let current_start = effective_ddf_start_hpos(
        control.agnus_revision,
        control.hires() || control.shres(),
        control.ddfstrt,
    );
    let current_words = if control.has_valid_ddf_window() {
        control.words_per_row(native_w)
    } else {
        0
    };
    if current_start == origin && current_words == words {
        return;
    }
    control.ddfstrt = origin;
    control.ddfstop = synth_stop;
}

fn line_words_per_row(base_control: ControlState, control_segments: &[ControlSegment]) -> usize {
    let base_native_w = native_frame_width_for_control(base_control);
    let mut words = if base_control.has_valid_ddf_window() {
        base_control.words_per_row(base_native_w)
    } else {
        0
    };
    for segment in control_segments {
        let segment_native_w = native_frame_width_for_control(segment.control);
        if segment.control.has_valid_ddf_window() {
            words = words.max(segment.control.words_per_row(segment_native_w));
        }
    }
    words.max(1)
}

fn line_has_valid_ddf_window(
    base_control: ControlState,
    control_segments: &[ControlSegment],
) -> bool {
    base_control.has_valid_ddf_window()
        || control_segments
            .iter()
            .any(|segment| segment.control.has_valid_ddf_window())
}

fn merge_display_window_anchor(
    anchor: &mut Option<usize>,
    control: ControlState,
    line: usize,
    visible_line0: i32,
    run_start: usize,
    run_stop: usize,
) {
    if run_start >= run_stop || !control.display_window_contains_line(line, visible_line0) {
        return;
    }
    let (window_x_start, _) = control.display_window_x();
    if window_x_start >= run_start && window_x_start <= run_stop {
        // The horizontal DIW start comparator fired while this control was
        // active, so it establishes the shifter origin: the playfield
        // painter's fetch-alignment math pairs plan.x_start with the
        // control's DIWSTRT-derived offsets.
        *anchor = Some(anchor.map_or(window_x_start, |a| a.min(window_x_start)));
    }
}

fn line_display_window_bounds(
    base_control: ControlState,
    control_segments: &[ControlSegment],
    line: usize,
    visible_line0: i32,
    h_row: &HWindowRow,
) -> Option<(usize, usize)> {
    // Horizontal reach from the comparator flip-flop model: a window that
    // never closes reaches the framebuffer edge; one that opened on an
    // earlier line starts at 0. Interior closed gaps are handled by the
    // per-pixel window gate and the closed-interval border mask.
    let (env_start, env_stop) = h_row.span();
    if env_start >= env_stop {
        return None;
    }
    // Vertical window: open if any control active on the line has it open.
    let vertical_open = std::iter::once(&base_control)
        .chain(control_segments.iter().map(|seg| &seg.control))
        .any(|control| control.display_window_contains_line(line, visible_line0));
    if !vertical_open {
        return None;
    }
    // The paint start is the shifter-origin anchor of the control whose
    // DIWSTRT comparator fired, not the flip-flop's first open pixel: a
    // mid-line DIWSTRT rewrite moves where the window opens without moving
    // the fetched picture.
    let mut anchor = None;
    let mut control = base_control;
    let mut run_start = 0usize;
    for segment in control_segments {
        let run_stop = segment.x.min(FB_WIDTH);
        merge_display_window_anchor(
            &mut anchor,
            control,
            line,
            visible_line0,
            run_start,
            run_stop,
        );
        control = segment.control;
        run_start = run_stop;
    }
    merge_display_window_anchor(
        &mut anchor,
        control,
        line,
        visible_line0,
        run_start,
        FB_WIDTH,
    );
    // A carried-open row whose comparator never fired inside the frame
    // keeps the base control's DIWSTRT anchor so the picture stays at its
    // fetch-derived position.
    let x_start = match (h_row.comparator_anchor, anchor) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => base_control.display_window_x().0,
    };
    (x_start < env_stop).then_some((x_start, env_stop))
}

fn line_max_display_planes(
    base_control: ControlState,
    control_segments: &[ControlSegment],
) -> usize {
    control_segments
        .iter()
        .map(|segment| segment.control.nplanes())
        .fold(base_control.nplanes(), usize::max)
}

fn line_max_dma_planes(base_control: ControlState, control_segments: &[ControlSegment]) -> usize {
    control_segments
        .iter()
        .map(|segment| segment.control.dma_planes())
        .fold(base_control.dma_planes(), usize::max)
}

fn any_control_matching(
    base_controls: &[ControlState],
    control_segments: &[Vec<ControlSegment>],
    mut predicate: impl FnMut(ControlState) -> bool,
) -> bool {
    base_controls.iter().copied().any(&mut predicate)
        || control_segments
            .iter()
            .flat_map(|segments| segments.iter())
            .any(|segment| predicate(segment.control))
}

fn advance_bitplane_ptrs_for_rows(
    ptrs: &mut [u32; 8],
    rows: usize,
    nplanes: usize,
    words_per_row: usize,
    control: &ControlState,
    first_vpos: i32,
    addr_mask: u32,
) {
    let row_data_bytes = (words_per_row * 2) as i64;
    for row in 0..rows {
        let vpos = first_vpos + row as i32;
        for (p, ptr) in ptrs.iter_mut().enumerate().take(nplanes.min(8)) {
            let delta = row_data_bytes + control.modulo_for_plane(p, vpos) as i64;
            *ptr = ((*ptr as i64).wrapping_add(delta) as u32) & addr_mask;
        }
    }
}

fn replay_bitplane_pointer_events_through_beam(
    events: &[BeamRegisterWrite],
    next_event: &mut usize,
    vpos: u32,
    hpos: u32,
    ptrs: &mut [u32; 8],
) {
    while let Some(event) = events.get(*next_event) {
        if !beam_event_at_or_before_beam(event, vpos, hpos) {
            break;
        }
        apply_bitplane_pointer_write(ptrs, event.offset & 0x01FE, event.value);
        *next_event += 1;
    }
}

fn replay_bitplane_data_events_through_beam(
    events: &[BeamRegisterWrite],
    next_event: &mut usize,
    vpos: u32,
    hpos: u32,
    bpldat: &mut [u16; 8],
) {
    while let Some(event) = events.get(*next_event) {
        if !beam_event_at_or_before_beam(event, vpos, hpos) {
            break;
        }
        apply_bitplane_data_write(bpldat, event.offset & 0x01FE, event.value);
        *next_event += 1;
    }
}

fn beam_event_at_or_before_beam(event: &BeamRegisterWrite, vpos: u32, hpos: u32) -> bool {
    event.vpos < vpos || (event.vpos == vpos && event.hpos <= hpos)
}

fn bitplane_fetch_hpos(control: ControlState, word_idx: usize) -> u32 {
    bitplane_fetch_hpos_for_plane(control, word_idx, 0)
}

fn bitplane_fetch_framebuffer_x(hpos: u32) -> usize {
    ((hpos as i32 * 2 - DIW_HSTART_FB0) * 2).clamp(0, FB_WIDTH as i32) as usize
}

fn apply_bitplane_pointer_write(ptrs: &mut [u32; 8], off: u16, val: u16) {
    if !(0x0E0..=0x0FF).contains(&off) {
        return;
    }
    let idx = ((off - 0x0E0) / 4) as usize;
    if idx >= ptrs.len() {
        return;
    }
    if off & 2 == 0 {
        let cur = ptrs[idx];
        ptrs[idx] = (cur & 0x0000_FFFF) | ((val as u32 & 0x001F) << 16);
    } else {
        let cur = ptrs[idx];
        ptrs[idx] = (cur & 0x00FF_0000) | (val as u32 & 0xFFFE);
    }
}

fn apply_bitplane_data_write(bpldat: &mut [u16; 8], off: u16, val: u16) {
    if !(0x110..=0x11E).contains(&off) {
        return;
    }
    let idx = ((off - 0x110) / 2) as usize;
    if idx < bpldat.len() {
        bpldat[idx] = val;
    }
}

fn seed_manual_bpl_segments_from_latches(
    segments: &mut [ManualBplSegment],
    frame_start_bpldat: [u16; 8],
    render_events: &[BeamRegisterWrite],
    base_controls: &[ControlState],
    control_segments: &[Vec<ControlSegment>],
    captured_bitplane_rows: &[Option<CapturedBitplaneRow>],
    visible_line0: i32,
) {
    if segments.is_empty() {
        return;
    }

    let mut segment_indices_by_beam: HashMap<(usize, u32), Vec<usize>> = HashMap::new();
    for (idx, segment) in segments.iter().enumerate() {
        segment_indices_by_beam
            .entry((segment.line, segment.hpos))
            .or_default()
            .push(idx);
    }
    for indices in segment_indices_by_beam.values_mut() {
        indices.reverse();
    }

    let rows = base_controls.len().min(control_segments.len());
    let mut bpldat = frame_start_bpldat;
    let mut event_idx = 0usize;

    for line in 0..rows {
        let beam_y_i = visible_line0 + line as i32;
        if beam_y_i < 0 {
            continue;
        }
        let beam_y = beam_y_i as u32;
        while let Some(event) = render_events.get(event_idx) {
            if event.vpos >= beam_y {
                break;
            }
            apply_bitplane_data_write(&mut bpldat, event.offset & 0x01FE, event.value);
            event_idx += 1;
        }

        let row_control_segments = &control_segments[line];
        let words_per_row = line_words_per_row(base_controls[line], row_control_segments);
        let dma_planes = line_max_dma_planes(base_controls[line], row_control_segments);
        let mut fetches = Vec::new();
        if dma_planes != 0 && line_has_valid_ddf_window(base_controls[line], row_control_segments) {
            if let Some(captured) = captured_bitplane_rows.get(line).and_then(Option::as_ref) {
                let fetch_plans = line_fetch_plans_for_line(
                    base_controls[line],
                    row_control_segments,
                    words_per_row,
                    dma_planes,
                );
                for (word_idx, plan) in fetch_plans
                    .iter()
                    .enumerate()
                    .take(words_per_row.min(captured.words_per_row))
                {
                    for (hpos, plane) in plan.iter() {
                        if plane < dma_planes.min(8)
                            && plane < captured.nplanes
                            && word_idx < captured.planes[plane].len()
                        {
                            fetches.push((hpos, plane, word_idx));
                        }
                    }
                }
                fetches.sort_unstable();
            }
        }

        let mut fetch_idx = 0usize;
        loop {
            let next_event_hpos = render_events
                .get(event_idx)
                .and_then(|event| (event.vpos == beam_y).then_some(event.hpos));
            let next_fetch = fetches.get(fetch_idx).copied();
            match (next_event_hpos, next_fetch) {
                (Some(event_hpos), Some((fetch_hpos, plane, word_idx)))
                    if fetch_hpos < event_hpos =>
                {
                    bpldat[plane] = captured_bitplane_rows[line]
                        .as_ref()
                        .and_then(|row| row.planes[plane].get(word_idx).copied())
                        .unwrap_or(0);
                    fetch_idx += 1;
                }
                (Some(_), _) => {
                    let event = render_events[event_idx];
                    let off = event.offset & 0x01FE;
                    apply_bitplane_data_write(&mut bpldat, off, event.value);
                    if off == 0x110 {
                        if let Some(indices) = segment_indices_by_beam.get_mut(&(line, event.hpos))
                        {
                            if let Some(segment_idx) = indices.pop() {
                                segments[segment_idx].planes = bpldat;
                            }
                        }
                    }
                    event_idx += 1;
                }
                (None, Some((_fetch_hpos, plane, word_idx))) => {
                    bpldat[plane] = captured_bitplane_rows[line]
                        .as_ref()
                        .and_then(|row| row.planes[plane].get(word_idx).copied())
                        .unwrap_or(0);
                    fetch_idx += 1;
                }
                (None, None) => break,
            }
        }
    }
}

struct TimedChipRam<'a> {
    ram: Cow<'a, [u8]>,
    writes: &'a [BeamChipRamWrite],
    next_write: usize,
}

impl<'a> TimedChipRam<'a> {
    fn new(ram: &'a [u8], writes: &'a [BeamChipRamWrite]) -> Self {
        Self {
            ram: Cow::Borrowed(ram),
            writes,
            next_write: 0,
        }
    }

    fn len(&self) -> usize {
        self.ram.len()
    }

    fn replay_through(&mut self, vpos: u32, hpos: u32) {
        while let Some(write) = self.writes.get(self.next_write) {
            if write.vpos > vpos || (write.vpos == vpos && write.hpos > hpos) {
                break;
            }
            let ram = self.ram.to_mut();
            let offset = write.offset as usize;
            for (idx, byte) in write.bytes().iter().copied().enumerate() {
                if let Some(dst) = ram.get_mut(offset + idx) {
                    *dst = byte;
                }
            }
            self.next_write += 1;
        }
    }

    fn read_word_wrapping(&mut self, addr: u32, vpos: u32, hpos: u32) -> u16 {
        self.replay_through(vpos, hpos);
        read_chip_word_wrapping(&self.ram, addr)
    }
}

fn apply_move(state: &mut RenderState, off: u16, val: u16) {
    match off {
        0x100 => state.bplcon0 = val,
        0x102 => state.bplcon1 = val,
        0x104 => state.bplcon2 = val,
        0x106 => state.bplcon3 = val,
        0x10C => state.bplcon4 = val,
        0x1FC => state.fmode = val & 0xC00F,
        0x108 => state.bpl1mod = (val & 0xFFFE) as i16,
        0x10A => state.bpl2mod = (val & 0xFFFE) as i16,
        0x098 => {
            state.clxcon = val;
            // AGA: a CLXCON write resets CLXCON2.
            state.clxcon2 = 0;
        }
        0x10E => state.clxcon2 = val & 0x0FFF,
        // Writing DIWSTRT/DIWSTOP re-arms the OCS-implicit high bits: an ECS
        // DIWHIGH value only applies until the next DIWSTRT/DIWSTOP write (the
        // Agnus side clears its diwhigh_written flag here for the same reason).
        // Without this, a stale DIWHIGH (e.g. $00FF, pushing V-start off-screen)
        // keeps the display window empty even after the window is reprogrammed.
        0x08E => {
            state.diwstrt = val;
            state.diwhigh = DiwHigh::ocs_implicit();
        }
        0x090 => {
            state.diwstop = val;
            state.diwhigh = DiwHigh::ocs_implicit();
        }
        0x1E4 => state.diwhigh = DiwHigh::ecs_explicit(val),
        0x092 => state.ddfstrt = val,
        0x094 => state.ddfstop = val,
        0x096 => {
            let bits = val & 0x07FF;
            if val & 0x8000 != 0 {
                state.dmacon |= bits;
            } else {
                state.dmacon &= !bits;
            }
        }
        0x110..=0x11E => {
            let idx = ((off - 0x110) / 2) as usize;
            if idx < 8 {
                state.bpldat[idx] = val;
            }
        }
        0x120..=0x13F => {
            let idx = ((off - 0x120) / 4) as usize;
            if idx < 8 {
                if off & 2 == 0 {
                    let cur = state.sprpt[idx];
                    state.sprpt[idx] = (cur & 0x0000_FFFF) | ((val as u32 & 0x001F) << 16);
                } else {
                    let cur = state.sprpt[idx];
                    state.sprpt[idx] = (cur & 0x00FF_0000) | (val as u32 & 0xFFFE);
                }
            }
        }
        0x140..=0x17F => {
            let idx = ((off - 0x140) / 8) as usize;
            let reg = (off - 0x140) & 0x0006;
            if idx < 8 {
                match reg {
                    0x0 => state.sprpos[idx] = val,
                    0x2 => {
                        state.sprctl[idx] = val;
                        state.spr_armed[idx] = false;
                    }
                    0x4 => {
                        state.sprdata[idx] = val;
                        state.spr_armed[idx] = true;
                    }
                    0x6 => state.sprdatb[idx] = val,
                    _ => {}
                }
            }
        }
        0x0E0..=0x0FF => {
            let idx = ((off - 0x0E0) / 4) as usize;
            if idx < 8 {
                if off & 2 == 0 {
                    let cur = state.bplpt[idx];
                    state.bplpt[idx] = (cur & 0x0000_FFFF) | ((val as u32 & 0x001F) << 16);
                } else {
                    let cur = state.bplpt[idx];
                    state.bplpt[idx] = (cur & 0x00FF_0000) | (val as u32 & 0xFFFE);
                }
            }
        }
        0x180..=0x1BE => {
            let idx = ((off - 0x180) / 2) as usize;
            if idx < 32 {
                let aga = matches!(state.agnus_revision, AgnusRevision::AgaAlice);
                let (entry, loct) = palette_entry_for_write(state.bplcon3, aga, idx);
                state
                    .palette
                    .write_entry(usize::from(entry), loct, color_register_value(val));
            }
        }
        _ => {}
    }
}

fn projected_primary_bitplane_pointer(mut ptr: u32, events: &[BeamRegisterWrite]) -> u32 {
    for event in events {
        match event.offset & 0x01FE {
            0x0E0 => {
                ptr = (ptr & 0x0000_FFFF) | ((event.value as u32 & 0x001F) << 16);
            }
            0x0E2 => {
                ptr = (ptr & 0x00FF_0000) | (event.value as u32 & 0xFFFE);
            }
            _ => {}
        }
    }
    ptr
}

fn primary_bitplane_buffer_carries_forward(
    completed_bpl0: u32,
    completed_events: &[BeamRegisterWrite],
    current_bpl0: u32,
    current_events: &[BeamRegisterWrite],
) -> bool {
    let completed = projected_primary_bitplane_pointer(completed_bpl0, completed_events);
    let current = projected_primary_bitplane_pointer(current_bpl0, current_events);
    completed != 0 && completed == current
}

fn is_cpu_copper_irq_palette_event(event: &BeamRegisterWrite) -> bool {
    matches!(event.offset & 0x01FE, 0x180..=0x1BE)
        && matches!(event.source, BeamWriteSource::CpuCopperIrq)
}

fn has_non_irq_beam_palette_events(events: &[BeamRegisterWrite], visible_line0: i32) -> bool {
    events.iter().any(|event| {
        if !matches!(event.offset & 0x01FE, 0x180..=0x1BE) {
            return false;
        }
        match event.source {
            BeamWriteSource::Copper => true,
            BeamWriteSource::Cpu => (event.vpos as i32) > visible_line0,
            BeamWriteSource::CpuCopperIrq => false,
        }
    })
}

#[cfg_attr(not(test), allow(dead_code))]
fn should_replay_bottom_palette_events(
    frame_events: &[BeamRegisterWrite],
    frame_cpu_copper_palette_events: &[BeamRegisterWrite],
    bottom_palette_replay_events: &[BeamRegisterWrite],
    beam_bottom_palette_valid: bool,
) -> bool {
    should_replay_bottom_palette_events_with_visible_line0(
        frame_events,
        frame_cpu_copper_palette_events,
        bottom_palette_replay_events,
        beam_bottom_palette_valid,
        PAL_VISIBLE_LINE0,
    )
}

fn should_replay_bottom_palette_events_with_visible_line0(
    frame_events: &[BeamRegisterWrite],
    frame_cpu_copper_palette_events: &[BeamRegisterWrite],
    bottom_palette_replay_events: &[BeamRegisterWrite],
    beam_bottom_palette_valid: bool,
    visible_line0: i32,
) -> bool {
    if bottom_palette_replay_events.is_empty() || !beam_bottom_palette_valid {
        return false;
    }
    if palette_event_sequences_equivalent(
        bottom_palette_replay_events,
        frame_cpu_copper_palette_events,
    ) {
        return true;
    }
    frame_cpu_copper_palette_events.is_empty()
        && !has_non_irq_beam_palette_events(frame_events, visible_line0)
}

/// Decide whether the copper-interrupt-positioned bottom-palette replay events
/// must be injected into this frame's render events.
///
/// They are needed only in the carry-forward case: the bottom palette was
/// established by a copper interrupt in an earlier frame, and this frame carries
/// no raw CpuCopperIrq palette writes of its own to position from. When the
/// frame does contain those raw writes (the same-frame case), they already carry
/// beam-accurate positions from the cycle-stepped CPU. Injecting the replay
/// events as well would apply each write a second time at the copper interrupt's
/// trigger beam position, which precedes the 68000 interrupt latency before the
/// handler's MOVE executes. That double-application recolors the scanline on
/// which the copper raised the interrupt, one line ahead of where the palette
/// truly changes.
fn should_inject_bottom_palette_replay_events_with_visible_line0(
    frame_events: &[BeamRegisterWrite],
    frame_cpu_copper_palette_events: &[BeamRegisterWrite],
    bottom_palette_replay_events: &[BeamRegisterWrite],
    beam_bottom_palette_valid: bool,
    visible_line0: i32,
) -> bool {
    frame_cpu_copper_palette_events.is_empty()
        && should_replay_bottom_palette_events_with_visible_line0(
            frame_events,
            frame_cpu_copper_palette_events,
            bottom_palette_replay_events,
            beam_bottom_palette_valid,
            visible_line0,
        )
}

#[cfg(test)]
fn should_inject_bottom_palette_replay_events(
    frame_events: &[BeamRegisterWrite],
    frame_cpu_copper_palette_events: &[BeamRegisterWrite],
    bottom_palette_replay_events: &[BeamRegisterWrite],
    beam_bottom_palette_valid: bool,
) -> bool {
    should_inject_bottom_palette_replay_events_with_visible_line0(
        frame_events,
        frame_cpu_copper_palette_events,
        bottom_palette_replay_events,
        beam_bottom_palette_valid,
        PAL_VISIBLE_LINE0,
    )
}

fn append_bottom_palette_replay_events(
    out: &mut Vec<BeamRegisterWrite>,
    events: &[BeamRegisterWrite],
    bottom_palette: Palette,
) {
    for event in events {
        let off = event.offset & 0x01FE;
        let idx = ((off - 0x180) / 2) as usize;
        if idx < bottom_palette.len() {
            let mut replay = *event;
            replay.value = bottom_palette[idx];
            out.push(replay);
        }
    }
}

fn palette_event_sequences_equivalent(a: &[BeamRegisterWrite], b: &[BeamRegisterWrite]) -> bool {
    !a.is_empty()
        && a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            (a.offset & 0x01FE) == (b.offset & 0x01FE)
                && color_register_value(a.value) == color_register_value(b.value)
        })
}

/// Everything `render_from_input` needs to paint a completed frame, owned so
/// it can outlive the `Bus` borrow (and, with the render-thread pipeline, be
/// moved to a worker). It is a snapshot of the just-finished frame: the bus
/// already double-buffers chip RAM and the beam-event/capture logs at the
/// end-of-frame swap, so rendering is a pure function of this bundle.
pub struct RenderInput {
    geometry: FrameGeometry,
    visible_start_vpos: u32,
    palette_split: (Palette, Palette, bool),
    render_base: RenderRegisterSnapshot,
    frame_render_events: Vec<BeamRegisterWrite>,
    current_render_base: RenderRegisterSnapshot,
    current_render_events: Vec<BeamRegisterWrite>,
    bottom_palette_events: Vec<BeamRegisterWrite>,
    top_palette_end: Palette,
    chip_ram: Vec<u8>,
    chip_ram_writes: Vec<BeamChipRamWrite>,
    captured_bitplane_rows: Vec<Option<CapturedBitplaneRow>>,
    captured_sprite_lines: Vec<CapturedSpriteLine>,
    held_sprites: [Option<HeldSpriteLine>; 8],
    sprite_display_enable_x_by_y: Vec<Option<usize>>,
    sprite_dma_observed: bool,
    // Agnus-derived blanking windows, sampled once per frame from the live
    // latches (the helpers below take these instead of borrowing the Bus).
    frame_lines: u32,
    programmable_vertical_blank: Option<(u32, u32)>,
    programmable_horizontal_blank: Option<(u32, u32)>,
    // Scalars only the COPPERLINE_DBG_* side-channels read.
    emulated_seconds: f64,
    emulated_frames: u64,
    /// Debugger layer-isolation masks (bit n = plane n / sprite n drawn),
    /// snapshotted with the frame so the render stays a pure function of
    /// its input. Applied to colour resolution only, never collisions.
    debug_plane_mask: u8,
    debug_sprite_mask: u8,
}

impl RenderInput {
    /// Snapshot the just-finished frame from the bus into an owned bundle.
    pub fn from_bus(bus: &Bus) -> Self {
        Self {
            geometry: bus.frame_geometry(),
            visible_start_vpos: bus.frame_visible_start_vpos(),
            palette_split: bus.frame_palette_split(),
            render_base: bus.frame_render_base(),
            frame_render_events: bus.frame_render_events().to_vec(),
            current_render_base: bus.current_render_base(),
            current_render_events: bus.current_render_events().to_vec(),
            bottom_palette_events: bus.frame_bottom_palette_events().to_vec(),
            top_palette_end: bus.frame_top_palette_end(),
            chip_ram: bus.frame_chip_ram().to_vec(),
            chip_ram_writes: bus.frame_chip_ram_writes().to_vec(),
            captured_bitplane_rows: bus.frame_captured_bitplane_rows().to_vec(),
            captured_sprite_lines: bus.frame_captured_sprite_lines().to_vec(),
            held_sprites: bus.frame_held_sprites(),
            sprite_display_enable_x_by_y: bus.frame_sprite_display_enable_x_by_y().to_vec(),
            sprite_dma_observed: bus.frame_sprite_dma_observed(),
            frame_lines: bus.frame_lines(),
            programmable_vertical_blank: bus.agnus.programmable_vertical_blank(),
            programmable_horizontal_blank: bus.agnus.programmable_horizontal_blank(),
            emulated_seconds: bus.emulated_seconds(),
            emulated_frames: bus.emulated_frames(),
            debug_plane_mask: bus.ui_layer_masks().planes,
            debug_sprite_mask: bus.ui_layer_masks().sprites,
        }
    }

    /// Re-snapshot the just-finished frame into this bundle, reusing the
    /// vector allocations left over from a previous frame (the chip-RAM
    /// copy alone is up to 2 MiB per frame). Field-for-field this must
    /// mirror [`RenderInput::from_bus`].
    pub fn refill_from_bus(&mut self, bus: &Bus) {
        fn copy_into<T: Clone>(dst: &mut Vec<T>, src: &[T]) {
            dst.clear();
            dst.extend_from_slice(src);
        }
        self.geometry = bus.frame_geometry();
        self.visible_start_vpos = bus.frame_visible_start_vpos();
        self.palette_split = bus.frame_palette_split();
        self.render_base = bus.frame_render_base();
        copy_into(&mut self.frame_render_events, bus.frame_render_events());
        self.current_render_base = bus.current_render_base();
        copy_into(&mut self.current_render_events, bus.current_render_events());
        copy_into(
            &mut self.bottom_palette_events,
            bus.frame_bottom_palette_events(),
        );
        self.top_palette_end = bus.frame_top_palette_end();
        copy_into(&mut self.chip_ram, bus.frame_chip_ram());
        copy_into(&mut self.chip_ram_writes, bus.frame_chip_ram_writes());
        copy_into(
            &mut self.captured_bitplane_rows,
            bus.frame_captured_bitplane_rows(),
        );
        copy_into(
            &mut self.captured_sprite_lines,
            bus.frame_captured_sprite_lines(),
        );
        self.held_sprites = bus.frame_held_sprites();
        copy_into(
            &mut self.sprite_display_enable_x_by_y,
            bus.frame_sprite_display_enable_x_by_y(),
        );
        self.sprite_dma_observed = bus.frame_sprite_dma_observed();
        self.frame_lines = bus.frame_lines();
        self.programmable_vertical_blank = bus.agnus.programmable_vertical_blank();
        self.programmable_horizontal_blank = bus.agnus.programmable_horizontal_blank();
        self.emulated_seconds = bus.emulated_seconds();
        self.emulated_frames = bus.emulated_frames();
        self.debug_plane_mask = bus.ui_layer_masks().planes;
        self.debug_sprite_mask = bus.ui_layer_masks().sprites;
    }

    /// Override the layer-isolation masks on an existing snapshot (tests
    /// exercise masking without touching the process-global knobs).
    #[cfg(test)]
    pub(crate) fn with_debug_masks(mut self, plane_mask: u8, sprite_mask: u8) -> Self {
        self.debug_plane_mask = plane_mask;
        self.debug_sprite_mask = sprite_mask;
        self
    }

    pub fn geometry(&self) -> FrameGeometry {
        self.geometry
    }

    pub fn visible_start_vpos(&self) -> u32 {
        self.visible_start_vpos
    }

    pub fn render_base(&self) -> RenderRegisterSnapshot {
        self.render_base
    }

    pub fn emulated_frames(&self) -> u64 {
        self.emulated_frames
    }
}

/// Outputs of `render_from_input`. Render timing is always recorded back on
/// the main thread. `clxdat` is applied only by the synchronous wrapper; the
/// threaded path completes CPU-visible Denise collision state at frame end
/// before the worker can lag behind.
pub struct RenderResult {
    pub timing: VideoRenderFrameTiming,
    pub clxdat: u16,
}

/// Paint the just-finished frame through the synchronous compatibility path.
/// The render itself is a pure function of the owned snapshot
/// (`render_from_input`); this wrapper owns the remaining bus coupling.
pub fn render(bus: &mut Bus, fb: &mut [u32]) {
    // This path re-snapshots the bus every frame; keep one RenderInput per
    // thread so its buffers are reused instead of reallocated each time.
    thread_local! {
        static RENDER_INPUT_SCRATCH: std::cell::RefCell<Option<RenderInput>> =
            const { std::cell::RefCell::new(None) };
    }
    RENDER_INPUT_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        match scratch.as_mut() {
            Some(input) => input.refill_from_bus(bus),
            None => *scratch = Some(RenderInput::from_bus(bus)),
        }
        let input = scratch.as_ref().expect("scratch render input present");
        let result = render_from_input(input, fb);
        bus.denise.or_clxdat(result.clxdat);
        bus.record_video_render_frame(result.timing);
    });
}

/// Render the just-finished frame without the bus feedback of [`render`]:
/// the collision bits and timing stats are dropped, so a debug-driven
/// re-render of the same frame (a layer-isolation toggle while paused)
/// cannot perturb the machine.
pub fn render_display_only(bus: &Bus, fb: &mut [u32]) {
    thread_local! {
        static RENDER_INPUT_SCRATCH: std::cell::RefCell<Option<RenderInput>> =
            const { std::cell::RefCell::new(None) };
    }
    RENDER_INPUT_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        match scratch.as_mut() {
            Some(input) => input.refill_from_bus(bus),
            None => *scratch = Some(RenderInput::from_bus(bus)),
        }
        let input = scratch.as_ref().expect("scratch render input present");
        let _ = render_from_input(input, fb);
    });
}

thread_local! {
    /// The running render's debug layer masks (plane, sprite), captured
    /// from the [`RenderInput`] at [`render_from_input`]'s entry so the
    /// pixel paths need no extra threading and a concurrent render on
    /// another thread (the worker, a parallel test) sees only its own.
    static ACTIVE_DEBUG_MASKS: std::cell::Cell<(u8, u8)> =
        const { std::cell::Cell::new((0xFF, 0xFF)) };
}

fn active_debug_plane_mask() -> u8 {
    ACTIVE_DEBUG_MASKS.with(|masks| masks.get().0)
}

pub(super) fn active_debug_sprite_mask() -> u8 {
    ACTIVE_DEBUG_MASKS.with(|masks| masks.get().1)
}

pub fn render_from_input(input: &RenderInput, fb: &mut [u32]) -> RenderResult {
    let render_started = Instant::now();
    ACTIVE_DEBUG_MASKS.with(|masks| masks.set((input.debug_plane_mask, input.debug_sprite_mask)));
    let mut render_timing = VideoRenderFrameTiming::default();
    let mut state = RenderState::from_snapshot(input.render_base);
    let geometry = input.geometry;
    // Rows rendered this frame: the frame geometry's scan height, bounded
    // by the caller's buffer (legacy fixed-size callers keep the classic
    // field height).
    let rows = geometry.visible_lines.min(fb.len() / FB_WIDTH);
    debug_assert!(fb.len() >= FB_WIDTH * rows);
    let visible_line0 = input.visible_start_vpos as i32;
    let (beam_top_palette, beam_bottom_palette, beam_bottom_palette_valid) = input.palette_split;
    let frame_render_events = input.frame_render_events.as_slice();
    let current_render_base = input.current_render_base;
    let current_render_events = input.current_render_events.as_slice();
    let primary_buffer_carries_forward = primary_bitplane_buffer_carries_forward(
        state.bplpt[0],
        frame_render_events,
        current_render_base.bplpt[0],
        current_render_events,
    );
    let frame_cpu_copper_palette_events: Vec<_> = frame_render_events
        .iter()
        .copied()
        .filter(is_cpu_copper_irq_palette_event)
        .collect();
    let bottom_palette_replay_events = input.bottom_palette_events.as_slice();
    let mut merged_render_events = Vec::new();
    let render_events = if should_inject_bottom_palette_replay_events_with_visible_line0(
        frame_render_events,
        &frame_cpu_copper_palette_events,
        bottom_palette_replay_events,
        beam_bottom_palette_valid,
        visible_line0,
    ) {
        // Carry-forward case: the bottom palette was established by a copper
        // interrupt in an earlier frame, so this frame contains no raw
        // CpuCopperIrq palette writes of its own. Replay the bottom-palette
        // writes at the copper interrupt beam position to reconstruct the
        // palette for this frame.
        merged_render_events.extend_from_slice(frame_render_events);
        append_bottom_palette_replay_events(
            &mut merged_render_events,
            bottom_palette_replay_events,
            beam_bottom_palette,
        );
        merged_render_events.sort_by_key(|event| (event.vpos, event.hpos));
        merged_render_events.as_slice()
    } else if should_replay_bottom_palette_events_with_visible_line0(
        frame_render_events,
        &frame_cpu_copper_palette_events,
        bottom_palette_replay_events,
        beam_bottom_palette_valid,
        visible_line0,
    ) {
        // Same-frame case: this frame already contains the raw CpuCopperIrq
        // palette writes that produced the bottom palette, and those raw writes
        // now carry beam-accurate positions because the CPU is cycle-stepped.
        // Re-injecting the replay events would apply each write a second time at
        // the copper interrupt's trigger position, which precedes the 68000
        // interrupt latency before the handler's MOVE executes, recoloring the
        // scanline on which the copper raised the interrupt one line ahead of
        // where the palette truly changes. Use the raw beam-accurate writes.
        frame_render_events
    } else if frame_cpu_copper_palette_events.is_empty() && primary_buffer_carries_forward {
        let current_cpu_copper_palette_events: Vec<_> = current_render_events
            .iter()
            .copied()
            .filter(is_cpu_copper_irq_palette_event)
            .collect();
        if !current_cpu_copper_palette_events.is_empty() {
            merged_render_events.extend_from_slice(frame_render_events);
            merged_render_events.extend_from_slice(&current_cpu_copper_palette_events);
            merged_render_events.sort_by_key(|event| (event.vpos, event.hpos));
            merged_render_events.as_slice()
        } else {
            frame_render_events
        }
    } else {
        frame_render_events
    };
    if beam_bottom_palette_valid {
        state.palette = if primary_buffer_carries_forward {
            input.top_palette_end
        } else {
            beam_top_palette
        };
    }
    let frame_start_bplpt = state.bplpt;
    let frame_start_bpldat = state.bpldat;
    let frame_start_control = ControlState::from_render_state(&state);
    maybe_log_frame_state(
        input.emulated_seconds,
        input.emulated_frames,
        input.geometry,
        &input.captured_sprite_lines,
        input.sprite_dma_observed,
        &frame_start_control,
        &state,
        &frame_start_bplpt,
        visible_line0,
    );
    let event_started = Instant::now();
    // Seed replay spans from beam-timed SPRx writes or DMA-established held
    // sprites. SPRxDATA latches remain armed across the frame boundary, but
    // they do not emit by themselves when captured DMA is the primary source;
    // a later SPRxPOS write can still reuse that latch after the DMA slot.
    let mut manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &state,
        render_events,
        &input.held_sprites,
        visible_line0,
        rows,
        false,
        input.sprite_dma_observed,
    );
    if input.sprite_dma_observed {
        let dma_seeded_lines = manual_sprite_lines_from_captured_dma_reuse(
            &state,
            render_events,
            &input.captured_sprite_lines,
            visible_line0,
            rows,
        );
        merge_dma_seeded_manual_sprite_lines(&mut manual_sprite_lines, dma_seeded_lines);
    }
    maybe_log_manual_sprite_intervals(
        input.emulated_seconds,
        input.emulated_frames,
        &state,
        render_events,
        &input.held_sprites,
        &manual_sprite_lines,
    );
    let mut base_palettes = vec![state.palette; rows];
    let mut palette_segments = vec![Vec::new(); rows];
    let mut base_controls = vec![frame_start_control; rows];
    let mut control_segments = vec![Vec::new(); rows];
    let mut manual_bpl_segments = Vec::new();
    #[cfg(any(test, debug_assertions, feature = "display-plan-trace"))]
    let mut display_frame_plan =
        crate::envcfg::var_os("COPPERLINE_TRACE_DISPLAY_PLAN").map(|_| DisplayFramePlan::new());
    #[cfg(any(test, debug_assertions, feature = "display-plan-trace"))]
    let display_line_events = display_frame_plan
        .as_mut()
        .map(DisplayFramePlan::register_events_mut);
    #[cfg(any(test, debug_assertions, feature = "display-plan-trace"))]
    apply_render_events_and_collect_display_plan_events_with_visible_line0(
        &mut state,
        render_events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
        visible_line0,
        display_line_events,
    );
    #[cfg(not(any(test, debug_assertions, feature = "display-plan-trace")))]
    apply_render_events_with_visible_line0(
        &mut state,
        render_events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
        visible_line0,
    );
    render_timing.event_nanos = event_started.elapsed().as_nanos();
    render_timing.events = render_events.len() as u64;
    render_timing.control_segments = control_segments
        .iter()
        .map(|segments| segments.len() as u64)
        .sum();
    let frame_ram = input.chip_ram.as_slice();
    let mut ram = TimedChipRam::new(frame_ram, input.chip_ram_writes.as_slice());
    let captured_bitplane_rows = input.captured_bitplane_rows.as_slice();
    // Rows whose DMA capture recorded a fetch run diverging from the
    // register-derived DDF window (the sequencer's missed-stop drains and
    // late starts) carry the run origin and true word count. Synthesize the
    // row's DDFSTRT/DDFSTOP from them so every register-derived fetch and
    // paint derivation (word plans, words-per-row, picture origin) agrees
    // with what the DMA actually did.
    for y in 0..base_controls.len() {
        if let Some(row) = captured_bitplane_rows.get(y).and_then(Option::as_ref) {
            if let Some(origin) = row.fetch_origin_cck {
                apply_captured_fetch_geometry(&mut base_controls[y], origin, row.words_per_row);
                // The row's control segments (mid-row register writes) must
                // agree as well: the sequencer already folded those writes
                // into the captured run, and a segment still carrying the
                // raw DDFSTRT/DDFSTOP values would re-derive a different
                // word count or picture origin for the same row (the
                // oldhwstop band-entry rows, where the copper rewrite lands
                // at the start of the next line's segment list).
                for segment in &mut control_segments[y] {
                    apply_captured_fetch_geometry(&mut segment.control, origin, row.words_per_row);
                }
            }
        }
    }
    let has_captured_bitplane_rows = captured_bitplane_rows.iter().any(Option::is_some);
    let captured_sprite_lines = input.captured_sprite_lines.as_slice();
    let sprite_display_enable_x_by_y = input.sprite_display_enable_x_by_y.as_slice();
    let sprite_dma_observed = input.sprite_dma_observed;
    render_timing.sprite_lines = captured_sprite_lines.len() as u64
        + manual_sprite_lines
            .iter()
            .map(|lines| lines.len() as u64)
            .sum::<u64>();

    let ram_mask = (ram.len() - 1) as u32;

    let mut ptrs: [u32; 8] = frame_start_bplpt;
    let mut next_bitplane_pointer_event = 0usize;
    let mut bpldat = frame_start_bpldat;
    let mut next_bitplane_data_event = 0usize;
    let mut playfield_mask = vec![0u8; FB_WIDTH * rows];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_WIDTH * rows];
    let mut clxdat = 0u16;
    let mut dma_output_start_x_by_line = vec![None; rows];

    let background_started = Instant::now();
    let h_window_rows = compute_h_window_rows(&base_controls, &control_segments, visible_line0);
    fill_background_with_visible_line0(
        fb,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &h_window_rows,
        visible_line0,
    );
    render_timing.background_nanos = background_started.elapsed().as_nanos();

    let any_bitplane_control = any_control_matching(&base_controls, &control_segments, |control| {
        control.nplanes() != 0
    });
    let any_bitplane_dma_control =
        any_control_matching(&base_controls, &control_segments, |control| {
            control.bitplane_dma_enabled() && control.nplanes() != 0
        });

    let playfield_started = Instant::now();
    if (has_captured_bitplane_rows || state.bplpt[0] != 0)
        && any_bitplane_control
        && (has_captured_bitplane_rows || any_bitplane_dma_control)
    {
        let frame_start_x = frame_start_control.display_window_x().0;
        let clipped_rows = frame_start_control.clipped_display_rows_before_frame(visible_line0);
        if clipped_rows != 0 {
            let control = line_control_at_x(&base_controls, &control_segments, 0, frame_start_x);
            replay_bitplane_pointer_events_through_beam(
                render_events,
                &mut next_bitplane_pointer_event,
                visible_line0 as u32,
                bitplane_fetch_hpos(control, 0),
                &mut ptrs,
            );
            // Bitplane DMA only fetched on the clipped lines where it was
            // enabled at the time: replay this frame's BPLCON0/DMACON writes
            // across the span instead of sampling the canvas-row-0 control
            // (mirrors the capture side's advance_display_dma_for_clipped_rows;
            // the CDTV boot screen opens DIW at line 5 but raises BPLCON0 to
            // 6 planes only at line 24).
            let mut line_control = control;
            line_control.bplcon0 = frame_start_control.bplcon0;
            line_control.dmacon = frame_start_control.dmacon;
            let fetch_gate_hpos = u32::from(BITPLANE_DDF_HARD_START);
            let first_line = visible_line0 - clipped_rows as i32;
            let mut event_idx = 0usize;
            for vpos in first_line..visible_line0 {
                while event_idx < render_events.len()
                    && ((render_events[event_idx].vpos as i32) < vpos
                        || (render_events[event_idx].vpos as i32 == vpos
                            && render_events[event_idx].hpos < fetch_gate_hpos))
                {
                    let event = render_events[event_idx];
                    match event.offset {
                        0x096 => {
                            if event.value & 0x8000 != 0 {
                                line_control.dmacon |= event.value & 0x7FFF;
                            } else {
                                line_control.dmacon &= !event.value;
                            }
                        }
                        0x100 => line_control.bplcon0 = event.value,
                        _ => {}
                    }
                    event_idx += 1;
                }
                if !line_control.bitplane_dma_enabled() {
                    continue;
                }
                let nplanes = line_control.dma_planes();
                if nplanes == 0 {
                    continue;
                }
                let native_w = native_frame_width_for_control(line_control);
                let words_per_row = line_control.words_per_row(native_w);
                advance_bitplane_ptrs_for_rows(
                    &mut ptrs,
                    1,
                    nplanes,
                    words_per_row,
                    &line_control,
                    vpos,
                    ram_mask,
                );
            }
        }
        let mut row_words: [Vec<u16>; 8] = std::array::from_fn(|_| Vec::new());
        // COPPERLINE_DBG_EXPORT_PLANES exports each bitplane and a composite
        // color-index image for every rendered frame in the requested
        // emulated-seconds window. It uses the exact per-line plane words the
        // renderer fetches, so it is a ground-truth view of each plane.
        const EXPORT_W: usize = 64 * 16;
        let mut export_planes: Option<Box<[Vec<u8>; 8]>> = None;
        let mut export_index: Option<Vec<u8>> = None;
        if crate::envcfg::flag("COPPERLINE_DBG_EXPORT_PLANES") {
            let after = env_f64("COPPERLINE_DBG_AFTER").unwrap_or(0.0);
            let until = env_f64("COPPERLINE_DBG_UNTIL").unwrap_or(f64::INFINITY);
            let secs = input.emulated_seconds;
            if secs >= after && secs < until {
                export_planes = Some(Box::new(std::array::from_fn(|_| {
                    vec![0u8; EXPORT_W * rows]
                })));
                export_index = Some(vec![0u8; EXPORT_W * rows]);
            }
        }
        // Tracks the last line that actually drew bitplanes, so a line whose
        // predecessor was border (no carried-over shifter data) can suppress
        // the BPLCON1 scroll pulling its leading pre-fetch words into view.
        let mut last_playfield_line: Option<usize> = None;
        let mut previous_playfield_tail_words: [Option<u16>; 8] = [None; 8];
        for y in 0..rows {
            let row_control_segments = &control_segments[y];
            let Some((x_start, x_stop)) = line_display_window_bounds(
                base_controls[y],
                row_control_segments,
                y,
                visible_line0,
                &h_window_rows[y],
            ) else {
                continue;
            };
            render_timing.playfield_pixels = render_timing
                .playfield_pixels
                .saturating_add((x_stop - x_start) as u64);
            let mut palette = base_palettes[y];
            let segments = &palette_segments[y];
            let mut segment_idx = 0usize;
            let control = line_control_at_x(&base_controls, &control_segments, y, x_start);
            let mut control_segment_idx = 0usize;
            let mut pixel_control = base_controls[y];
            let nplanes = line_max_display_planes(control, row_control_segments);
            if nplanes == 0 {
                continue;
            }
            let dma_planes = line_max_dma_planes(control, row_control_segments);
            if !line_has_valid_ddf_window(control, row_control_segments) {
                continue;
            }
            // A captured sequencer run with a comparator-anchored origin is
            // the authority for the row's word count: mid-row DDF rewrites
            // logged as control segments describe windows the sequencer
            // never ran (the oldhwstop band-entry rows), and the register-
            // derived maximum would otherwise disagree with the captured
            // planes and push the row onto the fallback re-fetch path.
            let words_per_row = captured_bitplane_rows
                .get(y)
                .and_then(Option::as_ref)
                .filter(|row| row.fetch_origin_cck.is_some() && row.words_per_row != 0)
                .map(|row| row.words_per_row)
                .unwrap_or_else(|| line_words_per_row(base_controls[y], row_control_segments));
            let beam_y = visible_line0 as u32 + y as u32;
            replay_bitplane_pointer_events_through_beam(
                render_events,
                &mut next_bitplane_pointer_event,
                beam_y,
                bitplane_fetch_hpos(control, 0),
                &mut ptrs,
            );
            let fetched_pixels = words_per_row * 16;
            while segment_idx < segments.len() && segments[segment_idx].x <= x_start {
                segments[segment_idx].apply(&mut palette);
                segment_idx += 1;
            }
            while control_segment_idx < row_control_segments.len()
                && row_control_segments[control_segment_idx].x <= x_start
            {
                pixel_control = row_control_segments[control_segment_idx].control;
                control_segment_idx += 1;
            }
            let captured_row = captured_bitplane_rows
                .get(y)
                .and_then(Option::as_ref)
                .filter(|row| {
                    row.nplanes >= nplanes
                        && row.words_per_row == words_per_row
                        && row.planes[..nplanes]
                            .iter()
                            .all(|plane| plane.len() >= words_per_row)
                });
            if captured_row.is_none() && !control.bitplane_dma_enabled() {
                continue;
            }
            // A DDFSTRT comparator below the hardwired start window ($18)
            // only arms a run when the sequencer's SHW latch survived from
            // the previous line (OCS clears it when a fetch run completes,
            // so such runs happen on alternating lines at most). The DMA
            // capture records which lines actually ran; a line without a
            // captured fetch fetched nothing, so the register-derived
            // window must not synthesize a picture for it (vAmigaTS
            // Agnus/DDF oldhwstop3/4: the black no-run lines between the
            // early-origin runs).
            if captured_row.is_none()
                && control.fetch_quantum() == 1
                && (1..BITPLANE_DDF_HARD_START).contains(&effective_ddf_start_hpos_raw(
                    control.agnus_revision,
                    control.hires() || control.shres(),
                    control.ddfstrt,
                ))
            {
                continue;
            }
            // Denise's playfield output arms on BPL1DAT loads. A mode whose
            // fetch table carries no plane streams at all (overprogrammed
            // hi-res/SHRES BPU) never loads BPL1DAT, so nothing displays -
            // the non-DMA planes' latches are not painted on their own
            // (hardware-verified: the invplanes1 A500 photo shows black for
            // hi-res BPU=7 despite armed BPL5DAT/BPL6DAT latches). Manual
            // BPL1DAT writes still display through the manual-BPL replay.
            if captured_row.is_none() && dma_planes == 0 {
                continue;
            }
            for (plane, words) in row_words.iter_mut().enumerate() {
                words.clear();
                if plane < nplanes {
                    words.resize(words_per_row, 0);
                }
            }
            if let Some(captured) = captured_row {
                for p in 0..nplanes {
                    row_words[p].copy_from_slice(&captured.planes[p][..words_per_row]);
                    if p < dma_planes {
                        ptrs[p] = ptrs[p].wrapping_add((words_per_row * 2) as u32);
                    }
                }
                if nplanes > dma_planes {
                    let line_fetch_plans = line_fetch_plans_for_line(
                        base_controls[y],
                        row_control_segments,
                        words_per_row,
                        dma_planes.min(nplanes),
                    );
                    for word_idx in 0..words_per_row {
                        let fetch_hpos = line_fetch_plans[word_idx]
                            .latched_plane_sample_hpos()
                            .unwrap_or_else(|| bitplane_fetch_hpos(control, word_idx));
                        replay_bitplane_data_events_through_beam(
                            render_events,
                            &mut next_bitplane_data_event,
                            beam_y,
                            fetch_hpos,
                            &mut bpldat,
                        );
                        for p in dma_planes..nplanes {
                            row_words[p][word_idx] = bpldat[p];
                        }
                    }
                    #[cfg(any(test, debug_assertions, feature = "display-plan-trace"))]
                    if let Some(display_frame_plan) = display_frame_plan.as_mut() {
                        display_frame_plan.record_line(
                            y,
                            beam_y,
                            x_start,
                            x_stop,
                            nplanes,
                            dma_planes,
                            words_per_row,
                            &line_fetch_plans,
                            &row_words,
                            captured_sprite_lines,
                            control,
                        );
                    }
                } else {
                    #[cfg(any(test, debug_assertions, feature = "display-plan-trace"))]
                    if let Some(display_frame_plan) = display_frame_plan.as_mut() {
                        let line_fetch_plans = line_fetch_plans_for_line(
                            base_controls[y],
                            row_control_segments,
                            words_per_row,
                            dma_planes.min(nplanes),
                        );
                        display_frame_plan.record_line(
                            y,
                            beam_y,
                            x_start,
                            x_stop,
                            nplanes,
                            dma_planes,
                            words_per_row,
                            &line_fetch_plans,
                            &row_words,
                            captured_sprite_lines,
                            control,
                        );
                    }
                }
            } else {
                let line_fetch_plans = line_fetch_plans_for_line(
                    base_controls[y],
                    row_control_segments,
                    words_per_row,
                    dma_planes.min(nplanes),
                );
                for word_idx in 0..words_per_row {
                    let fetch_plan = &line_fetch_plans[word_idx];
                    let fetch_hpos = fetch_plan.latched_plane_sample_hpos();
                    if nplanes > dma_planes {
                        if let Some(fetch_hpos) = fetch_hpos {
                            replay_bitplane_data_events_through_beam(
                                render_events,
                                &mut next_bitplane_data_event,
                                beam_y,
                                fetch_hpos,
                                &mut bpldat,
                            );
                        }
                    }
                    if fetch_hpos.is_some() {
                        for p in dma_planes..nplanes {
                            row_words[p][word_idx] = bpldat[p];
                        }
                    }
                    for (fetch_hpos, p) in fetch_plan.iter() {
                        replay_bitplane_pointer_events_through_beam(
                            render_events,
                            &mut next_bitplane_pointer_event,
                            beam_y,
                            fetch_hpos,
                            &mut ptrs,
                        );
                        row_words[p][word_idx] =
                            ram.read_word_wrapping(ptrs[p], beam_y, fetch_hpos);
                        ptrs[p] = ptrs[p].wrapping_add(2);
                    }
                }
                #[cfg(any(test, debug_assertions, feature = "display-plan-trace"))]
                if let Some(display_frame_plan) = display_frame_plan.as_mut() {
                    display_frame_plan.record_line(
                        y,
                        beam_y,
                        x_start,
                        x_stop,
                        nplanes,
                        dma_planes,
                        words_per_row,
                        &line_fetch_plans,
                        &row_words,
                        captured_sprite_lines,
                        control,
                    );
                }
            }
            if let (Some(planes), Some(index)) = (export_planes.as_mut(), export_index.as_mut()) {
                for word_i in 0..words_per_row.min(EXPORT_W / 16) {
                    for bit in 0..16 {
                        let col = word_i * 16 + bit;
                        let mask = 1u16 << (15 - bit);
                        let mut idx = 0u8;
                        for p in 0..nplanes.min(8) {
                            if row_words[p][word_i] & mask != 0 {
                                planes[p][y * EXPORT_W + col] = 255;
                                idx |= 1 << p;
                            }
                        }
                        index[y * EXPORT_W + col] = idx;
                    }
                }
            }
            let block_start = last_playfield_line != Some(y.wrapping_sub(1));
            let dma_output_start_x = bitplane_dma_output_start_x(
                base_controls[y],
                row_control_segments,
                x_start,
                words_per_row,
                dma_planes.min(nplanes),
            );
            let fetch_starts_inside_window = {
                let mut display_control = base_controls[y];
                for segment in row_control_segments {
                    if segment.x <= x_start {
                        display_control = segment.control;
                    }
                }
                let pixel_repeat = display_control.framebuffer_pixel_repeat();
                display_control.fetch_start_native_x(display_control.diw_h_start(), pixel_repeat)
                    > 0
            };
            let carry_words = bitplane_carry_words_for_line(
                block_start,
                x_start,
                dma_output_start_x,
                fetch_starts_inside_window,
                previous_playfield_tail_words,
            );
            let line_plan = DenisePlannedPlayfieldLine::new(
                y,
                x_start,
                x_stop,
                &row_words[..nplanes],
                fetched_pixels,
            )
            .with_carry_words(carry_words);
            let bpl_output_start_x = dma_output_start_x.unwrap_or(0);
            dma_output_start_x_by_line[y] = dma_output_start_x;
            last_playfield_line = Some(y);
            render_planned_playfield_line(
                &line_plan,
                fb,
                &mut playfield_mask,
                &mut collision_pixels,
                &mut clxdat,
                palette,
                segments,
                segment_idx,
                pixel_control,
                row_control_segments,
                control_segment_idx,
                base_controls[y].bplcon1,
                block_start,
                bpl_output_start_x,
                &h_window_rows[y],
                visible_line0,
                input.emulated_seconds,
                input.emulated_frames,
            );
            for p in 0..dma_planes {
                let m = control.modulo_for_plane(p, beam_y as i32);
                ptrs[p] = ((ptrs[p] as i64).wrapping_add(m as i64) as u32) & ram_mask;
            }
            previous_playfield_tail_words = std::array::from_fn(|plane| {
                (plane < nplanes)
                    .then(|| row_words[plane].last().copied())
                    .flatten()
            });
        }
        if let (Some(planes), Some(index)) = (export_planes.as_ref(), export_index.as_ref()) {
            let frame = input.emulated_frames;
            let dir = crate::envcfg::var("COPPERLINE_DBG_EXPORT_PLANES_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            match std::fs::create_dir_all(&dir) {
                Ok(()) => {
                    let write_pgm = |name: &str, data: &[u8]| {
                        let mut buf = format!("P5\n{EXPORT_W} {rows}\n255\n").into_bytes();
                        buf.extend_from_slice(data);
                        let path = dir.join(format!("{name}_{frame}.pgm"));
                        if let Err(err) = std::fs::write(&path, buf) {
                            log::warn!("writing plane export {}: {err}", path.display());
                        }
                    };
                    for (p, plane) in planes.iter().enumerate() {
                        write_pgm(&format!("plane{p}"), plane);
                    }
                    // Composite: scale color index to full range for visibility.
                    let scaled: Vec<u8> = index.iter().map(|&i| i.wrapping_mul(8)).collect();
                    write_pgm("composite", &scaled);
                    log::info!(
                        "exported planes for frame {frame} (secs={:.4}) to {}",
                        input.emulated_seconds,
                        dir.display(),
                    );
                }
                Err(err) => log::warn!("creating plane export dir {}: {err}", dir.display()),
            }
        }
    }
    render_timing.playfield_nanos = playfield_started.elapsed().as_nanos();
    maybe_log_frame_pixel_samples(
        "after-playfield",
        input.emulated_seconds,
        input.emulated_frames,
        fb,
        visible_line0,
    );

    let manual_bpl_started = Instant::now();
    seed_manual_bpl_segments_from_latches(
        &mut manual_bpl_segments,
        frame_start_bpldat,
        render_events,
        &base_controls,
        &control_segments,
        captured_bitplane_rows,
        visible_line0,
    );
    render_timing.manual_bpl_segments = manual_bpl_segments.len() as u64;
    render_manual_bpl_segments_with_visible_line0(
        &manual_bpl_segments,
        fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut clxdat,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &dma_output_start_x_by_line,
        visible_line0,
        input.emulated_seconds,
        input.emulated_frames,
    );
    render_timing.manual_bpl_nanos = manual_bpl_started.elapsed().as_nanos();
    maybe_log_frame_pixel_samples(
        "after-manual-bpl",
        input.emulated_seconds,
        input.emulated_frames,
        fb,
        visible_line0,
    );
    let sprite_started = Instant::now();
    clxdat |= render_sprites_with_manual_lines_and_writes(
        &state,
        frame_ram,
        fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: rows,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        sprite_display_enable_x_by_y,
        &playfield_mask,
        &mut collision_pixels,
        captured_sprite_lines,
        sprite_dma_observed,
        Some(&manual_sprite_lines),
        visible_line0,
    );
    render_timing.sprite_nanos = sprite_started.elapsed().as_nanos();
    maybe_log_frame_pixel_samples(
        "after-sprites",
        input.emulated_seconds,
        input.emulated_frames,
        fb,
        visible_line0,
    );
    maybe_log_sprite_pixel_samples(
        input.emulated_seconds,
        input.emulated_frames,
        &state,
        fb,
        input.captured_sprite_lines.as_slice(),
        input.sprite_dma_observed,
        &manual_sprite_lines,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        sprite_display_enable_x_by_y,
        &playfield_mask,
        visible_line0,
    );
    #[cfg(any(test, debug_assertions, feature = "display-plan-trace"))]
    if let Some(display_frame_plan) = display_frame_plan.as_mut() {
        display_frame_plan
            .finish_register_and_sprite_only_lines(captured_sprite_lines, visible_line0);
        display_frame_plan.log_summary();
    }
    // Final authority for the horizontal window: repaint composited
    // content in the flip-flop's closed intervals with the border pixel.
    // The span-based clip paths above only handle a single open span per
    // row; a mid-line close/reopen (or a window that never closes and
    // carries into the next line) produces multiple runs that only this
    // comparator-model mask captures. BRDSPRT (ECS/AGA border sprites)
    // segments keep their composited pixels.
    enforce_h_window_closed_intervals(
        fb,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &h_window_rows,
        visible_line0,
        rows,
    );
    apply_programmable_blanking(
        input.programmable_vertical_blank,
        input.programmable_horizontal_blank,
        fb,
        visible_line0,
        rows,
    );
    blank_rows_past_frame_end(input.frame_lines, fb, visible_line0, rows);
    maybe_log_frame_pixel_samples(
        "final",
        input.emulated_seconds,
        input.emulated_frames,
        fb,
        visible_line0,
    );
    render_timing.total_nanos = render_started.elapsed().as_nanos();
    RenderResult {
        timing: render_timing,
        clxdat,
    }
}

/// Canvas rows whose beam line is at or past the frame wrap do not exist on
/// the scan: the fixed 285-row canvas is taller than a standard PAL/NTSC
/// field actually scans (lines 44..311 on PAL), and a deep-overscan display
/// window otherwise lets the playfield replay keep walking bitplane memory
/// for lines the beam never produced. Regression example: the CDTV
/// extended-ROM boot screen opens DIW to vstop $140 and relies on the frame
/// ending at line 311, which left rows for lines 312..328 showing garbage
/// fetched past the image. Hardware is in vertical blank there; force black.
fn blank_rows_past_frame_end(frame_lines: u32, fb: &mut [u32], visible_line0: i32, rows: usize) {
    const BLANK_RGBA: u32 = 0xFF00_0000;
    let frame_lines = frame_lines as i32;
    let first_blank_row = (frame_lines - visible_line0).clamp(0, rows as i32) as usize;
    for row in fb[first_blank_row * FB_WIDTH..rows * FB_WIDTH].chunks_exact_mut(FB_WIDTH) {
        row.fill(BLANK_RGBA);
    }
}

/// ECS programmable blanking (plan 1.2): force the composite blank windows
/// to black on the finished frame. Vertical blanking follows VBSTRT/VBSTOP
/// under BEAMCON0.VARVBEN; horizontal blanking follows HBSTRT/HBSTOP under
/// BEAMCON0.BLANKEN. The windows are read from the live Agnus latches rather
/// than the beam-ordered replay log: software that runs a programmable scan
/// sets them once at mode switch, so per-frame sampling is sufficient.
///
/// Comparator semantics: blank asserts at the STRT match and clears at the
/// STOP match, so a window with STRT >= STOP wraps through the frame/line
/// origin. The fixed 716x285 canvas shows beam lines visible_line0.. only;
/// blanking outside the canvas is invisible by construction.
fn apply_programmable_blanking(
    programmable_vertical_blank: Option<(u32, u32)>,
    programmable_horizontal_blank: Option<(u32, u32)>,
    fb: &mut [u32],
    visible_line0: i32,
    rows: usize,
) {
    const BLANK_RGBA: u32 = 0xFF00_0000;
    let in_window = |pos: u32, strt: u32, stop: u32| {
        if strt < stop {
            pos >= strt && pos < stop
        } else {
            pos >= strt || pos < stop
        }
    };

    if let Some((strt, stop)) = programmable_vertical_blank {
        for y in 0..rows {
            let vpos = (visible_line0 + y as i32).max(0) as u32;
            if in_window(vpos, strt, stop) {
                fb[y * FB_WIDTH..(y + 1) * FB_WIDTH].fill(BLANK_RGBA);
            }
        }
    }

    if let Some((strt, stop)) = programmable_horizontal_blank {
        // HBSTRT/HBSTOP are in colour clocks; one colour clock spans two
        // lo-res DIW positions, i.e. four hi-res framebuffer pixels.
        let mut blank_cols = [false; FB_WIDTH];
        let mut any = false;
        for (x, blank) in blank_cols.iter_mut().enumerate() {
            let diw_pos = DIW_HSTART_FB0 + (x as i32) / 2;
            let cck = (diw_pos / 2).max(0) as u32;
            if in_window(cck, strt, stop) {
                *blank = true;
                any = true;
            }
        }
        if any {
            for row in fb.chunks_exact_mut(FB_WIDTH) {
                for (x, px) in row.iter_mut().enumerate() {
                    if blank_cols[x] {
                        *px = BLANK_RGBA;
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_planned_playfield_line(
    plan: &DenisePlannedPlayfieldLine<'_>,
    fb: &mut [u32],
    playfield_mask: &mut [u8],
    collision_pixels: &mut [CollisionPixel],
    clxdat: &mut u16,
    mut palette: Palette,
    palette_segments: &[PaletteSegment],
    mut segment_idx: usize,
    mut pixel_control: ControlState,
    control_segments: &[ControlSegment],
    mut control_segment_idx: usize,
    base_scroll_bplcon1: u16,
    suppress_prefetch_scroll_fill: bool,
    bpl_output_start_x: usize,
    h_row: &HWindowRow,
    visible_line0: i32,
    emulated_seconds: f64,
    emulated_frames: u64,
) {
    let mut ham_color = rgb12_to_rgb24(color_rgb12(palette[0]));
    let mut next_ham_native_x = 0usize;
    let mut x = plan.x_start;
    let beam_y = visible_line0 + plan.y as i32;
    let ham_diag = ham_pixel_diag_spec().filter(|spec| {
        spec.beam_y == beam_y && emulated_seconds >= spec.after && emulated_seconds < spec.until
    });
    // The bitplane scroll (BPLCON1) feeds the bitplane shifter, so a scroll
    // write normally applies on the bitplane coordinate
    // ([`BITPLANE_CONTROL_PIPELINE_FB`] left of the copper-x where the write was
    // recorded). Once the normal output position is at or past the DIW right
    // edge, the current scanline has no visible bitplane samples left to retap;
    // keep that write in the normal register domain so it seeds following
    // scanlines without disturbing the current HAM tail.
    let mut scroll_bplcon1 = base_scroll_bplcon1;
    let mut scroll_segment_idx = 0usize;
    // Playfield-collision classification (clxcon_planes_match) depends only on
    // the bitplane index and the constant control fields (CLXCON/CLXCON2, plane
    // count, dual-playfield), so memoize it per control run as a 256-entry
    // table indexed by the sample index. This lifts a per-plane matching loop
    // out of the per-pixel path; the table is rebuilt only when those control
    // inputs change.
    let mut collision_key: Option<(u16, u16, bool, usize)> = None;
    let mut collision_table = [CollisionPixel::default(); 256];
    // The loop runs in segment-bounded chunks: control, scroll, and palette
    // segments apply at pixel boundaries (x stepping by pixel_repeat), so
    // between two boundaries every control-derived value is constant and is
    // hoisted out of the per-pixel work. The per-pixel decisions are
    // unchanged from the previous pixel-at-a-time loop.
    while x < plan.x_stop {
        while control_segment_idx < control_segments.len()
            && control_segments[control_segment_idx].x <= x
        {
            pixel_control = control_segments[control_segment_idx].control;
            control_segment_idx += 1;
        }
        let scroll_visible_x_stop = pixel_control.display_window_x().1;
        while scroll_segment_idx < control_segments.len()
            && bitplane_scroll_effect_x(
                control_segments[scroll_segment_idx].x,
                scroll_visible_x_stop,
            ) <= x
        {
            scroll_bplcon1 = control_segments[scroll_segment_idx].control.bplcon1;
            scroll_segment_idx += 1;
        }
        let mut sample_control = pixel_control;
        sample_control.bplcon1 = scroll_bplcon1;
        while segment_idx < palette_segments.len() && palette_segments[segment_idx].x <= x {
            palette_segments[segment_idx].apply(&mut palette);
            segment_idx += 1;
        }

        // First x at which a pending segment could take effect. Segments
        // land on the pixel boundary at-or-after their x, exactly as the
        // per-pixel loop applied them.
        let mut run_stop = plan.x_stop;
        if control_segment_idx < control_segments.len() {
            run_stop = run_stop.min(control_segments[control_segment_idx].x);
        }
        if scroll_segment_idx < control_segments.len() {
            run_stop = run_stop.min(bitplane_scroll_effect_x(
                control_segments[scroll_segment_idx].x,
                scroll_visible_x_stop,
            ));
        }
        if segment_idx < palette_segments.len() {
            run_stop = run_stop.min(palette_segments[segment_idx].x);
        }
        // The horizontal window flip-flop state is constant between its
        // boundaries; fold them into the chunking so `window_open` can be
        // hoisted out of the pixel loop.
        let window_open = h_row.open_at(x);
        run_stop = run_stop.min(h_row.next_boundary_after(x));

        let pixel_repeat = pixel_control.framebuffer_pixel_repeat();
        let native_per_pixel = pixel_control.native_samples_per_framebuffer_pixel();
        let pixel_diw_h_start = pixel_control.diw_h_start();
        let pixel_fetch_start_native_x =
            pixel_control.fetch_start_native_x(pixel_diw_h_start, pixel_repeat);
        let native_x_offset = pixel_control.native_x_offset(pixel_diw_h_start, pixel_repeat);
        // BPLCON1 scroll fills the window's left edge from the bitplane
        // shifter. On a line whose predecessor also fetched bitplanes the
        // shifter still holds that line's tail, so the scroll-in is real
        // content. At the first line of a bitplane-DMA block (the previous
        // line was border) the shifter has no carried-over data, so the scroll
        // must not pull the leading pre-fetch words (already clocked into the
        // left border by the time the window opens) back into view. Suppress
        // those by treating fetch positions before `native_x_offset` as
        // background only on a block-start line.
        let min_fetch_x = if suppress_prefetch_scroll_fill {
            native_x_offset
        } else {
            0
        };
        let shres = pixel_control.shres();
        let line_visible = pixel_control.display_window_contains_line(plan.y, visible_line0);
        let background_rgb24 = rgb12_to_rgb24(color_rgb12(palette[0]));
        let nplanes = sample_control.nplanes().min(plan.plane_words.len());
        let delays = std::array::from_fn(|plane| sample_control.scroll_for_plane(plane));
        let hold_final_fetch_sample = pixel_control.holds_final_lowres_fetch_sample_at_diwstop();
        let ham_mode = sample_control.hold_and_modify();
        let ham_history_start_native_x = if ham_mode {
            pixel_control.ham_history_start_native_x(
                pixel_diw_h_start,
                pixel_repeat,
                native_x_offset,
            )
        } else {
            0
        };

        let collision_dual = pixel_control.dual_playfield();
        let collision_key_now = (
            pixel_control.clxcon,
            pixel_control.clxcon2,
            collision_dual,
            nplanes,
        );
        if collision_key != Some(collision_key_now) {
            collision_table = std::array::from_fn(|idx| {
                collision_pixel(
                    idx as u8,
                    pixel_control.clxcon,
                    pixel_control.clxcon2,
                    collision_dual,
                )
            });
            collision_key = Some(collision_key_now);
        }

        loop {
            let output_native_x = ((x - plan.x_start) / pixel_repeat) * native_per_pixel;
            let Some(relative_native_x) = output_native_x.checked_sub(pixel_fetch_start_native_x)
            else {
                x += pixel_repeat;
                if x >= run_stop {
                    break;
                }
                continue;
            };
            let native_x = relative_native_x + native_x_offset;
            let visible_sample = line_visible
                && window_open
                && (0..pixel_repeat).any(|dx| {
                    let pixel_x = x + dx;
                    pixel_x < plan.x_stop && pixel_x >= bpl_output_start_x
                });
            // Debugger layer isolation masks the colour-resolution index
            // only; `sample.idx` stays true for collisions and priority.
            let plane_mask = active_debug_plane_mask();
            if ham_mode {
                next_ham_native_x =
                    next_ham_native_x.max(ham_history_start_native_x.min(plan.fetched_pixels));
                let preroll_stop = native_x.min(plan.fetched_pixels);
                while next_ham_native_x < preroll_stop {
                    let skipped =
                        plan.sample_prepared(nplanes, &delays, min_fetch_x, next_ham_native_x);
                    denise_playfield_output(
                        sample_control,
                        palette,
                        skipped.idx & plane_mask,
                        &mut ham_color,
                    );
                    next_ham_native_x += 1;
                }
            }
            if !visible_sample {
                if ham_mode {
                    let sample = plan.sample_prepared(nplanes, &delays, min_fetch_x, native_x);
                    denise_playfield_output(
                        sample_control,
                        palette,
                        sample.idx & plane_mask,
                        &mut ham_color,
                    );
                    next_ham_native_x = next_ham_native_x.max(native_x + 1);
                } else if !shres {
                    ham_color = background_rgb24;
                    next_ham_native_x = next_ham_native_x.max(native_x + 1);
                }
                x += pixel_repeat;
                if x >= run_stop {
                    break;
                }
                continue;
            }
            let (sample, output) = if shres {
                let left = plan.sample_prepared(nplanes, &delays, min_fetch_x, native_x);
                let right = plan.sample_prepared(nplanes, &delays, min_fetch_x, native_x + 1);
                (
                    shres_composite_sample(left, right),
                    denise_shres_playfield_output(
                        palette,
                        left.idx & plane_mask,
                        right.idx & plane_mask,
                        &mut ham_color,
                    ),
                )
            } else {
                let sample = plan.sample_prepared_with_final_fetch_hold(
                    nplanes,
                    &delays,
                    min_fetch_x,
                    native_x,
                    hold_final_fetch_sample,
                );
                let ham_before = ham_color;
                let output = denise_playfield_output(
                    pixel_control,
                    palette,
                    sample.idx & plane_mask,
                    &mut ham_color,
                );
                if let Some(spec) = ham_diag {
                    if x >= spec.x_start
                        && x < spec.x_stop
                        && (x - spec.x_start).is_multiple_of(spec.step)
                    {
                        log::info!(
                            "ham-pixel secs={emulated_seconds:.4} frame={emulated_frames} y={beam_y} x={x} native={native_x} rel={} idx={:#04X} active={} ham={} before={:#08X} after={:#08X} color={:#08X} latch={:#06X} nplanes={} fetched={} delays={:?} bplcon0={:#06X} bplcon1={:#06X} diw={:#06X}/{:#06X} ddf={:#06X}/{:#06X} win={}..{}",
                            relative_native_x,
                            sample.idx,
                            u8::from(sample.active),
                            u8::from(ham_mode),
                            ham_before & 0x00FF_FFFF,
                            ham_color & 0x00FF_FFFF,
                            output.color & 0x00FF_FFFF,
                            output.color_latch,
                            nplanes,
                            plan.fetched_pixels,
                            delays,
                            pixel_control.bplcon0,
                            sample_control.bplcon1,
                            pixel_control.diwstrt,
                            pixel_control.diwstop,
                            pixel_control.ddfstrt,
                            pixel_control.ddfstop,
                            plan.x_start,
                            plan.x_stop,
                        );
                    }
                }
                (sample, output)
            };
            if ham_mode || !shres {
                next_ham_native_x = next_ham_native_x.max(native_x + 1);
            }
            // Collision classification is identical for every framebuffer
            // pixel of this native sample, so look it up once. CLXDAT only
            // accumulates set bits, so ORing it here (rather than once per
            // written pixel) is equivalent: a visible sample writes at least
            // one in-window pixel.
            let collision = collision_table[sample.idx as usize];
            let pf_mask = u8::from(collision.pf1) | (u8::from(collision.pf2) << 1);
            *clxdat |= collision.clxdat_bits();
            for dx in 0..pixel_repeat {
                let pixel_x = x + dx;
                if pixel_x >= plan.x_stop || !window_open {
                    continue;
                }
                let fb_idx = plan.y * FB_WIDTH + pixel_x;
                if pf_mask != 0 {
                    playfield_mask[fb_idx] = pf_mask;
                }
                collision_pixels[fb_idx] = collision;
                let transparent =
                    pixel_control.genlock_transparent(output.color_latch, Some(sample), false);
                fb[fb_idx] = rgb24_to_rgba8_alpha(output.color, !transparent);
            }
            x += pixel_repeat;
            if x >= run_stop {
                break;
            }
        }
    }
}

#[cfg(test)]
#[cfg(test)]
fn sprite_pointer_refreshes_from_mask(mask: [bool; 8]) -> [SpritePointerRefresh; 8] {
    std::array::from_fn(|idx| SpritePointerRefresh {
        refreshed: mask[idx],
        ptr: 0,
        beam: None,
    })
}

#[cfg(test)]
fn left_edge_blank_pixels(control: ControlState) -> usize {
    let delay = control.pf1_scroll();
    if control.hires() || control.shres() {
        delay
    } else {
        delay * 2
    }
}

#[derive(Clone, Copy, Default)]
struct CollisionPixel {
    pf1: bool,
    pf2: bool,
    pf1_match: bool,
    pf2_match: bool,
}

impl CollisionPixel {
    fn clxdat_bits(self) -> u16 {
        u16::from(self.pf1_match && self.pf2_match)
    }
}

fn collision_pixel(idx: u8, clxcon: u16, clxcon2: u16, dual_playfield: bool) -> CollisionPixel {
    let even_match = clxcon_planes_match(idx, clxcon, clxcon2, 1);
    let odd_match_raw = clxcon_planes_match(idx, clxcon, clxcon2, 0);
    let odd_match = odd_match_raw && (dual_playfield || even_match);
    CollisionPixel {
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

fn clxcon_planes_match(idx: u8, clxcon: u16, clxcon2: u16, first_plane: usize) -> bool {
    let mut matches = true;
    // Every CLXCON/CLXCON2-enabled plane participates in the match, not just
    // the planes the display currently fetches: a plane enabled beyond the BPU
    // count reads as 0 and still gates the match (vAmiga checkS2PCollisions
    // compares `(dBuffer & enbp) == (mvbp & enbp)` over all six/eight planes).
    // Regression: Denise/Sprites/collision/sprcoll* set CLXCON with match bits
    // for absent planes over a low-plane-count playfield.
    for plane in (first_plane..8).step_by(2) {
        // Planes 1-6 take their enable/match bits from CLXCON; the AGA
        // planes 7-8 from CLXCON2 (ENBP7/ENBP8 in bits 6-7, MVBP7/MVBP8 in
        // bits 0-1).
        let (enabled, desired) = if plane < 6 {
            (clxcon & (1 << (6 + plane)) != 0, clxcon & (1 << plane) != 0)
        } else {
            (
                clxcon2 & (1 << plane) != 0,
                clxcon2 & (1 << (plane - 6)) != 0,
            )
        };
        if !enabled {
            continue;
        }
        let actual = idx & (1 << plane) != 0;
        matches &= desired == actual;
    }
    matches
}

fn record_generated_playfield_collision_pixel(
    playfield_mask: &mut [u8],
    collision_pixels: &mut [CollisionPixel],
    clxdat: &mut u16,
    fb_idx: usize,
    sample: DeniseBitplaneSample,
    control: ControlState,
) {
    let collision = collision_pixel(
        sample.idx,
        control.clxcon,
        control.clxcon2,
        control.dual_playfield(),
    );
    let pf_mask = u8::from(collision.pf1) | (u8::from(collision.pf2) << 1);
    if pf_mask != 0 {
        playfield_mask[fb_idx] = pf_mask;
    }
    collision_pixels[fb_idx] = collision;
    *clxdat |= collision.clxdat_bits();
}

#[cfg_attr(not(test), allow(dead_code))]
fn render_manual_bpl_segments(
    segments: &[ManualBplSegment],
    fb: &mut [u32],
    playfield_mask: &mut [u8],
    collision_pixels: &mut [CollisionPixel],
    clxdat: &mut u16,
    base_palettes: &[Palette],
    palette_segments: &[Vec<PaletteSegment>],
    base_controls: &[ControlState],
    control_segments: &[Vec<ControlSegment>],
) {
    let dma_output_start_x_by_line = vec![None; base_controls.len()];
    render_manual_bpl_segments_with_visible_line0(
        segments,
        fb,
        playfield_mask,
        collision_pixels,
        clxdat,
        base_palettes,
        palette_segments,
        base_controls,
        control_segments,
        &dma_output_start_x_by_line,
        PAL_VISIBLE_LINE0,
        0.0,
        0,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_manual_bpl_segments_with_visible_line0(
    segments: &[ManualBplSegment],
    fb: &mut [u32],
    playfield_mask: &mut [u8],
    collision_pixels: &mut [CollisionPixel],
    clxdat: &mut u16,
    base_palettes: &[Palette],
    palette_segments: &[Vec<PaletteSegment>],
    base_controls: &[ControlState],
    control_segments: &[Vec<ControlSegment>],
    dma_output_start_x_by_line: &[Option<usize>],
    visible_line0: i32,
    emulated_seconds: f64,
    emulated_frames: u64,
) {
    if segments.is_empty() {
        return;
    }
    let mut ham_select_pixels = vec![0u8; fb.len()];
    for seg in segments {
        if seg.line >= base_controls.len() {
            continue;
        }
        let mut ham_color = manual_bpl_ham_seed_color(
            seg,
            fb,
            base_palettes,
            palette_segments,
            base_controls,
            control_segments,
        );
        let mut ham_select = manual_bpl_ham_seed_select(seg, &ham_select_pixels);
        let beam_y = visible_line0 + seg.line as i32;
        let diag = manual_bpl_pixel_diag_spec().filter(|spec| {
            spec.beam_y == beam_y && emulated_seconds >= spec.after && emulated_seconds < spec.until
        });
        draw_manual_bpl_word(
            seg,
            fb,
            playfield_mask,
            collision_pixels,
            clxdat,
            base_palettes,
            palette_segments,
            base_controls,
            control_segments,
            dma_output_start_x_by_line,
            &mut ham_color,
            &mut ham_select,
            &mut ham_select_pixels,
            visible_line0,
            emulated_seconds,
            emulated_frames,
            diag,
        );
    }
}

fn manual_bpl_ham_seed_color(
    seg: &ManualBplSegment,
    fb: &[u32],
    base_palettes: &[Palette],
    palette_segments: &[Vec<PaletteSegment>],
    base_controls: &[ControlState],
    control_segments: &[Vec<ControlSegment>],
) -> u32 {
    if seg.line >= base_controls.len() {
        return rgb12_to_rgb24(color_rgb12(seg.palette[0]));
    }
    let sample_x = seg.x.clamp(0, FB_WIDTH.saturating_sub(1) as i32) as usize;
    let control = control_at_x(
        base_controls[seg.line],
        &control_segments[seg.line],
        sample_x,
    );
    if !control.hold_and_modify() {
        return rgb12_to_rgb24(color_rgb12(seg.palette[0]));
    }
    if seg.x <= 0 {
        return rgb12_to_rgb24(color_rgb12(
            palette_at_x(base_palettes[seg.line], &palette_segments[seg.line], 0)[0],
        ));
    }
    let previous_x = (seg.x - 1).min(FB_WIDTH.saturating_sub(1) as i32) as usize;
    rgba8_to_rgb24(fb[seg.line * FB_WIDTH + previous_x])
}

fn manual_bpl_ham_seed_select(seg: &ManualBplSegment, ham_select_pixels: &[u8]) -> u8 {
    if (seg.line + 1) * FB_WIDTH > ham_select_pixels.len() || seg.x <= 0 {
        return 0;
    }
    let previous_x = (seg.x - 1).min(FB_WIDTH.saturating_sub(1) as i32) as usize;
    ham_select_pixels[seg.line * FB_WIDTH + previous_x]
}

fn draw_manual_bpl_word(
    seg: &ManualBplSegment,
    fb: &mut [u32],
    playfield_mask: &mut [u8],
    collision_pixels: &mut [CollisionPixel],
    clxdat: &mut u16,
    base_palettes: &[Palette],
    palette_segments: &[Vec<PaletteSegment>],
    base_controls: &[ControlState],
    control_segments: &[Vec<ControlSegment>],
    dma_output_start_x_by_line: &[Option<usize>],
    ham_color: &mut u32,
    ham_select: &mut u8,
    ham_select_pixels: &mut [u8],
    visible_line0: i32,
    emulated_seconds: f64,
    emulated_frames: u64,
    diag: Option<PixelDiagSpec>,
) {
    const MANUAL_BPL_WORD_BITS: usize = 16;
    const MAX_BPLCON1_DELAY: usize = 15;
    const MAX_MANUAL_BPL_NATIVE_SAMPLES: usize = MANUAL_BPL_WORD_BITS + MAX_BPLCON1_DELAY;

    let shifter = DeniseManualBitplaneShifter::new(seg.planes, MANUAL_BPL_WORD_BITS);
    let dma_clip_x = manual_bpl_dma_clip_x(
        seg,
        base_controls[seg.line],
        &control_segments[seg.line],
        dma_output_start_x_by_line.get(seg.line).copied().flatten(),
    );
    let mut x_cursor = seg.x;
    let mut native_idx = 0usize;
    while native_idx < MAX_MANUAL_BPL_NATIVE_SAMPLES {
        let source_sample_x = x_cursor.clamp(0, FB_WIDTH.saturating_sub(1) as i32) as usize;
        let source_control = control_at_x(
            base_controls[seg.line],
            &control_segments[seg.line],
            source_sample_x,
        );
        let pixel_repeat = source_control.framebuffer_pixel_repeat();
        let native_step = source_control.native_samples_per_framebuffer_pixel();
        let Some(left_sample) = shifter.sample(source_control, native_idx) else {
            x_cursor += pixel_repeat as i32;
            native_idx += native_step;
            continue;
        };
        let sample = if source_control.shres() {
            let right_sample = shifter
                .sample(source_control, native_idx + 1)
                .unwrap_or_default();
            shres_composite_sample(left_sample, right_sample)
        } else {
            left_sample
        };
        let source_palette = palette_at_x(
            base_palettes[seg.line],
            &palette_segments[seg.line],
            source_sample_x,
        );
        let visible_sample = (0..pixel_repeat).any(|dx| {
            let x = x_cursor + dx as i32;
            if !(0..FB_WIDTH as i32).contains(&x) {
                return false;
            }
            let x = x as usize;
            if dma_clip_x.is_some_and(|dma_x| x >= dma_x) {
                return false;
            }
            let pixel_control =
                control_at_x(base_controls[seg.line], &control_segments[seg.line], x);
            let (window_x_start, window_x_stop) = pixel_control.display_window_x();
            pixel_control.display_window_contains_line(seg.line, visible_line0)
                && x >= window_x_start
                && x < window_x_stop
        });
        if !visible_sample {
            if !source_control.shres() {
                *ham_color = rgb12_to_rgb24(color_rgb12(source_palette[0]));
                *ham_select = 0;
            }
            x_cursor += pixel_repeat as i32;
            native_idx += native_step;
            continue;
        }
        let ham_before = *ham_color;
        // Debugger layer isolation masks the colour-resolution index only;
        // `sample.idx` stays true for the collision recording below.
        let plane_mask = active_debug_plane_mask();
        let masked_idx = sample.idx & plane_mask;
        let output_idx = if source_control.hold_and_modify() {
            *ham_select
        } else {
            masked_idx
        };
        let source_output = if source_control.shres() {
            let right_sample = shifter
                .sample(source_control, native_idx + 1)
                .unwrap_or_default();
            denise_shres_playfield_output(
                source_palette,
                left_sample.idx & plane_mask,
                right_sample.idx & plane_mask,
                ham_color,
            )
        } else {
            let output =
                denise_playfield_output(source_control, source_palette, output_idx, ham_color);
            *ham_select = masked_idx;
            output
        };
        for dx in 0..pixel_repeat {
            let x = x_cursor + dx as i32;
            if !(0..FB_WIDTH as i32).contains(&x) {
                continue;
            }
            let x = x as usize;
            if dma_clip_x.is_some_and(|dma_x| x >= dma_x) {
                continue;
            }
            let pixel_control =
                control_at_x(base_controls[seg.line], &control_segments[seg.line], x);
            let (window_x_start, window_x_stop) = pixel_control.display_window_x();
            if !pixel_control.display_window_contains_line(seg.line, visible_line0)
                || x < window_x_start
                || x >= window_x_stop
            {
                continue;
            }
            let fb_idx = seg.line * FB_WIDTH + x;
            let pixel_palette =
                palette_at_x(base_palettes[seg.line], &palette_segments[seg.line], x);
            record_generated_playfield_collision_pixel(
                playfield_mask,
                collision_pixels,
                clxdat,
                fb_idx,
                sample,
                pixel_control,
            );
            if !source_control.shres() {
                ham_select_pixels[fb_idx] = masked_idx;
            }
            if let Some(spec) = diag {
                if x >= spec.x_start
                    && x < spec.x_stop
                    && (x - spec.x_start).is_multiple_of(spec.step)
                {
                    let beam_y = visible_line0 + seg.line as i32;
                    log::info!(
                        "manual-bpl-pixel secs={emulated_seconds:.4} frame={emulated_frames} y={beam_y} x={x} seg_x={} native={} idx={:#04X} output_idx={:#04X} ham_before={:#08X} ham_after={:#08X} color={:#08X} latch={:#06X} bplcon0={:#06X} bplcon1={:#06X} win={:?}",
                        seg.x,
                        native_idx,
                        sample.idx,
                        output_idx,
                        ham_before & 0x00FF_FFFF,
                        *ham_color & 0x00FF_FFFF,
                        source_output.color & 0x00FF_FFFF,
                        source_output.color_latch,
                        source_control.bplcon0,
                        source_control.bplcon1,
                        source_control.display_window_x(),
                    );
                }
            }
            let (pixel_color, pixel_color_latch) =
                if source_control.shres() || source_control.hold_and_modify() {
                    (source_output.color, source_output.color_latch)
                } else if source_control.aga() {
                    // Re-resolve against the palette at this x so mid-line
                    // palette diffs land, mirroring the pre-AGA arms below.
                    let mut pixel_ham = *ham_color;
                    let output = denise_aga_playfield_output(
                        source_control,
                        pixel_palette,
                        masked_idx,
                        &mut pixel_ham,
                    );
                    (output.color, output.color_latch)
                } else if masked_idx == 0 {
                    (
                        rgb12_to_rgb24(color_rgb12(pixel_palette[0])),
                        pixel_palette[0],
                    )
                } else if source_control.dual_playfield() {
                    let (_, color_idx) = dual_playfield_pixel(masked_idx, source_control);
                    let color_latch = pixel_palette.get(color_idx).copied().unwrap_or(0);
                    (rgb12_to_rgb24(color_rgb12(color_latch)), color_latch)
                } else {
                    (
                        rgb12_to_rgb24(palette_index_to_rgb12(
                            pixel_palette,
                            masked_idx,
                            source_control.extra_half_brite(),
                        )),
                        pixel_palette[(masked_idx as usize) & 0x1F],
                    )
                };
            *ham_color = pixel_color;
            let transparent =
                pixel_control.genlock_transparent(pixel_color_latch, Some(sample), false);
            fb[fb_idx] = rgb24_to_rgba8_alpha(pixel_color, !transparent);
        }
        x_cursor += pixel_repeat as i32;
        native_idx += native_step;
    }
}

fn read_chip_word_wrapping(ram: &[u8], addr: u32) -> u16 {
    let mask = ram.len() - 1;
    let a = addr as usize & mask;
    u16::from_be_bytes([ram[a], ram[(a + 1) & mask]])
}

fn ddf_register_mask(revision: AgnusRevision) -> u16 {
    if matches!(revision, AgnusRevision::Ocs) {
        0x00FC
    } else {
        0x00FE
    }
}

fn effective_ddf_start_hpos_raw(revision: AgnusRevision, hires: bool, raw: u16) -> u16 {
    let _ = hires;
    raw & ddf_register_mask(revision)
}

fn effective_ddf_stop_hpos(revision: AgnusRevision, hires: bool, raw: u16) -> u16 {
    let _ = hires;
    raw & ddf_register_mask(revision)
}

fn effective_ddf_start_hpos(revision: AgnusRevision, hires: bool, raw: u16) -> u16 {
    let start = effective_ddf_start_hpos_raw(revision, hires, raw);
    // A DDFSTRT below the hardwired start window ($18) is NOT clamped: the
    // comparator match position anchors the fetch grid at its raw value
    // whenever the sequencer arms a run at all (the SHW latch can survive
    // from the previous line), so word counts and picture placement stay
    // linear in the raw register (vAmigaTS Agnus/DDF oldhwstop3/4 photos).
    start.min(BITPLANE_DDF_HARD_STOP)
}

fn effective_ddf_window(
    revision: AgnusRevision,
    hires: bool,
    ddfstrt: u16,
    ddfstop: u16,
    harddis: bool,
) -> Option<(u16, u16)> {
    // The start is not floored to the hardwired window: a DDFSTRT below $18
    // keeps its raw fetch-grid position (see `effective_ddf_start_hpos`).
    let (_, hard_stop) = ddf_hard_bounds(harddis);
    let start = effective_ddf_start_hpos_raw(revision, hires, ddfstrt);
    let mut stop = effective_ddf_stop_hpos(revision, hires, ddfstop);
    if start == 0 || start > hard_stop {
        return None;
    }
    if matches!(revision, AgnusRevision::Ocs) && stop == start {
        stop = hard_stop;
    }
    let stop = stop.min(hard_stop);
    (stop >= start).then_some((start, stop))
}

mod diag;
mod fetch;
mod output;
mod sprite;

use diag::*;
use fetch::*;
use output::*;
use sprite::*;

#[cfg(test)]
mod tests;
