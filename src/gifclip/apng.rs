// SPDX-License-Identifier: GPL-3.0-or-later

//! Animated PNG clip output: the lossless alternative to the GIF writer,
//! fed the same [`ClipFrame`]s by the same callers.
//!
//! An APNG names its frame count, colour type and one palette for the
//! whole animation in its header, so frames cannot be written as they
//! arrive the way a GIF's are. Each frame is staged instead: only the
//! rectangle that changed since the previous frame is kept, deflated --
//! in memory up to [`STAGE_MEMORY_BUDGET`], in an anonymous temporary
//! file past it -- and the file is written in [`ApngWriter::finish`] once
//! every frame is known. A clip whose frames use 256 colours or fewer between
//! them is written palette-indexed; any more and it is written as 24-bit
//! RGB. Either way no colour is reduced, unlike a GIF, which quantizes a
//! frame of more than 256 colours.
//!
//! Each frame after the first covers just its changed rectangle and
//! replaces those pixels (dispose NONE, blend SOURCE), so a mostly still
//! picture costs little per frame. A frame identical to the canvas adds
//! nothing and lengthens the frame before it.
//!
//! Frame delays are fractions of a second, so they follow the emulated
//! timeline to the millisecond with the same cumulative rounding the GIF
//! writer uses in centiseconds. Browsers show a frame of 10 ms or less
//! for 100 ms, so no frame is shorter than [`MIN_APNG_DELAY_MS`]; a frame
//! replaced sooner gives its slot to the newer picture. 60 fps clips
//! therefore play at their real rate, which a GIF cannot. A final frame
//! whose slot rounds to nothing is left out unless it is the only one.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use super::{frame_rgba, resample_nearest, ClipFrame, FramePixels};

/// The shortest frame delay an APNG is written with. Browsers hold a
/// frame of 10 ms or less for 100 ms; 11 ms still plays at its rate and
/// leaves every clip rate up to 60 fps (16.7 ms) untouched.
pub const MIN_APNG_DELAY_MS: i64 = 11;
/// Colour bits that make a picture: the renderer's alpha byte is
/// presentation noise, not picture.
const RGB_MASK: u32 = 0x00FF_FFFF;
/// Deflated frame data an [`ApngWriter`] holds in memory; frames past it
/// are spooled to a temporary file until the APNG is written. A headless
/// capture is not bounded by the interactive ring's budget, and two
/// minutes of 60 fps truecolour noise stages gigabytes.
pub const STAGE_MEMORY_BUDGET: usize = 64 * 1024 * 1024;

/// Where a staged frame's deflated data is kept.
enum Payload {
    Memory(Vec<u8>),
    /// `len` bytes at `offset` in the writer's spool file.
    Spooled {
        offset: u64,
        len: usize,
    },
}

/// A frame's changed rectangle, staged until the file is written.
struct StagedFrame {
    delay_ms: i64,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    /// The rectangle's own colours (masked RGB) when `data` holds 8-bit
    /// indices into them, None when `data` holds RGB triples.
    palette: Option<Vec<u32>>,
    /// The indices or RGB triples, row by row, deflated.
    data: Payload,
}

/// Every colour the clip uses, in first-seen order, while there are 256
/// or fewer.
#[derive(Default)]
struct ClipPalette {
    colours: Vec<u32>,
    index: HashMap<u32, u8>,
}

impl ClipPalette {
    /// Add `colours`; false once the clip needs more than 256.
    fn add(&mut self, colours: impl IntoIterator<Item = u32>) -> bool {
        for colour in colours {
            if self.index.contains_key(&colour) {
                continue;
            }
            if self.colours.len() == 256 {
                return false;
            }
            self.index.insert(colour, self.colours.len() as u8);
            self.colours.push(colour);
        }
        true
    }
}

