// SPDX-License-Identifier: GPL-3.0-or-later

//! Previews of guest-registered debug resources (`crate::uaelib`).
//!
//! A program built against the vscode-amiga-debug template describes its
//! bitmaps and palettes to the emulator through the uaelib trap
//! (`debug_register_bitmap` / `debug_register_palette`); the frame
//! analyzer's Resources tab renders those descriptions. Everything here is
//! pure over bytes the caller already read from guest memory, so hostile
//! or stale guest data can at worst produce a clamped, noted preview --
//! never a panic and never a bus access.

use crate::chipset::denise::{rgb12_to_rgb24, rgb24_to_rgba8};

/// Clamps applied to a guest-declared geometry before any allocation.
pub const PREVIEW_MAX_WIDTH: usize = 1008;
pub const PREVIEW_MAX_HEIGHT: usize = 1024;
pub const PREVIEW_MAX_PLANES: usize = 8;
/// Total pixel cap: height is truncated (and noted) past this.
pub const PREVIEW_MAX_PIXELS: usize = 256 * 1024;

/// A decoded bitmap resource, ready to draw.
pub struct BitmapPreview {
    pub width: usize,
    /// Possibly truncated relative to the declared height (see `note`).
    pub height: usize,
    /// `width * height` framebuffer RGBA words (`rgb24_to_rgba8` layout).
    pub pixels: Vec<u32>,
    /// What the decoder had to gloss over, if anything.
    pub note: Option<String>,
}

/// Decode a planar bitmap the way the guest described it.
///
/// `data` is whatever the caller read from guest memory starting at the
/// resource's address; rows past the end of `data` stay background. The
/// row stride is the Amiga convention, `((width + 15) / 16) * 2` bytes.
/// Layouts:
/// - planar: plane `p` starts at `p * stride * height`;
/// - interleaved: row `r`, plane `p` starts at `(r * planes + p) * stride`;
/// - masked (the template doubles the declared size): interleaved keeps
///   one mask row after each row's plane rows; planar keeps one whole mask
///   plane after the data planes. The mask is skipped, not rendered.
///
/// The mask placement mirrors the layouts blitter cookie-cut sources use;
/// it is a convention, not something the resource struct spells out, so it
/// lives in this one function should a correction ever be needed.
///
/// A HAM-flagged bitmap is rendered as plain indexed pixels with a note:
/// reproducing HAM needs the exact left-edge state per line, which a
/// static preview does not have.
#[allow(clippy::too_many_arguments)]
pub fn decode_bitmap(
    data: &[u8],
    width: u16,
    height: u16,
    planes: u16,
    interleaved: bool,
    masked: bool,
    ham: bool,
    palette: &[u32],
) -> BitmapPreview {
    let mut notes: Vec<String> = Vec::new();
    let width_px = usize::from(width);
    let width_px = if width_px > PREVIEW_MAX_WIDTH {
        notes.push(format!("width clamped to {PREVIEW_MAX_WIDTH}"));
        PREVIEW_MAX_WIDTH
    } else {
        width_px
    };
    let mut height_px = usize::from(height);
    if height_px > PREVIEW_MAX_HEIGHT {
        notes.push(format!("height clamped to {PREVIEW_MAX_HEIGHT}"));
        height_px = PREVIEW_MAX_HEIGHT;
    }
    let plane_count = usize::from(planes);
    let plane_count = if plane_count > PREVIEW_MAX_PLANES {
        notes.push(format!("planes clamped to {PREVIEW_MAX_PLANES}"));
        PREVIEW_MAX_PLANES
    } else {
        plane_count
    };
    if width_px == 0 || height_px == 0 || plane_count == 0 {
        return BitmapPreview {
            width: 0,
            height: 0,
            pixels: Vec::new(),
            note: Some("degenerate geometry".to_string()),
        };
    }
    if width_px * height_px > PREVIEW_MAX_PIXELS {
        height_px = PREVIEW_MAX_PIXELS / width_px;
        notes.push(format!("preview truncated to {height_px} rows"));
    }
    if ham {
        notes.push("HAM shown as indexed".to_string());
    }
    if masked {
        notes.push("mask plane skipped".to_string());
    }

    // Source addressing follows the DECLARED geometry, whatever the
    // preview clamps above did: clamping only bounds what is allocated
    // and iterated, while the guest's bytes stay laid out by the real
    // width, height, and plane count. A clamped preview therefore shows
    // the top-left of the true layout instead of a scrambled one.
    let stride = usize::from(width).div_ceil(16) * 2;
    // Row step between one row's plane `p` and the next row's, and the
    // per-plane starting offset, per layout (mask rows widen the step).
    let (row_step, plane_step) = if interleaved {
        let per_row_planes = usize::from(planes) + usize::from(masked);
        (stride * per_row_planes, stride)
    } else {
        (stride, stride * usize::from(height))
    };

    const BACKGROUND: u32 = 0xFF20_2020;
    // Out-of-palette indices render a grey checker, so a bitmap whose
    // plane count exceeds its palette is visibly wrong, not black.
    const CHECKER_A: u32 = 0xFF60_6060;
    const CHECKER_B: u32 = 0xFF90_9090;

    let mut pixels = vec![BACKGROUND; width_px * height_px];
    let mut short_data = false;
    for row in 0..height_px {
        for x in 0..width_px {
            let byte = x / 8;
            let bit = 7 - (x % 8);
            let mut index = 0usize;
            let mut present = true;
            for plane in 0..plane_count {
                let offset = row * row_step + plane * plane_step + byte;
                match data.get(offset) {
                    Some(b) => index |= usize::from((b >> bit) & 1) << plane,
                    None => {
                        present = false;
                        break;
                    }
                }
            }
            if !present {
                short_data = true;
                continue;
            }
            pixels[row * width_px + x] = match palette.get(index) {
                Some(&colour) => colour,
                None => {
                    if (x / 4 + row / 4) % 2 == 0 {
                        CHECKER_A
                    } else {
                        CHECKER_B
                    }
                }
            };
        }
    }
    if short_data {
        notes.push("data ends before the declared geometry".to_string());
    }

    BitmapPreview {
        width: width_px,
        height: height_px,
        pixels,
        note: (!notes.is_empty()).then(|| notes.join("; ")),
    }
}

