// SPDX-License-Identifier: GPL-3.0-or-later

//! Video files to NewTek Video Flyer clips and back: the `flyerclip`
//! program and the MCP bridge's `flyer_import` / `flyer_export` tools.
//!
//! A Flyer clip holds NTSC composite video (the Toaster framestore's 4fsc
//! samples, 752 per line) coded field by field with VTASC, and 16-bit
//! audio at 44.1 kHz x 1000/1001, all in colour frames of four fields
//! (`flyerclip`, from NewTek's Flyer sources). Importing has ffmpeg square
//! the source's pixels (anamorphic video keeps its shape), fit it to 4:3
//! (letterboxed or pillarboxed like a Toaster input, cropped, or
//! stretched) and scale it to 752 x 480, then encodes the fields as
//! composite samples and codes them. By default each field is its own
//! moment, 59.94 a second: interlaced sources are split into their
//! fields, progressive ones sampled at the field rate (film gets 3:2
//! pulldown), and fields I-IV take the even, odd, even and odd lines of
//! four pictures in turn, III and IV with the subcarrier inverted (the
//! framestore's complemented copy). [`Motion::Frames`] takes both fields
//! of a frame from one picture instead. Stills are held for a set time and
//! numbered pictures play at a set rate. A crouton icon (`<clip>.I`) can
//! be written beside the clip. Exporting decodes the fields, separates
//! luma and chroma per field, weaves each pair back into a frame and has
//! ffmpeg write the file.

use crate::toaster::composite;
use anyhow::{anyhow, bail, Context, Result};
use flyerclip::vtasc::TOASTER_LINES;
use flyerclip::{Clip, ClipWriter, FrameVideo, Grade};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Samples per line, and the picture's lines.
const W: usize = 752;
const H: usize = 480;
/// Video frames and fields per second, and the Flyer's audio rate as
/// ffmpeg takes them.
const FPS: &str = "30000/1001";
const FIELD_RATE: &str = "60000/1001";
const AUDIO_RATE: u32 = 44056;
/// Audio samples per channel per field, and fields per colour frame.
const PER_FIELD: usize = flyerclip::audio::SAMPLES_PER_FIELD;

/// How motion is sampled into the clip's fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Motion {
    /// Every field its own moment, 59.94 a second, as a camera records
    /// (interlaced sources are field-separated, film gets 3:2 pulldown).
    #[default]
    Fields,
    /// Both fields of a frame from one picture, 29.97 frames a second.
    Frames,
}

/// How a picture that is not 4:3 fills the frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fit {
    /// The whole picture, with black bars (a Toaster input's look).
    #[default]
    Letterbox,
    /// The frame filled, the picture's edges cropped.
    Fill,
    /// The picture squeezed or stretched to 4:3.
    Stretch,
}

/// What a source is, as far as ffmpeg's input goes.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// A video file (anything ffmpeg reads), with its sound.
    Video(PathBuf),
    /// A still picture, held for [`ImportOptions::still_seconds`].
    Still(PathBuf),
    /// Numbered pictures: an ffmpeg pattern (`dir/shot%04d.png`), the
    /// first number and how many there are.
    Sequence {
        pattern: PathBuf,
        start: u32,
        count: u32,
    },
}

/// File extensions taken as still pictures.
pub const STILL_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "bmp", "tif", "tiff", "gif", "tga", "webp", "iff", "ilbm",
];
/// File extensions taken as video.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "avi", "mkv", "webm", "mpg", "mpeg", "mts", "m2ts", "ts", "wmv", "flv",
    "dv", "mxf", "3gp", "ogv", "vob", "anim",
];

fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

impl Input {
    /// A file as a source: a still picture by its extension, else video.
    pub fn of_file(path: &Path) -> Self {
        if STILL_EXTENSIONS.contains(&ext_of(path).as_str()) {
            Input::Still(path.to_path_buf())
        } else {
            Input::Video(path.to_path_buf())
        }
    }

    /// The file or pattern the source is read from.
    pub fn path(&self) -> &Path {
        match self {
            Input::Video(p) | Input::Still(p) => p,
            Input::Sequence { pattern, .. } => pattern,
        }
    }

    /// A name for the clip made from it.
    pub fn stem(&self) -> String {
        let p = match self {
            Input::Sequence { pattern, .. } => pattern
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| pattern.clone()),
            other => other.path().to_path_buf(),
        };
        p.file_stem()
            .or_else(|| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("Clip")
            .to_string()
    }

    fn args(&self, options: &ImportOptions, cmd: &mut Command) {
        match self {
            Input::Video(p) => {
                if let Some(t) = options.start_seconds.filter(|t| *t > 0.0) {
                    cmd.args(["-ss", &format!("{t}")]);
                }
                cmd.arg("-i").arg(p);
            }
            Input::Still(p) => {
                cmd.args(["-loop", "1", "-framerate", FIELD_RATE, "-i"])
                    .arg(p);
            }
            Input::Sequence { pattern, start, .. } => {
                cmd.args([
                    "-framerate",
                    &format!("{}", options.sequence_fps),
                    "-start_number",
                    &start.to_string(),
                    "-i",
                ])
                .arg(pattern);
            }
        }
    }

    /// How long the clip made from it runs, in seconds, when that is
    /// known without ffmpeg (a still or a sequence).
    pub fn known_seconds(&self, options: &ImportOptions) -> Option<f64> {
        match self {
            Input::Video(_) => None,
            Input::Still(_) => Some(options.still_seconds),
            Input::Sequence { count, .. } => Some(f64::from(*count) / options.sequence_fps),
        }
    }
}

