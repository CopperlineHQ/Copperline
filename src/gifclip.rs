// SPDX-License-Identifier: GPL-3.0-or-later

//! Animated GIF clip export: the rolling "last N seconds" ring behind the
//! window's Save Clip as GIF item, and the streaming capture behind the
//! headless `--gif-after` flag. Both feed the same [`GifWriter`], so a clip
//! saved from the window and one captured headlessly are the same picture.
//!
//! Frames arrive stamped with emulated time (one timestamp per presented
//! frame) and are thinned to the clip rate by a [`FrameSelector`] before
//! they are stored, so the ring holds at most `clip_seconds * clip_fps`
//! frames whatever the field rate. A stored frame is an exact-palette
//! indexed image when the picture has 256 colours or fewer (Amiga output
//! almost always does) and the RGBA pixels otherwise, quantized with
//! NeuQuant only when the frame is written. Identical consecutive frames
//! are stored once: a frame stays on screen until the next stored one.
//!
//! GIF frame delays are in centiseconds. Rounding each frame's own period
//! would drift (3.33 cs at 30 fps rounds to 3 and plays 10% fast), so each
//! delay is the difference of the cumulative rounded timestamps: the clip's
//! total length stays within half a centisecond of the emulated interval it
//! covers, and a stretch that presented no frames (a warp burst) plays as
//! one long delay rather than a burst of catch-up frames.
//!
//! Everything here is a pure function of the frames and their timestamps,
//! so the same emulated run produces a byte-identical file.

use anyhow::{Context, Result};
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::Path;

use crate::chipset::agnus::VideoStandard;

/// `[recording] clip_seconds` default: the ring keeps the last ten seconds.
pub const DEFAULT_CLIP_SECONDS: u32 = 10;
/// Upper bound on `[recording] clip_seconds` / `--gif-seconds`.
pub const MAX_CLIP_SECONDS: u32 = 120;
/// Upper bound on `[recording] clip_fps`: a GIF cannot usefully exceed the
/// field rate, and browsers clamp delays shorter than 2 cs anyway.
pub const MAX_CLIP_FPS: u32 = 60;
/// Memory ceiling for the interactive ring. Indexed frames of the full
/// 716-column presentation are ~385 KiB, so ten seconds at 25 fps of
/// constantly changing pictures is under 100 MiB; the budget only bites
/// on truecolour-ish output (HAM8, AGA 24-bit) kept as raw RGBA, where it
/// shortens the clip rather than growing without bound.
const RING_BYTE_BUDGET: usize = 256 * 1024 * 1024;
/// NeuQuant trains on one pixel in `samplefac`; this many pixels per
/// sample keeps a full 716-column frame at the usual factor of 10 while a
/// small frame still trains on all of its pixels.
const NEUQUANT_PIXELS_PER_SAMPLE: usize = 25_000;
/// Timestamp slack when deciding whether a frame reaches its clip slot:
/// far below any field period, so only float noise is absorbed.
const SLOT_TOLERANCE_SECS: f64 = 1e-7;

/// `[recording]` clip settings as the window and the headless capture use
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipSettings {
    /// Ring length in emulated seconds; 0 disables the ring.
    pub seconds: u32,
    /// Clip frame rate; 0 picks the standard's default (25 PAL, 30 NTSC).
    pub fps: u32,
}

impl Default for ClipSettings {
    fn default() -> Self {
        Self {
            seconds: DEFAULT_CLIP_SECONDS,
            fps: 0,
        }
    }
}

impl ClipSettings {
    /// The clip rate for a machine of `standard`, resolving the automatic
    /// setting.
    pub fn effective_fps(&self, standard: VideoStandard) -> u32 {
        if self.fps == 0 {
            default_clip_fps(standard)
        } else {
            self.fps
        }
    }
}

/// The default clip rate: half the field rate, which halves the file for
/// no visible loss on 50/60 Hz output.
pub fn default_clip_fps(standard: VideoStandard) -> u32 {
    match standard {
        VideoStandard::Pal => 25,
        VideoStandard::Ntsc => 30,
    }
}

/// One stored picture: exact indices when the frame fits a 256-colour
/// palette, the RGBA pixels otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FramePixels {
    Indexed {
        /// RGB triples, up to 256 entries.
        palette: Vec<u8>,
        indices: Vec<u8>,
    },
    Rgba(Vec<u32>),
}

