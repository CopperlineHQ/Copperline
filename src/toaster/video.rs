// SPDX-License-Identifier: GPL-3.0-or-later

//! The card's video side: its four inputs (fed from the host folders in
//! [`super::media`]), its two outputs (Program and Preview, published as
//! [`CardOutput`]s for the host's monitor windows and recordings), and the
//! framestore seen as pictures.

use std::path::PathBuf;
use std::sync::Arc;

use super::composite;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use super::media::{self, InputFeed, MediaFolders};
use super::regs::Cds;
use crate::videoslot::CardOutput;
use crate::videoslot::FieldPixels;

/// Picture size, in card pixels and interlaced lines.
pub const PICTURE_W: usize = 752;
pub const PICTURE_H: usize = 480;
/// Card pixel of the picture's first column: the switcher's bitmaps (and
/// so its framestore files) start at card pixel 12.
pub const PICTURE_X0: usize = 12;
/// Framestore line of the picture's first row: framestore loads start at
/// line 1 (line 0 is the half line), `frameslice.a` "destination line".
pub const PICTURE_LINE0: usize = 1;
/// The Amiga display line that carries the header of framestore line pair
/// 0: the header sprites start on line 21, the first line of active video.
pub const AMIGA_LINE0: u32 = 21;
/// Display aspect of a picture.
pub const ASPECT: f32 = 4.0 / 3.0;
/// One NTSC frame, in seconds.
pub const FRAME_SECONDS: f64 = 1001.0 / 30000.0;

/// How long the emulated Amiga takes over a frame (525 lines of 227.5
/// colour clocks) by the emulator's clock, which counts colour clocks at
/// the PAL rate for every machine: 1/29.70 s rather than NTSC's 1/29.97.
/// The cards' outputs advance a frame per Amiga frame, so a frame's sound
/// is stretched to this length (0.9 %) to keep step with the mixer.
pub const AMIGA_FRAME_SECONDS: f64 = 525.0 * 227.5 / crate::chipset::paula::PAULA_CLOCK_HZ as f64;

pub fn black() -> u32 {
    u32::from_le_bytes([0, 0, 0, 0xFF])
}

pub struct Video {
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    inputs: Vec<InputFeed>,
    program: Arc<CardOutput>,
    preview: Arc<CardOutput>,
    /// The host folder the connectors live in.
    media_root: Option<PathBuf>,
    /// Fractional mixer samples carried between frames.
    audio_carry: f64,
    /// Pictures decoded from the two framestore banks, kept until the
    /// bank is written ([`Self::invalidate`]): the picture part, and the
    /// lines below it a digital effect can reach.
    decoded: [Option<Vec<u32>>; 2],
    /// How many bank decodes there have been (tests).
    decodes: u64,
    /// The two fields of a frame: outputs publish on the second.
    odd_field: bool,
    /// Picture-sized buffers between uses ([`Self::take_picture`]).
    pool: Vec<Vec<u32>>,
    /// The last Program frame, for the IS mux's feedback position.
    last_program: Vec<u32>,
    /// Digitising the input this field (live DVE).
    live: bool,
    noise: u32,
}

impl Video {
    pub fn new(media_root: Option<PathBuf>) -> Self {
        let root = media_root.clone();
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        let (inputs, output_dir) = match media_root {
            Some(root) => {
                let folders = MediaFolders::new(root);
                if let Err(e) = folders.prepare() {
                    log::warn!("toaster: preparing {}: {e}", folders.root().display());
                }
                let ffmpeg = media::find_ffmpeg(folders.root());
                match &ffmpeg {
                    Some(p) => log::info!("toaster: ffmpeg for video inputs: {}", p.display()),
                    None => log::info!("toaster: no ffmpeg found; inputs play still pictures only"),
                }
                log::info!("toaster: host folder {}", folders.root().display());
                let inputs = (0..media::INPUTS)
                    .map(|n| InputFeed::spawn(n, folders.input(n), ffmpeg.clone()))
                    .collect();
                (inputs, Some(folders.output()))
            }
            None => (Vec::new(), None),
        };
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        let output_dir: Option<PathBuf> = {
            let _ = media_root;
            None
        };
        Self {
            #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
            inputs,
            program: CardOutput::new("Program", ASPECT, output_dir.clone()),
            preview: CardOutput::new("Preview", ASPECT, output_dir),
            media_root: root,
            audio_carry: 0.0,
            decoded: [None, None],
            decodes: 0,
            odd_field: false,
            pool: Vec::new(),
            last_program: Vec::new(),
            live: false,
            noise: 0x1234_5678,
        }
    }

