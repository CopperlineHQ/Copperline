// SPDX-License-Identifier: GPL-3.0-or-later

//! Motion-adaptive field deinterlacer for the presentation path.
//!
//! The renderer produces one 716x285 field per emulated frame. With
//! BPLCON0 LACE set, Agnus alternates long and short fields (LOF) and
//! interlaced software displays alternate rows of a ~570-line picture
//! one field at a time (the field bitplane pointers start one bitmap row
//! apart and the modulos skip the other field's rows). A real CRT draws
//! the short field's lines half a line below the long field's; phosphor
//! persistence merges the pair into a full-height picture, at the cost
//! of the famous interlace flicker.
//!
//! This module reconstructs that merged picture in a double-height
//! buffer: each pushed field lands on its parity's rows (long field =
//! upper = even rows). The opposite parity's rows keep the previous
//! field's lines where the picture is static -- a weave, recovering the
//! full vertical resolution -- and fall back to interpolating the
//! current field's neighbouring lines where the content changed between
//! same-parity fields, so motion bobs smoothly instead of combing into
//! alternate-line fringes. Progressive (non-lace) fields are simply
//! line-doubled, which presents identically to the old single-field
//! path.
//!
//! Field merging is on by default; `[display] deinterlace = false` (or
//! the COPPERLINE_DEINTERLACE env override) disables it, so every field
//! is line-doubled as it arrives, like the pre-deinterlacer
//! presentation.
//!
//! An optional CRT phosphor-persistence stage (`[display] phosphor` or
//! COPPERLINE_PHOSPHOR, 0.0..=0.95) blends each presented frame with a
//! fraction of the previous one, the exponential decay a CRT's phosphor
//! applies. Software that relies on the tube to fuse field-rate flicker
//! (alternate-field dither transparency, the CD32 boot intro's spinning
//! lettering) reads as intended with persistence around 0.3-0.5, at the
//! cost of a slight motion trail.

#[cfg(test)]
use super::FB_PIXELS;
use super::{FB_HEIGHT, FB_WIDTH, MAX_VISIBLE_LINES};

/// Double-height output: one row per interlaced picture line of a
/// standard field. Programmable scans may produce a different active
/// row count; consumers must use [`Deinterlacer::output_rows`].
pub const OUT_HEIGHT: usize = FB_HEIGHT * 2;
#[cfg(test)]
pub const OUT_PIXELS: usize = FB_WIDTH * OUT_HEIGHT;

pub struct Deinterlacer {
    /// Woven presentation buffer for the history-dependent path; the
    /// direct [`Self::present_field_into`] path bypasses it.
    out: Vec<u32>,
    /// Most recent field of each parity (0 = long, 1 = short), kept to
    /// detect motion between same-parity fields.
    prev: [Vec<u32>; 2],
    /// The field before `prev` of each parity: motion in the parity about
    /// to be woven is detected by comparing its last two fields, so a
    /// moving object captured one field ago is not woven in as a ghost.
    prev2: [Vec<u32>; 2],
    have: [bool; 2],
    have2: [bool; 2],
    /// Field row count of the history buffers; a geometry change drops
    /// the history (fields of different scans must not weave together).
    field_rows: usize,
    /// Field row width (pixels per row) of the history buffers; a canvas
    /// pitch change (a 35 ns super-hi-res scan arriving) drops the history
    /// like a row-count change.
    field_width: usize,
    /// Active output rows after the last push (the direct path sets this
    /// without writing `out`).
    out_rows: usize,
    /// Pixels per row of `out` after the last push.
    out_width: usize,
    /// Reusable motion mask for the weave path (one flag per canvas
    /// column); kept on the struct so a laced push does not allocate.
    moved: Vec<bool>,
    enabled: bool,
    /// CRT phosphor persistence: each presented frame keeps this fraction
    /// of the previous one (0 = off), expressed as an alpha in 0..=243
    /// (0.95 * 256). Approximates the phosphor decay that fuses
    /// field-rate dither and interlace flicker on a real CRT, e.g. the
    /// CD32 boot intro's flicker-dithered spinning lettering.
    phosphor_alpha: u32,
    /// Phosphor-blended presentation buffer (only when phosphor is on).
    presented: Option<Vec<u32>>,
    /// When set, the next presented frame copies the woven frame instead
    /// of blending: the blend buffer holds no real picture yet
    /// (construction, persistence just switched on, or the stream was
    /// reset), so decay starts from the new frame rather than fading it
    /// in from black.
    seed_presented: bool,
}

impl Default for Deinterlacer {
    fn default() -> Self {
        Self::new()
    }
}

impl Deinterlacer {
    pub fn new() -> Self {
        Self::with_options(crate::config::resolve_deinterlace(true), 0.0)
    }

    /// `deinterlace` enables motion-adaptive field merging (off,
    /// laced fields are plain line-doubled); `phosphor` is the
    /// persistence fraction in 0.0..=0.95.
    pub fn with_settings(deinterlace: bool, phosphor: f32) -> Self {
        Self::with_options(deinterlace, phosphor)
    }

    fn with_options(enabled: bool, phosphor: f32) -> Self {
        let phosphor_alpha = (phosphor.clamp(0.0, 0.95) * 256.0) as u32;
        Self {
            // All of these are history-path scratch. Keep the common
            // progressive/zero-phosphor path allocation-free, and grow them
            // on the first frame that actually needs weaving or decay.
            out: Vec::new(),
            prev: [Vec::new(), Vec::new()],
            prev2: [Vec::new(), Vec::new()],
            have: [false; 2],
            have2: [false; 2],
            field_rows: FB_HEIGHT,
            field_width: FB_WIDTH,
            out_rows: OUT_HEIGHT,
            out_width: FB_WIDTH,
            moved: Vec::new(),
            enabled,
            phosphor_alpha,
            presented: (phosphor_alpha > 0).then(Vec::new),
            // Seed so the first frame presents at full brightness; the
            // threaded pipeline starts its worker at phosphor 0 and seeds
            // through set_phosphor, and the synchronous path must present
            // identically rather than fading in from black.
            seed_presented: phosphor_alpha > 0,
        }
    }

