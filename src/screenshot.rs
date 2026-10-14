// SPDX-License-Identifier: GPL-3.0-or-later

//! Save the current framebuffer as a PNG. Useful for debugging the
//! video pipeline from a headless run (--screenshot-after) and for
//! capturing snapshots interactively (host screenshot shortcut in the window).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Capture original chipset pixels, cropped to the renderer's active
/// playfield envelope, or an RTG board's native scanout. Chipset output is
/// the current field without presentation scaling or phosphor; when it and
/// the field before it are the two halves of an interlaced picture, they
/// are woven at full height (long field on the even rows).
/// Returns `(pixels, height, width)`; available to core-only frontends too.
pub fn render_native(bus: &crate::bus::Bus) -> (Vec<u32>, usize, usize) {
    let mut fb = Vec::new();
    if let Some((width, height)) = bus.rtg_frame(&mut fb) {
        return (fb, height as usize, width as usize);
    }
    let input = crate::video::bitplane::RenderInput::from_bus(bus);
    let field = NativeField::render(&input);
    if let Some(previous) = bus
        .native_previous_field()
        .filter(|previous| fields_weave(&input, previous))
    {
        let other = NativeField::render(previous);
        return if input.render_base().long_field {
            weave_native(&field, &other)
        } else {
            weave_native(&other, &field)
        };
    }
    crop_native(&field.fb, field.width, field.rows, field.rect, field.repeat)
}

/// One chipset field rendered at its finest canvas pitch, before cropping.
struct NativeField {
    fb: Vec<u32>,
    width: usize,
    rows: usize,
    rect: Option<crate::video::bitplane::ContentRect>,
    repeat: usize,
}

impl NativeField {
    fn render(input: &crate::video::bitplane::RenderInput) -> Self {
        let canvas_scale = input.native_canvas_scale();
        let mut fb = vec![0; crate::video::MAX_CANVAS_PIXELS];
        let result = crate::video::bitplane::render_native_from_input(input, &mut fb);
        Self {
            fb,
            width: crate::video::FB_WIDTH * canvas_scale,
            rows: input.geometry().visible_lines,
            rect: result
                .content_rect
                .map(|r| crate::video::bitplane::ContentRect {
                    x0: r.x0 * canvas_scale,
                    x1: r.x1 * canvas_scale,
                    ..r
                }),
            // A mixed-resolution frame uses its finest programmed pixel pitch.
            repeat: input.native_horizontal_repeat(),
        }
    }
}

/// Two consecutive fields are the halves of one interlaced picture when
/// both were laced, one is the long field and the other the short one, and
/// they share a scan and canvas pitch: the same line length and row-0 beam
/// line, with no more than the one line a long field adds over a short one
/// in either the rows captured or the frame. A change of video standard or
/// of a programmable scan's vertical timing starts a new picture.
fn fields_weave(
    current: &crate::video::bitplane::RenderInput,
    previous: &crate::video::bitplane::RenderInput,
) -> bool {
    const LACE: u16 = 0x0004;
    let (now, before) = (current.render_base(), previous.render_base());
    let (scan, previous_scan) = (current.geometry(), previous.geometry());
    now.bplcon0 & LACE != 0
        && before.bplcon0 & LACE != 0
        && now.long_field != before.long_field
        && scan.programmable == previous_scan.programmable
        && scan.line_cck == previous_scan.line_cck
        && scan.visible_start_vpos == previous_scan.visible_start_vpos
        && current.visible_start_vpos() == previous.visible_start_vpos()
        && scan.visible_lines.abs_diff(previous_scan.visible_lines) <= 1
        && scan.frame_lines.abs_diff(previous_scan.frame_lines) <= 1
        && current.native_canvas_scale() == previous.native_canvas_scale()
}