    pub fn outputs(&self) -> Vec<Arc<CardOutput>> {
        vec![Arc::clone(&self.preview), Arc::clone(&self.program)]
    }

    pub fn media_root(&self) -> Option<&std::path::Path> {
        self.media_root.as_deref()
    }

    /// Start reading the inputs' folders (the switcher is driving the
    /// card).
    pub fn start_inputs(&self) {
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        for input in &self.inputs {
            input.start();
        }
    }

    /// Each input's folder, files, pinned file and what it plays now.
    pub fn input_status(&self) -> Vec<serde_json::Value> {
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            let show = |p: &std::path::Path| p.display().to_string();
            self.inputs
                .iter()
                .enumerate()
                .map(|(n, feed)| {
                    serde_json::json!({
                        "input": n + 1,
                        "folder": show(feed.dir()),
                        "files": feed.files().iter().map(|f| show(f)).collect::<Vec<_>>(),
                        "pinned": feed.pinned().as_deref().map(show),
                        "playing": feed.playing().as_deref().map(show),
                        "signal": feed.picture().is_some(),
                        "started": feed.started(),
                    })
                })
                .collect()
        }
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            Vec::new()
        }
    }

    /// Hold input `n` (0-based) to `file`, or back to its folder.
    pub fn pin_input(&self, n: usize, file: Option<PathBuf>) -> Result<(), String> {
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            let feed = self
                .inputs
                .get(n)
                .ok_or_else(|| "the card has no host folder, so no inputs".to_string())?;
            feed.pin(file);
            Ok(())
        }
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            let _ = (n, file);
            Err("inputs are not available in this build".into())
        }
    }

    /// The current picture of input `n`, if something is plugged in.
    pub fn input_picture(&self, n: usize) -> Option<&[u32]> {
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            self.inputs
                .get(n)
                .and_then(|i| i.picture())
                .map(|p| p.as_slice())
        }
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            let _ = n;
            None
        }
    }

    /// Start a field: `true` when it completes a frame.
    pub fn begin_field(&mut self) -> bool {
        self.odd_field = !self.odd_field;
        if !self.odd_field {
            #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
            for input in &mut self.inputs {
                input.advance();
            }
            return true;
        }
        false
    }

    /// Decode framestore bank `bank` to a picture: `PICTURE_H` rows, or
    /// with `below` every row to the end of the bank (a digital effect
    /// reaches the lines below the picture, where the switcher keeps its
    /// border colour). Decoded once, and kept until the bank is written.
    pub fn framestore_picture(
        &mut self,
        store: &super::FrameStore,
        bank: usize,
        below: bool,
    ) -> &[u32] {
        let rows = if below {
            super::LINES - PICTURE_LINE0
        } else {
            PICTURE_H
        };
        let slot = &mut self.decoded[bank & 1];
        let have = slot.as_ref().map_or(0, |p| p.len() / PICTURE_W);
        if have < rows {
            let mut pic = slot.take().unwrap_or_default();
            pic.resize(rows * PICTURE_W, black());
            for y in have..rows {
                let fl = PICTURE_LINE0 + y;
                let n = &store.line(bank, false, fl)[PICTURE_X0..PICTURE_X0 + PICTURE_W];
                let c = &store.line(bank, true, fl)[PICTURE_X0..PICTURE_X0 + PICTURE_W];
                composite::decode_line(n, c, y, &mut pic[y * PICTURE_W..(y + 1) * PICTURE_W]);
            }
            self.decodes += 1;
            *slot = Some(pic);
        }
        let pic = slot.as_deref().unwrap_or(&[]);
        &pic[..(rows * PICTURE_W).min(pic.len())]
    }

    /// Bank decodes so far (tests).
    pub fn decodes(&self) -> u64 {
        self.decodes
    }

    /// Digitise a picture into framestore bank `bank` (a grab).
    pub fn grab(store: &mut super::FrameStore, bank: usize, picture: &[u32]) {
        for y in 0..PICTURE_H {
            let fl = PICTURE_LINE0 + y;
            let row = &picture[y * PICTURE_W..(y + 1) * PICTURE_W];
            let mut n = [0u8; PICTURE_W];
            let mut c = [0u8; PICTURE_W];
            composite::encode_line(row, y, &mut n, &mut c);
            store.line_mut(bank, false, fl)[PICTURE_X0..PICTURE_X0 + PICTURE_W].copy_from_slice(&n);
            store.line_mut(bank, true, fl)[PICTURE_X0..PICTURE_X0 + PICTURE_W].copy_from_slice(&c);
        }
    }

    /// Publish a frame of each output, with the sound that goes with it.
    pub fn publish(
        &mut self,
        program: &[u32],
        preview: &[u32],
        program_sound: &[(f32, f32)],
        preview_sound: &[(f32, f32)],
    ) {
        self.program.publish(
            PICTURE_W,
            PICTURE_H,
            program,
            FRAME_SECONDS,
            Some(program_sound),
        );
        self.preview.publish(
            PICTURE_W,
            PICTURE_H,
            preview,
            FRAME_SECONDS,
            Some(preview_sound),
        );
        self.last_program.clear();
        self.last_program.extend_from_slice(program);
    }

    /// This frame's length in mixer samples: the emulated Amiga frame's
    /// length (about 1485; see [`AMIGA_FRAME_SECONDS`]).
    pub fn frame_samples(&mut self) -> usize {
        self.audio_carry += AMIGA_FRAME_SECONDS * f64::from(crate::audio::MIX_SAMPLE_RATE);
        let n = self.audio_carry.floor() as usize;
        self.audio_carry -= n as f64;
        n
    }

    /// The sound of input `n` for this frame, if it has any.
    pub fn input_sound(&self, n: usize) -> Option<&[(f32, f32)]> {
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            self.inputs
                .get(n)
                .and_then(|i| i.sound())
                .map(|s| s.as_slice())
        }
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            let _ = n;
            None
        }
    }

    /// The last Program frame (black before the first).
    pub fn last_program(&self, out: &mut [u32]) {
        if self.last_program.len() >= out.len() {
            out.copy_from_slice(&self.last_program[..out.len()]);
        } else {
            out.fill(black());
        }
    }

    pub fn set_live(&mut self, live: bool) {
        self.live = live;
    }

    pub fn live(&self) -> bool {
        self.live
    }

    /// A bank was written: decode it afresh.
    pub fn invalidate(&mut self, bank: usize) {
        self.decoded[bank & 1] = None;
    }

    /// A picture-sized buffer (`PICTURE_W * PICTURE_H`), from the pool or
    /// new. Its contents are whatever they were: every use writes every
    /// pixel.
    pub fn take_picture(&mut self) -> Vec<u32> {
        let mut p = self.pool.pop().unwrap_or_default();
        p.resize(PICTURE_W * PICTURE_H, black());
        p
    }

    /// Return a buffer from [`Self::take_picture`].
    pub fn give_back(&mut self, p: Vec<u32>) {
        if self.pool.len() < 12 {
            self.pool.push(p);
        }
    }

    /// Snow: the matte generator's noise.
    pub fn snow(&mut self, out: &mut [u32]) {
        let mut x = self.noise;
        for px in out.iter_mut() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let v = (x >> 24) as u8;
            *px = u32::from_le_bytes([v, v, v, 0xFF]);
        }
        self.noise = x;
    }
}

