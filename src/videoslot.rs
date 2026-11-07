// SPDX-License-Identifier: GPL-3.0-or-later

//! Video-slot peripheral boundary.
//!
//! The A2000/A3000/A4000 video slot carries Denise's digital RGB output and
//! the beam syncs. A card that sits there (the NewTek Video Toaster is the
//! reason this exists) watches the picture the Amiga is displaying and acts
//! on it. Some such cards also answer the host: the Toaster's switcher
//! reads framestore and calibration data back over the parallel port
//! (`$BFE101`, CIA-A port B), polling it at beam-timed positions while the
//! card drives the data pins.
//!
//! Copperline does not paint pixels while the chipset runs (see
//! `docs/internals/video.md`): register writes are recorded with their beam
//! positions and the frame is replayed afterwards. A video-slot device
//! therefore sees the picture in two ways:
//!
//! - [`VideoSlotDevice::field_complete`] receives every finished field,
//!   rendered through the same painter the display uses, at the frame wrap.
//! - [`VideoSlotDevice::parallel_read`] runs on every guest read of
//!   `$BFE101` and gets a [`LiveField`] that renders the line the beam is
//!   on, from the field still in progress. A card that answers a request
//!   drawn on the very line being displayed (the Toaster puts a byte on the
//!   port while its pixel is passing) uses this; the render is one row, so
//!   software that polls the port thousands of times a field stays cheap.
//!
//! Both views paint a line wider than the display framebuffer: colour
//! clocks [`SLOT_FIRST_CCK`] onwards for [`SLOT_LINE_CCK`] clocks, which
//! covers the whole active line a video card samples (the Toaster's 768
//! pixels start left of the display capture and run past its right edge).
//! Rows start at the field's first captured line, which a fitted card moves
//! up to the first line sprite DMA can run on (`Bus::capture_start_vpos`).
//! [`FieldPixels`] converts beam positions to the canvas.

/// Beam position of an event, plus the deterministic power-on colour-clock
/// timestamp.
use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BeamPos {
    pub vpos: u32,
    pub hpos: u32,
    pub cck: u64,
    /// Long-frame flag of the field the beam is in (LOF).
    pub long_frame: bool,
    /// Emulated frame counter of the field the beam is in. A field's
    /// completion ([`VideoSlotDevice::field_complete`]) carries the same
    /// number as the parallel reads made while it was in progress.
    pub frame: u64,
}

/// First colour clock of the slot canvas. Lo-res position `$5A` on the
/// Denise comparator: the Toaster's line header sprite sits at `$5B`.
pub const SLOT_FIRST_CCK: i32 = 0x24;
/// Colour clocks the slot canvas spans: through clock `$F7`, past the end
/// of the Toaster's 768-pixel line (`$5C` to `$1DC` lo-res) and short of
/// the line's own end (227).
pub const SLOT_LINE_CCK: usize = 212;

/// A rendered field, as Denise drove the video slot.
///
/// `pixels` is a painter canvas: RGBA8 packed little-endian (red in the low
/// byte), with the genlock transparency in the alpha byte. Column 0 is
/// colour clock `first_cck`; each clock is four hi-res pixels (times
/// `canvas_scale`).
#[derive(Clone, Copy, Debug)]
pub struct FieldPixels<'a> {
    pub pixels: &'a [u32],
    /// Row stride in pixels.
    pub width: usize,
    /// Rows of the field that were rendered.
    pub rows: usize,
    /// Beam line of row 0.
    pub visible_start_vpos: u32,
    /// Colour clock of column 0.
    pub first_cck: i32,
    /// Horizontal supersample factor (1 normally, 2 on 35 ns canvases).
    pub canvas_scale: usize,
    pub long_frame: bool,
    pub interlaced: bool,
    /// Rows `0..valid_rows` are final. Equal to `rows` for a completed field.
    /// A live field holds the beam's line alone, final to the left of the
    /// beam.
    pub valid_rows: usize,
}

impl<'a> FieldPixels<'a> {
    /// Canvas row for a beam line, if it was rendered and is final.
    pub fn row_index(&self, vpos: u32) -> Option<usize> {
        let row = vpos.checked_sub(self.visible_start_vpos)? as usize;
        (row < self.valid_rows.min(self.rows)).then_some(row)
    }