/// Numbered pictures in `dir`: the longest run sharing a name pattern,
/// as an [`Input::Sequence`] (at least two pictures).
pub fn find_sequence(dir: &Path) -> Option<Input> {
    use std::collections::BTreeMap;
    let mut runs: BTreeMap<(String, usize, String), Vec<u32>> = BTreeMap::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let ext = ext_of(&path);
        if !STILL_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let digits = stem.chars().rev().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            continue;
        }
        let (prefix, number) = stem.split_at(stem.len() - digits);
        let Ok(n) = number.parse::<u32>() else {
            continue;
        };
        let ext = path.extension()?.to_str()?.to_string();
        runs.entry((prefix.to_string(), digits, ext))
            .or_default()
            .push(n);
    }
    let ((prefix, digits, ext), mut numbers) = runs.into_iter().max_by_key(|(_, v)| v.len())?;
    if numbers.len() < 2 {
        return None;
    }
    numbers.sort_unstable();
    // ffmpeg reads from the first number until one is missing.
    let start = numbers[0];
    let count = numbers
        .iter()
        .enumerate()
        .take_while(|(i, n)| **n == start + *i as u32)
        .count() as u32;
    Some(Input::Sequence {
        pattern: dir.join(format!("{prefix}%0{digits}d.{ext}")),
        start,
        count,
    })
}

/// How to import.
#[derive(Clone, Debug)]
pub struct ImportOptions {
    /// Per-field size budget: std (default), HQ5, HQ6.
    pub grade: Grade,
    /// Keep the sound (stereo); off writes a silent, video-only clip.
    pub audio: bool,
    /// The sound's two channels mixed to one, on both.
    pub mono: bool,
    /// Gain of the left and right channel, in dB.
    pub gain_db: [f32; 2],
    /// Start this far into the source.
    pub start_seconds: Option<f64>,
    /// Stop after this many seconds of the source.
    pub max_seconds: Option<f64>,
    pub motion: Motion,
    pub fit: Fit,
    /// How long a still picture runs.
    pub still_seconds: f64,
    /// Pictures a second of an image sequence.
    pub sequence_fps: f64,
    /// Also write the crouton icon (`<clip>.I`) the Toaster's Grazer
    /// shows for it.
    pub icon: bool,
    /// Set from another thread to stop: the clip is not written.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            grade: Grade::Std,
            audio: true,
            mono: false,
            gain_db: [0.0; 2],
            start_seconds: None,
            max_seconds: None,
            motion: Motion::Fields,
            fit: Fit::Letterbox,
            still_seconds: 5.0,
            sequence_fps: 30000.0 / 1001.0,
            icon: true,
            cancel: None,
        }
    }
}

impl ImportOptions {
    fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }

    /// The video filter: square the pixels, separate fields, fit 4:3,
    /// then the Flyer's 752 samples by 480 lines at its rate.
    fn video_filter(&self) -> String {
        let deint = match self.motion {
            Motion::Fields => "bwdif=mode=send_field:parity=auto:deint=interlaced",
            Motion::Frames => "bwdif=mode=send_frame:parity=auto:deint=interlaced",
        };
        let fit = match self.fit {
            Fit::Letterbox => {
                "scale=640:480:force_original_aspect_ratio=decrease,\
                 pad=640:480:(ow-iw)/2:(oh-ih)/2:black"
            }
            Fit::Fill => "scale=640:480:force_original_aspect_ratio=increase,crop=640:480",
            Fit::Stretch => "scale=640:480",
        };
        let rate = match self.motion {
            Motion::Fields => FIELD_RATE,
            Motion::Frames => FPS,
        };
        format!(
            "{deint},scale='trunc(iw*sar/2)*2':ih:flags=lanczos,setsar=1,{fit},\
             scale={W}:{H}:flags=lanczos,fps={rate},format=rgba"
        )
    }

    /// The sound filter: gain per channel, or both channels mixed.
    fn audio_filter(&self) -> String {
        let g = |db: f32| 10f32.powf(db / 20.0);
        let (l, r) = (g(self.gain_db[0]), g(self.gain_db[1]));
        if self.mono {
            format!(
                "pan=stereo|c0={:.4}*c0+{:.4}*c1|c1={:.4}*c0+{:.4}*c1",
                l / 2.0,
                l / 2.0,
                r / 2.0,
                r / 2.0
            )
        } else {
            format!("pan=stereo|c0={l:.4}*c0|c1={r:.4}*c1")
        }
    }

    /// Output-side limits: the run time.
    fn duration(&self, input: &Input) -> Option<f64> {
        match (input.known_seconds(self), self.max_seconds) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// What a conversion did.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub frames: usize,
    pub fields: u32,
    pub seconds: f64,
    pub audio_channels: u8,
    pub bytes: u64,
    /// Fields that had to be coded sub-Nyquist or with loss to fit.
    pub degraded_fields: usize,
    /// Fields cut at the size budget (their bottom lines are garbage).
    pub truncated_fields: usize,
}

/// Where a conversion has got to: colour frames written, and the
/// latest picture (752 x 480 RGBA) every so often.
pub struct Progress<'a> {
    pub frames: usize,
    pub picture: Option<&'a [u32]>,
}

/// ffmpeg: `COPPERLINE_FFMPEG`, then `search` (e.g. the Toaster files
/// folder), beside this program, the `PATH`.
pub fn find_ffmpeg(search: Option<&Path>) -> Option<PathBuf> {
    let root = search
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::paths::toaster_files_dir);
    crate::toaster::media::find_ffmpeg(&root)
}