impl FramePixels {
    /// Store `rgba` (little-endian `[r, g, b, a]` pixels as the renderer
    /// produces them) as compactly as its colour count allows.
    pub fn from_rgba(rgba: &[u32]) -> Self {
        match exact_palette(rgba) {
            Some((palette, indices)) => Self::Indexed { palette, indices },
            None => Self::Rgba(rgba.to_vec()),
        }
    }

    pub fn bytes(&self) -> usize {
        match self {
            Self::Indexed { palette, indices } => palette.len() + indices.len(),
            Self::Rgba(px) => px.len() * 4,
        }
    }

    /// Whether the picture had to be kept as RGBA for later quantization.
    pub fn is_rgba(&self) -> bool {
        matches!(self, Self::Rgba(_))
    }

    /// The palette and indices to write: the exact ones, or a NeuQuant
    /// reduction of an RGBA frame.
    fn indexed(&self) -> (Cow<'_, [u8]>, Cow<'_, [u8]>) {
        match self {
            Self::Indexed { palette, indices } => (Cow::Borrowed(palette), Cow::Borrowed(indices)),
            Self::Rgba(px) => {
                let (palette, indices) = quantize(px);
                (Cow::Owned(palette), Cow::Owned(indices))
            }
        }
    }
}

/// A presented frame with its emulated timestamp.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipFrame {
    /// Emulated seconds at which the frame was presented.
    pub t: f64,
    pub width: usize,
    pub height: usize,
    pub pixels: FramePixels,
}

impl ClipFrame {
    pub fn new(t: f64, width: usize, height: usize, rgba: &[u32]) -> Self {
        debug_assert_eq!(rgba.len(), width * height);
        Self {
            t,
            width,
            height,
            pixels: FramePixels::from_rgba(rgba),
        }
    }

    pub fn bytes(&self) -> usize {
        self.pixels.bytes()
    }

    fn same_picture(&self, other: &Self) -> bool {
        self.width == other.width && self.height == other.height && self.pixels == other.pixels
    }
}

/// Thins presented frames to the clip rate on the emulated timeline: the
/// first frame offered opens slot 0, and a frame is taken once its time
/// reaches the next slot. Slots a gap skipped over are dropped rather
/// than back-filled, so the frames after a warp burst keep their own
/// timing instead of arriving as a burst.
#[derive(Debug, Clone, Copy)]
pub struct FrameSelector {
    fps: u32,
    origin: Option<f64>,
    emitted: u64,
}

impl FrameSelector {
    pub fn new(fps: u32) -> Self {
        Self {
            fps: fps.max(1),
            origin: None,
            emitted: 0,
        }
    }

    pub fn fps(&self) -> u32 {
        self.fps
    }

    /// Whether the frame presented at emulated time `t` belongs in the
    /// clip.
    pub fn take(&mut self, t: f64) -> bool {
        let origin = *self.origin.get_or_insert(t);
        let elapsed = t - origin + SLOT_TOLERANCE_SECS;
        let fps = f64::from(self.fps);
        if elapsed * fps < self.emitted as f64 {
            return false;
        }
        self.emitted = (elapsed * fps).floor() as u64 + 1;
        true
    }

    pub fn reset(&mut self) {
        self.origin = None;
        self.emitted = 0;
    }
}

/// The rolling ring of the last `seconds` of presented frames, thinned to
/// the clip rate and stored compactly. Bounded by time and by
/// [`RING_BYTE_BUDGET`]; the frame that was on screen when the window
/// opens is kept even when it was presented earlier, so a static picture
/// still fills the clip's head.
pub struct ClipRing {
    seconds: f64,
    selector: FrameSelector,
    frames: VecDeque<ClipFrame>,
    bytes: usize,
    byte_budget: usize,
    /// Emulated time of the newest frame offered and taken (not
    /// necessarily stored: a repeat of the previous picture is not).
    latest_t: Option<f64>,
}

impl ClipRing {
    pub fn new(seconds: u32, fps: u32) -> Self {
        Self::with_byte_budget(seconds, fps, RING_BYTE_BUDGET)
    }