    /// One rendered row, by beam line.
    pub fn row(&self, vpos: u32) -> Option<&'a [u32]> {
        let row = self.row_index(vpos)?;
        let start = row * self.width;
        self.pixels.get(start..start + self.width)
    }

    /// Canvas column of the first hi-res pixel of colour clock `hpos`
    /// (negative or past the row for clocks outside the canvas).
    pub fn column_for_hpos(&self, hpos: u32) -> i64 {
        (i64::from(hpos) - i64::from(self.first_cck)) * 4 * self.canvas_scale as i64
    }

    /// Canvas column of a Denise lo-res comparator position (the DIW and
    /// sprite coordinate), left edge of its two hi-res pixels.
    pub fn column_for_lores(&self, lores: i32) -> i64 {
        // Lo-res position L is colour clock (L - $12) / 2; see the
        // renderer's DIW_HSTART_FB0 / COPPER_WAIT_HPOS_FB0 anchors.
        (i64::from(lores) - 0x12 - 2 * i64::from(self.first_cck)) * 2 * self.canvas_scale as i64
    }

    /// The RGBA8 pixel the beam was emitting at (`vpos`, `hpos`) plus `sub`
    /// hi-res pixels (0-3 within the colour clock).
    pub fn pixel_at_beam(&self, vpos: u32, hpos: u32, sub: u32) -> Option<u32> {
        let row = self.row(vpos)?;
        let col = self.column_for_hpos(hpos) + i64::from(sub) * self.canvas_scale as i64;
        usize::try_from(col).ok().and_then(|c| row.get(c).copied())
    }

    /// `pixel_at_beam` as 12-bit Amiga RGB (`$0RGB`), the four bits per gun
    /// an ECS-era video-slot card latches.
    pub fn rgb12_at_beam(&self, vpos: u32, hpos: u32, sub: u32) -> Option<u16> {
        self.pixel_at_beam(vpos, hpos, sub).map(rgba8_to_rgb12)
    }
}

/// RGBA8 framebuffer pixel to its high nibbles as `$0RGB`.
pub fn rgba8_to_rgb12(c: u32) -> u16 {
    let r = (c >> 4) & 0xF;
    let g = (c >> 12) & 0xF;
    let b = (c >> 20) & 0xF;
    ((r << 8) | (g << 4) | b) as u16
}

/// RGBA8 framebuffer pixel to 24-bit `$RRGGBB`.
pub fn rgba8_to_rgb24(c: u32) -> u32 {
    let [r, g, b, _] = c.to_le_bytes();
    (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

/// On-demand render of lines of the field in progress.
///
/// [`LiveField::pixels`] renders the beam's line; [`LiveField::line`] any
/// line the beam has passed (a card that keeps per-line state, such as the
/// Toaster's line headers, walks the lines above the beam before answering
/// a read). A render is reused while the beam stays put.
pub struct LiveField<'a> {
    pub(crate) bus: &'a crate::bus::Bus,
    pub(crate) cache: &'a mut LiveFieldCache,
    pub(crate) beam: BeamPos,
}

impl<'a> LiveField<'a> {
    pub fn beam(&self) -> BeamPos {
        self.beam
    }

    /// First line of the field's capture: the line the completed field
    /// handed to [`VideoSlotDevice::field_complete`] will start at.
    pub fn first_vpos(&self) -> u32 {
        self.bus.in_progress_render_parts().visible_start_vpos
    }

    /// The beam's own line of the in-progress field, final to the left of
    /// the beam: a one-row canvas whose row 0 is the beam line.
    pub fn pixels(&mut self) -> FieldPixels<'_> {
        let vpos = self.beam.vpos;
        self.render_line(vpos, self.beam.hpos)
    }

    /// One line the beam has passed (`vpos < beam.vpos`), final across its
    /// width; `None` for the beam's line or below it, or above the capture.
    pub fn line(&mut self, vpos: u32) -> Option<FieldPixels<'_>> {
        if vpos >= self.beam.vpos || vpos < self.first_vpos() {
            return None;
        }
        Some(self.render_line(vpos, u32::MAX))
    }

    fn render_line(&mut self, vpos: u32, hpos: u32) -> FieldPixels<'_> {
        let key = (self.beam.frame, vpos, hpos);
        if self.cache.rendered_at != Some(key) {
            let mut input =
                crate::video::bitplane::RenderInput::from_bus_in_progress(self.bus, Some(vpos));
            // The render may start a lead row early (see
            // `from_bus_in_progress`): render through the line, keep it.
            let lead = vpos.saturating_sub(input.visible_start_vpos()) as usize;
            let mut wide = crate::video::bitplane::render_wide(
                &mut input,
                SLOT_FIRST_CCK,
                SLOT_LINE_CCK,
                lead + 1,
                &mut self.cache.fb,
                &mut self.cache.scratch,
            );
            if lead > 0 && wide.rows > lead {
                let stride = wide.stride;
                self.cache
                    .fb
                    .copy_within(lead * stride..(lead + 1) * stride, 0);
                self.cache.fb.truncate(stride);
                wide.rows = 1;
                wide.visible_start_vpos += lead as u32;
            }
            self.cache.wide = wide;
            self.cache.rendered_at = Some(key);
            self.cache.renders = self.cache.renders.saturating_add(1);
        }
        let wide = self.cache.wide;
        FieldPixels {
            pixels: &self.cache.fb,
            width: wide.stride,
            rows: wide.rows,
            visible_start_vpos: wide.visible_start_vpos,
            first_cck: SLOT_FIRST_CCK,
            canvas_scale: wide.canvas_scale,
            long_frame: self.beam.long_frame,
            interlaced: wide.interlaced,
            valid_rows: wide.rows,
        }
    }
}