fn command(ffmpeg: &Path) -> Command {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashing up.
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// The source's sound as interleaved stereo at the Flyer's rate; empty
/// when it has none.
fn read_audio(ffmpeg: &Path, input: &Input, options: &ImportOptions) -> Result<Vec<i16>> {
    if !matches!(input, Input::Video(_)) {
        return Ok(Vec::new());
    }
    let mut cmd = command(ffmpeg);
    input.args(options, &mut cmd);
    if let Some(t) = options.duration(input) {
        cmd.args(["-t", &format!("{t}")]);
    }
    cmd.args([
        "-vn",
        "-af",
        &options.audio_filter(),
        "-ac",
        "2",
        "-ar",
        &AUDIO_RATE.to_string(),
        "-f",
        "s16le",
        "-",
    ])
    .stdin(Stdio::null())
    .stderr(Stdio::null());
    let out = cmd.output().context("running ffmpeg for the sound")?;
    // No audio stream is an error exit with nothing written: silence.
    Ok(out
        .stdout
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect())
}

/// One line of picture `p` (black past its end) as composite samples.
fn encode_field(picture: &[u32], parity: usize, complemented_field: bool) -> Vec<Vec<u8>> {
    let mut normal = vec![0u8; W];
    let mut complemented = vec![0u8; W];
    let black = vec![u32::from_le_bytes([0, 0, 0, 0xFF]); W];
    (0..TOASTER_LINES)
        .map(|y| {
            let line = 2 * y + parity;
            let row = if line < H {
                &picture[line * W..(line + 1) * W]
            } else {
                &black[..]
            };
            composite::encode_line(row, line, &mut normal, &mut complemented);
            if complemented_field {
                complemented.clone()
            } else {
                normal.clone()
            }
        })
        .collect()
}

/// The four fields of a colour frame from two pictures (`b` the later):
/// I/II the even and odd lines of `a`, III/IV those of `b` with the
/// subcarrier inverted (the framestore's complemented copy).
fn colour_frame(a: &[u32], b: &[u32]) -> FrameVideo {
    [
        encode_field(a, 0, false),
        encode_field(a, 1, false),
        encode_field(b, 0, true),
        encode_field(b, 1, true),
    ]
}

/// The four fields of a colour frame from four pictures a field apart:
/// each field the lines of its own moment.
fn colour_frame_of_fields(p: [&[u32]; 4]) -> FrameVideo {
    [
        encode_field(p[0], 0, false),
        encode_field(p[1], 1, false),
        encode_field(p[2], 0, true),
        encode_field(p[3], 1, true),
    ]
}

/// Convert a video file (anything ffmpeg reads) into a Flyer clip: a
/// still picture by its extension, else video.
pub fn import(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    options: &ImportOptions,
    mut progress: impl FnMut(usize),
) -> Result<Report> {
    import_input(
        ffmpeg,
        &Input::of_file(input),
        output,
        options,
        |p: Progress<'_>| progress(p.frames),
    )
}

/// Convert a source into a Flyer clip at `output` (and its crouton icon
/// beside it when [`ImportOptions::icon`] is set).
pub fn import_input(
    ffmpeg: &Path,
    input: &Input,
    output: &Path,
    options: &ImportOptions,
    mut progress: impl FnMut(Progress<'_>),
) -> Result<Report> {
    let audio = if options.audio {
        read_audio(ffmpeg, input, options)?
    } else {
        Vec::new()
    };
    let channels: u8 = if audio.is_empty() { 0 } else { 2 };
    if options.cancelled() {
        bail!("stopped");
    }

    let mut cmd = command(ffmpeg);
    input.args(options, &mut cmd);
    if let Some(t) = options.duration(input) {
        cmd.args(["-t", &format!("{t}")]);
    }
    cmd.args([
        "-an",
        "-vf",
        &options.video_filter(),
        "-pix_fmt",
        "rgba",
        "-f",
        "rawvideo",
        "-",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("starting ffmpeg")?;
    let mut stdout = child.stdout.take().context("ffmpeg's output")?;

    let mut writer = ClipWriter::new(options.grade, channels);
    let mut report = Report {
        audio_channels: channels,
        ..Report::default()
    };
    let mut buf = vec![0u8; W * H * 4];
    let read_picture = |stdout: &mut dyn Read, buf: &mut Vec<u8>| -> Option<Vec<u32>> {
        stdout.read_exact(buf).ok()?;
        Some(
            buf.chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], 0xFF]))
                .collect(),
        )
    };
    let per_frame = 4 * PER_FIELD * usize::from(channels);
    // Pictures per colour frame.
    let take = match options.motion {
        Motion::Fields => 4,
        Motion::Frames => 2,
    };
    let mut icon_picture: Option<Vec<u32>> = None;
    let mut stopped = false;
    'frames: loop {
        let mut pictures: Vec<Vec<u32>> = Vec::with_capacity(take);
        while pictures.len() < take {
            match read_picture(&mut stdout, &mut buf) {
                Some(p) => pictures.push(p),
                None => break,
            }
        }
        if pictures.is_empty() {
            break;
        }
        let last = pictures.len() < take;
        // A short last colour frame repeats its last picture.
        while pictures.len() < take {
            let p = pictures[pictures.len() - 1].clone();
            pictures.push(p);
        }
        let video = match options.motion {
            Motion::Fields => {
                colour_frame_of_fields([&pictures[0], &pictures[1], &pictures[2], &pictures[3]])
            }
            Motion::Frames => colour_frame(&pictures[0], &pictures[1]),
        };
        let start = report.frames * per_frame;
        let sound = (channels > 0).then(|| {
            let end = (start + per_frame).min(audio.len());
            &audio[start.min(end)..end]
        });
        let stats = writer.push_frame(Some(&video), sound);
        for s in stats.iter().flatten() {
            if s.truncated {
                report.truncated_fields += 1;
            } else if s.tolerance > 0 || s.algo == flyerclip::Algo::SubNyquist {
                report.degraded_fields += 1;
            }
        }
        report.frames += 1;
        // The icon shows the picture a second in (15 colour frames), or
        // the first for a shorter clip.
        if report.frames == 1 || report.frames == 16 {
            icon_picture = Some(pictures[0].clone());
        }
        progress(Progress {
            frames: report.frames,
            picture: (report.frames % 4 == 1).then_some(pictures[0].as_slice()),
        });
        if options.cancelled() {
            stopped = true;
            break 'frames;
        }
        if last {
            break;
        }
    }
    drop(stdout);
    if stopped {
        let _ = child.kill();
        let _ = child.wait();
        bail!("stopped");
    }
    let status = child.wait()?;
    if report.frames == 0 {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        bail!(
            "ffmpeg read no video from {} ({status}): {}",
            input.path().display(),
            err.trim()
        );
    }
    let bytes = writer.finish();
    if let Some(dir) = output.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(output, &bytes).with_context(|| format!("writing {}", output.display()))?;
    if options.icon {
        if let Some(picture) = &icon_picture {
            let icon = crouton_icon(picture);
            let path = icon_path(output);
            if let Err(e) = std::fs::write(&path, icon) {
                log::warn!("writing {}: {e}", path.display());
            }
        }
    }
    report.fields = (report.frames * 4) as u32;
    report.seconds = report.fields as f64 / 60.0 * 1.001;
    report.bytes = bytes.len() as u64;
    Ok(report)
}