/// Which mux a source is for: position 7 differs between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Fader,
    Preview,
    Key,
    Is,
}

/// The fader's mix: one value for the picture, or one per pixel
/// (0 = the A bus, 255 = the B bus).
pub enum Key<'a> {
    Flat(u8),
    Map(&'a [u8]),
}

impl Key<'_> {
    /// The average mix, 0-255: how much of B the picture shows.
    pub fn mean(&self) -> f32 {
        match self {
            Key::Flat(k) => f32::from(*k),
            Key::Map([]) => 0.0,
            Key::Map(m) => m.iter().map(|&k| u64::from(k)).sum::<u64>() as f32 / m.len() as f32,
        }
    }
}

/// One pixel of Program = A·(1−k) + B·k, each gun rounded to nearest:
/// `(a·(255−k) + b·k + 127) / 255`. The three guns are weighed two at a
/// time in 16-bit lanes; `(t + (t >> 8)) >> 8` with `t = x + 128` is that
/// division exactly for every `x` a lane can hold.
#[inline]
pub fn blend(pa: u32, pb: u32, k: u8) -> u32 {
    match k {
        0 => pa,
        255 => pb,
        _ => {
            let (k, nk) = (u32::from(k), 255 - u32::from(k));
            let lanes = |x: u32| {
                let t = x + 0x0080_0080;
                ((t + ((t >> 8) & 0x00FF_00FF)) >> 8) & 0x00FF_00FF
            };
            // Red and blue in the low halves of two lanes, green in one.
            let rb = lanes((pa & 0x00FF_00FF) * nk + (pb & 0x00FF_00FF) * k);
            let g = lanes(((pa >> 8) & 0xFF) * nk + ((pb >> 8) & 0xFF) * k);
            rb | (g << 8) | 0xFF00_0000
        }
    }
}

