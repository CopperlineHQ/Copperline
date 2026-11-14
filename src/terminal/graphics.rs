// SPDX-License-Identifier: GPL-3.0-or-later

//! Kitty graphics protocol output: frames as zlib-compressed RGB images,
//! placed to fit the terminal, plus the status line beneath them.
//!
//! Each frame is transmitted inline (`t=d`, the only medium that crosses an
//! SSH connection) as a new image and placed over the old one before the old
//! one is deleted, alternating between two image ids. The terminal therefore
//! never shows an empty pane between frames, and no frame edits an image the
//! terminal is still compositing. The terminal scales the image to the cell
//! box it is placed in, so the frame always goes out at the machine's own
//! resolution whatever the pane size.

use flate2::{Compress, Compression, FlushCompress, Status};
use std::io::Write as _;

/// Raw bytes per transmitted chunk: base64 turns 3072 bytes into exactly the
/// protocol's 4096-byte chunk limit, and every chunk but the last stays a
/// multiple of four characters as the protocol requires.
const RAW_CHUNK: usize = 3072;

/// The two image ids frames alternate between.
pub const FRAME_IMAGE_IDS: [u32; 2] = [1, 2];

/// The terminal's text grid and the pixel size of one cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl Geometry {
    /// Rows the picture may use: everything above the status line.
    pub fn picture_rows(&self) -> u16 {
        self.rows.saturating_sub(1)
    }
}

/// Where a frame is placed, in cells (0-based column and row of its top-left
/// cell, and how many cells it spans).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
}

impl Placement {
    /// The largest cell box above the status line that shows an
    /// `image_width` x `image_height` frame (square pixels) undistorted, up to
    /// the rounding to whole cells, centred in the pane. `None` when the pane
    /// has no room or the frame no pixels.
    pub fn fit(image_width: u32, image_height: u32, geometry: Geometry) -> Option<Placement> {
        let area_cols = geometry.cols;
        let area_rows = geometry.picture_rows();
        if image_width == 0
            || image_height == 0
            || area_cols == 0
            || area_rows == 0
            || geometry.cell_width == 0
            || geometry.cell_height == 0
        {
            return None;
        }
        let cell_w = f64::from(geometry.cell_width);
        let cell_h = f64::from(geometry.cell_height);
        let area_w = f64::from(area_cols) * cell_w;
        let area_h = f64::from(area_rows) * cell_h;
        let scale = (area_w / f64::from(image_width)).min(area_h / f64::from(image_height));
        let cols = ((f64::from(image_width) * scale / cell_w).round() as u16).clamp(1, area_cols);
        let rows = ((f64::from(image_height) * scale / cell_h).round() as u16).clamp(1, area_rows);
        Some(Placement {
            col: (area_cols - cols) / 2,
            row: (area_rows - rows) / 2,
            cols,
            rows,
        })
    }

    /// The frame pixel under terminal pixel (`x`, `y`) of a frame
    /// `image_width` x `image_height` shown at this placement, or `None` off
    /// the picture.
    pub fn image_point(
        &self,
        geometry: Geometry,
        image_width: u32,
        image_height: u32,
        x: f64,
        y: f64,
    ) -> Option<(f64, f64)> {
        let cell_w = f64::from(geometry.cell_width);
        let cell_h = f64::from(geometry.cell_height);
        let width = f64::from(self.cols) * cell_w;
        let height = f64::from(self.rows) * cell_h;
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let fx = (x - f64::from(self.col) * cell_w) / width;
        let fy = (y - f64::from(self.row) * cell_h) / height;
        if !(0.0..1.0).contains(&fx) || !(0.0..1.0).contains(&fy) {
            return None;
        }
        Some((fx * f64::from(image_width), fy * f64::from(image_height)))
    }
}

/// Reusable buffers for encoding frames.
pub struct FrameEncoder {
    rgb: Vec<u8>,
    compressed: Vec<u8>,
    compress: Compress,
}

impl Default for FrameEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameEncoder {
    pub fn new() -> Self {
        Self {
            rgb: Vec::new(),
            compressed: Vec::new(),
            // The fastest level: Amiga frames are flat colour and compress
            // very well even so, and the encoder must keep up with 50 Hz.
            compress: Compress::new(Compression::fast(), true),
        }
    }

