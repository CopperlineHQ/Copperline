// SPDX-License-Identifier: GPL-3.0-or-later

//! Frontend-independent presentation helpers: the post-render pass that
//! turns a rendered field into a presentable frame (vertical/horizontal
//! recentring, the TV overscan bezel mask, programmable-scan stretch) and
//! the aperture/geometry predicates around it. Moved out of
//! `video/window/present.rs` so headless consumers (`cpu.rs`'s debug
//! screenshots, the wasm browser frontend) can present frames without the
//! winit frontend; `window/present.rs` re-exports everything here, so the
//! desktop path is unchanged.

use super::{bitplane, deinterlace::OUT_HEIGHT, FrameGeometry, FB_HEIGHT, FB_PIXELS, FB_WIDTH};
use crate::bus::RenderRegisterSnapshot;
use crate::config::Overscan;
use crate::screenshot;

pub const fn rgba(r: u32, g: u32, b: u32) -> u32 {
    0xFF00_0000 | (b << 16) | (g << 8) | r
}

/// Compose the active RTG board frame (Z3660 scanout). The board frame lands
/// in `scratch` at its native resolution; `out` gets an FB_WIDTH-stride copy
/// (nearest, one output row per board row) for the screenshot path, which
/// reads the shared presentation buffer. Returns
/// `(present_rows, native_w, native_h)`, or `None` while no RTG board drives
/// the display. The window presents the native `scratch` frame directly (see
/// [`crate::video::window`]); `out` exists only for the screenshot path.
pub fn compose_rtg_present(
    bus: &crate::bus::Bus,
    scratch: &mut Vec<u32>,
    out: &mut Vec<u32>,
) -> Option<(usize, u32, u32)> {
    let (w, h) = bus.rtg_frame(scratch)?;
    let (w, h) = (w as usize, h as usize);
    out.clear();
    out.resize(h * FB_WIDTH, 0xFF00_0000);
    for y in 0..h {
        let src = &scratch[y * w..(y + 1) * w];
        let dst = &mut out[y * FB_WIDTH..(y + 1) * FB_WIDTH];
        for (x, px) in dst.iter_mut().enumerate() {
            // Sample the centre of each output pixel's source span, not its
            // left edge: `x * w / FB_WIDTH` tops out below `w - 1` whenever
            // the board frame is wider than FB_WIDTH, so the rightmost
            // source columns never reach a downscaled screenshot.
            *px = src[(2 * x + 1) * w / (2 * FB_WIDTH)];
        }
    }
    Some((h, w as u32, h as u32))
}

pub const STANDARD_PAL_VISIBLE_WIDTH: usize = 320 * 2;
pub const STANDARD_PAL_VISIBLE_LINES: usize = 256;
pub const STANDARD_NTSC_VISIBLE_LINES: usize = 200;
pub const STANDARD_PAL_VISIBLE_START_VPOS: u32 = 0x2C;
// Default TV presentation keeps a small consumer-visible overscan margin while
// still hiding the deep edge columns that often contain unfinished effects.
pub const TV_HORIZONTAL_OVERSCAN_MARGIN: usize = 24 * 2;
// Overscan field lines the TV aperture keeps above and below the standard
// window.
pub const TV_VERTICAL_OVERSCAN_MARGIN: usize = 7;

// The TV aperture for standard 15 kHz displays, cropped from the woven
// presentation buffer. Horizontally both presentation paths (live window
// and PNG) show the captured aperture (`TV_CAPTURED_*` below) resampled
// onto the framebuffer-wide 4:3 glass by `tv_glass_sample`; the two video
// standards agree -- both place the standard 640-wide window at the same
// colour clocks -- so one cutout serves 50 Hz and 60 Hz scans alike.
// Vertically the aperture follows the field line count: a 312/313-line
// (50 Hz) scan carries the 256-line standard window, a 262/263-line
// (60 Hz) scan the 200-line one, each cropped with the same symmetric
// overscan margin.
pub const TV_PRESENT_SOURCE_Y: usize = 18;
pub const TV_PAL_PRESENT_HEIGHT: usize =
    (STANDARD_PAL_VISIBLE_LINES + 2 * TV_VERTICAL_OVERSCAN_MARGIN) * 2;