    pub fn with_byte_budget(seconds: u32, fps: u32, byte_budget: usize) -> Self {
        Self {
            seconds: f64::from(seconds),
            selector: FrameSelector::new(fps),
            frames: VecDeque::new(),
            bytes: 0,
            byte_budget,
            latest_t: None,
        }
    }

    pub fn fps(&self) -> u32 {
        self.selector.fps()
    }

    pub fn seconds(&self) -> f64 {
        self.seconds
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Emulated seconds the ring currently covers, for on-screen messages.
    pub fn span_seconds(&self) -> f64 {
        match (self.frames.front(), self.latest_t) {
            (Some(first), Some(latest)) => (latest - first.t).min(self.seconds),
            _ => 0.0,
        }
    }

    /// Offer the frame presented at emulated time `t`. Returns whether it
    /// was stored (it may be thinned out, or a repeat of the last picture).
    pub fn push(&mut self, t: f64, width: usize, height: usize, rgba: &[u32]) -> bool {
        self.wants(t) && self.store(t, width, height, rgba)
    }

    /// Whether the frame presented at emulated time `t` belongs in the
    /// ring; a caller builds the picture only for those and hands it to
    /// [`store`](Self::store).
    pub fn wants(&mut self, t: f64) -> bool {
        // The timeline moved backwards (a state load, a reset): a clip
        // cannot span the discontinuity, so start over.
        if self.latest_t.is_some_and(|latest| t < latest) {
            self.clear();
        }
        self.selector.take(t)
    }

    /// Store a frame [`wants`](Self::wants) accepted. Returns false when
    /// it repeats the previous picture (which then simply stays on
    /// screen longer).
    pub fn store(&mut self, t: f64, width: usize, height: usize, rgba: &[u32]) -> bool {
        self.latest_t = Some(t);
        let frame = ClipFrame::new(t, width, height, rgba);
        if self
            .frames
            .back()
            .is_some_and(|last| last.same_picture(&frame))
        {
            self.prune();
            return false;
        }
        self.bytes += frame.bytes();
        self.frames.push_back(frame);
        self.prune();
        true
    }

    pub fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
        self.latest_t = None;
        self.selector.reset();
    }

    /// Drop frames that ended before the window opened, and the oldest
    /// ones past the byte budget. The frame showing at the window's start
    /// is kept (it ends when the next one begins), so the ring never drops
    /// to nothing while it holds a picture.
    fn prune(&mut self) {
        let Some(latest) = self.latest_t else {
            return;
        };
        let window_start = latest - self.seconds;
        while self.frames.len() >= 2 && self.frames[1].t <= window_start {
            self.pop_front();
        }
        while self.frames.len() >= 2 && self.bytes > self.byte_budget {
            self.pop_front();
        }
    }

    fn pop_front(&mut self) {
        if let Some(frame) = self.frames.pop_front() {
            self.bytes -= frame.bytes();
        }
    }

    /// The frames to write as a clip, oldest first, with the head frame's
    /// time clamped to the window's start, and the time the clip ends
    /// (the newest frame plus one clip period).
    pub fn clip(&self) -> (Vec<ClipFrame>, f64) {
        let mut frames: Vec<ClipFrame> = self.frames.iter().cloned().collect();
        let latest = self.latest_t.unwrap_or(0.0);
        if let Some(first) = frames.first_mut() {
            first.t = first.t.max(latest - self.seconds);
        }
        (frames, latest + 1.0 / f64::from(self.fps()))
    }
}

/// Streams [`ClipFrame`]s into a looping GIF89a with a local palette per
/// frame. Each frame is written once the next one arrives (its delay is
/// the gap between them); [`finish`](Self::finish) writes the last frame
/// and the trailer.
pub struct GifWriter<W: Write> {
    encoder: gif::Encoder<W>,
    fps: u32,
    width: usize,
    height: usize,
    origin: Option<f64>,
    pending: Option<ClipFrame>,
    frames_written: u32,
    /// Scratch for a frame whose size differs from the canvas.
    resample: Vec<u32>,
}