    /// Append to `out` the escape sequences that move the cursor to
    /// `placement` and transmit and place `pixels` (`width` x `height`, RGBA8
    /// in memory order R, G, B, A) as image `image_id`, placement 1. The
    /// cursor does not move afterwards (`C=1`) and the terminal is asked not
    /// to answer (`q=2`): an answer would arrive as input.
    pub fn encode_frame(
        &mut self,
        pixels: &[u32],
        width: u32,
        height: u32,
        image_id: u32,
        placement: Placement,
        out: &mut Vec<u8>,
    ) {
        debug_assert_eq!(pixels.len(), width as usize * height as usize);
        self.rgb.clear();
        self.rgb.reserve(pixels.len() * 3);
        for px in pixels {
            self.rgb.extend_from_slice(&px.to_ne_bytes()[..3]);
        }
        self.compress_rgb();
        move_cursor(out, placement.row, placement.col);
        let chunks = self.compressed.chunks(RAW_CHUNK);
        let last = chunks.len().saturating_sub(1);
        for (index, chunk) in chunks.enumerate() {
            let more = u8::from(index != last);
            out.extend_from_slice(b"\x1b_G");
            if index == 0 {
                let _ = write!(
                    out,
                    "a=T,f=24,o=z,s={width},v={height},i={image_id},p=1,c={},r={},C=1,q=2,m={more}",
                    placement.cols, placement.rows
                );
            } else {
                let _ = write!(out, "m={more},q=2");
            }
            out.push(b';');
            base64_encode_into(out, chunk);
            out.extend_from_slice(b"\x1b\\");
        }
    }

    fn compress_rgb(&mut self) {
        self.compress.reset();
        self.compressed.clear();
        self.compressed
            .reserve(self.rgb.len() + self.rgb.len() / 8 + 256);
        loop {
            let consumed = self.compress.total_in() as usize;
            let status = self
                .compress
                .compress_vec(
                    &self.rgb[consumed..],
                    &mut self.compressed,
                    FlushCompress::Finish,
                )
                .expect("zlib compression of an in-memory buffer cannot fail");
            match status {
                Status::StreamEnd => break,
                Status::Ok | Status::BufError => self.compressed.reserve(64 * 1024),
            }
        }
    }
}