pub const TV_NTSC_PRESENT_HEIGHT: usize =
    (STANDARD_NTSC_VISIBLE_LINES + 2 * TV_VERTICAL_OVERSCAN_MARGIN) * 2;

// Output rows a TV-aperture crop presents: both standards' apertures fill
// the same 4:3 glass, so the crop always presents at the 50 Hz aperture's
// native row count. A 50 Hz crop maps 1:1; a 60 Hz crop's 428 rows stretch
// onto it, the taller picture lines of a 200-line display on the same
// screen.
pub const TV_GLASS_PRESENT_ROWS: usize = TV_PAL_PRESENT_HEIGHT;

// The TV aperture clipped to columns the framebuffer actually captures, for
// frontends whose frame should end on real pixels instead of black bezel
// (the browser canvas hugs its border on every side). The margin is the
// captured right-overscan width, mirrored to the left so the standard
// window stays exactly centred; the right edge lands on the framebuffer's
// edge by construction.
pub const TV_CAPTURED_MARGIN_X: usize =
    FB_WIDTH - bitplane::STANDARD_VISIBLE_X0 - STANDARD_PAL_VISIBLE_WIDTH;
pub const TV_CAPTURED_SOURCE_X: usize = bitplane::STANDARD_VISIBLE_X0 - TV_CAPTURED_MARGIN_X;
pub const TV_CAPTURED_WIDTH: usize = STANDARD_PAL_VISIBLE_WIDTH + 2 * TV_CAPTURED_MARGIN_X;

// The captured aperture's invariants: it ends exactly on the framebuffer
// edge (no bezel columns), keeps symmetric margins around the standard
// window, starts inside the reference aperture, and both standards'
// vertical crops fit the woven field.
const _: () = {
    assert!(TV_CAPTURED_SOURCE_X + TV_CAPTURED_WIDTH == FB_WIDTH);
    assert!(bitplane::STANDARD_VISIBLE_X0 - TV_CAPTURED_SOURCE_X == TV_CAPTURED_MARGIN_X);
    assert!(
        TV_CAPTURED_SOURCE_X + TV_CAPTURED_WIDTH
            - (bitplane::STANDARD_VISIBLE_X0 + STANDARD_PAL_VISIBLE_WIDTH)
            == TV_CAPTURED_MARGIN_X
    );
    assert!(TV_PRESENT_SOURCE_Y + TV_PAL_PRESENT_HEIGHT <= OUT_HEIGHT);
    assert!(TV_NTSC_PRESENT_HEIGHT < TV_PAL_PRESENT_HEIGHT);
};

/// One glass pixel of the 4:3 TV presentation: the captured aperture's
/// `TV_CAPTURED_WIDTH` real columns fill the `FB_WIDTH`-wide glass through
/// a fractional resample (`blend_rgba`, the programmable-scan idiom), the
/// way a real set's raster fills its glass. Every glass pixel derives from
/// real captured pixels -- nothing is padded black -- and the standard
/// window stays exactly centred because the captured margins are symmetric
/// by construction. 8.8 fixed point, sampling between texel centres.
#[inline]
pub fn tv_glass_sample(row: &[u32], out_x: usize) -> u32 {
    debug_assert!(row.len() >= FB_WIDTH);
    let s = (TV_CAPTURED_SOURCE_X as i64) * 256
        + ((2 * out_x as i64 + 1) * (TV_CAPTURED_WIDTH as i64) * 256) / (2 * FB_WIDTH as i64)
        - 128;
    let s = s.clamp(0, (FB_WIDTH as i64 - 1) * 256);
    let i = (s >> 8) as usize;
    let frac = (s & 255) as u32;
    crate::video::blend_rgba(row[i], row[(i + 1).min(FB_WIDTH - 1)], frac)
}

