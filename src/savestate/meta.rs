// SPDX-License-Identifier: GPL-3.0-or-later

//! The `META` chunk of a `.clstate` file: what a person (or a tool) wants
//! to know about a state before deciding to load it. A thumbnail of the
//! display at the moment of the save, the emulated and wall-clock times,
//! a one-line machine summary, and the names of the media that were in
//! the drives. None of it is machine state: a load ignores it, and a
//! state without it (or with a damaged one) still restores its machine.
//!
//! The chunk sits in the clear between the `DESC` chunk and the zlib
//! stream, so [`super::peek`] reads it without inflating anything.

use std::io::Cursor;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Width of the thumbnail in pixels. The height follows the display's
/// 4:3 presentation shape for a chipset frame (180 rows) and the board's
/// own shape for an RTG frame.
pub const THUMBNAIL_WIDTH: usize = 240;

/// The thumbnail's height for a chipset frame: the 4:3 glass every
/// screenshot is saved at.
pub const THUMBNAIL_HEIGHT: usize = THUMBNAIL_WIDTH * 3 / 4;

/// One floppy drive's contents at save time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloppyMedia {
    /// Drive number, `DF0:` being 0.
    pub drive: u8,
    /// File name of the inserted image, `None` for an empty drive.
    pub name: Option<String>,
}

/// The media that were in the machine's drives at save time, by file name.
/// Names, not paths: the state embeds floppy images whole and reopens
/// hard-drive and CD images by the paths its own chunks carry, so these
/// are for reading, not for reopening anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MediaNames {
    /// One entry per connected floppy drive, in drive order.
    #[serde(default)]
    pub floppies: Vec<FloppyMedia>,
    /// Every hard-drive image on every controller (IDE ports, SCSI buses,
    /// copperhf units), in controller order.
    #[serde(default)]
    pub hard_disks: Vec<String>,
    /// The disc in the CD drive, if the machine has one and it holds a
    /// disc.
    #[serde(default)]
    pub cd: Option<String>,
}

impl MediaNames {
    /// One line naming everything, e.g. `DF0: game.adf, DF1: -, HD: work.hdf`.
    /// Empty when nothing is fitted at all.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for floppy in &self.floppies {
            parts.push(format!(
                "DF{}: {}",
                floppy.drive,
                floppy.name.as_deref().unwrap_or("-")
            ));
        }
        for disk in &self.hard_disks {
            parts.push(format!("HD: {disk}"));
        }
        if let Some(cd) = &self.cd {
            parts.push(format!("CD: {cd}"));
        }
        parts.join(", ")
    }
}

/// What the `META` chunk carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateMeta {
    /// PNG-encoded thumbnail of the presented frame at save time, RGB,
    /// `thumbnail_width` x `thumbnail_height`. Empty when no frame could
    /// be rendered. Stored as MessagePack `bin` by the chunk codec.
    pub thumbnail_png: Vec<u8>,
    pub thumbnail_width: u32,
    pub thumbnail_height: u32,
    /// Emulated time at the save, in seconds since power-on.
    pub emulated_seconds: f64,
    /// Emulated frames since power-on.
    pub emulated_frames: u64,
    /// Host wall-clock time of the save, Unix seconds; 0 when the host has
    /// no clock to read (the browser build).
    pub saved_at_unix: u64,
    /// One-line machine summary: model, CPU, chipset, video standard,
    /// memory. The `DESC` chunk holds the structured form.
    pub machine: String,
    #[serde(default)]
    pub media: MediaNames,
}

impl StateMeta {
    /// The metadata as JSON for the control protocol and `copperline-ctl
    /// state-info`. The thumbnail is described (size in pixels and bytes),
    /// not embedded: a tool that wants the picture asks for it as a file.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "emulated_seconds": self.emulated_seconds,
            "emulated_frames": self.emulated_frames,
            "saved_at_unix": self.saved_at_unix,
            "saved_at": crate::timestamp::readable(self.saved_at_unix),
            "machine": self.machine,
            "media": {
                "floppies": self.media.floppies.iter().map(|f| serde_json::json!({
                    "drive": f.drive,
                    "name": f.name,
                })).collect::<Vec<_>>(),
                "hard_disks": self.media.hard_disks,
                "cd": self.media.cd,
            },
            "thumbnail": {
                "width": self.thumbnail_width,
                "height": self.thumbnail_height,
                "png_bytes": self.thumbnail_png.len(),
            },
        })
    }

    /// Decode the thumbnail into packed RGBA pixels (memory order R, G, B,
    /// A, the framebuffer's own layout), with its width and height. `None`
    /// when the state carries no picture.
    pub fn thumbnail_pixels(&self) -> Result<Option<(Vec<u32>, usize, usize)>> {
        if self.thumbnail_png.is_empty() {
            return Ok(None);
        }
        decode_png_rgba(&self.thumbnail_png).map(Some)
    }
}