/// Collects [`ClipFrame`]s and writes them as a looping APNG on
/// [`finish`](Self::finish). Each frame's delay is fixed when the next one
/// arrives, as with [`super::GifWriter`].
pub struct ApngWriter<W: Write> {
    out: W,
    fps: u32,
    width: usize,
    height: usize,
    origin: Option<f64>,
    pending: Option<ClipFrame>,
    /// Where the pending frame starts, in rounded milliseconds from the
    /// origin. A newer frame that took over its slot keeps this start.
    pending_start_ms: i64,
    /// The picture the staged frames leave on screen (masked RGB), for
    /// the next frame's changed rectangle. Empty before the first frame.
    canvas: Vec<u32>,
    staged: Vec<StagedFrame>,
    /// None once the clip has used more than 256 colours.
    palette: Option<ClipPalette>,
    /// Scratch for a frame whose size differs from the canvas.
    resample: Vec<u32>,
    /// Staged data held in memory, against `memory_budget`.
    in_memory: usize,
    memory_budget: usize,
    /// The temporary file frames past the budget are appended to, created
    /// with the first of them, and its length.
    spool: Option<File>,
    spool_len: u64,
}

impl<W: Write> ApngWriter<W> {
    /// Start a `width` x `height` clip at `fps` (the nominal rate; only
    /// the final frame's default length uses it, every other delay comes
    /// from the frames' own timestamps).
    pub fn new(out: W, width: usize, height: usize, fps: u32) -> Result<Self> {
        anyhow::ensure!(
            width > 0
                && height > 0
                && u32::try_from(width).is_ok()
                && u32::try_from(height).is_ok(),
            "APNG canvas {width}x{height} is out of range"
        );
        Ok(Self {
            out,
            fps: fps.max(1),
            width,
            height,
            origin: None,
            pending: None,
            pending_start_ms: 0,
            canvas: Vec::new(),
            staged: Vec::new(),
            palette: Some(ClipPalette::default()),
            resample: Vec::new(),
            in_memory: 0,
            // A browser build has no host files to spool to.
            memory_budget: if cfg!(target_arch = "wasm32") {
                usize::MAX
            } else {
                STAGE_MEMORY_BUDGET
            },
            spool: None,
            spool_len: 0,
        })
    }

    /// Frames staged so far (the pending one is not counted until its
    /// delay is known).
    pub fn frames_written(&self) -> u32 {
        self.staged.len() as u32
    }

    /// Queue `frame`; the previously queued frame is staged with the delay
    /// up to this one, or dropped (with `frame` taking over its start)
    /// when that delay would be shorter than [`MIN_APNG_DELAY_MS`].
    pub fn push(&mut self, frame: ClipFrame) -> Result<()> {
        let origin = *self.origin.get_or_insert(frame.t);
        let start = millis(frame.t - origin);
        if let Some(prev) = self.pending.take() {
            let delay = start - self.pending_start_ms;
            if delay < MIN_APNG_DELAY_MS {
                self.pending = Some(frame);
                return Ok(());
            }
            self.stage(&prev, delay)?;
        }
        self.pending_start_ms = start;
        self.pending = Some(frame);
        Ok(())
    }

    /// Stage the last queued frame -- shown until `end` (default: one clip
    /// period after it) -- then write the file, returning the sink and the
    /// number of frames. A last frame identical to the one before only
    /// lengthens it by its own time; a new picture is shown for at least
    /// [`MIN_APNG_DELAY_MS`], unless its slot rounds to no time at all and
    /// an earlier frame is staged, when it is left out rather than
    /// lengthen the loop.
    pub fn finish(mut self, end: Option<f64>) -> Result<(W, u32)> {
        if let Some(last) = self.pending.take() {
            let end = end.unwrap_or(last.t + 1.0 / f64::from(self.fps));
            let origin = self.origin.unwrap_or(last.t);
            let delay = millis(end - origin) - self.pending_start_ms;
            if delay > 0 || self.staged.is_empty() {
                self.stage(&last, delay)?;
            }
        }
        anyhow::ensure!(!self.staged.is_empty(), "no frames to write");
        let frames = self.staged.len() as u32;
        self.write_file()?;
        Ok((self.out, frames))
    }