pub fn post_process_rendered_field(
    fb: &mut [u32],
    geometry: FrameGeometry,
    canvas_scale: usize,
    presentation_h_window: Option<(i32, u32)>,
    presentation_v_window: Option<(i32, u32)>,
    visible_start_vpos: u32,
    h_shift: usize,
    overscan: Overscan,
) -> usize {
    let canvas_width = FB_WIDTH * canvas_scale;
    let field_rows = geometry.visible_lines.min(fb.len() / canvas_width);
    // Vertical centring, optional full-overscan horizontal recentring, and the
    // TV bezel mask are 15 kHz CRT concepts anchored to the standard PAL/NTSC
    // window; a programmable scan defines its own window and presents in full,
    // like a multisync monitor.
    if !geometry.programmable {
        // Standard scans always render the classic canvas pitch
        // (`bitplane::canvas_scale_for`).
        debug_assert_eq!(canvas_scale, 1);
        center_present_frame_for_visible_start(fb, visible_start_vpos);
        center_present_frame_horizontally(fb, h_shift);
        if overscan == Overscan::Tv {
            mask_present_frame_to_tv(fb, h_shift, standard_window_top_row(visible_start_vpos));
        }
        return field_rows;
    }
    if let Some((src_x0, src_w)) = presentation_h_window {
        // A multisync monitor locks its horizontal deflection to the
        // programmed sync pulse: the glass shows the line from the sync
        // trailing edge to the next pulse, centring the picture the way
        // the mode's own porches place it. The window is computed in
        // classic-canvas pixels; scale it to this frame's pitch.
        screenshot::stretch_rows_x_window(
            fb,
            canvas_width,
            field_rows,
            src_x0 * canvas_scale as i32,
            src_w * canvas_scale as u32,
        );
    } else if geometry.line_cck != 227 {
        // No programmed sync to anchor on: fall back to the time-linear
        // whole-line map (each colour clock of this scan's shorter/longer
        // line covers 227/line_cck of the glass a standard line's clock
        // would).
        screenshot::stretch_rows_x(fb, canvas_width, field_rows, geometry.line_cck, 227);
    }
    apply_presentation_v_window(fb, canvas_width, field_rows, presentation_v_window)
}

/// Vertical counterpart of the sync-anchored horizontal window: place the
/// captured rows inside the glass's full vertical span (the frame minus the
/// programmed sync pulse), bordered with blanked rows where the mode's own
/// porches put them, and return the new row count. Without a programmed
/// vertical sync the captured rows keep covering the whole glass height.
pub fn apply_presentation_v_window(
    fb: &mut [u32],
    canvas_width: usize,
    field_rows: usize,
    presentation_v_window: Option<(i32, u32)>,
) -> usize {
    let Some((v_offset, glass_rows)) = presentation_v_window else {
        return field_rows;
    };
    let buf_rows = fb.len() / canvas_width;
    let glass_rows = (glass_rows as usize).min(buf_rows).max(1);
    if glass_rows <= field_rows {
        return field_rows;
    }
    // Rows the sync-anchored glass top cuts off the capture (offset < 0)
    // vs. blanked glass rows above the captured window (offset > 0).
    let skip_top = (-v_offset).max(0) as usize;
    let pad_top = (v_offset).max(0) as usize;
    let black = rgba(0, 0, 0);
    if skip_top >= field_rows || pad_top >= glass_rows {
        fb[..glass_rows * canvas_width].fill(black);
        return glass_rows;
    }
    let content_rows = (field_rows - skip_top).min(glass_rows - pad_top);
    if pad_top > skip_top {
        for row in (0..content_rows).rev() {
            let src = (skip_top + row) * canvas_width;
            let dst = (pad_top + row) * canvas_width;
            fb.copy_within(src..src + canvas_width, dst);
        }
    } else if pad_top < skip_top {
        for row in 0..content_rows {
            let src = (skip_top + row) * canvas_width;
            let dst = (pad_top + row) * canvas_width;
            fb.copy_within(src..src + canvas_width, dst);
        }
    }
    fb[..pad_top * canvas_width].fill(black);
    fb[(pad_top + content_rows) * canvas_width..glass_rows * canvas_width].fill(black);
    glass_rows
}