/// Interleave a long and a short field row by row (long field on the even
/// rows) and crop the result to the union of both fields' playfields.
fn weave_native(long: &NativeField, short: &NativeField) -> (Vec<u32>, usize, usize) {
    use crate::video::bitplane::ContentRect;
    let width = long.width;
    let rows = long.rows.min(short.rows);
    let mut woven = Vec::with_capacity(width * rows * 2);
    for y in 0..rows {
        woven.extend_from_slice(&long.fb[y * width..(y + 1) * width]);
        woven.extend_from_slice(&short.fb[y * width..(y + 1) * width]);
    }
    let field_rect = match (long.rect, short.rect) {
        (Some(a), Some(b)) => Some(ContentRect {
            x0: a.x0.min(b.x0),
            x1: a.x1.max(b.x1),
            y0: a.y0.min(b.y0),
            y1: a.y1.max(b.y1),
        }),
        (rect, None) | (None, rect) => rect,
    };
    let rect = field_rect.map(|r| ContentRect {
        y0: 2 * r.y0.min(rows),
        y1: 2 * r.y1.min(rows),
        ..r
    });
    let repeat = if long.repeat == short.repeat {
        long.repeat
    } else {
        1
    };
    crop_native(&woven, width, rows * 2, rect, repeat)
}

/// Collapse only exact repeated samples. Copper effects or sprite edges
/// at a finer pitch keep that detail rather than dropping or averaging it.
fn crop_native(
    fb: &[u32],
    width: usize,
    rows: usize,
    rect: Option<crate::video::bitplane::ContentRect>,
    repeat: usize,
) -> (Vec<u32>, usize, usize) {
    let rect = rect.unwrap_or(crate::video::bitplane::ContentRect {
        x0: 0,
        x1: width,
        y0: 0,
        y1: rows,
    });
    let crop_width = rect.x1 - rect.x0;
    let repeat = if crop_width.is_multiple_of(repeat)
        && (rect.y0..rect.y1).all(|y| {
            fb[y * width + rect.x0..y * width + rect.x1]
                .chunks_exact(repeat)
                .all(|group| group.iter().all(|p| *p == group[0]))
        }) {
        repeat
    } else {
        1
    };
    let out_width = crop_width / repeat;
    let out_rows = rect.y1 - rect.y0;
    let mut out = Vec::with_capacity(out_width * out_rows);
    for y in rect.y0..rect.y1 {
        out.extend(
            fb[y * width + rect.x0..y * width + rect.x1]
                .iter()
                .step_by(repeat)
                .copied(),
        );
    }
    (out, out_rows, out_width)
}

/// Encode `fb` (RGBA8 packed in memory order R,G,B,A per pixel, as
/// produced by `video::bitplane::render`) into a PNG at `path`.
pub fn save(path: &Path, fb: &[u32], width: u32, height: u32) -> Result<()> {
    let expected = (width as usize) * (height as usize);
    if fb.len() != expected {
        anyhow::bail!(
            "framebuffer size mismatch: got {} pixels, expected {}x{}={}",
            fb.len(),
            width,
            height,
            expected
        );
    }
    crate::paths::ensure_parent(path)
        .with_context(|| format!("creating the directory for {}", path.display()))?;
    let file =
        std::fs::File::create(path).with_context(|| format!("opening {}", path.display()))?;
    let writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut wr = encoder
        .write_header()
        .with_context(|| format!("writing PNG header to {}", path.display()))?;
    let bytes = unsafe { std::slice::from_raw_parts(fb.as_ptr() as *const u8, fb.len() * 4) };
    wr.write_image_data(bytes)
        .with_context(|| format!("writing PNG data to {}", path.display()))?;
    Ok(())
}

/// Save `fb` with centre-aligned vertical scaling. Used for
/// PAL presentation screenshots, where the internal overscan field
/// buffer should be viewed with non-square pixels. The normal
/// presentation path preserves source-row colours instead of blending
/// adjacent scanlines; filtered output belongs behind an explicit
/// display filter.
pub fn save_scaled_y(
    path: &Path,
    fb: &[u32],
    width: u32,
    height: u32,
    out_height: u32,
) -> Result<()> {
    let expected = (width as usize) * (height as usize);
    // The source may be a maximum-sized presentation buffer with only the
    // leading `height` rows active.
    if fb.len() < expected {
        anyhow::bail!(
            "framebuffer size mismatch: got {} pixels, expected at least {}x{}={}",
            fb.len(),
            width,
            height,
            expected
        );
    }
    let fb = &fb[..expected];
    if out_height == height {
        return save(path, fb, width, height);
    }

    let mut scaled = Vec::new();
    scale_y_into(
        fb,
        width as usize,
        height as usize,
        out_height as usize,
        &mut scaled,
    );
    save(path, &scaled, width, out_height)
}

/// Centre-aligned source row for presentation row `y`.
#[inline]
pub fn scaled_source_row(y: usize, src_rows: usize, dst_rows: usize) -> usize {
    (((2 * y + 1) * src_rows) / (2 * dst_rows)).min(src_rows.saturating_sub(1))
}