impl<W: Write> GifWriter<W> {
    /// Start a `width` x `height` clip at `fps` (the nominal rate; only
    /// the final frame's delay and the loop's playback rate use it, every
    /// other delay comes from the frames' own timestamps).
    pub fn new(out: W, width: usize, height: usize, fps: u32) -> Result<Self> {
        anyhow::ensure!(
            width > 0
                && width <= usize::from(u16::MAX)
                && height > 0
                && height <= usize::from(u16::MAX),
            "GIF canvas {width}x{height} is out of range"
        );
        let mut encoder = gif::Encoder::new(out, width as u16, height as u16, &[])
            .context("writing the GIF header")?;
        encoder
            .set_repeat(gif::Repeat::Infinite)
            .context("writing the GIF loop extension")?;
        Ok(Self {
            encoder,
            fps: fps.max(1),
            width,
            height,
            origin: None,
            pending: None,
            frames_written: 0,
            resample: Vec::new(),
        })
    }

    pub fn frames_written(&self) -> u32 {
        self.frames_written
    }

    /// Queue `frame`; the previously queued frame is written with the
    /// delay up to this one.
    pub fn push(&mut self, frame: ClipFrame) -> Result<()> {
        if let Some(prev) = self.pending.take() {
            let delay = self.delay_between(prev.t, frame.t);
            self.write(&prev, delay)?;
        }
        self.origin.get_or_insert(frame.t);
        self.pending = Some(frame);
        Ok(())
    }

    /// Write the last queued frame -- shown until `end` (default: one clip
    /// period after it) -- and the trailer, returning the sink and the
    /// number of frames written.
    pub fn finish(mut self, end: Option<f64>) -> Result<(W, u32)> {
        if let Some(last) = self.pending.take() {
            let end = end.unwrap_or(last.t + 1.0 / f64::from(self.fps));
            let delay = self.delay_between(last.t, end);
            self.write(&last, delay)?;
        }
        let out = self
            .encoder
            .into_inner()
            .context("writing the GIF trailer")?;
        Ok((out, self.frames_written))
    }

    /// Centiseconds between two timestamps as the difference of their
    /// cumulative rounded offsets from the clip's origin, never below 1.
    fn delay_between(&self, from: f64, to: f64) -> u16 {
        let origin = self.origin.unwrap_or(from);
        let a = ((from - origin) * 100.0).round();
        let b = ((to - origin) * 100.0).round();
        (b - a).clamp(1.0, f64::from(u16::MAX)) as u16
    }

    fn write(&mut self, frame: &ClipFrame, delay: u16) -> Result<()> {
        let fitted;
        let pixels = if frame.width == self.width && frame.height == self.height {
            &frame.pixels
        } else {
            // A frame whose presentation shape changed mid-clip (an
            // overscan or programmable-mode switch) is point-resampled
            // onto the canvas rather than dropped.
            let rgba = frame_rgba(&frame.pixels);
            resample_nearest(
                &rgba,
                frame.width,
                frame.height,
                self.width,
                self.height,
                &mut self.resample,
            );
            fitted = FramePixels::from_rgba(&self.resample);
            &fitted
        };
        let (palette, indices) = pixels.indexed();
        let gif_frame = gif::Frame {
            delay,
            width: self.width as u16,
            height: self.height as u16,
            palette: Some(palette.into_owned()),
            buffer: indices,
            dispose: gif::DisposalMethod::Keep,
            ..Default::default()
        };
        self.encoder
            .write_frame(&gif_frame)
            .context("writing a GIF frame")?;
        self.frames_written += 1;
        Ok(())
    }
}

/// Write `frames` (oldest first, ending at `end`) to `path` as a GIF whose
/// canvas is the first frame's size. Returns the number of frames written.
pub fn write_clip(path: &Path, frames: &[ClipFrame], fps: u32, end: Option<f64>) -> Result<u32> {
    let first = frames
        .first()
        .ok_or_else(|| anyhow::anyhow!("no frames to write"))?;
    crate::paths::ensure_parent(path)
        .with_context(|| format!("creating the directory for {}", path.display()))?;
    let file =
        std::fs::File::create(path).with_context(|| format!("creating clip {}", path.display()))?;
    let mut writer = GifWriter::new(
        std::io::BufWriter::new(file),
        first.width,
        first.height,
        fps,
    )?;
    for frame in frames {
        writer.push(frame.clone())?;
    }
    let (mut out, written) = writer.finish(end)?;
    out.flush()
        .with_context(|| format!("flushing clip {}", path.display()))?;
    Ok(written)
}

/// Pick a default filename for an interactive clip.
pub fn auto_filename() -> std::path::PathBuf {
    crate::paths::clip_file()
}