/// The crouton icon file beside a clip: `<clip>.I`.
pub fn icon_path(clip: &Path) -> PathBuf {
    let mut name = clip.file_name().unwrap_or_default().to_os_string();
    name.push(".I");
    clip.with_file_name(name)
}

/// A clip's crouton icon as the Toaster's Grazer reads it
/// (`edit/src/iconfile.c`): a 64-colour, 80 x 50 ILBM of the crouton (a
/// raised tile with the picture in a well), a 4-colour ILBM of the same
/// tile, and a `CrUD` FORM saying the file is a CLIP crouton, each FORM
/// followed by a ` PUS` FORM giving its length so the file can be read
/// from the end backwards. The picture is `picture` (752 x 480 RGBA, 4:3)
/// in colours 8-63; colours 0-7 are the tile's greys and golds.
pub fn crouton_icon(picture: &[u32]) -> Vec<u8> {
    const IW: usize = 80;
    const IH: usize = 50;
    let small = icon_picture(picture);
    let palette = median_cut(&small, 64 - ICON_FIRST);
    let mut colour = [[0u8; 3]; 64];
    colour[..8].copy_from_slice(&TILE_COLOURS);
    for (k, c) in palette.iter().enumerate() {
        colour[ICON_FIRST + k] = *c;
    }
    let nearest = |c: [u8; 3]| -> u8 {
        let d = |p: [u8; 3]| {
            (0..3)
                .map(|k| (i32::from(c[k]) - i32::from(p[k])).pow(2))
                .sum::<i32>()
        };
        let mut best = (d([0, 0, 0]), 0u8);
        for (k, p) in palette.iter().enumerate() {
            let e = d(*p);
            if e < best.0 {
                best = (e, (ICON_FIRST + k) as u8);
            }
        }
        best.1
    };
    let mut full = vec![0u8; IW * IH];
    let mut four = vec![0u8; IW * IH];
    for y in 0..IH {
        for x in 0..IW {
            let tile = icon_tile(x, y);
            four[y * IW + x] = match tile {
                Tile::Picture | Tile::Well => 0,
                Tile::Back | Tile::Outline | Tile::Shadow => 1,
                Tile::Inner => 2,
                Tile::Light => 3,
            };
            full[y * IW + x] = match tile {
                Tile::Picture => {
                    let (px, py) = (x - ICON_PICTURE.0, y - ICON_PICTURE.1);
                    nearest(small[py * ICON_PICTURE.2 + px])
                }
                Tile::Well => 0,
                Tile::Back | Tile::Shadow => 1,
                Tile::Inner => 2,
                Tile::Light => 3,
                Tile::Outline => 4,
            };
        }
    }
    let mut out = Vec::new();
    let colour_bytes: Vec<u8> = colour.iter().flatten().copied().collect();
    let first = ilbm(IW, IH, 6, &colour_bytes, &full);
    let second_colours = [
        0, 0, 0, 0x60, 0x60, 0x60, 0x90, 0x90, 0x90, 0xD0, 0xD0, 0xD0,
    ];
    let second = ilbm(IW, IH, 2, &second_colours, &four);
    // TYPE CLIP, the effects library the crouton belongs to, and no tags.
    let mut crud = b"CrUD".to_vec();
    crud.extend(iff_chunk(b"TYPE", b"CLIP\0\0\0\0"));
    let mut libs = 0xFFFF_FCD0u32.to_be_bytes().to_vec();
    libs.extend(0x0010u16.to_be_bytes());
    libs.extend(b"effects.library\0\0\0");
    crud.extend(iff_chunk(b"LIBS", &libs));
    crud.extend(iff_chunk(b"TAGS", &[0; 4]));
    for (kind, form) in [(*b"ILBM", first), (*b"ILBM", second), (*b"CrUD", crud)] {
        out.extend(iff_chunk(b"FORM", &form));
        let mut pus = b" PUS".to_vec();
        pus.extend(kind);
        pus.extend(4u32.to_be_bytes());
        pus.extend((form.len() as u32).to_be_bytes());
        out.extend(iff_chunk(b"FORM", &pus));
    }
    out
}