/// Delete image `image_id` and free its data.
pub fn delete_image(out: &mut Vec<u8>, image_id: u32) {
    let _ = write!(out, "\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\");
}

/// Delete every image and free its data.
pub fn delete_all_images(out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b_Ga=d,d=A,q=2\x1b\\");
}

/// Move the cursor to 0-based `row`, `col`.
pub fn move_cursor(out: &mut Vec<u8>, row: u16, col: u16) {
    let _ = write!(out, "\x1b[{};{}H", u32::from(row) + 1, u32::from(col) + 1);
}

/// Draw `text` as a reverse-video bar across the bottom row of `geometry`.
/// Anything but printable ASCII is replaced, and the text is cut to the
/// width of the pane.
pub fn status_line(out: &mut Vec<u8>, geometry: Geometry, text: &str) {
    if geometry.rows == 0 || geometry.cols == 0 {
        return;
    }
    move_cursor(out, geometry.rows - 1, 0);
    out.extend_from_slice(b"\x1b[0m\x1b[2K\x1b[7m");
    let cols = usize::from(geometry.cols);
    let mut written = 0;
    for ch in text.chars().take(cols) {
        out.push(if ch == ' ' || ch.is_ascii_graphic() {
            ch as u8
        } else {
            b'?'
        });
        written += 1;
    }
    out.extend(std::iter::repeat_n(b' ', cols - written));
    out.extend_from_slice(b"\x1b[0m");
}

/// Standard base64 (RFC 4648, padded) of `data`, appended to `out`.
pub fn base64_encode_into(out: &mut Vec<u8>, data: &[u8]) {
    use base64::Engine;
    let start = out.len();
    out.resize(start + data.len().div_ceil(3) * 4, 0);
    let written = base64::engine::general_purpose::STANDARD
        .encode_slice(data, &mut out[start..])
        .expect("the buffer holds the padded encoding");
    debug_assert_eq!(start + written, out.len());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn b64(data: &[u8]) -> String {
        let mut out = Vec::new();
        base64_encode_into(&mut out, data);
        String::from_utf8(out).unwrap()
    }

    fn b64_decode(text: &[u8]) -> Vec<u8> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .unwrap()
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"foob"), "Zm9vYg==");
        assert_eq!(b64(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
    }

    const PANE: Geometry = Geometry {
        cols: 160,
        rows: 50,
        cell_width: 10,
        cell_height: 20,
    };

    #[test]
    fn fit_keeps_aspect_and_centres_above_status_line() {
        // 1600x980 px of picture area; a 716x568 frame is height-limited.
        let p = Placement::fit(716, 568, PANE).unwrap();
        assert_eq!(p.rows, 49);
        // 980 px tall -> 716 * 980 / 568 = 1235 px wide = 124 cells.
        assert_eq!(p.cols, 124);
        assert_eq!(p.col, (160 - 124) / 2);
        assert_eq!(p.row, 0);
        assert!(p.row + p.rows <= PANE.picture_rows());
    }

    #[test]
    fn fit_refuses_a_pane_with_no_picture_rows() {
        let one_row = Geometry { rows: 1, ..PANE };
        assert_eq!(Placement::fit(716, 568, one_row), None);
        assert_eq!(Placement::fit(0, 568, PANE), None);
    }

    #[test]
    fn image_point_inverts_the_placement() {
        let p = Placement {
            col: 10,
            row: 2,
            cols: 100,
            rows: 40,
        };
        // Top-left corner of the picture is frame pixel (0, 0).
        assert_eq!(
            p.image_point(PANE, 1000, 800, 100.0, 40.0),
            Some((0.0, 0.0))
        );
        // The picture's centre is the frame's centre.
        let (x, y) = p.image_point(PANE, 1000, 800, 600.0, 440.0).unwrap();
        assert!((x - 500.0).abs() < 1e-9 && (y - 400.0).abs() < 1e-9);
        // Off the picture.
        assert_eq!(p.image_point(PANE, 1000, 800, 99.0, 40.0), None);
        assert_eq!(p.image_point(PANE, 1000, 800, 1100.0, 40.0), None);
    }

    #[test]
    fn encoded_frame_chunks_round_trip_to_the_rgb_pixels() {
        let (w, h) = (97u32, 61u32);
        // A noisy frame so it spans several chunks even compressed.
        let mut seed = 0x1234_5678u32;
        let pixels: Vec<u32> = (0..w * h)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                u32::from_ne_bytes([
                    (seed >> 24) as u8,
                    (seed >> 16) as u8,
                    (seed >> 8) as u8,
                    255,
                ])
            })
            .collect();
        let placement = Placement {
            col: 3,
            row: 1,
            cols: 40,
            rows: 20,
        };
        let mut out = Vec::new();
        FrameEncoder::new().encode_frame(&pixels, w, h, 2, placement, &mut out);

        let text = out.as_slice();
        assert!(text.starts_with(b"\x1b[2;4H"));
        let mut payload = Vec::new();
        let mut chunks = 0;
        let mut rest = &text[6..];
        while !rest.is_empty() {
            assert!(rest.starts_with(b"\x1b_G"));
            let end = rest.windows(2).position(|w| w == b"\x1b\\").unwrap();
            let body = &rest[3..end];
            let semi = body.iter().position(|&b| b == b';').unwrap();
            let keys = std::str::from_utf8(&body[..semi]).unwrap();
            let data = &body[semi + 1..];
            assert!(data.len() <= 4096);
            if chunks == 0 {
                assert!(keys.starts_with("a=T,f=24,o=z,s=97,v=61,i=2,p=1,c=40,r=20,C=1,q=2"));
            } else {
                assert!(keys.starts_with("m="));
            }
            let last = keys.contains("m=0");
            if !last {
                assert_eq!(data.len() % 4, 0);
            }
            payload.extend(b64_decode(data));
            chunks += 1;
            rest = &rest[end + 2..];
            if last {
                assert!(rest.is_empty());
            }
        }
        assert!(chunks > 1, "expected a multi-chunk transmission");
        let mut rgb = Vec::new();
        flate2::read::ZlibDecoder::new(payload.as_slice())
            .read_to_end(&mut rgb)
            .unwrap();
        let expected: Vec<u8> = pixels
            .iter()
            .flat_map(|px| px.to_ne_bytes()[..3].to_vec())
            .collect();
        assert_eq!(rgb, expected);
    }

    #[test]
    fn encoder_reuse_produces_identical_output() {
        let pixels = vec![u32::from_ne_bytes([10, 20, 30, 255]); 64 * 32];
        let placement = Placement {
            col: 0,
            row: 0,
            cols: 8,
            rows: 4,
        };
        let mut enc = FrameEncoder::new();
        let (mut a, mut b) = (Vec::new(), Vec::new());
        enc.encode_frame(&pixels, 64, 32, 1, placement, &mut a);
        enc.encode_frame(&pixels, 64, 32, 1, placement, &mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn status_line_pads_and_sanitises() {
        let pane = Geometry {
            cols: 12,
            rows: 3,
            cell_width: 8,
            cell_height: 16,
        };
        let mut out = Vec::new();
        status_line(&mut out, pane, "ab\u{1b}cd");
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "\x1b[3;1H\x1b[0m\x1b[2K\x1b[7mab?cd       \x1b[0m");
        let mut out = Vec::new();
        status_line(&mut out, pane, "0123456789abcdef");
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("0123456789ab\x1b[0m"));
    }
}