/// Index `rgba` against its own colours in first-seen order, or None once
/// a 257th colour appears. A small direct-mapped cache in front of the
/// palette scan keeps the common few-colour frame to one pass.
fn exact_palette(rgba: &[u32]) -> Option<(Vec<u8>, Vec<u8>)> {
    const CACHE_SLOTS: usize = 4096;
    const EMPTY: u32 = u32::MAX; // never a masked colour
    let mut palette: Vec<u32> = Vec::with_capacity(64);
    let mut indices = Vec::with_capacity(rgba.len());
    let mut cache = vec![(EMPTY, 0u8); CACHE_SLOTS];
    let mut last = (EMPTY, 0u8);
    for &px in rgba {
        let colour = px & 0x00FF_FFFF;
        if colour == last.0 {
            indices.push(last.1);
            continue;
        }
        let slot = (colour.wrapping_mul(0x9E37_79B1) >> 20) as usize;
        let index = if cache[slot].0 == colour {
            cache[slot].1
        } else {
            match palette.iter().position(|&c| c == colour) {
                Some(i) => i as u8,
                None => {
                    if palette.len() == 256 {
                        return None;
                    }
                    palette.push(colour);
                    (palette.len() - 1) as u8
                }
            }
        };
        cache[slot] = (colour, index);
        last = (colour, index);
        indices.push(index);
    }
    let mut rgb = Vec::with_capacity(palette.len() * 3);
    for colour in palette {
        let [r, g, b, _] = colour.to_le_bytes();
        rgb.extend_from_slice(&[r, g, b]);
    }
    Some((rgb, indices))
}

/// Reduce a frame with more than 256 colours to a NeuQuant palette. The
/// network is trained on the frame itself and pixels are mapped through
/// a colour cache, so the cost scales with distinct colours, not pixels.
fn quantize(rgba: &[u32]) -> (Vec<u8>, Vec<u8>) {
    let mut bytes = Vec::with_capacity(rgba.len() * 4);
    for &px in rgba {
        let [r, g, b, _] = px.to_le_bytes();
        bytes.extend_from_slice(&[r, g, b, 255]);
    }
    let samplefac = (rgba.len() / NEUQUANT_PIXELS_PER_SAMPLE).clamp(1, 10) as i32;
    let quant = color_quant::NeuQuant::new(samplefac, 256, &bytes);
    let palette = quant.color_map_rgb();
    let mut lookup: HashMap<u32, u8> = HashMap::new();
    let indices = rgba
        .iter()
        .map(|&px| {
            let colour = px & 0x00FF_FFFF;
            *lookup.entry(colour).or_insert_with(|| {
                let [r, g, b, _] = colour.to_le_bytes();
                quant.index_of(&[r, g, b, 255]) as u8
            })
        })
        .collect();
    (palette, indices)
}

/// The RGBA pixels of a stored frame (alpha opaque).
fn frame_rgba(pixels: &FramePixels) -> Vec<u32> {
    match pixels {
        FramePixels::Rgba(px) => px.clone(),
        FramePixels::Indexed { palette, indices } => indices
            .iter()
            .map(|&i| {
                let base = usize::from(i) * 3;
                u32::from_le_bytes([palette[base], palette[base + 1], palette[base + 2], 255])
            })
            .collect(),
    }
}