/// `[display] overscan = "tv"`: black out the deep-overscan margins like the
/// bezel of a CRT. Demos routinely leave junk in the deep overscan (e.g. HAM
/// streams that converge off-screen, as on the 9 Fingers title); a TV hides
/// it and so does this mask. The emulated framebuffer itself always carries
/// the full field; this runs on the presentation copy only.
///
/// The window is a realistic PAL TV visible area rather than the bare standard
/// window: real sets show a margin of overscan, which intentional overscan
/// displays rely on, while the deep-overscan junk the mask exists to hide sits
/// further out. Default TV presentation keeps 24 lo-res pixels of horizontal
/// overscan beside the standard PAL window; full overscan remains available
/// through `Overscan::Full`. The mask is horizontal only: vertical border
/// colour changes are part of the Denise output and can be intentional
/// effects, so rows above or below the standard display remain as rendered.
/// `h_shift` is any horizontal presentation shift already applied to the
/// frame, so the bezel tracks the shifted picture instead of clipping its left
/// edge.
pub fn mask_present_frame_to_tv(fb: &mut [u32], h_shift: usize, _standard_top_row: usize) {
    debug_assert!(fb.len() >= FB_PIXELS);
    let black = rgba(0, 0, 0);
    let (source_left, source_right) = tv_source_h_bounds();
    let left = source_left.saturating_sub(h_shift);
    let right = source_right.saturating_sub(h_shift).min(FB_WIDTH).max(left);
    for row in fb.chunks_mut(FB_WIDTH) {
        row[..left].fill(black);
        if right < FB_WIDTH {
            row[right..].fill(black);
        }
    }
}

/// The framebuffer row carrying the standard window's first line after
/// `center_present_frame_for_visible_start` has run: the centring shift
/// plus however many overscan rows were already visible above it.
pub fn standard_window_top_row(visible_start_vpos: u32) -> usize {
    let overscan_rows_already_visible =
        STANDARD_PAL_VISIBLE_START_VPOS.saturating_sub(visible_start_vpos) as usize;
    presentation_source_y_offset(visible_start_vpos) + overscan_rows_already_visible
}

/// Shift the rendered frame left by `shift` framebuffer pixels, filling the
/// vacated right columns with black. Used to recentre a standard display
/// whose deep left overscan would otherwise push the picture right of
/// centre. A no-op when `shift` is 0 (overscan frames).
pub fn center_present_frame_horizontally(fb: &mut [u32], shift: usize) {
    debug_assert!(fb.len() >= FB_PIXELS);
    if shift == 0 {
        return;
    }
    let shift = shift.min(FB_WIDTH);
    let black = rgba(0, 0, 0);
    for y in 0..FB_HEIGHT {
        let row = &mut fb[y * FB_WIDTH..(y + 1) * FB_WIDTH];
        row.copy_within(shift.., 0);
        row[FB_WIDTH - shift..].fill(black);
    }
}

pub fn center_present_frame_for_visible_start(fb: &mut [u32], visible_start_vpos: u32) {
    debug_assert!(fb.len() >= FB_PIXELS);
    let offset = presentation_source_y_offset(visible_start_vpos);
    if offset == 0 {
        return;
    }

    for y in (0..FB_HEIGHT - offset).rev() {
        let src = y * FB_WIDTH;
        let dst = (y + offset) * FB_WIDTH;
        fb.copy_within(src..src + FB_WIDTH, dst);
    }
    fb[..offset * FB_WIDTH].fill(rgba(0, 0, 0));
}

pub fn presentation_source_y_offset(visible_start_vpos: u32) -> usize {
    let standard_offset = FB_HEIGHT.saturating_sub(STANDARD_PAL_VISIBLE_LINES) / 2;
    let overscan_rows_already_visible =
        STANDARD_PAL_VISIBLE_START_VPOS.saturating_sub(visible_start_vpos) as usize;
    standard_offset.saturating_sub(overscan_rows_already_visible)
}

/// Presentation-geometry decisions carried across border-only frames.
///
/// The TV aperture crop and the full-overscan recentring shift are judged
/// from each frame's playfield, but a border-only frame has no playfield to
/// judge -- and screen changes blank the display for a frame or two while
/// Intuition rebuilds the copper list. Deciding those frames "full
/// framebuffer" made the whole picture lurch sideways (and rescale) at
/// every Kickstart screen change, conspicuous inside the monitor bezel's
/// fixed frame. A border-only frame instead keeps the previous decision --
/// the monitor does not move between screens -- with the stock standard
/// display as the power-on default. The two decisions latch independently:
/// the shift resolves when a frame is submitted for rendering, the
/// aperture when its presentation comes back, and each is only consumed by
/// its own overscan mode.
#[derive(Clone, Debug)]
pub struct PresentationLatch {
    standard_aperture: bool,
    h_shift: usize,
}