/// Program = A·(1−k) + B·k.
pub fn mix(a: &[u32], b: &[u32], key: &Key<'_>, out: &mut [u32]) {
    match key {
        Key::Flat(0) => out.copy_from_slice(&a[..out.len()]),
        Key::Flat(255) => out.copy_from_slice(&b[..out.len()]),
        Key::Flat(k) => {
            for ((o, &pa), &pb) in out.iter_mut().zip(a).zip(b) {
                *o = blend(pa, pb, *k);
            }
        }
        Key::Map(map) => {
            for (((o, &pa), &pb), &k) in out.iter_mut().zip(a).zip(b).zip(map.iter()) {
                *o = blend(pa, pb, k);
            }
        }
    }
}

fn luma(px: u32) -> f32 {
    let [r, g, b, _] = px.to_le_bytes();
    0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)
}

/// Replace a picture by its luminance (the MONO bus).
pub fn to_mono(pic: &mut [u32]) {
    for px in pic.iter_mut() {
        let y = luma(*px).round().clamp(0.0, 255.0) as u8;
        *px = u32::from_le_bytes([y, y, y, 0xFF]);
    }
}

/// A luma key on `src`: with `key_black`, what is darker than the clip
/// shows the A bus (the background) and the rest the B bus (the fill);
/// keying on white, the other way round. A soft edge of a few levels.
pub fn luma_key_into(src: &[u32], clip: u8, key_black: bool, out: &mut Vec<u8>) {
    const SOFT: f32 = 12.0;
    let clip = f32::from(clip);
    out.clear();
    out.extend(src.iter().map(|&px| {
        let d = if key_black {
            luma(px) - clip
        } else {
            clip - luma(px)
        };
        ((d / SOFT + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8
    }));
}

/// The Amiga pixels over picture row `y`: the line whose header addresses
/// the picture line, sampled at the card's pixel pitch, picture column 0
/// first. `None` where the field has no such line.
fn amiga_row<'a>(
    field: &'a FieldPixels<'_>,
    x0: i64,
    y: usize,
) -> Option<impl Iterator<Item = Option<u32>> + 'a> {
    let vpos = AMIGA_LINE0 + ((y + PICTURE_LINE0) >> 1) as u32;
    let row = field.row(vpos)?;
    let scale = field.canvas_scale as i64;
    let first = x0 + PICTURE_X0 as i64 * scale;
    Some((0..PICTURE_W as i64).map(move |x| {
        usize::try_from(first + x * scale)
            .ok()
            .and_then(|c| row.get(c))
            .copied()
    }))
}

/// The visible colour of an AGA pixel: the high nibbles, which the
/// switcher keeps for what the Amiga monitor shows (the low nibbles carry
/// the card's control bits).
fn visible_colour(px: u32) -> u32 {
    let [r, g, b, a] = px.to_le_bytes();
    let hi = |v: u8| (v & 0xF0) | (v >> 4);
    u32::from_le_bytes([hi(r), hi(g), hi(b), a | 0xFF])
}