/// The crouton tile's colours 0-7: black, the greys of its bevel and two
/// golds.
const TILE_COLOURS: [[u8; 3]; 8] = [
    [0x00, 0x00, 0x00],
    [0x50, 0x50, 0x50],
    [0x90, 0x90, 0x90],
    [0xD8, 0xD8, 0xD8],
    [0x2C, 0x2C, 0x2C],
    [0x40, 0x40, 0x40],
    [0xBC, 0xAE, 0x6E],
    [0xF2, 0xE2, 0x70],
];
/// The first colour the picture uses.
const ICON_FIRST: usize = 8;
/// The picture's place in the icon: x, y, width, height.
const ICON_PICTURE: (usize, usize, usize, usize) = (5, 4, 70, 40);

/// What a pixel of the crouton tile is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tile {
    /// Around the tile (and its cut corners).
    Back,
    Outline,
    /// The bevel's lit top and left, and its shaded bottom and right.
    Light,
    Shadow,
    /// The step down into the well.
    Inner,
    /// The black well round the picture.
    Well,
    Picture,
}

/// The crouton tile at `(x, y)`: an 80 x 48 raised tile with cut corners
/// over two rows of background.
fn icon_tile(x: usize, y: usize) -> Tile {
    const TW: usize = 80;
    const TH: usize = 48;
    let (px, py, pw, ph) = ICON_PICTURE;
    if (px..px + pw).contains(&x) && (py..py + ph).contains(&y) {
        return Tile::Picture;
    }
    if y >= TH {
        return Tile::Back;
    }
    let (dx, dy) = (x.min(TW - 1 - x), y.min(TH - 1 - y));
    let d = dx.min(dy);
    if dx + dy < 3 {
        return Tile::Back;
    }
    if d == 0 || dx + dy == 3 {
        return Tile::Outline;
    }
    let lit = if dy < dx { y < TH / 2 } else { x < TW / 2 };
    match d {
        1 | 2 => {
            if lit {
                Tile::Light
            } else {
                Tile::Shadow
            }
        }
        3 => {
            if lit {
                Tile::Inner
            } else {
                Tile::Light
            }
        }
        _ => Tile::Well,
    }
}

/// `picture` (752 x 480) box-filtered down to the icon's picture.
fn icon_picture(picture: &[u32]) -> Vec<[u8; 3]> {
    let (_, _, pw, ph) = ICON_PICTURE;
    let mut small = vec![[0u8; 3]; pw * ph];
    for (i, px) in small.iter_mut().enumerate() {
        let (x, y) = (i % pw, i / pw);
        let (x0, x1) = (x * W / pw, ((x + 1) * W / pw).max(x * W / pw + 1));
        let (y0, y1) = (y * H / ph, ((y + 1) * H / ph).max(y * H / ph + 1));
        let mut sum = [0u32; 3];
        for yy in y0..y1 {
            for xx in x0..x1 {
                let c = picture.get(yy * W + xx).copied().unwrap_or(0).to_le_bytes();
                for k in 0..3 {
                    sum[k] += u32::from(c[k]);
                }
            }
        }
        let n = ((x1 - x0) * (y1 - y0)) as u32;
        *px = sum.map(|v| (v / n) as u8);
    }
    small
}

/// An IFF chunk: id, big-endian length, data, padded to even.
fn iff_chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut c = id.to_vec();
    c.extend((data.len() as u32).to_be_bytes());
    c.extend(data);
    if data.len() % 2 == 1 {
        c.push(0);
    }
    c
}

/// An uncompressed ILBM's FORM contents (`ILBM`, BMHD, CMAP, BODY) of
/// `w` x `h` pixels of `planes` bit planes.
fn ilbm(w: usize, h: usize, planes: usize, cmap: &[u8], pixels: &[u8]) -> Vec<u8> {
    let mut bmhd = Vec::new();
    bmhd.extend((w as u16).to_be_bytes());
    bmhd.extend((h as u16).to_be_bytes());
    bmhd.extend([0u8; 4]); // x, y
    bmhd.extend([planes as u8, 0, 0, 0]); // planes, no mask, no compression
    bmhd.extend([0u8; 2]); // transparent colour
    bmhd.extend([1u8, 1]); // aspect
    bmhd.extend((w as u16).to_be_bytes());
    bmhd.extend((h as u16).to_be_bytes());
    let row = w.div_ceil(16) * 2;
    let mut body = vec![0u8; row * planes * h];
    for y in 0..h {
        for x in 0..w {
            let v = pixels[y * w + x];
            for p in 0..planes {
                if v >> p & 1 != 0 {
                    body[(y * planes + p) * row + x / 8] |= 0x80 >> (x % 8);
                }
            }
        }
    }
    let mut form = b"ILBM".to_vec();
    form.extend(iff_chunk(b"BMHD", &bmhd));
    form.extend(iff_chunk(b"CMAP", cmap));
    form.extend(iff_chunk(b"BODY", &body));
    form
}

