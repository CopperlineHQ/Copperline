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
            *px = src[x * w / FB_WIDTH];
        }
    }
    Some((h, w as u32, h as u32))
}

pub const STANDARD_PAL_VISIBLE_WIDTH: usize = 320 * 2;
pub const STANDARD_PAL_VISIBLE_LINES: usize = 256;
pub const STANDARD_PAL_VISIBLE_START_VPOS: u32 = 0x2C;
// Default TV presentation keeps a small consumer-visible overscan margin while
// still hiding the deep edge columns that often contain unfinished effects.
pub const TV_HORIZONTAL_OVERSCAN_MARGIN: usize = 24 * 2;

// The reference TV aperture for standard PAL displays: the standard window
// plus a symmetric 26-column overscan margin, cropped from the woven
// presentation buffer. The desktop window and screenshot paths present this
// rect; its right margin reaches 12 columns past the framebuffer's right
// edge, which those paths pad with black bezel.
pub const TV_PAL_PRESENT_WIDTH: usize = STANDARD_PAL_VISIBLE_WIDTH + 2 * 26;
pub const TV_PAL_PRESENT_HEIGHT: usize = 540;
pub const TV_PAL_PRESENT_SOURCE_X: usize = bitplane::STANDARD_VISIBLE_X0 - 26;
pub const TV_PAL_PRESENT_SOURCE_Y: usize = 18;

// The TV aperture clipped to columns the framebuffer actually captures, for
// frontends whose frame should end on real pixels instead of black bezel
// (the browser canvas hugs its border on every side). The margin is the
// captured right-overscan width, mirrored to the left so the standard
// window stays exactly centred; the right edge lands on the framebuffer's
// edge by construction.
pub const TV_PAL_CAPTURED_MARGIN_X: usize =
    FB_WIDTH - bitplane::STANDARD_VISIBLE_X0 - STANDARD_PAL_VISIBLE_WIDTH;
pub const TV_PAL_CAPTURED_SOURCE_X: usize =
    bitplane::STANDARD_VISIBLE_X0 - TV_PAL_CAPTURED_MARGIN_X;
pub const TV_PAL_CAPTURED_WIDTH: usize = STANDARD_PAL_VISIBLE_WIDTH + 2 * TV_PAL_CAPTURED_MARGIN_X;

// The captured aperture's invariants: it ends exactly on the framebuffer
// edge (no bezel columns), keeps symmetric margins around the standard
// window, starts inside the reference aperture, and its vertical crop fits
// the woven field.
const _: () = {
    assert!(TV_PAL_CAPTURED_SOURCE_X + TV_PAL_CAPTURED_WIDTH == FB_WIDTH);
    assert!(bitplane::STANDARD_VISIBLE_X0 - TV_PAL_CAPTURED_SOURCE_X == TV_PAL_CAPTURED_MARGIN_X);
    assert!(
        TV_PAL_CAPTURED_SOURCE_X + TV_PAL_CAPTURED_WIDTH
            - (bitplane::STANDARD_VISIBLE_X0 + STANDARD_PAL_VISIBLE_WIDTH)
            == TV_PAL_CAPTURED_MARGIN_X
    );
    assert!(TV_PAL_CAPTURED_SOURCE_X >= TV_PAL_PRESENT_SOURCE_X);
    assert!(TV_PAL_PRESENT_SOURCE_Y + TV_PAL_PRESENT_HEIGHT <= OUT_HEIGHT);
};

pub fn post_process_rendered_field(
    fb: &mut [u32],
    geometry: FrameGeometry,
    visible_start_vpos: u32,
    h_shift: usize,
    overscan: Overscan,
) -> usize {
    let field_rows = geometry.visible_lines.min(fb.len() / FB_WIDTH);
    // Vertical centring, optional full-overscan horizontal recentring, and the
    // TV bezel mask are 15 kHz CRT concepts anchored to the standard PAL/NTSC
    // window; a programmable scan defines its own window and presents in full,
    // like a multisync monitor.
    if !geometry.programmable {
        center_present_frame_for_visible_start(fb, visible_start_vpos);
        center_present_frame_horizontally(fb, h_shift);
        if overscan == Overscan::Tv {
            mask_present_frame_to_tv(fb, h_shift, standard_window_top_row(visible_start_vpos));
        }
    } else if geometry.line_cck != 227 {
        // A multisync monitor's horizontal deflection is time-linear:
        // each colour clock of this scan's shorter/longer line covers
        // 227/line_cck of the glass a standard line's clock would.
        screenshot::stretch_rows_x(fb, FB_WIDTH, field_rows, geometry.line_cck, 227);
    }
    field_rows
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

pub fn presentation_h_shift_for(snapshot: &RenderRegisterSnapshot, overscan: Overscan) -> usize {
    match overscan {
        // TV mode is an aperture over the emulated framebuffer, matching the
        // fixed source cutout used by reference emulators. Do not copy pixels
        // sideways here: a standard hi-res screen already occupies the right
        // edge of Copperline's 716-pixel cutout.
        Overscan::Tv => 0,
        Overscan::Full => bitplane::present_h_shift_for(snapshot),
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

pub fn is_standard_pal_presentation(geometry: FrameGeometry, src_rows: usize) -> bool {
    !geometry.programmable && geometry.frame_lines >= 312 && src_rows == OUT_HEIGHT
}

pub fn uses_standard_pal_tv_aperture(
    geometry: FrameGeometry,
    src_rows: usize,
    snapshot: &RenderRegisterSnapshot,
) -> bool {
    is_standard_pal_presentation(geometry, src_rows)
        && bitplane::uses_standard_horizontal_content(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_aperture_clears_the_tv_bezel_mask() {
        // The const block by the TV_PAL_CAPTURED_* definitions pins the
        // aperture geometry at compile time; the mask bounds come from a
        // runtime helper, so check here that a captured-aperture crop never
        // shows masked black columns.
        assert!(TV_PAL_CAPTURED_SOURCE_X >= tv_source_h_bounds().0);
    }
}