/// Centre-aligned vertical resample of `fb` into `scaled` (cleared and
/// resized). Shared by the presentation screenshot writer and the video
/// recorder, which scale the same field buffer per frame.
pub fn scale_y_into(fb: &[u32], width: usize, height: usize, out: usize, scaled: &mut Vec<u32>) {
    debug_assert!(fb.len() >= width * height);
    scaled.clear();
    scaled.resize(width * out, 0);
    for y in 0..out {
        let src_y = scaled_source_row(y, height, out);
        let row = &fb[src_y * width..(src_y + 1) * width];
        let dst = &mut scaled[y * width..(y + 1) * width];
        dst.copy_from_slice(row);
    }
}

/// Narrow a wide canvas to `dst_width` pixels per row by averaging each
/// source pixel group (cleared and resized). Consumers whose frame width
/// is fixed (the video recorder) use this to fold a 35 ns-pitch canvas
/// back to the classic width.
pub fn downsample_x_into(
    fb: &[u32],
    src_width: usize,
    rows: usize,
    dst_width: usize,
    out: &mut Vec<u32>,
) {
    debug_assert!(fb.len() >= src_width * rows);
    debug_assert!(src_width >= dst_width && src_width.is_multiple_of(dst_width));
    let group = (src_width / dst_width).max(1);
    out.clear();
    out.resize(dst_width * rows, 0);
    for y in 0..rows {
        let src = &fb[y * src_width..(y + 1) * src_width];
        let dst = &mut out[y * dst_width..(y + 1) * dst_width];
        for (x, px) in dst.iter_mut().enumerate() {
            if group == 2 {
                // Overflow-free per-channel mean of the pair (the 35 ns
                // canvas case).
                let a = src[x * 2];
                let b = src[x * 2 + 1];
                *px = ((a ^ b) & 0xFEFE_FEFE) / 2 + (a & b);
            } else {
                let mut sums = [0u32; 4];
                for &p in &src[x * group..(x + 1) * group] {
                    for (lane, sum) in sums.iter_mut().enumerate() {
                        *sum += (p >> (lane * 8)) & 0xFF;
                    }
                }
                let n = group as u32;
                *px = sums
                    .iter()
                    .enumerate()
                    .fold(0u32, |acc, (lane, sum)| acc | ((sum / n) << (lane * 8)));
            }
        }
    }
}

/// Centre-aligned bilinear horizontal resample, in place, of the leading
/// `rows` rows of the `width`-pixel-wide `fb`: output pixel x samples
/// source position x * src_num / src_den. The presentation uses this to
/// map a programmable scan's line onto the fixed glass width - a
/// multisync monitor's horizontal deflection is time-linear, so a colour
/// clock of a short (e.g. 31 kHz, ~130-cck) line covers proportionally
/// more of the screen than one of a 227-cck standard line
/// (src_num = line_cck, src_den = 227). Source pixels pushed past the
/// right edge by a factor > 1 are cut; a factor < 1 leaves black on the
/// right.
pub fn stretch_rows_x(fb: &mut [u32], width: usize, rows: usize, src_num: u32, src_den: u32) {
    debug_assert!(fb.len() >= width * rows);
    if src_num == src_den || width == 0 {
        return;
    }
    let mut scratch = vec![0u32; width];
    for y in 0..rows {
        let row = &mut fb[y * width..(y + 1) * width];
        scratch.copy_from_slice(row);
        for (x, out) in row.iter_mut().enumerate() {
            let (src_x0, frac) = stretch_rows_x_source(x, width, src_num, src_den);
            *out = if frac == 0 || src_x0 + 1 >= width {
                scratch[src_x0]
            } else {
                crate::video::blend_rgba(scratch[src_x0], scratch[src_x0 + 1], frac)
            };
        }
    }
}

/// The source taps output column `x` of [`stretch_rows_x`] reads: the
/// integer source column and the 8-bit fraction toward its right
/// neighbour (a zero fraction, or a neighbour past the row end, reads
/// the one column alone). The resample and the presentation's
/// content-envelope mapping share this, so a crop follows exactly the
/// columns the resample painted.
#[inline]
pub fn stretch_rows_x_source(x: usize, width: usize, src_num: u32, src_den: u32) -> (usize, u32) {
    // Source-pixel centre in 24.8 fixed point:
    // (x + 0.5) * src_num / src_den - 0.5.
    let pos = ((2 * x as i64 + 1) * src_num as i64 * 128 / src_den as i64 - 128)
        .clamp(0, ((width - 1) as i64) << 8) as usize;
    (pos >> 8, (pos & 0xFF) as u32)
}