/// Up to `n` colours standing for `pixels` (median cut).
fn median_cut(pixels: &[[u8; 3]], n: usize) -> Vec<[u8; 3]> {
    let mut boxes: Vec<Vec<[u8; 3]>> = vec![pixels.to_vec()];
    while boxes.len() < n {
        // Split the box with the widest channel range.
        let widest = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .map(|(i, b)| {
                let range = (0..3)
                    .map(|k| {
                        let (lo, hi) = b
                            .iter()
                            .fold((255u8, 0u8), |(lo, hi), p| (lo.min(p[k]), hi.max(p[k])));
                        (hi - lo, k)
                    })
                    .max()
                    .unwrap_or((0, 0));
                (range, i)
            })
            .max();
        let Some(((range, k), i)) = widest else {
            break;
        };
        if range == 0 {
            break;
        }
        let mut b = boxes.swap_remove(i);
        b.sort_unstable_by_key(|p| p[k]);
        let half = b.split_off(b.len() / 2);
        boxes.push(b);
        boxes.push(half);
    }
    boxes
        .iter()
        .filter(|b| !b.is_empty())
        .map(|b| {
            let mut sum = [0u32; 3];
            for p in b {
                for k in 0..3 {
                    sum[k] += u32::from(p[k]);
                }
            }
            sum.map(|v| (v / b.len() as u32) as u8)
        })
        .collect()
}

/// What ffmpeg says of a source: its run time, picture size and rate,
/// and whether it has sound.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe {
    pub seconds: Option<f64>,
    pub size: Option<(u32, u32)>,
    pub fps: Option<f64>,
    pub audio: bool,
    pub interlaced: bool,
}

/// Ask ffmpeg about `input` (`ffmpeg -i`, read from its report).
pub fn probe(ffmpeg: &Path, input: &Path) -> Result<Probe> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-nostdin", "-hide_banner", "-i"]).arg(input);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .context("running ffmpeg")?;
    Ok(parse_probe(&String::from_utf8_lossy(&out.stderr)))
}

fn parse_probe(text: &str) -> Probe {
    let mut p = Probe::default();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Duration: ") {
            let t = rest.split(',').next().unwrap_or("");
            let parts: Vec<f64> = t.split(':').filter_map(|v| v.parse().ok()).collect();
            if parts.len() == 3 {
                p.seconds = Some(parts[0] * 3600.0 + parts[1] * 60.0 + parts[2]);
            }
        } else if line.starts_with("Stream #") && line.contains("Video:") && p.size.is_none() {
            for word in line.split([',', ' ']) {
                if let Some((w, h)) = word.split_once('x') {
                    if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
                        if w > 0 && h > 0 && p.size.is_none() {
                            p.size = Some((w, h));
                        }
                    }
                }
            }
            let words: Vec<&str> = line.split([',', ' ']).filter(|w| !w.is_empty()).collect();
            for pair in words.windows(2) {
                if pair[1] == "fps" {
                    p.fps = pair[0].parse().ok();
                }
            }
            p.interlaced = line.contains("top first")
                || line.contains("bottom first")
                || line.contains("interlaced");
        } else if line.starts_with("Stream #") && line.contains("Audio:") {
            p.audio = true;
        }
    }
    p
}

/// One picture of `input` at `seconds`, as the clip would carry it
/// (fitted, 752 x 480 RGBA): a preview.
pub fn preview(
    ffmpeg: &Path,
    input: &Input,
    seconds: f64,
    options: &ImportOptions,
) -> Result<Vec<u32>> {
    let mut opts = options.clone();
    opts.motion = Motion::Frames;
    let mut cmd = command(ffmpeg);
    match input {
        Input::Video(p) => {
            cmd.args(["-ss", &format!("{:.3}", seconds.max(0.0))])
                .arg("-i")
                .arg(p);
        }
        Input::Still(p) => {
            cmd.arg("-i").arg(p);
        }
        Input::Sequence {
            pattern,
            start,
            count,
        } => {
            let n = start + ((seconds * opts.sequence_fps) as u32).min(count.saturating_sub(1));
            cmd.args(["-start_number", &n.to_string(), "-i"])
                .arg(pattern);
        }
    }
    cmd.args([
        "-frames:v",
        "1",
        "-an",
        "-vf",
        &opts.video_filter().replace(&format!(",fps={FPS}"), ""),
        "-pix_fmt",
        "rgba",
        "-f",
        "rawvideo",
        "-",
    ])
    .stdin(Stdio::null())
    .stderr(Stdio::null());
    let out = cmd.output().context("running ffmpeg")?;
    if out.stdout.len() < W * H * 4 {
        bail!("no picture");
    }
    Ok(out.stdout[..W * H * 4]
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], 0xFF]))
        .collect())
}

/// Blocks a field takes at most for `grade`, with `channels` of sound:
/// a clip's size runs up to this.
pub fn bytes_per_second(grade: Grade, channels: u8) -> f64 {
    let blocks = f64::from(grade.field_limit())
        + f64::from(channels) * flyerclip::audio::BLOCKS_PER_FIELD as f64;
    blocks * flyerclip::BLOCK as f64 * 60_000.0 / 1001.0
}