/// Expand a palette resource's 12-bit words (big-endian, as stored in
/// guest memory) to framebuffer RGBA, exactly as Denise would show them.
pub fn decode_palette_words(data: &[u8], entries: u16) -> Vec<u32> {
    let entries = usize::from(entries).min(256);
    (0..entries)
        .filter_map(|i| {
            let hi = data.get(i * 2)?;
            let lo = data.get(i * 2 + 1)?;
            let word = u16::from_be_bytes([*hi, *lo]) & 0x0FFF;
            Some(rgb24_to_rgba8(rgb12_to_rgb24(word)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn grey(v: u32) -> u32 {
        0xFF00_0000 | (v << 16) | (v << 8) | v
    }

    /// A 32x4x2 test picture: pixel (x, row) has index ((x / 8) + row) % 4.
    fn plane_bit(plane: usize, x: usize, row: usize) -> bool {
        let index = ((x / 8) + row) % 4;
        index & (1 << plane) != 0
    }

    fn build(interleaved: bool, masked: bool) -> Vec<u8> {
        let (width, height, planes) = (32usize, 4usize, 2usize);
        let stride = width / 8;
        let per_row_planes = planes + usize::from(masked && interleaved);
        let total = if interleaved {
            stride * per_row_planes * height
        } else {
            stride * height * (planes + usize::from(masked))
        };
        let mut data = vec![0u8; total];
        for row in 0..height {
            for plane in 0..planes {
                for x in 0..width {
                    if !plane_bit(plane, x, row) {
                        continue;
                    }
                    let offset = if interleaved {
                        (row * per_row_planes + plane) * stride + x / 8
                    } else {
                        (plane * height + row) * stride + x / 8
                    };
                    data[offset] |= 1 << (7 - (x % 8));
                }
            }
            if masked && interleaved {
                let mask_row = (row * per_row_planes + planes) * stride;
                data[mask_row..mask_row + stride].fill(0xFF);
            }
        }
        if masked && !interleaved {
            let mask_plane = stride * height * planes;
            data[mask_plane..].fill(0xFF);
        }
        data
    }

    const PALETTE: [u32; 4] = [grey(0x00), grey(0x40), grey(0x80), grey(0xC0)];

    fn check_picture(preview: &BitmapPreview) {
        assert_eq!((preview.width, preview.height), (32, 4));
        for row in 0..4 {
            for x in 0..32 {
                let index = ((x / 8) + row) % 4;
                assert_eq!(
                    preview.pixels[row * 32 + x],
                    PALETTE[index],
                    "pixel ({x},{row})"
                );
            }
        }
    }

    #[test]
    fn planar_and_interleaved_agree_on_the_same_picture() {
        let planar = decode_bitmap(
            &build(false, false),
            32,
            4,
            2,
            false,
            false,
            false,
            &PALETTE,
        );
        let inter = decode_bitmap(&build(true, false), 32, 4, 2, true, false, false, &PALETTE);
        assert!(planar.note.is_none());
        check_picture(&planar);
        check_picture(&inter);
        assert_eq!(planar.pixels, inter.pixels);
    }

    #[test]
    fn masked_layouts_skip_the_mask_rows() {
        let planar = decode_bitmap(&build(false, true), 32, 4, 2, false, true, false, &PALETTE);
        let inter = decode_bitmap(&build(true, true), 32, 4, 2, true, true, false, &PALETTE);
        check_picture(&planar);
        check_picture(&inter);
        assert!(planar.note.as_deref().unwrap().contains("mask"));
    }

    #[test]
    fn hostile_geometry_is_clamped_and_noted() {
        let preview = decode_bitmap(&[0u8; 64], 0xFFFF, 0xFFFF, 0xFFFF, false, false, false, &[]);
        assert_eq!(preview.width, PREVIEW_MAX_WIDTH);
        assert!(preview.width * preview.height <= PREVIEW_MAX_PIXELS);
        let note = preview.note.unwrap();
        assert!(note.contains("width clamped"), "{note}");
        assert!(note.contains("planes clamped"), "{note}");
        assert!(note.contains("truncated"), "{note}");

        let degenerate = decode_bitmap(&[], 0, 10, 1, false, false, false, &[]);
        assert_eq!(degenerate.pixels.len(), 0);
        assert_eq!(degenerate.note.as_deref(), Some("degenerate geometry"));
    }

    #[test]
    fn short_data_renders_partial_rows_and_notes_it() {
        let full = build(false, false);
        let preview = decode_bitmap(
            &full[..full.len() / 2],
            32,
            4,
            2,
            false,
            false,
            false,
            &PALETTE,
        );
        assert!(preview
            .note
            .as_deref()
            .unwrap()
            .contains("data ends before"));
        // The final row of the truncated planar data has no plane-1 bytes,
        // so its pixels stay background.
        assert_eq!(preview.pixels[3 * 32], 0xFF20_2020);
    }

    #[test]
    fn out_of_palette_indices_render_the_checker() {
        // 2 planes but a 2-entry palette: indices 2 and 3 miss.
        let preview = decode_bitmap(
            &build(false, false),
            32,
            4,
            2,
            false,
            false,
            false,
            &PALETTE[..2],
        );
        assert_eq!(preview.pixels[0], PALETTE[0]);
        // Pixel (16, 0) has index 2 -> checker grey, never a palette colour.
        let checker = preview.pixels[16];
        assert!(checker == 0xFF60_6060 || checker == 0xFF90_9090);
    }

    #[test]
    fn ham_is_rendered_indexed_with_a_note() {
        let preview = decode_bitmap(&build(false, false), 32, 4, 2, false, false, true, &PALETTE);
        check_picture(&preview);
        assert!(preview.note.as_deref().unwrap().contains("HAM"));
    }

    #[test]
    fn clamped_planes_keep_the_declared_interleaved_row_step() {
        // 10 declared planes (clamped to 8 for decoding): row 1's data
        // still starts 10 plane-rows in, not 8, or every row after the
        // first reads the wrong bytes.
        let width = 16u16;
        let stride = 2usize;
        let declared_planes = 10usize;
        let mut data = vec![0u8; 2 * declared_planes * stride];
        // Row 1, plane 0, first pixel.
        data[declared_planes * stride] = 0x80;
        let preview = decode_bitmap(
            &data,
            width,
            2,
            declared_planes as u16,
            true,
            false,
            false,
            &[0xFF00_0000, 0xFF11_1111],
        );
        assert_eq!(preview.width, 16);
        assert_eq!(preview.height, 2);
        assert_eq!(
            preview.pixels[16], 0xFF11_1111,
            "row 1 pixel 0 must come from the declared row step"
        );
    }

    #[test]
    fn truncated_height_keeps_the_declared_planar_plane_offsets() {
        // 512x1024x2 planar: the pixel cap truncates the preview to 512
        // rows, but plane 1 still starts a full declared-height plane in.
        let width = 512u16;
        let height = 1024u16;
        let stride = 64usize;
        let mut data = vec![0u8; 2 * usize::from(height) * stride];
        // Plane 1, row 0, first pixel.
        data[usize::from(height) * stride] = 0x80;
        let palette = [0xFF00_0000, 0xFF11_1111, 0xFF22_2222, 0xFF33_3333];
        let preview = decode_bitmap(&data, width, height, 2, false, false, false, &palette);
        assert!(preview.height < usize::from(height), "cap truncates");
        assert_eq!(
            preview.pixels[0], 0xFF22_2222,
            "pixel (0,0) carries plane 1's bit from the declared offset"
        );
    }

    #[test]
    fn palette_words_expand_like_denise() {
        // $0F00 -> pure red, $008F -> $0008F -> green 0x88, blue 0xFF.
        let data = [0x0F, 0x00, 0x00, 0x8F];
        let colours = decode_palette_words(&data, 2);
        assert_eq!(
            colours,
            vec![rgb24_to_rgba8(0x00FF_0000), rgb24_to_rgba8(0x0000_88FF)]
        );
        // Short data yields only the words present; entry count caps at 256.
        assert_eq!(decode_palette_words(&data[..2], 8).len(), 1);
    }
}