impl Default for PresentationLatch {
    fn default() -> Self {
        Self {
            standard_aperture: true,
            h_shift: bitplane::standard_present_h_shift(),
        }
    }
}

impl PresentationLatch {
    /// Back to the power-on default, for presentation discontinuities
    /// (machine swap, reset, state load).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The horizontal recentring shift for one frame under `overscan`,
    /// latching the decision of any frame that carries content.
    pub fn presentation_h_shift(
        &mut self,
        snapshot: &RenderRegisterSnapshot,
        overscan: Overscan,
    ) -> usize {
        match overscan {
            // TV mode is an aperture over the emulated framebuffer, matching
            // the fixed source cutout used by reference emulators. Do not
            // copy pixels sideways here: a standard hi-res screen already
            // occupies the right edge of Copperline's 716-pixel cutout.
            Overscan::Tv => 0,
            Overscan::Full => match bitplane::horizontal_content_class(snapshot) {
                bitplane::HorizontalContentClass::Standard { shift } => {
                    self.h_shift = shift;
                    shift
                }
                bitplane::HorizontalContentClass::Overscan => {
                    self.h_shift = 0;
                    0
                }
                bitplane::HorizontalContentClass::Neutral => self.h_shift,
            },
        }
    }

    /// Resolve one frame's aperture classification into the crop to apply,
    /// latching the decision of any frame that carries content.
    pub fn resolve_tv_aperture(&mut self, frame: TvApertureFrame) -> Option<usize> {
        match frame {
            TvApertureFrame::Standard(rows) => {
                self.standard_aperture = true;
                Some(rows)
            }
            TvApertureFrame::Full => {
                self.standard_aperture = false;
                None
            }
            TvApertureFrame::Neutral(rows) => self.standard_aperture.then_some(rows),
        }
    }
}

pub fn tv_source_h_bounds() -> (usize, usize) {
    let left = bitplane::STANDARD_VISIBLE_X0.saturating_sub(TV_HORIZONTAL_OVERSCAN_MARGIN);
    let right = bitplane::STANDARD_VISIBLE_X0
        .saturating_add(STANDARD_PAL_VISIBLE_WIDTH)
        .saturating_add(TV_HORIZONTAL_OVERSCAN_MARGIN)
        .min(FB_WIDTH)
        .max(left);
    (left, right)
}

pub fn should_render_emulated_frame(last_rendered: Option<u64>, current: u64) -> bool {
    last_rendered != Some(current)
}

pub fn is_standard_presentation(geometry: FrameGeometry, src_rows: usize) -> bool {
    !geometry.programmable && src_rows == OUT_HEIGHT
}

/// One frame's TV-aperture classification, resolved into a crop by
/// [`PresentationLatch::resolve_tv_aperture`]: crop to the standard
/// aperture (`Standard`), present the full framebuffer (`Full`), or keep
/// the previous geometry (`Neutral`, a border-only frame with no content
/// to judge). `Standard` and `Neutral` carry the crop height in woven
/// rows, which follows the scan the frame actually ran rather than the
/// configured standard: a 312/313-line (50 Hz) field carries the 256-line
/// standard window, a 262/263-line (60 Hz) field the 200-line one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TvApertureFrame {
    Standard(usize),
    Full,
    Neutral(usize),
}