/// One field's 242 lines as RGBA rows of the picture they belong to.
fn decode_field_rows(lines: &[Vec<u8>], field: usize, picture: &mut [u32]) {
    let mut row = vec![0u32; W];
    for (y, samples) in lines.iter().enumerate() {
        let line = 2 * y + (field & 1);
        if line >= H {
            break;
        }
        let positive = flyerclip::composite::phase_positive(field as u32, y);
        composite::decode_single(samples, positive, &mut row);
        picture[line * W..(line + 1) * W].copy_from_slice(&row);
    }
}

/// Convert a Flyer clip into a video file (the container and codec come
/// from the extension: MP4 gets H.264 and AAC).
pub fn export(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    mut progress: impl FnMut(usize),
) -> Result<Report> {
    let bytes = std::fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let clip =
        Clip::read(&bytes).map_err(|e| anyhow!("{}: not a Flyer clip: {e:?}", input.display()))?;
    let fields = clip.field_count();
    if fields == 0 {
        bail!("{} has no video", input.display());
    }
    let (channels, audio) = clip.audio();

    // The sound goes through a temporary file: ffmpeg reads two inputs.
    let audio_file = if channels > 0 && !audio.is_empty() {
        let path = std::env::temp_dir().join(format!(
            "flyerclip-{}-{}.pcm",
            std::process::id(),
            crate::timestamp::compact_now()
        ));
        let mut raw = Vec::with_capacity(audio.len() * 2);
        for s in &audio {
            raw.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(&path, raw)?;
        Some(path)
    } else {
        None
    };

    let mut cmd = command(ffmpeg);
    cmd.arg("-y")
        .args([
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            &format!("{W}x{H}"),
        ])
        .args(["-r", FPS, "-i", "-"]);
    if let Some(path) = &audio_file {
        cmd.args(["-f", "s16le", "-ar", &AUDIO_RATE.to_string()])
            .args(["-ac", &channels.to_string(), "-i"])
            .arg(path);
    }
    cmd.args(["-vf", "scale=640:480,setsar=1"]);
    let is_mp4 = output
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mp4" | "m4v" | "mov"));
    if is_mp4 {
        cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "16"]);
        if audio_file.is_some() {
            cmd.args(["-c:a", "aac", "-b:a", "192k"]);
        }
    }
    cmd.arg(output)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("starting ffmpeg")?;
    let mut stdin = child.stdin.take().context("ffmpeg's input")?;

    let mut report = Report {
        audio_channels: channels,
        fields,
        ..Report::default()
    };
    let mut picture = vec![u32::from_le_bytes([0, 0, 0, 0xFF]); W * H];
    let mut bytes_out = vec![0u8; W * H * 4];
    let mut write_err = None;
    for n in 0..fields {
        let lines = clip
            .decode_field(n)
            .map_err(|e| anyhow!("field {n}: {e:?}"))?;
        let f = (n % 4) as usize;
        decode_field_rows(&lines, f, &mut picture);
        // Fields II and IV complete a picture.
        if f & 1 == 1 || n + 1 == fields {
            for (o, p) in bytes_out.chunks_exact_mut(4).zip(&picture) {
                o.copy_from_slice(&p.to_le_bytes());
            }
            if let Err(e) = stdin.write_all(&bytes_out) {
                write_err = Some(e);
                break;
            }
            report.frames += 1;
            progress(report.frames);
        }
    }
    drop(stdin);
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    let status = child.wait()?;
    if let Some(path) = audio_file {
        let _ = std::fs::remove_file(path);
    }
    if !status.success() || write_err.is_some() {
        bail!(
            "ffmpeg could not write {} ({status}): {}",
            output.display(),
            err.trim()
        );
    }
    report.seconds = f64::from(fields) / 60.0 * 1.001;
    report.bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    Ok(report)
}

/// `path`, or when that exists `name (2).ext`, `name (3).ext`...: a
/// default output never replaces a file.
pub fn unused_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    let ext = path.extension().and_then(|e| e.to_str());
    (2..)
        .map(|n| {
            path.with_file_name(match ext {
                Some(ext) => format!("{stem} ({n}).{ext}"),
                None => format!("{stem} ({n})"),
            })
        })
        .find(|p| !p.exists())
        .expect("an unused name")
}