    /// Diff `frame` against the canvas and stage the changed rectangle,
    /// shown for `delay_ms` (at least [`MIN_APNG_DELAY_MS`]). A frame that
    /// changes nothing adds `delay_ms` to the frame before it instead.
    fn stage(&mut self, frame: &ClipFrame, delay_ms: i64) -> Result<()> {
        let mut rgb = frame_rgba(&frame.pixels);
        if frame.width != self.width || frame.height != self.height {
            // A frame whose presentation shape changed mid-clip (an
            // overscan or programmable-mode switch) is point-resampled
            // onto the canvas rather than dropped, as in the GIF.
            resample_nearest(
                &rgb,
                frame.width,
                frame.height,
                self.width,
                self.height,
                &mut self.resample,
            );
            std::mem::swap(&mut rgb, &mut self.resample);
        }
        for px in &mut rgb {
            *px &= RGB_MASK;
        }
        let rect = if self.canvas.is_empty() {
            // The first frame is the PNG's default image: the whole canvas.
            Some((0, 0, self.width, self.height))
        } else {
            changed_rect(&self.canvas, &rgb, self.width)
        };
        let Some((x, y, w, h)) = rect else {
            // Nothing changed: the frame on screen simply stays longer, by
            // this frame's own time (that frame already has the minimum).
            if let Some(last) = self.staged.last_mut() {
                last.delay_ms += delay_ms.max(0);
            }
            return Ok(());
        };
        let mut pixels = Vec::with_capacity(w * h);
        for row in y..y + h {
            pixels.extend_from_slice(&rgb[row * self.width + x..row * self.width + x + w]);
        }
        let (palette, raw) = match FramePixels::from_rgba(&pixels) {
            FramePixels::Indexed { palette, indices } => {
                let colours: Vec<u32> = palette
                    .chunks_exact(3)
                    .map(|c| u32::from_le_bytes([c[0], c[1], c[2], 0]))
                    .collect();
                let fits = match self.palette.as_mut() {
                    Some(clip) => clip.add(colours.iter().copied()),
                    None => false,
                };
                if !fits {
                    self.palette = None;
                }
                (Some(colours), indices)
            }
            FramePixels::Rgba(px) => {
                self.palette = None;
                (None, rgb_bytes(&px))
            }
        };
        let data = self.keep(deflate(&raw)?)?;
        self.staged.push(StagedFrame {
            delay_ms: delay_ms.max(MIN_APNG_DELAY_MS),
            x: x as u32,
            y: y as u32,
            width: w as u32,
            height: h as u32,
            palette,
            data,
        });
        self.canvas = rgb;
        Ok(())
    }

    /// Keep a staged frame's deflated `data`: in memory while the staged
    /// data fits the budget, else appended to the spool file.
    fn keep(&mut self, data: Vec<u8>) -> Result<Payload> {
        let held = self.in_memory.saturating_add(data.len());
        if held <= self.memory_budget {
            self.in_memory = held;
            return Ok(Payload::Memory(data));
        }
        let mut spool = match self.spool.take() {
            Some(file) => file,
            None => spool_file().context("creating a temporary file for the APNG clip")?,
        };
        spool
            .write_all(&data)
            .context("spooling an APNG frame to a temporary file")?;
        self.spool = Some(spool);
        let offset = self.spool_len;
        self.spool_len += data.len() as u64;
        Ok(Payload::Spooled {
            offset,
            len: data.len(),
        })
    }