    /// Switch motion-adaptive field merging on or off live. A change
    /// drops the weave history (fields either side of the switch present
    /// differently and must not weave together); an unchanged value is a
    /// no-op, so this is safe to call per frame.
    pub fn set_deinterlace(&mut self, enabled: bool) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        self.have = [false; 2];
        self.have2 = [false; 2];
    }

    /// Whether motion-adaptive field merging is enabled.
    pub fn deinterlace_enabled(&self) -> bool {
        self.enabled
    }

    /// Change the persistence fraction (0.0..=0.95) live. Switching
    /// persistence on starts the trail from the next woven frame;
    /// switching it off drops the blend buffer so [`Self::output`]
    /// returns the woven frame directly. An unchanged value is a no-op,
    /// so this is safe to call per frame.
    pub fn set_phosphor(&mut self, phosphor: f32) {
        let alpha = (phosphor.clamp(0.0, 0.95) * 256.0) as u32;
        if alpha == self.phosphor_alpha {
            return;
        }
        self.phosphor_alpha = alpha;
        if alpha == 0 {
            self.presented = None;
            self.seed_presented = false;
        } else if self.presented.is_none() {
            self.presented = Some(vec![0; self.out.len()]);
            self.seed_presented = true;
        }
    }

    /// The quantised CRT persistence fraction currently in use.
    pub fn phosphor(&self) -> f32 {
        self.phosphor_alpha as f32 / 256.0
    }

    /// Drop the weave history and phosphor trail: the next field starts a
    /// new picture (machine swap, reset, state load), so nothing from the
    /// previous stream may weave or glow into it.
    pub fn reset_history(&mut self) {
        self.have = [false; 2];
        self.have2 = [false; 2];
        if self.presented.is_some() {
            self.seed_presented = true;
        }
    }

    /// The merged presentation buffer (phosphor-blended when persistence
    /// is enabled). The first `output_rows()` rows are active.
    pub fn output(&self) -> &[u32] {
        self.presented.as_deref().unwrap_or(&self.out)
    }

    /// Active rows in [`Self::output`] after the last pushed field:
    /// 2x the field rows for woven/doubled standard fields, the native
    /// scan height for programmable progressive fields.
    pub fn output_rows(&self) -> usize {
        self.out_rows
    }

    /// Pixels per row of [`Self::output`] after the last pushed field:
    /// FB_WIDTH for the classic canvas, twice that for a 35 ns
    /// super-hi-res canvas.
    pub fn output_width(&self) -> usize {
        self.out_width
    }

    /// Decay the presented frame towards the freshly woven one across
    /// `fields` elapsed fields. Each field keeps `phosphor_alpha`/256 of
    /// its previous value, an exponential trail like CRT persistence.
    /// Combining the exponent into one blend keeps a long-deferred browser
    /// presentation O(pixels), not O(pixels * hidden fields).
    fn present_with_phosphor_elapsed(&mut self, fields: u32) {
        if fields == 0 {
            return;
        }
        let Some(presented) = &mut self.presented else {
            return;
        };
        let active = self.out_rows * self.out_width;
        if self.seed_presented {
            presented[..active].copy_from_slice(&self.out[..active]);
            self.seed_presented = false;
            return;
        }
        let a = elapsed_phosphor_alpha(self.phosphor_alpha, fields);
        for (shown, &new) in presented[..active]
            .iter_mut()
            .zip(self.out[..active].iter())
        {
            *shown = blend_rgba(new, *shown, a);
        }
    }

    fn present_with_phosphor(&mut self) {
        self.present_with_phosphor_elapsed(1);
    }

    /// Merge one rendered field of `rows` lines. `lace` and `long_field`
    /// describe the field's BPLCON0 LACE bit and Agnus LOF at its frame
    /// start. `double_rows` selects the progressive presentation: standard
    /// 15 kHz fields line-double (each field line covers two output rows,
    /// as on a TV), while a programmable progressive scan already carries
    /// every output line and passes through at native height.
    pub fn push_field(
        &mut self,
        field: &[u32],
        rows: usize,
        width: usize,
        lace: bool,
        long_field: bool,
        double_rows: bool,
    ) {
        debug_assert!(field.len() >= rows * width);
        let rows = rows.clamp(1, MAX_VISIBLE_LINES);
        if rows != self.field_rows || width != self.field_width {
            // Fields of a different scan must not weave with the old
            // history (mode switch); drop it.
            self.have = [false; 2];
            self.have2 = [false; 2];
            self.field_rows = rows;
            self.field_width = width;
        }
        self.out_width = width;
        // A 35 ns-pitch canvas outgrows the standard-canvas buffers.
        let out_need = rows * width * if double_rows || lace { 2 } else { 1 };
        if self.out.len() < out_need {
            self.out.resize(out_need, 0);
        }
        if let Some(presented) = &mut self.presented {
            if presented.len() < out_need {
                presented.resize(out_need, 0);
            }
        }
        if !lace && !double_rows {
            // Programmable progressive scan: every output line is already
            // in the field; present at native height.
            self.out[..rows * width].copy_from_slice(&field[..rows * width]);
            self.out_rows = rows;
            self.have = [false; 2];
            self.have2 = [false; 2];
            self.present_with_phosphor();
            return;
        }
        if !lace || !self.enabled {
            // Progressive: line-double. Field history would pair lines
            // from unrelated displays across a mode switch; drop it.
            for y in 0..rows {
                let row = &field[y * width..(y + 1) * width];
                self.out[2 * y * width..(2 * y + 1) * width].copy_from_slice(row);
                self.out[(2 * y + 1) * width..(2 * y + 2) * width].copy_from_slice(row);
            }
            self.out_rows = rows * 2;
            self.have = [false; 2];
            self.have2 = [false; 2];
            self.present_with_phosphor();
            return;
        }

        let parity = usize::from(!long_field);
        // The lace history buffers must hold this scan's field size.
        let field_need = rows * width;
        for buf in self.prev.iter_mut().chain(self.prev2.iter_mut()) {
            if buf.len() < field_need {
                buf.resize(field_need, 0);
            }
        }
        // This field's rows land on its own parity lines.
        for y in 0..rows {
            let row = &field[y * width..(y + 1) * width];
            let r = 2 * y + parity;
            self.out[r * width..(r + 1) * width].copy_from_slice(row);
        }
        self.out_rows = rows * 2;

        // Opposite-parity rows: weave the previous field's line where the
        // picture is static, interpolate this field's neighbours where it
        // moved (or while no opposite field has been woven yet). Motion is
        // checked on both parities: between the current field and the
        // previous field of its own parity (content arriving around the
        // woven line), and between the last two fields of the opposite
        // parity (content moving within the woven line itself, e.g. an
        // animation drawn one field ago that has since moved on).
        let opposite = parity ^ 1;
        if self.moved.len() < width {
            self.moved.resize(width, false);
        }
        let prev_same = &self.prev[parity];
        let prev_opp = &self.prev[opposite];
        let prev2_opp = &self.prev2[opposite];
        let moved = &mut self.moved[..width];
        for y in 0..rows {
            let r = 2 * y + opposite;
            // The current-parity field rows directly above and below
            // output row r (clamped at the frame edges).
            let above = if r == 0 { 0 } else { (r - 1 - parity) / 2 };
            let below = (((r + 1 - parity) / 2).min(rows - 1)).max(above);
            let above_row = &field[above * width..(above + 1) * width];
            let below_row = &field[below * width..(below + 1) * width];
            let out_row = &mut self.out[r * width..(r + 1) * width];
            if !self.have[opposite] {
                for x in 0..width {
                    out_row[x] = avg_rgba(above_row[x], below_row[x]);
                }
                continue;
            }
            let opp_row = &prev_opp[y * width..(y + 1) * width];
            let opp2_row = &prev2_opp[y * width..(y + 1) * width];
            let check_same = self.have[parity];
            let check_opp = self.have2[opposite];
            if check_same || check_opp {
                let prev_above = &prev_same[above * width..(above + 1) * width];
                let prev_below = &prev_same[below * width..(below + 1) * width];
                for x in 0..width {
                    let same_moved = check_same
                        && (above_row[x] != prev_above[x] || below_row[x] != prev_below[x]);
                    moved[x] = same_moved || (check_opp && opp_row[x] != opp2_row[x]);
                }
                for x in 0..width {
                    // Dilate the motion mask one pixel sideways so dithered
                    // moving art bobs as a region instead of weaving and
                    // interpolating on alternate pixels.
                    let near_motion =
                        moved[x] || (x > 0 && moved[x - 1]) || (x + 1 < width && moved[x + 1]);
                    if near_motion {
                        out_row[x] = avg_rgba(above_row[x], below_row[x]);
                    }
                }
            }
            // No usable history yet: keep the woven opposite field
            // untouched; motion adaptation starts with the next field.
        }

        std::mem::swap(&mut self.prev[parity], &mut self.prev2[parity]);
        if self.prev[parity].len() < field_need {
            self.prev[parity].resize(field_need, 0);
        }
        self.prev[parity][..rows * width].copy_from_slice(&field[..rows * width]);
        self.have2[parity] = self.have[parity];
        self.have[parity] = true;
        self.present_with_phosphor();
    }

    /// Present a field directly into an owned frontend buffer.
    ///
    /// The overwhelmingly common standard progressive path needs neither
    /// weave history nor phosphor history. Writing its doubled rows straight
    /// to the frontend buffer avoids first filling `self.out` and then copying
    /// the whole 570-line image once more. Phosphor-blended frames, and
    /// interlaced frames with deinterlacing enabled, retain the
    /// history-dependent [`Self::push_field`] path and copy its result, so
    /// their output is unchanged.
    #[doc(hidden)]
    pub fn present_field_into(
        &mut self,
        field: &[u32],
        rows: usize,
        width: usize,
        lace: bool,
        long_field: bool,
        double_rows: bool,
        destination: &mut Vec<u32>,
    ) -> (usize, usize) {
        self.present_field_into_elapsed(
            field,
            rows,
            width,
            lace,
            long_field,
            double_rows,
            1,
            destination,
        )
    }

    /// [`Self::present_field_into`] with the number of emulated fields that
    /// elapsed since the previous presentation. Frontends that deliberately
    /// defer rendering use this to age phosphor persistence by every skipped
    /// field while rendering only the newest image.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn present_field_into_elapsed(
        &mut self,
        field: &[u32],
        rows: usize,
        width: usize,
        lace: bool,
        long_field: bool,
        double_rows: bool,
        elapsed_fields: u32,
        destination: &mut Vec<u32>,
    ) -> (usize, usize) {
        let rows = rows.clamp(1, MAX_VISIBLE_LINES);
        debug_assert!(field.len() >= rows * width);
        let direct = self.phosphor_alpha == 0 && (!lace || !self.enabled);
        if direct {
            if rows != self.field_rows || width != self.field_width {
                self.field_rows = rows;
                self.field_width = width;
            }
            self.have = [false; 2];
            self.have2 = [false; 2];
            self.out_width = width;
            if !lace && !double_rows {
                let active = rows * width;
                destination.resize(active, 0);
                destination.copy_from_slice(&field[..active]);
                self.out_rows = rows;
            } else {
                let active = rows * width * 2;
                destination.resize(active, 0);
                for y in 0..rows {
                    let row = &field[y * width..(y + 1) * width];
                    destination[2 * y * width..(2 * y + 1) * width].copy_from_slice(row);
                    destination[(2 * y + 1) * width..(2 * y + 2) * width].copy_from_slice(row);
                }
                self.out_rows = rows * 2;
            }
            return (self.out_rows, self.out_width);
        }

        self.push_field(field, rows, width, lace, long_field, double_rows);
        self.present_with_phosphor_elapsed(elapsed_fields.max(1) - 1);
        let active = self.out_rows * self.out_width;
        destination.resize(active, 0);
        destination.copy_from_slice(&self.output()[..active]);
        (self.out_rows, self.out_width)
    }

    /// Present a vertically scaled rectangular window of a field directly
    /// into an owned frontend buffer.
    ///
    /// Standard browser TV presentation needs only the captured glass
    /// aperture, not the full 716x570 woven buffer. On the common
    /// progressive, zero-phosphor path this maps each destination row back
    /// to the source field and writes the crop once, avoiding both the full
    /// intermediate weave and the subsequent aperture copy. Interlaced or
    /// phosphor-dependent frames retain [`Self::push_field`] and crop its
    /// history-dependent result, so their pixels are unchanged.
    ///
    /// `source_x`/`source_y` are signed: an H/V-centred aperture can slide
    /// partly off the captured raster, and the glass it exposes there is
    /// unscanned and fills black.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn present_field_region_into(
        &mut self,
        field: &[u32],
        rows: usize,
        width: usize,
        lace: bool,
        long_field: bool,
        double_rows: bool,
        source_x: i32,
        source_y: i32,
        source_rows: usize,
        destination_width: usize,
        destination_rows: usize,
        destination: &mut Vec<u32>,
    ) -> (usize, usize) {
        self.present_field_region_into_elapsed(
            field,
            rows,
            width,
            lace,
            long_field,
            double_rows,
            source_x,
            source_y,
            source_rows,
            destination_width,
            destination_rows,
            1,
            destination,
        )
    }

    /// [`Self::present_field_region_into`] with elapsed-field phosphor aging,
    /// for a frontend that deferred intermediate presentations.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn present_field_region_into_elapsed(
        &mut self,
        field: &[u32],
        rows: usize,
        width: usize,
        lace: bool,
        long_field: bool,
        double_rows: bool,
        source_x: i32,
        source_y: i32,
        source_rows: usize,
        destination_width: usize,
        destination_rows: usize,
        elapsed_fields: u32,
        destination: &mut Vec<u32>,
    ) -> (usize, usize) {
        let rows = rows.clamp(1, MAX_VISIBLE_LINES);
        debug_assert!(field.len() >= rows * width);
        let doubled = lace || double_rows;
        let full_rows = if doubled { rows * 2 } else { rows };
        debug_assert!(source_x + destination_width as i32 <= width as i32);
        debug_assert!(source_y + source_rows as i32 <= full_rows as i32 + 2 * V_CENTRE_SLACK);

        let direct = self.phosphor_alpha == 0 && (!lace || !self.enabled);
        if direct {
            if rows != self.field_rows || width != self.field_width {
                self.field_rows = rows;
                self.field_width = width;
            }
            self.have = [false; 2];
            self.have2 = [false; 2];
            self.out_rows = full_rows;
            self.out_width = width;
        } else {
            self.push_field(field, rows, width, lace, long_field, double_rows);
            self.present_with_phosphor_elapsed(elapsed_fields.max(1) - 1);
        }

        // One destination row: source columns [source_x, source_x +
        // destination_width) of `row`, black where the window has slid off
        // the captured raster.
        let copy_row = |dst: &mut [u32], row: &[u32]| {
            let left_pad = (-source_x).clamp(0, destination_width as i32) as usize;
            let copied =
                (width as i32 - source_x).clamp(0, (destination_width - left_pad) as i32) as usize;
            dst[..left_pad].fill(0xFF00_0000);
            dst[left_pad + copied..].fill(0xFF00_0000);
            if copied > 0 {
                let src_x = source_x.max(0) as usize;
                dst[left_pad..left_pad + copied].copy_from_slice(&row[src_x..src_x + copied]);
            }
        };
        destination.resize(destination_width * destination_rows, 0);
        if direct {
            for (y, dst) in destination.chunks_exact_mut(destination_width).enumerate() {
                let woven_y = source_y
                    + crate::screenshot::scaled_source_row(y, source_rows, destination_rows) as i32;
                if !(0..full_rows as i32).contains(&woven_y) {
                    dst.fill(0xFF00_0000);
                    continue;
                }
                let source_row = if doubled {
                    woven_y as usize / 2
                } else {
                    woven_y as usize
                };
                copy_row(dst, &field[source_row * width..(source_row + 1) * width]);
            }
        } else {
            let source = self.output();
            for (y, dst) in destination.chunks_exact_mut(destination_width).enumerate() {
                let source_row = source_y
                    + crate::screenshot::scaled_source_row(y, source_rows, destination_rows) as i32;
                if !(0..full_rows as i32).contains(&source_row) {
                    dst.fill(0xFF00_0000);
                    continue;
                }
                let source_row = source_row as usize;
                copy_row(dst, &source[source_row * width..(source_row + 1) * width]);
            }
        }
        (destination_rows, destination_width)
    }
}