/// Like [`stretch_rows_x`], but resampling an explicit source window:
/// output pixel x samples source position src_x0 + (x + 0.5) * src_w /
/// width - 0.5. The presentation uses this to show the visible interval
/// of a programmable scan the way a multisync monitor does: the glass is
/// anchored at the horizontal sync pulse, not at the capture buffer's
/// left edge, so `src_x0` may be negative (the sync tail sits left of
/// the captured aperture) and positions outside the buffer clamp to its
/// edge columns (the border colour).
pub fn stretch_rows_x_window(fb: &mut [u32], width: usize, rows: usize, src_x0: i32, src_w: u32) {
    debug_assert!(fb.len() >= width * rows);
    if width == 0 || src_w == 0 || (src_x0 == 0 && src_w as usize == width) {
        return;
    }
    let mut scratch = vec![0u32; width];
    for y in 0..rows {
        let row = &mut fb[y * width..(y + 1) * width];
        scratch.copy_from_slice(row);
        for (x, out) in row.iter_mut().enumerate() {
            let (src_x0i, frac) = stretch_rows_x_window_source(x, width, src_x0, src_w);
            *out = if frac == 0 || src_x0i + 1 >= width {
                scratch[src_x0i]
            } else {
                crate::video::blend_rgba(scratch[src_x0i], scratch[src_x0i + 1], frac)
            };
        }
    }
}

/// The source taps output column `x` of [`stretch_rows_x_window`] reads,
/// as [`stretch_rows_x_source`] gives them for the whole-line map.
#[inline]
pub fn stretch_rows_x_window_source(
    x: usize,
    width: usize,
    src_x0: i32,
    src_w: u32,
) -> (usize, u32) {
    // Source-pixel centre in 24.8 fixed point:
    // src_x0 + (x + 0.5) * src_w / width - 0.5.
    let pos =
        ((src_x0 as i64) << 8) + ((2 * x as i64 + 1) * src_w as i64 * 128 / width as i64 - 128);
    let pos = pos.clamp(0, ((width - 1) as i64) << 8) as usize;
    (pos >> 8, (pos & 0xFF) as u32)
}

/// Whether an output column with source taps `taps` (from
/// [`stretch_rows_x_source`] or [`stretch_rows_x_window_source`]) in a
/// `width`-column row reads any source column in `x0..x1`: the tap
/// itself, or the right neighbour a non-zero fraction blends in.
pub fn resampled_column_reads(taps: (usize, u32), width: usize, x0: usize, x1: usize) -> bool {
    let (src, frac) = taps;
    let reads = |column: usize| (x0..x1).contains(&column);
    reads(src) || (frac != 0 && src + 1 < width && reads(src + 1))
}