/// Classify one frame for the TV aperture. Programmable scans, frames not
/// rendered at the standard woven height, and true horizontal-overscan
/// fetches present on the full framebuffer.
pub fn standard_tv_aperture_frame(
    geometry: FrameGeometry,
    src_rows: usize,
    snapshot: &RenderRegisterSnapshot,
) -> TvApertureFrame {
    if !is_standard_presentation(geometry, src_rows) {
        return TvApertureFrame::Full;
    }
    let rows = if geometry.frame_lines >= 312 {
        TV_PAL_PRESENT_HEIGHT
    } else {
        TV_NTSC_PRESENT_HEIGHT
    };
    match bitplane::horizontal_content_class(snapshot) {
        // The glass does not move for the content: a display fetching
        // into the overscan extends past the aperture and the monitor
        // crops it, exactly as a real set does (overscan = "full" is the
        // mode for seeing all of it). Only the scan's shape -- the
        // programmable geometry and native-height fields handled above --
        // changes what the presentation shows.
        bitplane::HorizontalContentClass::Standard { .. }
        | bitplane::HorizontalContentClass::Overscan => TvApertureFrame::Standard(rows),
        bitplane::HorizontalContentClass::Neutral => TvApertureFrame::Neutral(rows),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_aperture_clears_the_tv_bezel_mask() {
        // The const block by the TV_CAPTURED_* definitions pins the
        // aperture geometry at compile time; the mask bounds come from a
        // runtime helper, so check here that a captured-aperture crop never
        // shows masked black columns. The glass resample blends one column
        // left of the aperture start (its first sample centre sits half an
        // output pixel before the first source centre), so that column must
        // clear the mask too: the aperture must start strictly inside it.
        assert!(TV_CAPTURED_SOURCE_X > tv_source_h_bounds().0);
    }

    #[test]
    fn tv_glass_resample_spans_the_captured_aperture() {
        // A row with a marker at each end of the captured aperture: the
        // glass's first and last pixels must derive from them (the edges
        // reach the glass), and a marker just outside the aperture must
        // not appear anywhere.
        let mut row = vec![0xFF00_0000u32; FB_WIDTH];
        let first = 0xFF20_4060;
        let last = 0xFF60_4020;
        for x in TV_CAPTURED_SOURCE_X..FB_WIDTH {
            row[x] = 0xFF7F_7F7F;
        }
        row[TV_CAPTURED_SOURCE_X] = first;
        row[FB_WIDTH - 1] = last;

        let g0 = tv_glass_sample(&row, 0);
        let g_last = tv_glass_sample(&row, FB_WIDTH - 1);
        // The first glass pixel blends the first captured column with its
        // left neighbour; the marker must dominate or match.
        assert_ne!(g0, 0xFF7F_7F7F, "left aperture edge missing from glass");
        assert_eq!(g_last, last, "right aperture edge missing from glass");

        // The standard window's centre column maps to the glass centre.
        let mut centre_row = vec![0xFF00_0000u32; FB_WIDTH];
        let centre_src = TV_CAPTURED_SOURCE_X + TV_CAPTURED_WIDTH / 2;
        centre_row[centre_src - 1] = 0xFFFF_FFFF;
        centre_row[centre_src] = 0xFFFF_FFFF;
        assert_eq!(tv_glass_sample(&centre_row, FB_WIDTH / 2), 0xFFFF_FFFF);
    }

    /// A standard full-width display (stock Kickstart screen).
    fn standard_snapshot() -> RenderRegisterSnapshot {
        RenderRegisterSnapshot {
            bplcon0: 0x5200,
            diwstrt: 0x2C81,
            diwstop: 0xF4C1,
            ddfstrt: 0x38,
            ddfstop: 0xD0,
            ..Default::default()
        }
    }

    /// A wide-DIW display whose fetch reaches into the overscan border.
    fn overscan_snapshot() -> RenderRegisterSnapshot {
        RenderRegisterSnapshot {
            bplcon0: 0x5200,
            diwstrt: 0x5702,
            diwstop: 0xFFFF,
            ddfstrt: 0x30,
            ddfstop: 0xD8,
            ..Default::default()
        }
    }

    /// A border-only frame: registers cleared, no fetch (the state a screen
    /// change or the Kickstart boot blanks present for a few frames).
    fn blank_snapshot() -> RenderRegisterSnapshot {
        RenderRegisterSnapshot::default()
    }

    #[test]
    fn tv_aperture_rows_follow_the_scans_field_line_count() {
        // A standard scan's TV aperture is picked by the lines the frame
        // actually ran (BEAMCON0 can retune Agnus mid-session), holding the
        // 256-line standard window of a 50 Hz field and the 200-line window
        // of a 60 Hz field with the same overscan margin.
        let snapshot = standard_snapshot();
        let pal = FrameGeometry::standard(0x1C, 313, false);
        let ntsc = FrameGeometry::standard(0x1C, 262, false);
        assert_eq!(
            standard_tv_aperture_frame(pal, OUT_HEIGHT, &snapshot),
            TvApertureFrame::Standard(TV_PAL_PRESENT_HEIGHT)
        );
        assert_eq!(
            standard_tv_aperture_frame(ntsc, OUT_HEIGHT, &snapshot),
            TvApertureFrame::Standard(TV_NTSC_PRESENT_HEIGHT)
        );
        // A programmable scan or a native-height field presents in full.
        let mut programmable = ntsc;
        programmable.programmable = true;
        assert_eq!(
            standard_tv_aperture_frame(programmable, OUT_HEIGHT, &snapshot),
            TvApertureFrame::Full
        );
        assert_eq!(
            standard_tv_aperture_frame(ntsc, 400, &snapshot),
            TvApertureFrame::Full
        );
        // A border-only frame on a standard scan is neutral: it carries the
        // scan's crop height but no crop decision of its own.
        assert_eq!(
            standard_tv_aperture_frame(pal, OUT_HEIGHT, &blank_snapshot()),
            TvApertureFrame::Neutral(TV_PAL_PRESENT_HEIGHT)
        );
    }

    #[test]
    fn overscan_content_keeps_the_glass_aperture() {
        // A display fetching into the overscan (the CD32 boot screen, demo
        // loaders) extends past the aperture; the monitor's glass crops it
        // rather than zooming out to show it -- the glass does not move
        // for the content. Falling back to the full framebuffer here used
        // to expose the unpainted capture margins as black bands beside
        // the picture.
        let overscan = RenderRegisterSnapshot {
            agnus_revision: crate::chipset::agnus::AgnusRevision::AgaAlice,
            bplcon0: 0x8214,
            diwstrt: 0x1D61,
            diwstop: 0x37C7,
            ddfstrt: 0x0028,
            ddfstop: 0x00D8,
            ..RenderRegisterSnapshot::default()
        };
        assert_eq!(
            bitplane::horizontal_content_class(&overscan),
            bitplane::HorizontalContentClass::Overscan,
            "snapshot must classify as overscan for the assertion to bite"
        );
        let pal = FrameGeometry::standard(0x1C, 313, false);
        assert_eq!(
            standard_tv_aperture_frame(pal, OUT_HEIGHT, &overscan),
            TvApertureFrame::Standard(TV_PAL_PRESENT_HEIGHT)
        );
    }

    #[test]
    fn border_only_frames_keep_the_previous_aperture_decision() {
        // The Kickstart 2.05 boot regression: screen changes emit a frame
        // or two of border-only display, and deciding those "full
        // framebuffer" flipped the whole presentation geometry there and
        // back, lurching the picture sideways at every mode change.
        let mut latch = PresentationLatch::default();
        // Power-on default: crop like the stock display the blanks precede.
        assert_eq!(
            latch.resolve_tv_aperture(TvApertureFrame::Neutral(TV_PAL_PRESENT_HEIGHT)),
            Some(TV_PAL_PRESENT_HEIGHT)
        );
        assert_eq!(
            latch.resolve_tv_aperture(TvApertureFrame::Standard(TV_PAL_PRESENT_HEIGHT)),
            Some(TV_PAL_PRESENT_HEIGHT)
        );
        // The blank between two standard screens stays cropped...
        assert_eq!(
            latch.resolve_tv_aperture(TvApertureFrame::Neutral(TV_PAL_PRESENT_HEIGHT)),
            Some(TV_PAL_PRESENT_HEIGHT)
        );
        // ...and a neutral frame still follows the current scan's height
        // (the crop decision is latched, not the row count).
        assert_eq!(
            latch.resolve_tv_aperture(TvApertureFrame::Neutral(TV_NTSC_PRESENT_HEIGHT)),
            Some(TV_NTSC_PRESENT_HEIGHT)
        );
        // An overscan demo blanking between parts must NOT snap to the
        // aperture: full presentation latches across its blanks too.
        assert_eq!(latch.resolve_tv_aperture(TvApertureFrame::Full), None);
        assert_eq!(
            latch.resolve_tv_aperture(TvApertureFrame::Neutral(TV_PAL_PRESENT_HEIGHT)),
            None
        );
        // A presentation discontinuity restores the power-on default.
        latch.reset();
        assert_eq!(
            latch.resolve_tv_aperture(TvApertureFrame::Neutral(TV_PAL_PRESENT_HEIGHT)),
            Some(TV_PAL_PRESENT_HEIGHT)
        );
    }

    #[test]
    fn border_only_frames_keep_the_previous_recentring_shift() {
        let mut latch = PresentationLatch::default();
        let standard_shift = bitplane::standard_present_h_shift();
        // Power-on default: the stock display's shift, so the first real
        // screen does not jump against the blanks before it.
        assert_eq!(
            latch.presentation_h_shift(&blank_snapshot(), Overscan::Full),
            standard_shift
        );
        assert_eq!(
            latch.presentation_h_shift(&standard_snapshot(), Overscan::Full),
            standard_shift
        );
        // A true overscan display presents unshifted, and its blanks keep
        // that.
        assert_eq!(
            latch.presentation_h_shift(&overscan_snapshot(), Overscan::Full),
            0
        );
        assert_eq!(
            latch.presentation_h_shift(&blank_snapshot(), Overscan::Full),
            0
        );
        // TV mode is a fixed aperture: never shifted, and not latched.
        assert_eq!(
            latch.presentation_h_shift(&standard_snapshot(), Overscan::Tv),
            0
        );
        assert_eq!(
            latch.presentation_h_shift(&blank_snapshot(), Overscan::Full),
            0
        );
    }

    #[test]
    fn ntsc_aperture_stays_inside_the_rendered_field() {
        // A 60 Hz field renders (frame_lines - visible_start) field rows;
        // the aperture crop must end inside them so it never shows
        // unscanned buffer rows as picture.
        let rendered_woven_rows = 2 * (262 - 0x1C);
        assert!(TV_PRESENT_SOURCE_Y + TV_NTSC_PRESENT_HEIGHT <= rendered_woven_rows);
    }

    fn v_window_fixture(field_rows: usize, total_rows: usize) -> Vec<u32> {
        // Each captured row is tagged with its index + 1 in every column so
        // relocation is visible; rows past field_rows start as garbage the
        // applier must overwrite or ignore.
        let mut fb = vec![0xDEAD_BEEF; total_rows * FB_WIDTH];
        for row in 0..field_rows {
            fb[row * FB_WIDTH..(row + 1) * FB_WIDTH].fill(row as u32 + 1);
        }
        fb
    }

    #[test]
    fn presentation_v_window_places_rows_by_the_modes_porches() {
        let mut fb = v_window_fixture(4, 12);
        let rows = apply_presentation_v_window(&mut fb, FB_WIDTH, 4, Some((3, 10)));
        assert_eq!(rows, 10);
        let black = rgba(0, 0, 0);
        for row in 0..10 {
            let expected = match row {
                3..=6 => (row - 2) as u32,
                _ => black,
            };
            assert_eq!(fb[row * FB_WIDTH], expected, "row {row}");
        }
    }

    #[test]
    fn presentation_v_window_clips_rows_above_the_glass_top() {
        let mut fb = v_window_fixture(4, 12);
        let rows = apply_presentation_v_window(&mut fb, FB_WIDTH, 4, Some((-2, 10)));
        assert_eq!(rows, 10);
        let black = rgba(0, 0, 0);
        // Captured rows 0-1 fall above the sync-anchored glass; rows 2-3
        // land at the top.
        assert_eq!(fb[0], 3);
        assert_eq!(fb[FB_WIDTH], 4);
        for row in 2..10 {
            assert_eq!(fb[row * FB_WIDTH], black, "row {row}");
        }
    }

    #[test]
    fn presentation_v_window_absent_or_degenerate_keeps_the_field() {
        let mut fb = v_window_fixture(4, 12);
        assert_eq!(apply_presentation_v_window(&mut fb, FB_WIDTH, 4, None), 4);
        assert_eq!(fb[0], 1);
        // A glass no taller than the captured field cannot add borders.
        assert_eq!(
            apply_presentation_v_window(&mut fb, FB_WIDTH, 4, Some((1, 4))),
            4
        );
        assert_eq!(fb[0], 1);
    }
}