/// Woven rows the region window may extend past either end of the field: a
/// V-centred aperture slides at most the knob's range off the captured
/// raster (`config::TV_V_CENTRE_RANGE` scan lines, two woven rows each).
const V_CENTRE_SLACK: i32 = 2 * crate::config::TV_V_CENTRE_RANGE;

/// Channel-wise average of two packed RGBA pixels.
fn avg_rgba(a: u32, b: u32) -> u32 {
    ((a ^ b) & 0xFEFE_FEFE) / 2 + (a & b)
}

/// Channel-wise blend of two packed RGBA pixels:
/// `new * (256 - a) / 256 + old * a / 256` with `a` in 0..=255. The two
/// 8-bit channel pairs are processed in parallel in their 16-bit lanes
/// (255 * 256 fits in 16 bits).
fn blend_rgba(new: u32, old: u32, a: u32) -> u32 {
    let na = 256 - a;
    let rb = (((new & 0x00FF_00FF) * na + (old & 0x00FF_00FF) * a) >> 8) & 0x00FF_00FF;
    let ag =
        ((((new >> 8) & 0x00FF_00FF) * na + ((old >> 8) & 0x00FF_00FF) * a) >> 8) & 0x00FF_00FF;
    (ag << 8) | rb
}

/// Effective old-frame coefficient after `fields` identical persistence
/// blends, kept in the deinterlacer's 8-bit fixed-point convention.
fn elapsed_phosphor_alpha(alpha: u32, fields: u32) -> u32 {
    if fields <= 1 {
        return alpha;
    }
    (((alpha as f64 / 256.0).powf(f64::from(fields)) * 256.0).round() as u32).min(255)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field_filled_rows(make: impl Fn(usize) -> u32) -> Vec<u32> {
        let mut f = vec![0u32; FB_PIXELS];
        for y in 0..FB_HEIGHT {
            f[y * FB_WIDTH..(y + 1) * FB_WIDTH].fill(make(y));
        }
        f
    }

    fn out_row(d: &Deinterlacer, r: usize) -> u32 {
        let row = &d.output()[r * FB_WIDTH..(r + 1) * FB_WIDTH];
        assert!(row.iter().all(|&p| p == row[0]), "row {r} not uniform");
        row[0]
    }

    #[test]
    fn progressive_fields_are_line_doubled() {
        let mut d = Deinterlacer::with_options(true, 0.0);
        let f = field_filled_rows(|y| y as u32 + 1);
        d.push_field(&f, FB_HEIGHT, FB_WIDTH, false, true, true);
        for y in 0..FB_HEIGHT {
            assert_eq!(out_row(&d, 2 * y), y as u32 + 1);
            assert_eq!(out_row(&d, 2 * y + 1), y as u32 + 1);
        }
    }

    #[test]
    fn direct_progressive_presentation_matches_deinterlacer_output() {
        let field: Vec<u32> = (0..FB_PIXELS).map(|idx| 0xFF00_0000 | idx as u32).collect();
        let mut reference = Deinterlacer::with_options(true, 0.0);
        reference.push_field(&field, FB_HEIGHT, FB_WIDTH, false, true, true);

        let mut direct = Deinterlacer::with_options(true, 0.0);
        let mut destination = Vec::new();
        let dims = direct.present_field_into(
            &field,
            FB_HEIGHT,
            FB_WIDTH,
            false,
            true,
            true,
            &mut destination,
        );

        assert_eq!(dims, (reference.output_rows(), reference.output_width()));
        assert_eq!(
            destination,
            reference.output()[..reference.output_rows() * reference.output_width()]
        );
    }

    #[test]
    fn direct_progressive_region_matches_crop_of_deinterlacer_output() {
        let field: Vec<u32> = (0..FB_PIXELS).map(|idx| 0xFF00_0000 | idx as u32).collect();
        let mut reference = Deinterlacer::with_options(true, 0.0);
        reference.push_field(&field, FB_HEIGHT, FB_WIDTH, false, true, true);

        let source_x = 19;
        let source_y = 18;
        let source_rows = 431;
        let destination_width = 127;
        let destination_rows = 263;
        let mut expected = vec![0; destination_width * destination_rows];
        for (y, row) in expected.chunks_exact_mut(destination_width).enumerate() {
            let source_row =
                source_y + crate::screenshot::scaled_source_row(y, source_rows, destination_rows);
            let offset = source_row * FB_WIDTH + source_x;
            row.copy_from_slice(&reference.output()[offset..offset + destination_width]);
        }

        let mut direct = Deinterlacer::with_options(true, 0.0);
        let mut destination = Vec::new();
        let dims = direct.present_field_region_into(
            &field,
            FB_HEIGHT,
            FB_WIDTH,
            false,
            true,
            true,
            source_x as i32,
            source_y as i32,
            source_rows,
            destination_width,
            destination_rows,
            &mut destination,
        );

        assert_eq!(dims, (destination_rows, destination_width));
        assert_eq!(destination, expected);
    }

    /// An H/V-centred aperture that slides past the captured raster gets
    /// black for the unscanned glass, never edge-repeated pixels.
    #[test]
    fn region_window_off_the_capture_fills_black() {
        let field: Vec<u32> = (0..FB_PIXELS).map(|idx| 0xFF00_0000 | idx as u32).collect();
        let black = 0xFF00_0000u32;
        let source_rows = 431;
        let destination_width = 127;
        let destination_rows = 263;

        // Window sliding off the left edge: the first columns are black,
        // the copied span starts at the field's own column 0.
        let mut direct = Deinterlacer::with_options(true, 0.0);
        let mut destination = Vec::new();
        direct.present_field_region_into(
            &field,
            FB_HEIGHT,
            FB_WIDTH,
            false,
            true,
            true,
            -3,
            18,
            source_rows,
            destination_width,
            destination_rows,
            &mut destination,
        );
        let row = &destination[..destination_width];
        assert!(row[..3].iter().all(|&px| px == black));
        let woven = crate::screenshot::scaled_source_row(0, source_rows, destination_rows) + 18;
        assert_eq!(row[3], field[(woven / 2) * FB_WIDTH]);

        // Window sliding past the bottom: rows mapped past the field are
        // whole black rows.
        let mut direct = Deinterlacer::with_options(true, 0.0);
        direct.present_field_region_into(
            &field,
            FB_HEIGHT,
            FB_WIDTH,
            false,
            true,
            true,
            19,
            (2 * FB_HEIGHT - source_rows + 4) as i32,
            source_rows,
            destination_width,
            destination_rows,
            &mut destination,
        );
        let last = &destination[(destination_rows - 1) * destination_width..];
        assert!(last.iter().all(|&px| px == black));
    }

    #[test]
    fn interlaced_region_preserves_history_dependent_output() {
        let long = field_filled_rows(|y| 0x1000 + y as u32);
        let short = field_filled_rows(|y| 0x2000 + y as u32);
        let mut reference = Deinterlacer::with_options(true, 0.0);
        let mut cropped = Deinterlacer::with_options(true, 0.0);
        for (field, long_field) in [(&long, true), (&short, false), (&long, true)] {
            reference.push_field(field, FB_HEIGHT, FB_WIDTH, true, long_field, true);
            cropped.push_field(field, FB_HEIGHT, FB_WIDTH, true, long_field, true);
        }
        reference.push_field(&short, FB_HEIGHT, FB_WIDTH, true, false, true);

        let source_x = 11;
        let source_y = 17;
        let source_rows = 421;
        let destination_width = 83;
        let destination_rows = 271;
        let mut expected = vec![0; destination_width * destination_rows];
        for (y, row) in expected.chunks_exact_mut(destination_width).enumerate() {
            let source_row =
                source_y + crate::screenshot::scaled_source_row(y, source_rows, destination_rows);
            let offset = source_row * FB_WIDTH + source_x;
            row.copy_from_slice(&reference.output()[offset..offset + destination_width]);
        }

        let mut destination = Vec::new();
        cropped.present_field_region_into(
            &short,
            FB_HEIGHT,
            FB_WIDTH,
            true,
            false,
            true,
            source_x as i32,
            source_y as i32,
            source_rows,
            destination_width,
            destination_rows,
            &mut destination,
        );
        assert_eq!(destination, expected);
    }

    #[test]
    fn direct_presentation_clamps_rows_before_validating_input() {
        let width = 4;
        let field = vec![0xFF12_3456; MAX_VISIBLE_LINES * width];
        let mut direct = Deinterlacer::with_options(true, 0.0);
        let mut destination = Vec::new();

        let dims = direct.present_field_into(
            &field,
            MAX_VISIBLE_LINES + 1,
            width,
            false,
            true,
            false,
            &mut destination,
        );

        assert_eq!(dims, (MAX_VISIBLE_LINES, width));
        assert_eq!(destination, field);
    }

    #[test]
    fn static_lace_fields_weave_to_full_resolution() {
        let mut d = Deinterlacer::with_options(true, 0.0);
        // Distinct per-parity content, as an interlaced display's odd and
        // even picture rows would be.
        let long = field_filled_rows(|y| 0x1000 + y as u32);
        let short = field_filled_rows(|y| 0x2000 + y as u32);
        // Two full field pairs: the second pair is static against the
        // first, so every opposite-parity line weaves.
        d.push_field(&long, FB_HEIGHT, FB_WIDTH, true, true, true);
        d.push_field(&short, FB_HEIGHT, FB_WIDTH, true, false, true);
        d.push_field(&long, FB_HEIGHT, FB_WIDTH, true, true, true);
        d.push_field(&short, FB_HEIGHT, FB_WIDTH, true, false, true);
        for y in 0..FB_HEIGHT {
            assert_eq!(out_row(&d, 2 * y), 0x1000 + y as u32, "even row {y}");
            assert_eq!(out_row(&d, 2 * y + 1), 0x2000 + y as u32, "odd row {y}");
        }
    }

    #[test]
    fn motion_interpolates_instead_of_combing() {
        let mut d = Deinterlacer::with_options(true, 0.0);
        let long_a = field_filled_rows(|_| 0x10);
        let short_a = field_filled_rows(|_| 0x30);
        d.push_field(&long_a, FB_HEIGHT, FB_WIDTH, true, true, true);
        d.push_field(&short_a, FB_HEIGHT, FB_WIDTH, true, false, true);
        d.push_field(&long_a, FB_HEIGHT, FB_WIDTH, true, true, true);
        d.push_field(&short_a, FB_HEIGHT, FB_WIDTH, true, false, true);
        // Static so far: full weave.
        assert_eq!(out_row(&d, 10), 0x10);
        assert_eq!(out_row(&d, 11), 0x30);
        // The short field changes everywhere: its own rows update at once,
        // and the following long field must not weave the one-field-stale
        // short lines back in as a ghost - it interpolates its own
        // neighbours there until the short content settles.
        let short_b = field_filled_rows(|_| 0x50);
        d.push_field(&short_b, FB_HEIGHT, FB_WIDTH, true, false, true);
        assert_eq!(out_row(&d, 11), 0x50);
        d.push_field(&long_a, FB_HEIGHT, FB_WIDTH, true, true, true);
        assert_eq!(out_row(&d, 10), 0x10);
        assert_eq!(out_row(&d, 11), avg_rgba(0x10, 0x10));
        // The short content settles: weave resumes with its next pair.
        d.push_field(&short_b, FB_HEIGHT, FB_WIDTH, true, false, true);
        d.push_field(&long_a, FB_HEIGHT, FB_WIDTH, true, true, true);
        assert_eq!(out_row(&d, 11), 0x50);
        // Now the long field moves: its own rows update immediately and
        // the short-parity rows interpolate the new long field instead of
        // keeping stale short_b lines.
        let long_b = field_filled_rows(|_| 0x70);
        d.push_field(&long_b, FB_HEIGHT, FB_WIDTH, true, true, true);
        assert_eq!(out_row(&d, 10), 0x70);
        assert_eq!(out_row(&d, 11), avg_rgba(0x70, 0x70));
    }

    #[test]
    fn lace_to_progressive_switch_drops_field_history() {
        let mut d = Deinterlacer::with_options(true, 0.0);
        let long = field_filled_rows(|_| 0x11);
        let short = field_filled_rows(|_| 0x22);
        d.push_field(&long, FB_HEIGHT, FB_WIDTH, true, true, true);
        d.push_field(&short, FB_HEIGHT, FB_WIDTH, true, false, true);
        let prog = field_filled_rows(|_| 0x33);
        d.push_field(&prog, FB_HEIGHT, FB_WIDTH, false, true, true);
        assert_eq!(out_row(&d, 10), 0x33);
        assert_eq!(out_row(&d, 11), 0x33);
        // Lace resumes: no stale pre-switch lines weave back in; the
        // missing parity interpolates until its field arrives.
        d.push_field(&long, FB_HEIGHT, FB_WIDTH, true, true, true);
        assert_eq!(out_row(&d, 10), 0x11);
        assert_eq!(out_row(&d, 11), 0x11);
    }

    /// A programmable progressive scan already carries every output line:
    /// it presents at native height instead of line-doubling.
    #[test]
    fn programmable_progressive_field_passes_through_at_native_rows() {
        let mut d = Deinterlacer::with_options(true, 0.0);
        let rows = 552usize;
        let mut f = vec![0u32; rows * FB_WIDTH];
        for y in 0..rows {
            f[y * FB_WIDTH..(y + 1) * FB_WIDTH].fill(y as u32 + 1);
        }
        d.push_field(&f, rows, FB_WIDTH, false, true, false);
        assert_eq!(d.output_rows(), rows);
        for y in (0..rows).step_by(97) {
            assert_eq!(out_row(&d, y), y as u32 + 1);
        }
    }

    /// Fields of a different scan height must not weave with the old
    /// history (mode switch): the first field of the new geometry
    /// interpolates instead of resurrecting stale lines.
    #[test]
    fn field_row_count_change_drops_weave_history() {
        let mut d = Deinterlacer::with_options(true, 0.0);
        let long = field_filled_rows(|_| 0x11);
        let short = field_filled_rows(|_| 0x22);
        d.push_field(&long, FB_HEIGHT, FB_WIDTH, true, true, true);
        d.push_field(&short, FB_HEIGHT, FB_WIDTH, true, false, true);
        assert_eq!(d.output_rows(), OUT_HEIGHT);

        // A shorter laced scan arrives: nothing from the 285-row fields
        // may weave into its missing parity.
        let rows = 200usize;
        let mut f = vec![0u32; rows * FB_WIDTH];
        f.fill(0x77);
        d.push_field(&f, rows, FB_WIDTH, true, true, true);
        assert_eq!(d.output_rows(), rows * 2);
        assert_eq!(out_row(&d, 10), 0x77);
        assert_eq!(out_row(&d, 11), 0x77);
    }

    #[test]
    fn disabled_deinterlacer_line_doubles_lace_fields() {
        let mut d = Deinterlacer::with_options(false, 0.0);
        let long = field_filled_rows(|y| y as u32);
        let short = field_filled_rows(|y| 0x8000 + y as u32);
        d.push_field(&long, FB_HEIGHT, FB_WIDTH, true, true, true);
        d.push_field(&short, FB_HEIGHT, FB_WIDTH, true, false, true);
        for y in 0..FB_HEIGHT {
            assert_eq!(out_row(&d, 2 * y), 0x8000 + y as u32);
            assert_eq!(out_row(&d, 2 * y + 1), 0x8000 + y as u32);
        }
    }

    #[test]
    fn avg_rgba_averages_each_channel() {
        assert_eq!(avg_rgba(0x00FF_00FF, 0x00FF_00FF), 0x00FF_00FF);
        assert_eq!(avg_rgba(0xFF00_FF00, 0x0000_0000), 0x7F00_7F00);
        assert_eq!(avg_rgba(0x0000_00FE, 0x0000_0000), 0x0000_007F);
        assert_eq!(avg_rgba(0x1010_1010, 0x3030_3030), 0x2020_2020);
    }

    #[test]
    fn blend_rgba_mixes_each_channel_by_alpha() {
        // a=0: the new frame only.
        assert_eq!(blend_rgba(0x1122_3344, 0xFFFF_FFFF, 0), 0x1122_3344);
        // a=128: halfway.
        assert_eq!(blend_rgba(0xFF00_FF00, 0x0000_0000, 128), 0x7F00_7F00);
        assert_eq!(blend_rgba(0x0000_0000, 0x00FF_00FF, 128), 0x007F_007F);
        // Channels never bleed into their neighbours.
        assert_eq!(blend_rgba(0x00FF_0000, 0x0000_FF00, 128), 0x007F_7F00);
    }

    #[test]
    fn phosphor_persistence_leaves_an_exponential_trail() {
        let mut d = Deinterlacer::with_options(true, 0.5);
        let bright = field_filled_rows(|_| 0x00FF_FFFF);
        let black = field_filled_rows(|_| 0);
        // The first frame seeds the blend buffer and presents at full
        // brightness, exactly as the threaded pipeline's set_phosphor
        // switch-on does - no fade-in from black.
        d.push_field(&bright, FB_HEIGHT, FB_WIDTH, false, true, true);
        assert_eq!(out_row(&d, 10), 0x00FF_FFFF);
        // A black frame keeps half of the previous output as the trail,
        // and each further frame halves it again.
        d.push_field(&black, FB_HEIGHT, FB_WIDTH, false, true, true);
        assert_eq!(out_row(&d, 10), 0x007F_7F7F);
        d.push_field(&black, FB_HEIGHT, FB_WIDTH, false, true, true);
        assert_eq!(out_row(&d, 10), 0x003F_3F3F);
    }

    #[test]
    fn deferred_fields_advance_phosphor_before_the_latest_presentation() {
        let mut d = Deinterlacer::with_options(true, 0.5);
        let bright = field_filled_rows(|_| 0x00FF_FFFF);
        let black = field_filled_rows(|_| 0);
        let mut destination = Vec::new();

        d.present_field_into_elapsed(
            &bright,
            FB_HEIGHT,
            FB_WIDTH,
            false,
            true,
            true,
            1,
            &mut destination,
        );
        d.present_field_into_elapsed(
            &black,
            FB_HEIGHT,
            FB_WIDTH,
            false,
            true,
            true,
            3,
            &mut destination,
        );

        assert_eq!(destination[10 * FB_WIDTH], 0x001F_1F1F);
    }

    #[test]
    fn zero_phosphor_presents_the_woven_frame_untouched() {
        let mut d = Deinterlacer::with_options(true, 0.0);
        let f = field_filled_rows(|_| 0x0012_3456);
        d.push_field(&f, FB_HEIGHT, FB_WIDTH, false, true, true);
        assert_eq!(out_row(&d, 10), 0x0012_3456);
        assert!(d.presented.is_none(), "no blend buffer when disabled");
    }

    #[test]
    fn disabled_effects_keep_history_scratch_unallocated() {
        let mut d = Deinterlacer::with_settings(false, 0.0);
        let field = field_filled_rows(|_| 0x0012_3456);
        let mut destination = Vec::new();

        d.present_field_into(
            &field,
            FB_HEIGHT,
            FB_WIDTH,
            false,
            true,
            true,
            &mut destination,
        );

        assert!(!d.deinterlace_enabled());
        assert_eq!(d.phosphor(), 0.0);
        assert!(d.out.is_empty());
        assert!(d.prev.iter().all(Vec::is_empty));
        assert!(d.prev2.iter().all(Vec::is_empty));
        assert!(d.moved.is_empty());
        assert!(d.presented.is_none());

        // Enabling the effect itself is cheap; the buffers appear only
        // when an interlaced field actually exercises it.
        d.set_deinterlace(true);
        assert!(d.prev.iter().all(Vec::is_empty));
        d.present_field_into(
            &field,
            FB_HEIGHT,
            FB_WIDTH,
            true,
            true,
            true,
            &mut destination,
        );
        assert!(!d.out.is_empty());
        assert!(d.prev.iter().all(|buf| !buf.is_empty()));
        assert!(!d.moved.is_empty());
    }

    #[test]
    fn set_phosphor_switches_persistence_live() {
        let mut d = Deinterlacer::with_options(true, 0.0);
        let bright = field_filled_rows(|_| 0x00FF_FFFF);
        let black = field_filled_rows(|_| 0);
        d.push_field(&bright, FB_HEIGHT, FB_WIDTH, false, true, true);
        assert_eq!(out_row(&d, 10), 0x00FF_FFFF);
        // Switching on seeds the trail from the next woven frame rather
        // than fading the picture in from a black blend buffer.
        d.set_phosphor(0.5);
        d.push_field(&bright, FB_HEIGHT, FB_WIDTH, false, true, true);
        assert_eq!(out_row(&d, 10), 0x00FF_FFFF);
        d.push_field(&black, FB_HEIGHT, FB_WIDTH, false, true, true);
        assert_eq!(out_row(&d, 10), 0x007F_7F7F);
        // Switching off drops the blend buffer: the woven frame passes
        // through untouched immediately.
        d.set_phosphor(0.0);
        d.push_field(&black, FB_HEIGHT, FB_WIDTH, false, true, true);
        assert_eq!(out_row(&d, 10), 0);
        assert!(d.presented.is_none(), "blend buffer dropped when off");
    }

    #[test]
    fn reset_history_starts_a_fresh_phosphor_trail() {
        let mut d = Deinterlacer::with_options(true, 0.5);
        let bright = field_filled_rows(|_| 0x00FF_FFFF);
        d.push_field(&bright, FB_HEIGHT, FB_WIDTH, false, true, true);
        d.push_field(&bright, FB_HEIGHT, FB_WIDTH, false, true, true);
        // A new presentation stream: no glow of the old picture may
        // survive into its first frame.
        d.reset_history();
        let black = field_filled_rows(|_| 0);
        d.push_field(&black, FB_HEIGHT, FB_WIDTH, false, true, true);
        assert_eq!(out_row(&d, 10), 0);
    }

    #[test]
    fn set_deinterlace_switches_field_merging_live() {
        let mut d = Deinterlacer::with_options(true, 0.0);
        let long = field_filled_rows(|_| 0x11);
        let short = field_filled_rows(|_| 0x22);
        d.push_field(&long, FB_HEIGHT, FB_WIDTH, true, true, true);
        d.push_field(&short, FB_HEIGHT, FB_WIDTH, true, false, true);
        assert_eq!(out_row(&d, 10), 0x11);
        assert_eq!(out_row(&d, 11), 0x22);
        // Off: the next field is plain line-doubled, no weave.
        d.set_deinterlace(false);
        d.push_field(&long, FB_HEIGHT, FB_WIDTH, true, true, true);
        assert_eq!(out_row(&d, 10), 0x11);
        assert_eq!(out_row(&d, 11), 0x11);
        // Back on: the switch dropped the history, so the first laced
        // field interpolates rather than weaving stale lines, and a full
        // pair weaves again.
        d.set_deinterlace(true);
        d.push_field(&long, FB_HEIGHT, FB_WIDTH, true, true, true);
        assert_eq!(out_row(&d, 11), 0x11);
        d.push_field(&short, FB_HEIGHT, FB_WIDTH, true, false, true);
        d.push_field(&long, FB_HEIGHT, FB_WIDTH, true, true, true);
        d.push_field(&short, FB_HEIGHT, FB_WIDTH, true, false, true);
        assert_eq!(out_row(&d, 10), 0x11);
        assert_eq!(out_row(&d, 11), 0x22);
    }

    #[test]
    fn reset_history_drops_the_weave_history() {
        let mut d = Deinterlacer::with_options(true, 0.0);
        let long = field_filled_rows(|_| 0x11);
        let short = field_filled_rows(|_| 0x22);
        d.push_field(&long, FB_HEIGHT, FB_WIDTH, true, true, true);
        d.push_field(&short, FB_HEIGHT, FB_WIDTH, true, false, true);
        assert_eq!(out_row(&d, 11), 0x22);
        d.reset_history();
        // The stale short field must not weave back in; the missing
        // parity interpolates the fresh field until its own arrives.
        d.push_field(&long, FB_HEIGHT, FB_WIDTH, true, true, true);
        assert_eq!(out_row(&d, 10), 0x11);
        assert_eq!(out_row(&d, 11), 0x11);
    }
}