/// Pick a default filename for an interactive screenshot grab.
pub fn auto_filename() -> PathBuf {
    crate::paths::screenshot_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_crop_keeps_black_content_and_exact_sample_colours() {
        use crate::video::bitplane::ContentRect;
        let black = 0xFF00_0000;
        let red = 0xFF00_00FF;
        let green = 0xFF00_FF00;
        let mut fb = vec![black; 8 * 4];
        fb[10..14].copy_from_slice(&[black, black, red, red]);
        fb[18..22].copy_from_slice(&[green, green, black, black]);
        let rect = ContentRect {
            x0: 2,
            x1: 6,
            y0: 1,
            y1: 3,
        };
        let (pixels, rows, width) = crop_native(&fb, 8, 4, Some(rect), 2);
        assert_eq!((width, rows), (2, 2));
        assert_eq!(pixels, [black, red, green, black]);
        // One subpixel sprite or Copper edge must keep every source sample.
        fb[11] = green;
        let (pixels, rows, width) = crop_native(&fb, 8, 4, Some(rect), 2);
        assert_eq!((width, rows), (4, 2));
        assert_eq!(pixels, [black, green, red, red, green, green, black, black]);
        // Hi-res stays hi-res even when the picture happens to contain pairs.
        assert_eq!(crop_native(&fb, 8, 4, Some(rect), 1).2, 4);
        assert_eq!(crop_native(&fb, 8, 4, None, 1), (fb, 4, 8));
    }

    #[test]
    fn interlaced_fields_weave_long_field_on_even_rows() {
        use crate::video::bitplane::ContentRect;
        let long_px = 0xFF00_00FF;
        let short_px = 0xFF00_FF00;
        let field = |px, rect| NativeField {
            fb: vec![px; 4 * 3],
            width: 4,
            rows: 3,
            rect: Some(rect),
            repeat: 1,
        };
        let long = field(
            long_px,
            ContentRect {
                x0: 1,
                x1: 3,
                y0: 0,
                y1: 2,
            },
        );
        let short = field(
            short_px,
            ContentRect {
                x0: 0,
                x1: 3,
                y0: 1,
                y1: 3,
            },
        );
        // The crop is the union of both playfields, at full picture height.
        let (pixels, rows, width) = weave_native(&long, &short);
        assert_eq!((width, rows), (3, 6));
        for (y, row) in pixels.chunks(width).enumerate() {
            let px = if y % 2 == 0 { long_px } else { short_px };
            assert!(row.iter().all(|p| *p == px), "row {y}");
        }
    }

    #[test]
    fn vertical_presentation_scale_preserves_source_pixel_values() {
        let a = 0x1122_3344;
        let b = 0x5566_7788;
        let fb = [a, b, a, b, a, b];
        let mut scaled = Vec::new();

        scale_y_into(&fb, 1, fb.len(), 5, &mut scaled);

        assert_eq!(scaled.len(), 5);
        assert!(scaled.iter().all(|px| *px == a || *px == b));
    }

    #[test]
    fn downsample_averages_each_source_pixel_group() {
        // 2:1 (the 35 ns canvas case): per-channel mean of the pair.
        let fb = [0xFF00_00FF, 0xFF00_0001, 0x0000_0000, 0x0808_0808];
        let mut out = Vec::new();
        downsample_x_into(&fb, 4, 1, 2, &mut out);
        assert_eq!(out, vec![0xFF00_0080, 0x0404_0404]);

        // Wider groups average too, matching the documented behaviour.
        let fb = [0x0000_0003, 0x0000_0006, 0x0000_0009];
        downsample_x_into(&fb, 3, 1, 1, &mut out);
        assert_eq!(out, vec![0x0000_0006]);
    }

    /// The tap helpers describe exactly which source columns each
    /// resampled column reads: a row carrying one lit column comes out
    /// lit (fully or blended) in precisely the output columns whose taps
    /// read it, under both the whole-line map and a sync-anchored window
    /// that starts left of the buffer.
    #[test]
    fn resample_taps_name_the_columns_the_resamplers_read() {
        const WIDTH: usize = 64;
        let lit = 0xFFFF_FFFF;
        let lit_column = 20;
        let fresh = || {
            let mut fb = vec![0u32; WIDTH];
            fb[lit_column] = lit;
            fb
        };

        let mut linear = fresh();
        stretch_rows_x(&mut linear, WIDTH, 1, 130, 227);
        for (x, px) in linear.iter().enumerate() {
            let taps = stretch_rows_x_source(x, WIDTH, 130, 227);
            let reads = resampled_column_reads(taps, WIDTH, lit_column, lit_column + 1);
            assert_eq!(*px != 0, reads, "linear column {x} taps {taps:?}");
        }

        let mut window = fresh();
        stretch_rows_x_window(&mut window, WIDTH, 1, -8, 40);
        let mut any = false;
        for (x, px) in window.iter().enumerate() {
            let taps = stretch_rows_x_window_source(x, WIDTH, -8, 40);
            let reads = resampled_column_reads(taps, WIDTH, lit_column, lit_column + 1);
            assert_eq!(*px != 0, reads, "window column {x} taps {taps:?}");
            any |= reads;
        }
        assert!(any);

        // Off the source's left edge the window clamps to column 0, so a
        // lit edge column is read by every output column the sync tail
        // covers; a window past the right edge reads the last column.
        assert!(resampled_column_reads(
            stretch_rows_x_window_source(0, WIDTH, -8, 40),
            WIDTH,
            0,
            1
        ));
        assert_eq!(
            stretch_rows_x_window_source(WIDTH - 1, WIDTH, 40, 40),
            (WIDTH - 1, 0)
        );
    }
}