/// The Amiga's own picture over the card's picture area (the encoder
/// without MATTE).
pub fn amiga_picture(field: &FieldPixels<'_>, x0: i64, out: &mut [u32]) {
    for (y, line) in out.chunks_exact_mut(PICTURE_W).enumerate() {
        match amiga_row(field, x0, y) {
            Some(row) => {
                for (o, px) in line.iter_mut().zip(row) {
                    *o = px.map_or_else(black, visible_colour);
                }
            }
            None => line.fill(black()),
        }
    }
}

/// The per-pixel source codes of a latch effect (`AMWIPE`/`BMWIPE` with
/// the bus at 7): a pixel whose colour carries the genlock T bit (shown
/// transparent) sets the source from its low nibbles, b2 + 2·r2 + 4·r1,
/// in the muxes' numbering (`LatchLUT`, animfxhandler.a); other pixels
/// keep the source last set on the line. `None` before the first.
pub fn latch_codes(field: &FieldPixels<'_>, x0: i64) -> Vec<Option<u8>> {
    let mut codes = Vec::new();
    latch_codes_into(field, x0, &mut codes);
    codes
}

/// [`latch_codes`] into a buffer kept between frames.
pub fn latch_codes_into(field: &FieldPixels<'_>, x0: i64, codes: &mut Vec<Option<u8>>) {
    codes.clear();
    codes.resize(PICTURE_W * PICTURE_H, None);
    for (y, line) in codes.chunks_exact_mut(PICTURE_W).enumerate() {
        let Some(row) = amiga_row(field, x0, y) else {
            continue;
        };
        let mut held = None;
        for (slot, px) in line.iter_mut().zip(row) {
            if let Some(px) = px {
                let [r, _, b, a] = px.to_le_bytes();
                if a == 0 {
                    held = Some(((b >> 2) & 1) | (((r >> 2) & 1) << 1) | (((r >> 1) & 1) << 2));
                }
            }
            *slot = held;
        }
    }
}

/// A per-pixel fader mix from the Amiga's control bits (the AGA low
/// nibbles), for the modes that take one.
pub fn amiga_key_into(field: &FieldPixels<'_>, x0: i64, mode: Cds, cd: u8, key: &mut Vec<u8>) {
    key.clear();
    key.resize(PICTURE_W * PICTURE_H, 0);
    for (y, line) in key.chunks_exact_mut(PICTURE_W).enumerate() {
        let Some(row) = amiga_row(field, x0, y) else {
            continue;
        };
        for (slot, px) in line.iter_mut().zip(row) {
            let Some(px) = px else {
                continue;
            };
            let [r, g, b, a] = px.to_le_bytes();
            let k = match mode {
                // Two bits: b0, b1. 0 shows A, 3 shows B.
                Cds::Dib => u32::from(b & 3) * 255 / 3,
                // Four bits: r1, g1, b0, b1, scaled by CD for key fades.
                Cds::Dibgr => {
                    let code = u32::from((r >> 1) & 1)
                        | (u32::from((g >> 1) & 1) << 1)
                        | (u32::from(b & 3) << 2);
                    code * u32::from(cd) / 15
                }
                // Genlock transparency: see-through pixels show A.
                Cds::PixelSw => {
                    if a == 0 {
                        0
                    } else {
                        u32::from(cd)
                    }
                }
                _ => u32::from(cd),
            };
            *slot = k.min(255) as u8;
        }
    }
}

/// USERON: the switcher's interface over the Preview output, where its
/// pixels carry an overlay level (the r1/g1 control bits).
pub fn overlay_interface(field: &FieldPixels<'_>, x0: i64, out: &mut [u32]) {
    for (y, line) in out.chunks_exact_mut(PICTURE_W).enumerate() {
        let Some(row) = amiga_row(field, x0, y) else {
            continue;
        };
        for (o, px) in line.iter_mut().zip(row) {
            let Some(px) = px else {
                continue;
            };
            let [r, g, _, _] = px.to_le_bytes();
            if (r | g) & 0x02 != 0 {
                *o = visible_colour(px);
            }
        }
    }
}