    /// Encode the staged frames as the APNG.
    fn write_file(&mut self) -> Result<()> {
        let mut encoder = png::Encoder::new(&mut self.out, self.width as u32, self.height as u32);
        encoder.set_depth(png::BitDepth::Eight);
        // set_compression picks a filter of its own, so the filter follows.
        encoder.set_compression(png::Compression::High);
        // The clip palette's colour-to-index map, when the clip is indexed.
        let clip_index = match &self.palette {
            Some(clip) => {
                let mut plte = Vec::with_capacity(clip.colours.len() * 3);
                for colour in &clip.colours {
                    let [r, g, b, _] = colour.to_le_bytes();
                    plte.extend_from_slice(&[r, g, b]);
                }
                encoder.set_color(png::ColorType::Indexed);
                encoder.set_palette(plte);
                // Indices are labels, not intensities: predicting one from
                // its neighbours only scrambles runs deflate would find.
                encoder.set_filter(png::Filter::NoFilter);
                Some(&clip.index)
            }
            None => {
                encoder.set_color(png::ColorType::Rgb);
                encoder.set_filter(png::Filter::Adaptive);
                None
            }
        };
        encoder
            .set_animated(self.staged.len() as u32, 0)
            .context("writing the APNG animation header")?;
        let mut writer = encoder.write_header().context("writing the APNG header")?;
        for frame in &self.staged {
            let data = match &frame.data {
                Payload::Memory(bytes) => inflate(bytes)?,
                &Payload::Spooled { offset, len } => {
                    let spool = self
                        .spool
                        .as_mut()
                        .context("the APNG clip's temporary file is missing")?;
                    let mut bytes = vec![0; len];
                    spool
                        .seek(SeekFrom::Start(offset))
                        .and_then(|_| spool.read_exact(&mut bytes))
                        .context("reading back a spooled APNG frame")?;
                    inflate(&bytes)?
                }
            };
            let raw = match (&frame.palette, clip_index) {
                // Indices into the frame's own colours, mapped to the
                // clip's palette. (Every frame of an indexed clip has its
                // own palette: an RGB frame makes the clip truecolour.)
                (Some(local), Some(index)) => {
                    let lut: Vec<u8> = local.iter().map(|c| index[c]).collect();
                    data.iter().map(|&i| lut[usize::from(i)]).collect()
                }
                // Indices expanded to RGB for a truecolour clip.
                (Some(local), None) => {
                    let mut rgb = Vec::with_capacity(data.len() * 3);
                    for &i in &data {
                        let [r, g, b, _] = local[usize::from(i)].to_le_bytes();
                        rgb.extend_from_slice(&[r, g, b]);
                    }
                    rgb
                }
                (None, _) => data,
            };
            // Shrink to the origin first: the position is checked against
            // the current size and the size against the current position.
            writer.set_frame_position(0, 0)?;
            writer.set_frame_dimension(frame.width, frame.height)?;
            writer.set_frame_position(frame.x, frame.y)?;
            let (num, den) = delay_fraction(frame.delay_ms);
            writer.set_frame_delay(num, den)?;
            writer.set_dispose_op(png::DisposeOp::None)?;
            writer.set_blend_op(png::BlendOp::Source)?;
            writer
                .write_image_data(&raw)
                .context("writing an APNG frame")?;
        }
        writer.finish().context("writing the APNG trailer")?;
        Ok(())
    }
}

/// An anonymous temporary file for staged frames, removed when closed.
#[cfg(not(target_arch = "wasm32"))]
fn spool_file() -> std::io::Result<File> {
    tempfile::tempfile()
}

/// A browser build has no host files (and never spools: its budget is
/// unlimited).
#[cfg(target_arch = "wasm32")]
fn spool_file() -> std::io::Result<File> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// `secs` rounded to whole milliseconds.
fn millis(secs: f64) -> i64 {
    (secs * 1000.0).round() as i64
}

/// A frame delay as the fcTL fraction: milliseconds while they fit the
/// 16-bit numerator, centiseconds for a longer hold (a still picture can
/// last a whole two-minute clip).
fn delay_fraction(delay_ms: i64) -> (u16, u16) {
    let ms = delay_ms.max(MIN_APNG_DELAY_MS);
    match u16::try_from(ms) {
        Ok(ms) => (ms, 1000),
        Err(_) => (((ms + 5) / 10).clamp(1, i64::from(u16::MAX)) as u16, 100),
    }
}