/// Bus-owned scratch for [`LiveField`] renders.
#[derive(Default)]
pub(crate) struct LiveFieldCache {
    fb: Vec<u32>,
    scratch: Vec<u32>,
    wide: crate::video::bitplane::WideRender,
    rendered_at: Option<(u64, u32, u32)>,
    /// Live renders performed (diagnostics and tests).
    pub(crate) renders: u64,
}

impl LiveFieldCache {
    /// Forget the cached render: the beam position it was keyed by can
    /// recur on a new timeline (a reset restarts the frame count).
    pub(crate) fn invalidate(&mut self) {
        self.rendered_at = None;
    }
}

pub trait VideoSlotDevice: Send {
    /// Short name for logs and the UI.
    fn name(&self) -> &'static str;

    /// Whether the device wants [`Self::field_complete`]. Rendering every
    /// field a second time costs a full replay, so a device that only
    /// answers parallel reads can opt out.
    fn wants_completed_fields(&self) -> bool {
        true
    }

    /// A field just finished. `at` is the beam at the wrap (line 0 of the
    /// next field).
    fn field_complete(&mut self, _field: &FieldPixels<'_>, _at: BeamPos) {}

    /// Whether the device paints completed fields itself from the frame's
    /// render input ([`Self::field_render_input`]) instead of taking them
    /// painted ([`Self::field_complete`]): a card that does its work on a
    /// thread of its own takes the input, and the bus skips the paint.
    fn takes_render_input(&self) -> bool {
        false
    }

    /// A field just finished, as the bundle the display renders it from
    /// (`bitplane::render_wide` with [`SLOT_FIRST_CCK`] and
    /// [`SLOT_LINE_CCK`] paints the card's view of it). `at` is the beam at
    /// the wrap. The device gives the bundle back through
    /// [`Self::take_spent_render_inputs`] once it is done with it.
    fn field_render_input(
        &mut self,
        _input: Box<crate::video::bitplane::RenderInput>,
        _at: BeamPos,
    ) {
    }

    /// Render inputs the device is done with, moved into `pool` for the
    /// bus to refill.
    fn take_spent_render_inputs(
        &mut self,
        _pool: &mut Vec<Box<crate::video::bitplane::RenderInput>>,
    ) {
    }

    /// Drive the parallel data pins for one guest read of `$BFE101`.
    /// `Some(byte)` replaces what the guest reads (before the parallel-port
    /// peripheral and joystick adapter are consulted); `None` leaves the
    /// pins to them. `ddrb` is CIA-A's port-B direction register: a card
    /// can only drive pins the guest has made inputs.
    fn parallel_read(&mut self, _live: &mut LiveField<'_>, _ddrb: u8) -> Option<u8> {
        None
    }

    /// Whether the card supplies genlock sync to the Amiga (the Toaster
    /// does). While fitted, `BPLCON0.ERSY` slaves the beam to it instead
    /// of stopping the counters.
    fn supplies_sync(&self) -> bool {
        false
    }

    /// Whether speculative run-ahead may run with this device attached.
    /// A card holding framestores and reacting to the picture is machine
    /// state run-ahead cannot rewind, so the default is no.
    fn runahead_safe(&self) -> bool {
        false
    }

    /// The card's own video outputs (the Toaster's Program and Preview),
    /// for the host to show and record. Shared handles: the card publishes
    /// into them as it produces frames.
    fn video_outputs(&self) -> Vec<Arc<CardOutput>> {
        Vec::new()
    }

    /// What the card is doing, for the control protocol's
    /// `toaster.status`: the sources on its outputs, its inputs, its host
    /// folders. Null for a card with nothing to report.
    fn control_status(&mut self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// Sound the card produced since the last call, stereo at the host
    /// mixer rate ([`crate::audio::MIX_SAMPLE_RATE`]), appended to `out`.
    /// The bus feeds it to the audio mixer after each field.
    fn take_audio(&mut self, _out: &mut Vec<(f32, f32)>) {}

    /// A card-specific control-protocol command (`toaster.input`).
    fn control(
        &mut self,
        command: &str,
        _params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        Err(format!("the {} card has no {command} control", self.name()))
    }
}

/// `src` stretched or squeezed to `n` samples by linear interpolation
/// (silence for an empty `src`).
pub fn resample(src: &[(f32, f32)], n: usize) -> Vec<(f32, f32)> {
    if src.is_empty() {
        return vec![(0.0, 0.0); n];
    }
    if src.len() == n {
        return src.to_vec();
    }
    let step = src.len() as f64 / n as f64;
    (0..n)
        .map(|j| {
            let t = j as f64 * step;
            let i = t as usize;
            let f = (t - i as f64) as f32;
            let a = src[i.min(src.len() - 1)];
            let b = src[(i + 1).min(src.len() - 1)];
            (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f)
        })
        .collect()
}

/// Turn a recording's AVI into an MP4 (H.264 with square pixels, AAC).
fn make_mp4(
    ffmpeg: &std::path::Path,
    avi: &std::path::Path,
    mp4: &std::path::Path,
    width: u32,
    high: bool,
) -> anyhow::Result<()> {
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(avi)
        .args([
            "-vf",
            &format!("scale={width}:480:flags=lanczos,setsar=1"),
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            if high { "12" } else { "18" },
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            if high { "320k" } else { "192k" },
            "-movflags",
            "+faststart",
        ])
        .arg(mp4)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashing up.
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        anyhow::bail!("ffmpeg: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Where an output's recording settings are kept: `.copperline` in the
/// card's folder (beside the recordings folder).
fn settings_file(record_dir: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    Some(
        record_dir?
            .parent()?
            .join(".copperline")
            .join("recording.json"),
    )
}

fn load_settings(record_dir: Option<&std::path::Path>) -> RecordSettings {
    let mut s = RecordSettings::default();
    let Some(text) = settings_file(record_dir).and_then(|p| std::fs::read_to_string(p).ok()) else {
        return s;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return s;
    };
    if v.get("format").and_then(|f| f.as_str()) == Some("avi") {
        s.format = RecordFormat::Avi;
    }
    s.high_quality = v
        .get("high_quality")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    s
}

fn save_settings(record_dir: Option<&std::path::Path>, s: RecordSettings) {
    let Some(path) = settings_file(record_dir) else {
        return;
    };
    let v = serde_json::json!({
        "format": match s.format { RecordFormat::Mp4 => "mp4", RecordFormat::Avi => "avi" },
        "high_quality": s.high_quality,
    });
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, v.to_string());
}

/// One video output of a card: the latest frame it produced, and an
/// optional recording of every frame.
///
/// The card publishes from whichever thread composes it; the host reads
/// the frame when it repaints (a monitor window's painter waits for the
/// next one) and starts or stops recordings. Recording happens on
/// publish, so a recording has every frame whatever the host's paint rate.
pub struct CardOutput {
    name: &'static str,
    /// Display aspect ratio of a frame (width / height as shown).
    aspect: f32,
    /// Where [`CardOutput::start_recording_auto`] puts recordings.
    record_dir: Option<std::path::PathBuf>,
    state: Mutex<OutputState>,
    /// Signalled on every publish.
    published: Condvar,
    /// How recordings are written.
    settings: Mutex<RecordSettings>,
    /// Recordings still being made into MP4s.
    saving: std::sync::atomic::AtomicUsize,
}

/// The file a recording ends up as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordFormat {
    /// H.264 and AAC in an MP4, made with ffmpeg once the recording stops.
    Mp4,
    /// The recorder's own lossless AVI (ZMBV and PCM), as it is.
    Avi,
}

/// How an output records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordSettings {
    pub format: RecordFormat,
    /// MP4 at a higher quality (CRF 12, 320 kb/s AAC) rather than CRF 18
    /// and 192 kb/s.
    pub high_quality: bool,
}

impl Default for RecordSettings {
    fn default() -> Self {
        Self {
            format: RecordFormat::Mp4,
            high_quality: false,
        }
    }
}

/// Whether `path` names an MP4 (or QuickTime) file.
fn is_mp4(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mp4" | "m4v" | "mov"))
}

#[derive(Default)]
struct OutputState {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
    serial: u64,
    recorder: Option<crate::recorder::VideoRecorder>,
    /// Fractional audio samples owed to the recording's silent track.
    audio_carry: f64,
    /// The MP4 the recording becomes when it stops (it is recorded into
    /// an AVI beside it first).
    mp4: Option<std::path::PathBuf>,
}

impl CardOutput {
    pub fn new(
        name: &'static str,
        aspect: f32,
        record_dir: Option<std::path::PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            aspect,
            settings: Mutex::new(load_settings(record_dir.as_deref())),
            record_dir,
            state: Mutex::new(OutputState::default()),
            published: Condvar::new(),
            saving: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// How this output records.
    pub fn record_settings(&self) -> RecordSettings {
        *self.settings.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Change how this output records (from the next recording on); kept
    /// for the next run beside the recordings.
    pub fn set_record_settings(&self, settings: RecordSettings) {
        *self.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings;
        save_settings(self.record_dir.as_deref(), settings);
    }

    /// The folder recordings go to.
    pub fn record_dir(&self) -> Option<&std::path::Path> {
        self.record_dir.as_deref()
    }

    /// Whether a stopped recording is still being made into an MP4.
    pub fn saving(&self) -> bool {
        self.saving.load(std::sync::atomic::Ordering::Relaxed) > 0
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn aspect(&self) -> f32 {
        self.aspect
    }

    /// The card's host folder, if it has one (where a recording goes, and
    /// where a replacement monitor artwork may sit). Recordings go to its
    /// `Output` subfolder.
    pub fn media_root(&self) -> Option<std::path::PathBuf> {
        self.record_dir
            .as_ref()
            .and_then(|d| d.parent())
            .map(std::path::Path::to_path_buf)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, OutputState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Publish a frame `seconds` long, with its sound (stereo at the mixer
    /// rate) when it has any: a recording gets silence otherwise.
    pub fn publish(
        &self,
        width: usize,
        height: usize,
        pixels: &[u32],
        seconds: f64,
        sound: Option<&[(f32, f32)]>,
    ) {
        let mut st = self.state();
        st.width = width;
        st.height = height;
        st.pixels.clear();
        st.pixels
            .extend_from_slice(&pixels[..(width * height).min(pixels.len())]);
        st.serial = st.serial.wrapping_add(1);
        if st.recorder.is_some() {
            // The recorder times video against its audio track: the
            // frame's sound, or silence of the frame's length.
            let owed = st.audio_carry + seconds * f64::from(crate::audio::MIX_SAMPLE_RATE);
            let n = owed.floor() as usize;
            st.audio_carry = owed - n as f64;
            let OutputState {
                recorder, pixels, ..
            } = &mut *st;
            if let Some(rec) = recorder.as_mut() {
                // The frame's sound fitted to the recording's frame: a
                // card's frame need not be exactly one recording frame long.
                let samples = match sound {
                    Some(s) => resample(s, n),
                    None => vec![(0.0, 0.0); n],
                };
                rec.push_audio(&samples);
                if let Err(e) = rec.push_frame(pixels) {
                    log::warn!("{} recording stopped: {e:#}", self.name);
                    let _ = rec.finish();
                    *recorder = None;
                }
            }
        }
        drop(st);
        self.published.notify_all();
    }

    /// The latest frame, if it is newer than `seen`: copied into `out`,
    /// with its size and serial.
    pub fn frame_since(&self, seen: u64, out: &mut Vec<u32>) -> Option<(usize, usize, u64)> {
        Self::take_since(&self.state(), seen, out)
    }

    /// [`Self::frame_since`], waiting up to `timeout` for a frame newer
    /// than `seen` to be published.
    pub fn wait_frame_since(
        &self,
        seen: u64,
        timeout: std::time::Duration,
        out: &mut Vec<u32>,
    ) -> Option<(usize, usize, u64)> {
        let st = self.state();
        if let Some(frame) = Self::take_since(&st, seen, out) {
            return Some(frame);
        }
        let (st, _) = self
            .published
            .wait_timeout(st, timeout)
            .unwrap_or_else(|e| e.into_inner());
        Self::take_since(&st, seen, out)
    }

    fn take_since(st: &OutputState, seen: u64, out: &mut Vec<u32>) -> Option<(usize, usize, u64)> {
        if st.serial == seen || st.pixels.is_empty() {
            return None;
        }
        out.clear();
        out.extend_from_slice(&st.pixels);
        Some((st.width, st.height, st.serial))
    }

    /// Start recording into `path` from the next frame: an AVI as it
    /// is, or for an `.mp4` an AVI beside it that becomes the MP4 when
    /// the recording stops.
    pub fn start_recording(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let mut st = self.state();
        if st.width == 0 || st.height == 0 {
            anyhow::bail!("{} has not produced a frame yet", self.name);
        }
        if let Some(mut old) = st.recorder.take() {
            let _ = old.finish();
        }
        let (avi, mp4) = if is_mp4(path) {
            (
                path.with_extension("recording.avi"),
                Some(path.to_path_buf()),
            )
        } else {
            (path.to_path_buf(), None)
        };
        st.recorder = Some(crate::recorder::VideoRecorder::create(
            &avi, st.width, st.height,
        )?);
        st.mp4 = mp4;
        st.audio_carry = 0.0;
        Ok(())
    }

    /// Start recording into a new, time-stamped file in the output's
    /// recording folder (the emulator's recordings folder without one),
    /// in the output's [`RecordSettings`] format.
    pub fn start_recording_auto(&self) -> anyhow::Result<std::path::PathBuf> {
        let ext = match self.record_settings().format {
            RecordFormat::Mp4 => "mp4",
            RecordFormat::Avi => "avi",
        };
        let name = format!("{}-{}.{ext}", self.name, crate::timestamp::compact_now());
        let path = match &self.record_dir {
            Some(dir) => dir.join(name),
            None => crate::paths::recording_file().with_file_name(name),
        };
        self.start_recording(&path)?;
        Ok(path)
    }

    /// Stop recording; the finished file's path. An MP4 is made from the
    /// recording on a thread of its own and appears at that path when it
    /// is done ([`Self::saving`] until then); without ffmpeg the AVI is
    /// kept and returned.
    pub fn stop_recording(self: &Arc<Self>) -> anyhow::Result<Option<std::path::PathBuf>> {
        let mut st = self.state();
        let Some(mut rec) = st.recorder.take() else {
            return Ok(None);
        };
        rec.finish()?;
        let avi = rec.path().to_path_buf();
        let Some(mp4) = st.mp4.take() else {
            return Ok(Some(avi));
        };
        drop(st);
        let Some(ffmpeg) = self.ffmpeg() else {
            log::warn!(
                "{}: no ffmpeg to make {}; the recording stays {}",
                self.name,
                mp4.display(),
                avi.display()
            );
            return Ok(Some(avi));
        };
        let high = self.record_settings().high_quality;
        let width = ((480.0 * self.aspect / 2.0).round() as u32 * 2).max(2);
        let out = Arc::clone(self);
        out.saving
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let target = mp4.clone();
        std::thread::Builder::new()
            .name("toaster-mp4".into())
            .spawn(move || {
                match make_mp4(&ffmpeg, &avi, &target, width, high) {
                    Ok(()) => {
                        let _ = std::fs::remove_file(&avi);
                        log::info!("{} recording saved: {}", out.name, target.display());
                    }
                    Err(e) => log::warn!(
                        "{}: making {}: {e:#}; the recording stays {}",
                        out.name,
                        target.display(),
                        avi.display()
                    ),
                }
                out.saving
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            })?;
        Ok(Some(mp4))
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    fn ffmpeg(&self) -> Option<std::path::PathBuf> {
        let root = self.media_root()?;
        crate::toaster::media::find_ffmpeg(&root)
    }

    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    fn ffmpeg(&self) -> Option<std::path::PathBuf> {
        None
    }

    pub fn is_recording(&self) -> bool {
        self.state().recorder.is_some()
    }

    /// The file being recorded, while recording.
    /// For an MP4 recording, the MP4 it becomes.
    pub fn recording_path(&self) -> Option<std::path::PathBuf> {
        let st = self.state();
        let rec = st.recorder.as_ref()?;
        Some(st.mp4.clone().unwrap_or_else(|| rec.path().to_path_buf()))
    }

    /// A copy of the latest frame with its size; `None` before the first.
    pub fn snapshot(&self) -> Option<(usize, usize, Vec<u32>)> {
        let st = self.state();
        (!st.pixels.is_empty()).then(|| (st.width, st.height, st.pixels.clone()))
    }
}

impl Drop for CardOutput {
    fn drop(&mut self) {
        if let Some(mut rec) = self.state().recorder.take() {
            let _ = rec.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_output_records_every_published_frame() {
        let dir = tempfile::tempdir().unwrap();
        let out = CardOutput::new("Program", 4.0 / 3.0, Some(dir.path().join("Output")));
        assert_eq!(out.media_root().as_deref(), Some(dir.path()));
        out.set_record_settings(RecordSettings {
            format: RecordFormat::Avi,
            high_quality: false,
        });
        // Nothing to record before the first frame.
        assert!(out.start_recording_auto().is_err());
        let frame = vec![u32::from_le_bytes([200, 30, 30, 255]); 16 * 12];
        out.publish(16, 12, &frame, 1001.0 / 30000.0, None);
        let path = out.start_recording_auto().unwrap();
        assert!(out.is_recording());
        for _ in 0..30 {
            out.publish(16, 12, &frame, 1001.0 / 30000.0, None);
        }
        let saved = out.stop_recording().unwrap().unwrap();
        assert_eq!(saved, path);
        assert!(!out.is_recording());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"AVI ");
        // Frames since the last read, and none when nothing is new.
        let mut buf = Vec::new();
        let (w, h, serial) = out.frame_since(0, &mut buf).unwrap();
        assert_eq!((w, h, buf.len()), (16, 12, 192));
        assert!(out.frame_since(serial, &mut buf).is_none());
        // The setting is kept for the next run.
        let again = CardOutput::new("Program", 4.0 / 3.0, Some(dir.path().join("Output")));
        assert_eq!(again.record_settings().format, RecordFormat::Avi);
    }

    #[test]
    fn an_mp4_recording_is_made_from_its_avi_when_it_stops() {
        let dir = tempfile::tempdir().unwrap();
        let out = CardOutput::new("Program", 4.0 / 3.0, Some(dir.path().join("Output")));
        assert_eq!(out.record_settings(), RecordSettings::default());
        let frame = vec![u32::from_le_bytes([30, 200, 30, 255]); 16 * 12];
        out.publish(16, 12, &frame, 1001.0 / 30000.0, None);
        let path = out.start_recording_auto().unwrap();
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("mp4"));
        assert_eq!(out.recording_path(), Some(path.clone()));
        let sound = vec![(0.25f32, -0.25f32); 1471];
        for _ in 0..20 {
            out.publish(16, 12, &frame, 1001.0 / 30000.0, Some(&sound));
        }
        let saved = out.stop_recording().unwrap().unwrap();
        while out.saving() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if out.ffmpeg().is_some() {
            assert_eq!(saved, path);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(&bytes[4..8], b"ftyp");
            assert!(!path.with_extension("recording.avi").exists());
        } else {
            // Without ffmpeg the AVI stays.
            assert_eq!(saved, path.with_extension("recording.avi"));
            assert!(saved.is_file());
        }
    }
}