/// A one-paragraph description of a clip.
pub fn describe(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let clip =
        Clip::read(&bytes).map_err(|e| anyhow!("{}: not a Flyer clip: {e:?}", path.display()))?;
    let h = &clip.header;
    let fields = clip.field_count();
    Ok(format!(
        "{}: Flyer clip, {} fields ({:.2} s), {} audio channel(s), video grade {}, \
         Flyer version {}, VTASC chips {}, {} bytes",
        path.display(),
        fields,
        f64::from(fields) / 60.0 * 1.001,
        h.audio_chans,
        match h.vid_grade {
            0 => "std",
            1 => "HQ5",
            2 => "HQ6",
            _ => "?",
        },
        h.version,
        h.vid_cmp_ver,
        bytes.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpegs_report_gives_the_run_time_size_rate_and_sound() {
        let text = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'a.mp4':
  Duration: 00:01:02.50, start: 0.000000, bitrate: 2000 kb/s
  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(tv, bt709, top first), 1920x1080 [SAR 1:1 DAR 16:9], 1800 kb/s, 29.97 fps, 29.97 tbr, 30k tbn (default)
  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp, 192 kb/s (default)";
        let p = parse_probe(text);
        assert_eq!(p.seconds, Some(62.5));
        assert_eq!(p.size, Some((1920, 1080)));
        assert_eq!(p.fps, Some(29.97));
        assert!(p.audio);
        assert!(p.interlaced);
    }

    #[test]
    fn numbered_pictures_in_a_folder_are_a_sequence() {
        let dir = tempfile::tempdir().unwrap();
        for n in [7, 8, 9, 10, 12] {
            std::fs::write(dir.path().join(format!("shot_{n:04}.png")), b"").unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), b"").unwrap();
        std::fs::write(dir.path().join("logo.png"), b"").unwrap();
        let Some(Input::Sequence {
            pattern,
            start,
            count,
        }) = find_sequence(dir.path())
        else {
            panic!("no sequence");
        };
        assert_eq!(pattern, dir.path().join("shot_%04d.png"));
        assert_eq!((start, count), (7, 4));
        assert!(find_sequence(&dir.path().join("none")).is_none());
    }

    #[test]
    fn the_crouton_icon_carries_the_picture_inside_the_bevel() {
        let red = u32::from_le_bytes([220, 20, 20, 0xFF]);
        let blue = u32::from_le_bytes([20, 20, 220, 0xFF]);
        let picture: Vec<u32> = (0..W * H)
            .map(|i| if i % W < W / 2 { red } else { blue })
            .collect();
        let icon = crouton_icon(&picture);
        assert_eq!(&icon[..4], b"FORM");
        let cmap = icon.windows(4).position(|w| w == b"CMAP").unwrap() + 8;
        let body = icon.windows(4).position(|w| w == b"BODY").unwrap() + 8;
        let index = |x: usize, y: usize| {
            (0..6).fold(0usize, |v, p| {
                let b = icon[body + (y * 6 + p) * 10 + x / 8] >> (7 - x % 8) & 1;
                v | usize::from(b) << p
            })
        };
        let colour = |i: usize| {
            [
                icon[cmap + 3 * i],
                icon[cmap + 3 * i + 1],
                icon[cmap + 3 * i + 2],
            ]
        };
        assert_eq!(colour(index(10, 20)), [220, 20, 20]);
        assert_eq!(colour(index(70, 20)), [20, 20, 220]);
        // The tile round it, and the rest of the file the Grazer reads.
        assert_eq!(index(0, 0), 1);
        assert_eq!(index(40, 1), 3);
        assert_eq!(index(40, 46), 1);
        assert_eq!(&icon[cmap..cmap + 3], &[0, 0, 0]);
        let crud = icon
            .windows(8)
            .position(|w| w == b"TYPE\0\0\0\x08")
            .unwrap();
        assert_eq!(&icon[crud + 8..crud + 12], b"CLIP");
        // Read backwards from the end, as iconfile.c does: the last ` PUS`
        // gives the CrUD FORM's length.
        let end = icon.len();
        assert_eq!(&icon[end - 24..end - 16], b"FORM\0\0\0\x10");
        assert_eq!(&icon[end - 16..end - 8], b" PUSCrUD");
        let len = u32::from_be_bytes(icon[end - 4..end].try_into().unwrap()) as usize;
        let form = end - 24 - len - 8;
        assert_eq!(&icon[form..form + 4], b"FORM");
        assert_eq!(&icon[form + 8..form + 12], b"CrUD");
    }

    #[test]
    fn a_colour_frame_round_trips_through_vtasc() {
        // Colour bars on picture a, a grey ramp on picture b.
        let bars = [
            [191u8, 191, 191],
            [191, 191, 0],
            [0, 191, 191],
            [0, 191, 0],
            [191, 0, 191],
            [191, 0, 0],
            [0, 0, 191],
        ];
        let a: Vec<u32> = (0..W * H)
            .map(|i| {
                let [r, g, b] = bars[(i % W) * 7 / W];
                u32::from_le_bytes([r, g, b, 0xFF])
            })
            .collect();
        let b: Vec<u32> = (0..W * H)
            .map(|i| {
                let v = ((i % W) * 255 / W) as u8;
                u32::from_le_bytes([v, v, v, 0xFF])
            })
            .collect();
        let mut writer = ClipWriter::new(Grade::Hq6, 0);
        writer.push_frame(Some(&colour_frame(&a, &b)), None);
        let bytes = writer.finish();
        let clip = Clip::read(&bytes).unwrap();
        assert_eq!(clip.field_count(), 4);
        let mut pa = vec![0u32; W * H];
        let mut pb = vec![0u32; W * H];
        for n in 0..4 {
            let lines = clip.decode_field(n).unwrap();
            decode_field_rows(&lines, n as usize, if n < 2 { &mut pa } else { &mut pb });
        }
        let close = |got: u32, want: u32, slack: i32| {
            let (g, w) = (got.to_le_bytes(), want.to_le_bytes());
            (0..3).all(|k| (i32::from(g[k]) - i32::from(w[k])).abs() <= slack)
        };
        // Inside each bar (away from the edges the filters smear) and on
        // the ramp, on both fields' lines. The Flyer's FIR comes back
        // with chroma a little hot, so colour gets more slack.
        for row in [10, 11, 200, 301] {
            for bar in 0..7 {
                let x = bar * W / 7 + W / 14;
                assert!(
                    close(pa[row * W + x], a[row * W + x], 40),
                    "bar {bar} row {row}: {:?}",
                    pa[row * W + x].to_le_bytes()
                );
            }
            for x in [100, 376, 650] {
                assert!(
                    close(pb[row * W + x], b[row * W + x], 12),
                    "ramp {x} row {row}: {:?}",
                    pb[row * W + x].to_le_bytes()
                );
            }
        }
    }
}