/// The file name of a media path as text, for the media list: `None` for
/// a path with no final component (an in-memory volume built from nothing
/// has an empty path).
pub fn file_name(path: &std::path::Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

/// Downscale a rendered frame to the thumbnail size and PNG-encode it.
/// `fb` holds `width * lines` packed RGBA pixels in the renderer's memory
/// order. `chipset_glass` says the frame is a chipset field, whose
/// presentation is always the 4:3 glass whatever its line count; an RTG
/// frame keeps the board's own shape instead. Returns the PNG bytes and
/// the thumbnail's dimensions.
pub fn encode_thumbnail(
    fb: &[u32],
    width: usize,
    lines: usize,
    chipset_glass: bool,
) -> Result<(Vec<u8>, u32, u32)> {
    if width == 0 || lines == 0 || fb.len() < width * lines {
        bail!("frame is empty ({width}x{lines}, {} pixels)", fb.len());
    }
    let out_w = THUMBNAIL_WIDTH;
    let out_h = if chipset_glass {
        THUMBNAIL_HEIGHT
    } else {
        (out_w * lines / width).clamp(1, out_w * 2)
    };
    let rgb = box_downscale_rgb(&fb[..width * lines], width, lines, out_w, out_h);
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, out_w as u32, out_h as u32);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .context("writing thumbnail PNG header")?;
        writer
            .write_image_data(&rgb)
            .context("writing thumbnail PNG data")?;
    }
    Ok((png_bytes, out_w as u32, out_h as u32))
}