/// The matte generator's colour for a code: the eight BKG colours are
/// exact; other codes follow the generator's model (luma from the mean of
/// the two levels, chroma from their difference, hue from MP).
pub fn matte_colour(ma: u8, mb: u8, mp: u8) -> u32 {
    let rgb = |r: u8, g: u8, b: u8| u32::from_le_bytes([r, g, b, 0xFF]);
    match (ma, mb, mp) {
        (0, 0, 0) => return rgb(0, 0, 0),
        (0xB, 0xB, 0) => return rgb(191, 191, 191),
        (0xF, 0xF, 0xF) | (0xF, 0xF, 0) => return rgb(255, 255, 255),
        (9, 0, 6) => return rgb(191, 0, 0),
        (0, 0xB, 0xA) => return rgb(0, 191, 0),
        (0, 7, 1) => return rgb(0, 0, 191),
        (0xD, 5, 1) => return rgb(191, 191, 0),
        (9, 0, 0xA) => return rgb(191, 0, 191),
        (2, 0xD, 6) => return rgb(0, 191, 191),
        _ => {}
    }
    let y = (f32::from(ma) + f32::from(mb)) / 30.0;
    let c = (f32::from(ma) - f32::from(mb)).abs() / 30.0;
    let mut hue = f32::from(mp) * 11.25 - 48.0;
    if ma < mb {
        hue += 180.0;
    }
    let (s, co) = hue.to_radians().sin_cos();
    let (i, q) = (c * co, c * s);
    let r = y + 0.956 * i + 0.621 * q;
    let g = y - 0.272 * i - 0.647 * q;
    let b = y - 1.106 * i + 1.703 * q;
    let c8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    rgb(c8(r), c8(g), c8(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mix as a plain formula, per gun.
    fn blend_reference(pa: u32, pb: u32, k: u8) -> u32 {
        match k {
            0 => pa,
            255 => pb,
            _ => {
                let k = u32::from(k);
                let [ar, ag, ab, _] = pa.to_le_bytes();
                let [br, bg, bb, _] = pb.to_le_bytes();
                let m = |x: u8, y: u8| {
                    ((u32::from(x) * (255 - k) + u32::from(y) * k + 127) / 255) as u8
                };
                u32::from_le_bytes([m(ar, br), m(ag, bg), m(ab, bb), 0xFF])
            }
        }
    }

    #[test]
    fn the_packed_blend_rounds_every_gun_like_the_formula() {
        for k in 0..=255u8 {
            for x in (0..=255u32).step_by(3) {
                for y in (0..=255u32).step_by(5) {
                    // Each gun gets a different pair, so a carry between
                    // lanes would show.
                    let pa = u32::from_le_bytes([x as u8, (255 - x) as u8, (x * 7 % 256) as u8, 0]);
                    let pb = u32::from_le_bytes([y as u8, (y * 3 % 256) as u8, (255 - y) as u8, 0]);
                    let want = blend_reference(pa, pb, k);
                    assert_eq!(blend(pa, pb, k), want, "k={k} a={pa:08X} b={pb:08X}");
                }
            }
        }
        // Every value of one gun against every key, exactly.
        for k in 1..=254u8 {
            for x in 0..=255u32 {
                let pa = u32::from_le_bytes([x as u8, 0, x as u8, 0xFF]);
                let pb = u32::from_le_bytes([255 - x as u8, 0, 0, 0xFF]);
                assert_eq!(blend(pa, pb, k), blend_reference(pa, pb, k));
            }
        }
    }

    #[test]
    fn a_flat_key_at_either_end_is_a_copy() {
        let a: Vec<u32> = (0..8u32).map(|i| (i * 0x0102_0304) | 0xFF00_0000).collect();
        let b: Vec<u32> = (0..8u32).map(|i| (i * 0x0403_0201) | 0xFF00_0000).collect();
        let mut out = vec![0; 8];
        mix(&a, &b, &Key::Flat(0), &mut out);
        assert_eq!(out, a);
        mix(&a, &b, &Key::Flat(255), &mut out);
        assert_eq!(out, b);
        mix(
            &a,
            &b,
            &Key::Map(&[0, 255, 0, 255, 0, 255, 0, 255]),
            &mut out,
        );
        for i in 0..8 {
            assert_eq!(out[i], if i % 2 == 0 { a[i] } else { b[i] });
        }
    }
}