fn resample_nearest(
    src: &[u32],
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
    out: &mut Vec<u32>,
) {
    out.clear();
    out.reserve(dst_w * dst_h);
    for y in 0..dst_h {
        let sy = (y * src_h / dst_h).min(src_h.saturating_sub(1));
        let row = &src[sy * src_w..(sy + 1) * src_w];
        for x in 0..dst_w {
            let sx = (x * src_w / dst_w).min(src_w.saturating_sub(1));
            out.push(row[sx]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(r: u8, g: u8, b: u8) -> u32 {
        u32::from_le_bytes([r, g, b, 255])
    }

    /// A `w` x `h` frame filled with one colour.
    fn solid(w: usize, h: usize, colour: u32) -> Vec<u32> {
        vec![colour; w * h]
    }

    /// A 32x32 frame with 1024 distinct colours.
    fn gradient_1024() -> Vec<u32> {
        (0..1024u32)
            .map(|i| rgba((i % 32 * 8) as u8, (i / 32 * 8) as u8, (i % 7 * 36) as u8))
            .collect()
    }

    fn decode(bytes: &[u8]) -> Vec<gif::Frame<'static>> {
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder = options.read_info(bytes).expect("GIF header parses");
        let mut frames = Vec::new();
        while let Some(frame) = decoder.read_next_frame().expect("GIF frame parses") {
            frames.push(frame.clone());
        }
        frames
    }

    fn encode(frames: &[ClipFrame], fps: u32, end: Option<f64>) -> (Vec<u8>, u32) {
        let first = &frames[0];
        let mut writer = GifWriter::new(Vec::new(), first.width, first.height, fps).unwrap();
        for frame in frames {
            writer.push(frame.clone()).unwrap();
        }
        writer.finish(end).unwrap()
    }

    #[test]
    fn encoder_output_parses_back_with_the_gif_decoder() {
        let colours = [rgba(255, 0, 0), rgba(0, 255, 0), rgba(0, 0, 255)];
        let frames: Vec<ClipFrame> = colours
            .iter()
            .enumerate()
            .map(|(i, &c)| ClipFrame::new(i as f64 * 0.04, 8, 4, &solid(8, 4, c)))
            .collect();
        let (bytes, written) = encode(&frames, 25, None);
        assert_eq!(written, 3);
        assert_eq!(&bytes[..6], b"GIF89a");
        // The NETSCAPE2.0 loop extension is present.
        assert!(bytes.windows(11).any(|w| w == b"NETSCAPE2.0"));

        let decoded = decode(&bytes);
        assert_eq!(decoded.len(), 3);
        for (frame, &colour) in decoded.iter().zip(&colours) {
            assert_eq!((frame.width, frame.height), (8, 4));
            // 25 fps: every frame is 4 cs, the last one too.
            assert_eq!(frame.delay, 4);
            let expected = colour.to_le_bytes();
            for px in frame.buffer.chunks_exact(4) {
                assert_eq!(px, &expected);
            }
        }
    }

    #[test]
    fn stored_frames_index_exactly_when_the_palette_fits() {
        let mut px = solid(16, 2, rgba(10, 20, 30));
        px[5] = rgba(1, 2, 3);
        px[31] = rgba(200, 100, 0);
        let frame = ClipFrame::new(0.0, 16, 2, &px);
        match &frame.pixels {
            FramePixels::Indexed { palette, indices } => {
                assert_eq!(palette, &[10, 20, 30, 1, 2, 3, 200, 100, 0]);
                assert_eq!(indices[0], 0);
                assert_eq!(indices[5], 1);
                assert_eq!(indices[31], 2);
                assert_eq!(indices.iter().filter(|&&i| i == 0).count(), 30);
            }
            other => panic!("expected an indexed frame, got {other:?}"),
        }
        assert_eq!(frame.bytes(), 9 + 32);
        // Alpha does not split colours: the renderer's alpha is presentation
        // noise, not picture.
        let opaque = ClipFrame::new(0.0, 1, 1, &[rgba(5, 5, 5)]);
        let clear = ClipFrame::new(0.0, 1, 1, &[u32::from_le_bytes([5, 5, 5, 0])]);
        assert_eq!(opaque.pixels, clear.pixels);
    }

    #[test]
    fn a_frame_with_more_than_256_colours_is_quantized_on_write() {
        let px = gradient_1024();
        let frame = ClipFrame::new(0.0, 32, 32, &px);
        assert!(frame.pixels.is_rgba(), "1024 colours cannot index exactly");
        assert_eq!(frame.bytes(), 1024 * 4);

        let (bytes, _) = encode(&[frame], 25, None);
        let decoded = decode(&bytes);
        assert_eq!(decoded.len(), 1);
        // Indexed decoding exposes the local palette's size.
        let mut indexed = gif::DecodeOptions::new();
        indexed.set_color_output(gif::ColorOutput::Indexed);
        let mut decoder = indexed.read_info(&bytes[..]).unwrap();
        let first = decoder.read_next_frame().unwrap().unwrap();
        let palette = first.palette.as_ref().expect("local palette");
        assert!(palette.len() / 3 <= 256);
        assert!(palette.len() / 3 > 64, "the gradient needs a real palette");
        // Decoded pixels stay near their sources: 256 palette entries for
        // 1024 colours on a ramp with steps of 8 and 36 should land within
        // a step or two on average, far from the greyscale a random
        // palette would give.
        let mut total = 0i64;
        let mut worst = 0i32;
        for (out, &src) in decoded[0].buffer.chunks_exact(4).zip(&px) {
            let s = src.to_le_bytes();
            for c in 0..3 {
                let err = (i32::from(out[c]) - i32::from(s[c])).abs();
                total += i64::from(err);
                worst = worst.max(err);
            }
        }
        // Four colours per palette entry on steps of 8 and 36 leaves a
        // mean error around two steps; a random palette would be past 60.
        let mean = total as f64 / (px.len() * 3) as f64;
        assert!(mean <= 24.0, "mean quantization error {mean} too large");
        assert!(worst <= 96, "worst quantization error {worst} too large");
    }

    #[test]
    fn the_same_sequence_encodes_byte_identical() {
        let mut frames = vec![
            ClipFrame::new(0.0, 32, 32, &gradient_1024()),
            ClipFrame::new(0.04, 32, 32, &solid(32, 32, rgba(1, 2, 3))),
        ];
        let mut px = gradient_1024();
        px.reverse();
        frames.push(ClipFrame::new(0.08, 32, 32, &px));
        let (a, _) = encode(&frames, 25, None);
        let (b, _) = encode(&frames, 25, None);
        assert_eq!(a, b);
    }

    #[test]
    fn delays_follow_cumulative_rounding() {
        // 30 fps out of 60 Hz fields: 3.33 cs per frame must average out
        // to 100 cs per 30 frames, not round to 3 and play fast.
        let mut selector = FrameSelector::new(30);
        let frames: Vec<ClipFrame> = (0..60u32)
            .map(|i| f64::from(i) / 60.0)
            .filter(|&t| selector.take(t))
            .map(|t| ClipFrame::new(t, 2, 2, &solid(2, 2, rgba(0, 0, 0))))
            .collect();
        assert_eq!(frames.len(), 30);
        let (bytes, _) = encode(&frames, 30, Some(1.0));
        let delays: Vec<u16> = decode(&bytes).iter().map(|f| f.delay).collect();
        assert_eq!(&delays[..6], &[3, 4, 3, 3, 4, 3]);
        assert_eq!(delays.iter().map(|&d| u32::from(d)).sum::<u32>(), 100);
    }

    #[test]
    fn selector_thins_pal_fields_and_skips_gaps() {
        let mut s = FrameSelector::new(25);
        let taken: Vec<u32> = (0..10u32)
            .filter(|&i| s.take(f64::from(i) / 50.0))
            .collect();
        assert_eq!(taken, vec![0, 2, 4, 6, 8]);
        // A gap (a warp burst that presented nothing) drops the slots it
        // covered: the next frame is taken at once, then the cadence
        // continues from there without catch-up frames.
        assert!(s.take(3.0));
        assert!(!s.take(3.0 + 1.0 / 50.0));
        assert!(s.take(3.0 + 2.0 / 50.0));
        // A rate above the field rate takes every frame.
        let mut every = FrameSelector::new(60);
        assert!((0..5u32).all(|i| every.take(f64::from(i) / 50.0)));
    }

    #[test]
    fn ring_keeps_only_the_last_window() {
        let mut ring = ClipRing::new(10, 25);
        // 30 s of 50 Hz fields, every field a new picture.
        for i in 0..1500u32 {
            let t = f64::from(i) / 50.0;
            ring.push(t, 4, 4, &solid(4, 4, rgba(i as u8, (i >> 8) as u8, 0)));
        }
        // 250 frames fill ten seconds at 25 fps, plus the frame showing at
        // the window's start.
        assert!(
            (250..=251).contains(&ring.frame_count()),
            "{} frames",
            ring.frame_count()
        );
        let (frames, end) = ring.clip();
        // Every even field is taken at 25 fps, so the newest frame in the
        // ring is field 1498 and the clip ends one period after it.
        let latest = 1498.0 / 50.0;
        assert!(
            (frames[0].t - (latest - 10.0)).abs() < 0.05,
            "{}",
            frames[0].t
        );
        assert!((end - (latest + 0.04)).abs() < 1e-9, "{end}");
        assert!((ring.span_seconds() - 10.0).abs() < 0.05);
        assert_eq!(
            ring.bytes(),
            frames.iter().map(ClipFrame::bytes).sum::<usize>()
        );
    }

    #[test]
    fn ring_stores_a_repeated_picture_once_and_keeps_the_head_frame() {
        let mut ring = ClipRing::new(10, 25);
        let still = solid(4, 4, rgba(9, 9, 9));
        for i in 0..1500u32 {
            ring.push(f64::from(i) / 50.0, 4, 4, &still);
        }
        assert_eq!(ring.frame_count(), 1, "a static screen is one frame");
        // The picture changes 30 s in: the static head frame stays, clamped
        // to the window start, so the clip shows it for the ten seconds it
        // was on screen before the change.
        assert!(ring.push(30.0, 4, 4, &solid(4, 4, rgba(1, 1, 1))));
        assert_eq!(ring.frame_count(), 2);
        let (frames, _) = ring.clip();
        assert!((frames[0].t - 20.0).abs() < 1e-9);
        assert!((frames[1].t - 30.0).abs() < 1e-9);
        let (bytes, _) = encode(&frames, 25, Some(30.04));
        let delays: Vec<u16> = decode(&bytes).iter().map(|f| f.delay).collect();
        assert_eq!(delays, vec![1000, 4]);
    }

    #[test]
    fn ring_restarts_when_the_timeline_rewinds() {
        let mut ring = ClipRing::new(10, 25);
        for i in 0..100u32 {
            ring.push(f64::from(i) / 50.0, 2, 2, &solid(2, 2, rgba(i as u8, 0, 0)));
        }
        assert!(ring.frame_count() > 1);
        // A state load moved emulated time back: the ring starts over
        // from the new frame.
        ring.push(0.5, 2, 2, &solid(2, 2, rgba(7, 7, 7)));
        assert_eq!(ring.frame_count(), 1);
        assert_eq!(ring.bytes(), 3 + 4);
    }

    #[test]
    fn ring_evicts_oldest_frames_past_the_byte_budget() {
        // Each 32x32 gradient frame is 4 KiB of RGBA; a 20 KiB budget
        // holds four of them (the fifth push evicts the oldest).
        let mut ring = ClipRing::with_byte_budget(10, 50, 20 * 1024);
        for i in 0..8u32 {
            let mut px = gradient_1024();
            px[0] = rgba(i as u8, 0, 0);
            ring.push(f64::from(i) / 50.0, 32, 32, &px);
        }
        assert_eq!(ring.frame_count(), 5);
        assert!(ring.bytes() <= 20 * 1024);
        let (frames, _) = ring.clip();
        assert!((frames[0].t - 3.0 / 50.0).abs() < 1e-9);
    }

    #[test]
    fn frames_of_another_size_are_fitted_to_the_canvas() {
        let frames = vec![
            ClipFrame::new(0.0, 4, 2, &solid(4, 2, rgba(1, 1, 1))),
            ClipFrame::new(0.04, 8, 4, &solid(8, 4, rgba(2, 2, 2))),
        ];
        let (bytes, written) = encode(&frames, 25, None);
        assert_eq!(written, 2);
        let decoded = decode(&bytes);
        assert_eq!((decoded[1].width, decoded[1].height), (4, 2));
        assert!(decoded[1]
            .buffer
            .chunks_exact(4)
            .all(|px| px == [2, 2, 2, 255]));
    }

    #[test]
    fn write_clip_creates_the_file_and_its_directory() {
        let dir = std::env::temp_dir().join(format!(
            "copperline-gifclip-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("nested").join("clip.gif");
        let frames = vec![ClipFrame::new(0.0, 2, 2, &solid(2, 2, rgba(3, 3, 3)))];
        assert_eq!(write_clip(&path, &frames, 25, None).unwrap(), 1);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(decode(&bytes).len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(write_clip(&path, &[], 25, None).is_err());
    }

    #[test]
    fn default_rate_follows_the_video_standard() {
        assert_eq!(
            ClipSettings::default().effective_fps(VideoStandard::Pal),
            25
        );
        assert_eq!(
            ClipSettings::default().effective_fps(VideoStandard::Ntsc),
            30
        );
        let fixed = ClipSettings {
            seconds: 5,
            fps: 10,
        };
        assert_eq!(fixed.effective_fps(VideoStandard::Ntsc), 10);
    }
}