/// The bounding box `(x, y, width, height)` of the pixels that differ
/// between two `width`-wide pictures, or None when they are identical.
fn changed_rect(old: &[u32], new: &[u32], width: usize) -> Option<(usize, usize, usize, usize)> {
    let rows_old = old.chunks_exact(width);
    let rows_new = new.chunks_exact(width);
    let mut top = None;
    let mut bottom = 0;
    let mut left = width;
    let mut right = 0;
    for (y, (a, b)) in rows_old.zip(rows_new).enumerate() {
        if a == b {
            continue;
        }
        top.get_or_insert(y);
        bottom = y;
        let first = a.iter().zip(b).position(|(p, q)| p != q).unwrap_or(0);
        let last = width
            - 1
            - a.iter()
                .rev()
                .zip(b.iter().rev())
                .position(|(p, q)| p != q)
                .unwrap_or(0);
        left = left.min(first);
        right = right.max(last);
    }
    top.map(|top| (left, top, right - left + 1, bottom - top + 1))
}

/// Masked RGB pixels as RGB triples.
fn rgb_bytes(px: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(px.len() * 3);
    for &p in px {
        let [r, g, b, _] = p.to_le_bytes();
        bytes.extend_from_slice(&[r, g, b]);
    }
    bytes
}

/// Staging compression: fast, since the data is compressed again when
/// the file is written; it only has to keep a long clip's frames small.
fn deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(data).context("staging an APNG frame")?;
    encoder.finish().context("staging an APNG frame")
}

fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(data)
        .read_to_end(&mut out)
        .context("reading back a staged APNG frame")?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(r: u8, g: u8, b: u8) -> u32 {
        u32::from_le_bytes([r, g, b, 255])
    }

    fn solid(w: usize, h: usize, colour: u32) -> Vec<u32> {
        vec![colour; w * h]
    }

    /// A 32x32 frame with 1024 distinct colours.
    fn gradient_1024() -> Vec<u32> {
        (0..1024u32)
            .map(|i| rgba((i % 32 * 8) as u8, (i / 32 * 8) as u8, (i % 7 * 36) as u8))
            .collect()
    }

    fn encode(frames: &[ClipFrame], fps: u32, end: Option<f64>) -> (Vec<u8>, u32) {
        let first = &frames[0];
        let mut writer = ApngWriter::new(Vec::new(), first.width, first.height, fps).unwrap();
        for frame in frames {
            writer.push(frame.clone()).unwrap();
        }
        writer.finish(end).unwrap()
    }

    /// One decoded animation frame: its fcTL and its pixels composited
    /// onto the canvas, as RGB triples.
    struct Decoded {
        control: png::FrameControl,
        canvas: Vec<u8>,
    }

    /// Decode an APNG frame by frame, compositing each sub-rectangle onto
    /// the canvas (every frame here is dispose NONE, blend SOURCE).
    fn decode(bytes: &[u8]) -> (png::ColorType, Vec<Decoded>) {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder.read_info().expect("APNG header parses");
        let info = reader.info();
        let (width, height) = (info.width as usize, info.height as usize);
        let colour = info.color_type;
        let frames = info.animation_control().expect("acTL present").num_frames;
        assert_eq!(
            info.animation_control().unwrap().num_plays,
            0,
            "loops forever"
        );
        let mut canvas = vec![0u8; width * height * 3];
        let mut out = Vec::new();
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        for _ in 0..frames {
            let frame = reader.next_frame(&mut buf).expect("APNG frame parses");
            let control = *reader.info().frame_control().expect("fcTL present");
            assert_eq!(control.dispose_op, png::DisposeOp::None);
            assert_eq!(control.blend_op, png::BlendOp::Source);
            let bpp = frame.line_size / control.width as usize;
            for row in 0..control.height as usize {
                for col in 0..control.width as usize {
                    let src = row * frame.line_size + col * bpp;
                    let x = control.x_offset as usize + col;
                    let y = control.y_offset as usize + row;
                    let dst = (y * width + x) * 3;
                    canvas[dst..dst + 3].copy_from_slice(&buf[src..src + 3]);
                }
            }
            out.push(Decoded {
                control,
                canvas: canvas.clone(),
            });
        }
        (colour, out)
    }

    fn delays_ms(frames: &[Decoded]) -> Vec<f64> {
        frames
            .iter()
            .map(|f| f64::from(f.control.delay_num) * 1000.0 / f64::from(f.control.delay_den))
            .collect()
    }

    fn rgb_of(px: &[u32]) -> Vec<u8> {
        rgb_bytes(px)
    }

    #[test]
    fn a_few_colours_write_an_indexed_apng_that_decodes_back() {
        let colours = [rgba(255, 0, 0), rgba(0, 255, 0), rgba(0, 0, 255)];
        let frames: Vec<ClipFrame> = colours
            .iter()
            .enumerate()
            .map(|(i, &c)| ClipFrame::new(i as f64 * 0.04, 8, 4, &solid(8, 4, c)))
            .collect();
        let (bytes, written) = encode(&frames, 25, None);
        assert_eq!(written, 3);
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let (colour, decoded) = decode(&bytes);
        assert_eq!(colour, png::ColorType::Indexed);
        assert_eq!(decoded.len(), 3);
        for (frame, &c) in decoded.iter().zip(&colours) {
            assert_eq!(frame.canvas, rgb_of(&solid(8, 4, c)));
        }
        // 25 fps: every frame is 40 ms, the last one too.
        assert_eq!(delays_ms(&decoded), vec![40.0, 40.0, 40.0]);
    }

    #[test]
    fn more_than_256_colours_write_lossless_rgb() {
        let px = gradient_1024();
        let mut flipped = px.clone();
        flipped.reverse();
        let frames = vec![
            ClipFrame::new(0.0, 32, 32, &px),
            ClipFrame::new(0.04, 32, 32, &flipped),
        ];
        let (bytes, _) = encode(&frames, 25, None);
        let (colour, decoded) = decode(&bytes);
        assert_eq!(colour, png::ColorType::Rgb);
        // Every pixel comes back exactly: nothing was quantized.
        assert_eq!(decoded[0].canvas, rgb_of(&px));
        assert_eq!(decoded[1].canvas, rgb_of(&flipped));
    }

    #[test]
    fn frames_with_few_colours_each_go_truecolour_once_the_clip_needs_more() {
        // 300 frames of one new colour each: every frame fits a palette,
        // the clip does not.
        let frames: Vec<ClipFrame> = (0..300u32)
            .map(|i| {
                ClipFrame::new(
                    f64::from(i) * 0.04,
                    2,
                    2,
                    &solid(2, 2, rgba(i as u8, (i >> 8) as u8, 7)),
                )
            })
            .collect();
        let (bytes, written) = encode(&frames, 25, None);
        assert_eq!(written, 300);
        let (colour, decoded) = decode(&bytes);
        assert_eq!(colour, png::ColorType::Rgb);
        assert_eq!(decoded[299].canvas, rgb_of(&solid(2, 2, rgba(43, 1, 7))));
    }

    #[test]
    fn later_frames_carry_only_the_changed_rectangle() {
        let base = solid(16, 8, rgba(10, 10, 10));
        let mut moved = base.clone();
        // Change a 3x2 block at (5, 4)..(7, 5).
        for y in 4..6 {
            for x in 5..8 {
                moved[y * 16 + x] = rgba(200, 0, 0);
            }
        }
        let frames = vec![
            ClipFrame::new(0.0, 16, 8, &base),
            ClipFrame::new(0.04, 16, 8, &moved),
        ];
        let (bytes, written) = encode(&frames, 25, None);
        assert_eq!(written, 2);
        let (_, decoded) = decode(&bytes);
        let first = &decoded[0].control;
        assert_eq!(
            (first.x_offset, first.y_offset, first.width, first.height),
            (0, 0, 16, 8)
        );
        let second = &decoded[1].control;
        assert_eq!(
            (
                second.x_offset,
                second.y_offset,
                second.width,
                second.height
            ),
            (5, 4, 3, 2)
        );
        assert_eq!(decoded[1].canvas, rgb_of(&moved));
    }

    #[test]
    fn an_unchanged_frame_lengthens_the_one_before() {
        let still = solid(4, 4, rgba(1, 2, 3));
        let frames = vec![
            ClipFrame::new(0.0, 4, 4, &still),
            ClipFrame::new(0.04, 4, 4, &still),
            ClipFrame::new(0.08, 4, 4, &solid(4, 4, rgba(4, 5, 6))),
        ];
        let (bytes, written) = encode(&frames, 25, None);
        assert_eq!(written, 2);
        let (_, decoded) = decode(&bytes);
        assert_eq!(delays_ms(&decoded), vec![80.0, 40.0]);
    }

    #[test]
    fn sixty_fps_keeps_every_frame_and_its_timing() {
        // 60 Hz fields at 60 fps: 16.7 ms frames, which a GIF cannot show
        // but an APNG can. Cumulative rounding gives 17, 16, 17, ...
        let frames: Vec<ClipFrame> = (0..60u32)
            .map(|i| ClipFrame::new(f64::from(i) / 60.0, 2, 2, &solid(2, 2, rgba(i as u8, 0, 0))))
            .collect();
        let (bytes, written) = encode(&frames, 60, Some(1.0));
        assert_eq!(written, 60);
        let (_, decoded) = decode(&bytes);
        let delays = delays_ms(&decoded);
        assert_eq!(&delays[..3], &[17.0, 16.0, 17.0]);
        assert_eq!(delays.iter().sum::<f64>(), 1000.0);
    }

    #[test]
    fn a_frame_replaced_within_ten_milliseconds_gives_way_to_the_newer() {
        let blue = rgba(0, 0, 255);
        let frames = vec![
            ClipFrame::new(0.0, 2, 2, &solid(2, 2, rgba(255, 0, 0))),
            ClipFrame::new(0.020, 2, 2, &solid(2, 2, rgba(0, 255, 0))),
            ClipFrame::new(0.025, 2, 2, &solid(2, 2, blue)),
        ];
        let (bytes, written) = encode(&frames, 50, Some(0.060));
        assert_eq!(written, 2);
        let (_, decoded) = decode(&bytes);
        assert_eq!(delays_ms(&decoded), vec![20.0, 40.0]);
        assert_eq!(decoded[1].canvas, rgb_of(&solid(2, 2, blue)));
    }

    #[test]
    fn an_identical_last_sample_adds_only_its_own_time() {
        let red = solid(2, 2, rgba(255, 0, 0));
        let blue = solid(2, 2, rgba(0, 0, 255));
        let two = vec![
            ClipFrame::new(0.0, 2, 2, &red),
            ClipFrame::new(0.040, 2, 2, &blue),
        ];
        let mut three = two.clone();
        three.push(ClipFrame::new(0.080, 2, 2, &blue));
        // The repeated blue's 1 ms tail adds that millisecond to the blue
        // frame, which already has the minimum: both loop every 81 ms.
        for frames in [&two, &three] {
            let (bytes, written) = encode(frames, 25, Some(0.081));
            assert_eq!(written, 2);
            assert_eq!(delays_ms(&decode(&bytes).1), vec![40.0, 41.0]);
        }
    }

    #[test]
    fn a_final_frame_with_no_time_left_is_left_out() {
        let frames = vec![
            ClipFrame::new(0.0, 2, 2, &solid(2, 2, rgba(1, 1, 1))),
            ClipFrame::new(0.040, 2, 2, &solid(2, 2, rgba(2, 2, 2))),
        ];
        // The clip ends 0.4 ms after the last frame, so both round to
        // 40 ms: holding that frame for the minimum would make a 40 ms
        // clip loop every 51 ms.
        let (bytes, written) = encode(&frames, 25, Some(0.0404));
        assert_eq!(written, 1);
        assert_eq!(delays_ms(&decode(&bytes).1), vec![40.0]);
        // A clip's only frame is still written, held for the minimum.
        let (bytes, written) = encode(&frames[..1], 25, Some(0.0));
        assert_eq!(written, 1);
        assert_eq!(delays_ms(&decode(&bytes).1), vec![MIN_APNG_DELAY_MS as f64]);
    }

    #[test]
    fn frames_past_the_memory_budget_spool_to_a_file_and_write_the_same_clip() {
        // Eight different 1024-colour pictures: each stages a truecolour
        // rectangle of about 3 KiB.
        let pictures: Vec<Vec<u32>> = (0..8usize)
            .map(|i| {
                let mut px = gradient_1024();
                px.rotate_left(i * 37);
                px
            })
            .collect();
        let frames: Vec<ClipFrame> = pictures
            .iter()
            .enumerate()
            .map(|(i, px)| ClipFrame::new(i as f64 * 0.04, 32, 32, px))
            .collect();
        let (unspooled, _) = encode(&frames, 25, None);
        let budget = 8 * 1024;
        let mut writer = ApngWriter::new(Vec::new(), 32, 32, 25).unwrap();
        writer.memory_budget = budget;
        for frame in &frames {
            writer.push(frame.clone()).unwrap();
        }
        assert!(writer.in_memory > 0 && writer.in_memory <= budget);
        assert!(writer.spool_len > 0, "later frames were spooled");
        let (spooled, written) = writer.finish(None).unwrap();
        assert_eq!(written, 8);
        assert_eq!(spooled, unspooled, "spooling does not change the file");
        let (_, decoded) = decode(&spooled);
        for (frame, px) in decoded.iter().zip(&pictures) {
            assert_eq!(frame.canvas, rgb_of(px));
        }
    }

    #[test]
    fn a_long_still_frame_switches_to_centiseconds() {
        assert_eq!(delay_fraction(40), (40, 1000));
        assert_eq!(delay_fraction(65_535), (65_535, 1000));
        assert_eq!(delay_fraction(120_000), (12_000, 100));
        assert_eq!(delay_fraction(3), (MIN_APNG_DELAY_MS as u16, 1000));
        let frames = vec![
            ClipFrame::new(0.0, 2, 2, &solid(2, 2, rgba(1, 1, 1))),
            ClipFrame::new(100.0, 2, 2, &solid(2, 2, rgba(2, 2, 2))),
        ];
        let (bytes, _) = encode(&frames, 25, Some(100.04));
        let (_, decoded) = decode(&bytes);
        assert_eq!(decoded[0].control.delay_num, 10_000);
        assert_eq!(decoded[0].control.delay_den, 100);
    }

    #[test]
    fn frames_of_another_size_are_fitted_to_the_canvas() {
        let frames = vec![
            ClipFrame::new(0.0, 4, 2, &solid(4, 2, rgba(1, 1, 1))),
            ClipFrame::new(0.04, 8, 4, &solid(8, 4, rgba(2, 2, 2))),
        ];
        let (bytes, written) = encode(&frames, 25, None);
        assert_eq!(written, 2);
        let (_, decoded) = decode(&bytes);
        assert_eq!(decoded[1].canvas, rgb_of(&solid(4, 2, rgba(2, 2, 2))));
    }

    #[test]
    fn the_same_sequence_encodes_byte_identical() {
        let mut flipped = gradient_1024();
        flipped.reverse();
        let frames = vec![
            ClipFrame::new(0.0, 32, 32, &gradient_1024()),
            ClipFrame::new(0.04, 32, 32, &solid(32, 32, rgba(1, 2, 3))),
            ClipFrame::new(0.08, 32, 32, &flipped),
        ];
        let (a, _) = encode(&frames, 25, None);
        let (b, _) = encode(&frames, 25, None);
        assert_eq!(a, b);
    }

    #[test]
    fn changed_rect_bounds_every_difference() {
        let a = vec![0u32; 6 * 3];
        assert_eq!(changed_rect(&a, &a, 6), None);
        let mut b = a.clone();
        b[6 + 1] = 1;
        b[2 * 6 + 4] = 1;
        assert_eq!(changed_rect(&a, &b, 6), Some((1, 1, 4, 2)));
        let mut c = a.clone();
        c[0] = 1;
        c[17] = 1;
        assert_eq!(changed_rect(&a, &c, 6), Some((0, 0, 6, 3)));
    }
}