/// Area-average `src` (packed RGBA, `sw` x `sh`) down to `dw` x `dh`
/// RGB bytes. Every destination pixel averages the source box it covers,
/// so thin lines and text survive as tone rather than vanishing the way
/// they would under point sampling. Integer arithmetic throughout: the
/// picture is a deterministic function of the frame.
fn box_downscale_rgb(src: &[u32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(dw * dh * 3);
    for dy in 0..dh {
        let y0 = dy * sh / dh;
        let y1 = ((dy + 1) * sh / dh).max(y0 + 1).min(sh);
        for dx in 0..dw {
            let x0 = dx * sw / dw;
            let x1 = ((dx + 1) * sw / dw).max(x0 + 1).min(sw);
            let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
            for y in y0..y1 {
                for &px in &src[y * sw + x0..y * sw + x1] {
                    r += u64::from(px & 0xFF);
                    g += u64::from((px >> 8) & 0xFF);
                    b += u64::from((px >> 16) & 0xFF);
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            out.push((r / n) as u8);
            out.push((g / n) as u8);
            out.push((b / n) as u8);
        }
    }
    out
}

/// Decode a PNG into packed RGBA pixels. Accepts what `encode_thumbnail`
/// writes (8-bit RGB) and 8-bit RGBA, which is what an edited thumbnail
/// or a different encoder would most likely produce.
fn decode_png_rgba(bytes: &[u8]) -> Result<(Vec<u32>, usize, usize)> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info().context("reading thumbnail PNG")?;
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    let info = reader
        .next_frame(&mut buf)
        .context("decoding thumbnail PNG")?;
    let (w, h) = (info.width as usize, info.height as usize);
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        bail!("thumbnail has an unreasonable size ({w}x{h})");
    }
    let data = &buf[..info.buffer_size()];
    let pixels = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgb, png::BitDepth::Eight) => data
            .chunks_exact(3)
            .map(|p| {
                0xFF00_0000 | (u32::from(p[2]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[0])
            })
            .collect::<Vec<u32>>(),
        (png::ColorType::Rgba, png::BitDepth::Eight) => data
            .chunks_exact(4)
            .map(|p| {
                (u32::from(p[3]) << 24)
                    | (u32::from(p[2]) << 16)
                    | (u32::from(p[1]) << 8)
                    | u32::from(p[0])
            })
            .collect::<Vec<u32>>(),
        (color, depth) => bail!("thumbnail PNG is {color:?}/{depth:?}, expected 8-bit RGB or RGBA"),
    };
    if pixels.len() != w * h {
        bail!(
            "thumbnail PNG data is short ({} of {} pixels)",
            pixels.len(),
            w * h
        );
    }
    Ok((pixels, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(r: u32, g: u32, b: u32) -> u32 {
        0xFF00_0000 | (b << 16) | (g << 8) | r
    }

    #[test]
    fn thumbnails_keep_the_glass_shape_and_average_their_source() {
        // A 716x285 chipset field: red, then blue from x = 357 on.
        let (w, h) = (716, 285);
        let mut fb = vec![rgba(255, 0, 0); w * h];
        for y in 0..h {
            for x in 357..w {
                fb[y * w + x] = rgba(0, 0, 255);
            }
        }
        let (png_bytes, tw, th) = encode_thumbnail(&fb, w, h, true).unwrap();
        assert_eq!((tw, th), (240, 180));
        let meta = StateMeta {
            thumbnail_png: png_bytes,
            thumbnail_width: tw,
            thumbnail_height: th,
            emulated_seconds: 1.5,
            emulated_frames: 75,
            saved_at_unix: 0,
            machine: "test".into(),
            media: MediaNames::default(),
        };
        let (pixels, pw, ph) = meta.thumbnail_pixels().unwrap().unwrap();
        assert_eq!((pw, ph), (240, 180));
        assert_eq!(pixels[90 * 240 + 10], rgba(255, 0, 0));
        assert_eq!(pixels[90 * 240 + 230], rgba(0, 0, 255));
        // 716/240 = 2.98 source columns per output column: column 119
        // covers x = 355..358, so it averages two red columns and a blue
        // one, while its neighbours are pure.
        let seam = pixels[90 * 240 + 119];
        assert!(
            seam & 0xFF > 0 && (seam >> 16) & 0xFF > 0,
            "seam {seam:08x}"
        );
        assert_eq!(pixels[90 * 240 + 118], rgba(255, 0, 0));
        assert_eq!(pixels[90 * 240 + 120], rgba(0, 0, 255));
        // Same frame, same bytes: the thumbnail is deterministic.
        let again = encode_thumbnail(&fb, w, h, true).unwrap();
        assert_eq!(again.0, meta.thumbnail_png);
    }

    #[test]
    fn rtg_thumbnails_keep_the_board_shape_and_empty_frames_are_refused() {
        let (w, h) = (640, 480);
        let fb = vec![rgba(10, 20, 30); w * h];
        let (_, tw, th) = encode_thumbnail(&fb, w, h, false).unwrap();
        assert_eq!((tw, th), (240, 180));
        let (_, tw, th) = encode_thumbnail(&fb[..640 * 200], 640, 200, false).unwrap();
        assert_eq!((tw, th), (240, 75));
        assert!(encode_thumbnail(&[], 0, 0, true).is_err());
        assert!(encode_thumbnail(&fb[..10], 640, 480, true).is_err());
    }

    #[test]
    fn media_summary_names_every_drive_and_json_describes_the_picture() {
        let media = MediaNames {
            floppies: vec![
                FloppyMedia {
                    drive: 0,
                    name: Some("game.adf".into()),
                },
                FloppyMedia {
                    drive: 1,
                    name: None,
                },
            ],
            hard_disks: vec!["work.hdf".into()],
            cd: Some("disc.cue".into()),
        };
        assert_eq!(
            media.summary(),
            "DF0: game.adf, DF1: -, HD: work.hdf, CD: disc.cue"
        );
        assert_eq!(MediaNames::default().summary(), "");
        let meta = StateMeta {
            thumbnail_png: Vec::new(),
            thumbnail_width: 0,
            thumbnail_height: 0,
            emulated_seconds: 2.0,
            emulated_frames: 100,
            saved_at_unix: 1_699_956_800,
            machine: "A500 / 68000".into(),
            media,
        };
        assert!(meta.thumbnail_pixels().unwrap().is_none());
        let json = meta.to_json();
        assert_eq!(json["emulated_frames"], 100);
        assert_eq!(json["media"]["cd"], "disc.cue");
        assert_eq!(
            json["media"]["floppies"][1]["name"],
            serde_json::Value::Null
        );
        assert_eq!(json["thumbnail"]["png_bytes"], 0);
        assert!(json["saved_at"].as_str().unwrap().starts_with("2023/11/14"));
    }

    #[test]
    fn damaged_thumbnails_are_reported_not_trusted() {
        let meta = StateMeta {
            thumbnail_png: b"not a png".to_vec(),
            thumbnail_width: 240,
            thumbnail_height: 180,
            emulated_seconds: 0.0,
            emulated_frames: 0,
            saved_at_unix: 0,
            machine: String::new(),
            media: MediaNames::default(),
        };
        assert!(meta.thumbnail_pixels().is_err());
    }
}
