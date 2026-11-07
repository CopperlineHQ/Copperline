// SPDX-License-Identifier: GPL-3.0-or-later

//! The Toaster's connection to the host: a folder on the host disk
//! ("Amiga Toaster Files") whose subfolders stand in for the card's
//! connectors.
//!
//! - `Input 1` .. `Input 4`: what is plugged into each video input. The
//!   files in a folder play in name order and loop: a video plays through,
//!   a still picture holds (five seconds each when there are several; for
//!   good when it is alone). An empty folder is an unplugged input. The
//!   folders are rescanned while the machine runs, so dropping a file in
//!   is plugging a source in.
//! - `Output`: recordings of the Program output.
//! - `Flyer`: the Flyer's clips, for when the Flyer is emulated.
//!
//! Stills (PNG, JPEG, BMP) are decoded here. Video files go through
//! ffmpeg, found as `ffmpeg`/`ffmpeg.exe` in the folder itself, beside the
//! emulator, on the `PATH`, or at `COPPERLINE_FFMPEG`; without it a video
//! file reads as no signal and the log says why.
//!
//! Every source is scaled into the card's picture: 752 x 480 at the
//! framestore's sampling rate, 4:3, letterboxed or pillarboxed to fit.
//! Frames are produced on a worker thread per input and consumed one per
//! emulated video frame, so playback follows the emulated clock (a slow
//! host plays slowly rather than dropping frames). The workers wait until
//! the switcher first drives the card: nothing is decoded while the
//! machine boots.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use super::video::{PICTURE_H, PICTURE_W};
/// The picture's 4:3 frame in square pixels, before the horizontal
/// stretch to the sampling rate.
const SQUARE_W: usize = 640;

pub const INPUTS: usize = 4;

/// One picture: `PICTURE_W * PICTURE_H` RGBA pixels.
pub type Picture = Arc<Vec<u32>>;

/// One video frame's sound: stereo samples at the host mixer rate
/// ([`crate::audio::MIX_SAMPLE_RATE`]), about 1471 per frame.
pub type Sound = Arc<Vec<(f32, f32)>>;

/// Mixer samples per video frame (29.97 frames a second).
pub const SAMPLES_PER_FRAME: f64 = crate::audio::MIX_SAMPLE_RATE as f64 * 1001.0 / 30000.0;

const STILL_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "bmp"];
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "avi", "mkv", "webm", "wmv", "mpg", "mpeg", "mts", "m2ts", "ts", "dv",
    "flv", "gif", "ogv", "3gp",
];
/// How long a still holds when it is one of several in a folder.
const STILL_HOLD: Duration = Duration::from_secs(5);
/// How often a folder is looked at for new or removed files.
const RESCAN: Duration = Duration::from_millis(1500);

/// The folder layout.
#[derive(Clone, Debug)]
pub struct MediaFolders {
    root: PathBuf,
}

impl MediaFolders {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `Input 1` .. `Input 4`, for `n` in 0..4.
    pub fn input(&self, n: usize) -> PathBuf {
        self.root.join(format!("Input {}", n + 1))
    }

    pub fn output(&self) -> PathBuf {
        self.root.join("Output")
    }

    pub fn flyer(&self) -> PathBuf {
        self.root.join("Flyer")
    }

    /// Create the folders that are missing, and a note on what goes where.
    pub fn prepare(&self) -> std::io::Result<()> {
        for n in 0..INPUTS {
            std::fs::create_dir_all(self.input(n))?;
        }
        std::fs::create_dir_all(self.output())?;
        std::fs::create_dir_all(self.flyer())?;
        let readme = self.root.join("README.txt");
        if !readme.exists() {
            std::fs::write(&readme, README.replace('\n', "\r\n"))?;
        }
        Ok(())
    }
}

const README: &str = "\
Amiga Toaster Files
===================

These folders are the emulated Video Toaster's connectors.

Input 1 .. Input 4
    What is plugged into each of the Toaster's video inputs. Drop a video
    file or a still picture in. Several files play one after another, in
    name order, and loop. An empty folder is an unplugged input. Changes
    are picked up while the emulator runs.

    Stills: PNG, JPEG, BMP. Video: anything ffmpeg plays (MP4, MOV, AVI,
    MKV, ...). Video needs ffmpeg: put ffmpeg.exe in this folder, beside
    copperline.exe, or on the PATH.

Output
    Recordings of the Toaster's Program output.

Flyer
    Clips for the Video Flyer (not emulated yet).
";

/// Is `path` something an input can play, and how.
fn classify(path: &Path) -> Option<bool> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if STILL_EXTENSIONS.contains(&ext.as_str()) {
        Some(false)
    } else if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        Some(true)
    } else {
        None
    }
}

/// The playable files of `dir`, in name order.
fn scan(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && classify(p).is_some())
        .collect();
    files.sort_by_key(|p| p.file_name().map(|n| n.to_ascii_lowercase()));
    files
}

/// Where the 4:3 picture sits inside a `w` x `h` source, in square pixels
/// of the 640 x 480 frame: `(x, y, width, height)`.
fn fit(w: usize, h: usize) -> (usize, usize, usize, usize) {
    if w == 0 || h == 0 {
        return (0, 0, SQUARE_W, PICTURE_H);
    }
    // Compare aspect ratios without floats: w/h against 4/3.
    if w * 3 >= h * 4 {
        let fh = (SQUARE_W * h / w).clamp(1, PICTURE_H);
        (0, (PICTURE_H - fh) / 2, SQUARE_W, fh)
    } else {
        let fw = (PICTURE_H * w / h).clamp(1, SQUARE_W);
        ((SQUARE_W - fw) / 2, 0, fw, PICTURE_H)
    }
}

/// Scale an RGBA image (`w` x `h`, row-major, `[r, g, b, a]` bytes) into a
/// card picture, letterboxed into the 4:3 frame and stretched to the
/// sampling rate. Bilinear.
pub fn picture_from_rgba(src: &[u8], w: usize, h: usize) -> Vec<u32> {
    let black = u32::from_le_bytes([0, 0, 0, 0xFF]);
    let mut out = vec![black; PICTURE_W * PICTURE_H];
    if w == 0 || h == 0 || src.len() < w * h * 4 {
        return out;
    }
    let (fx, fy, fw, fh) = fit(w, h);
    // Destination columns covering the fitted frame, in card pixels.
    let x0 = fx * PICTURE_W / SQUARE_W;
    let x1 = ((fx + fw) * PICTURE_W / SQUARE_W).min(PICTURE_W);
    let sample = |x: usize, y: usize, c: usize| f32::from(src[(y * w + x) * 4 + c]);
    for dy in fy..(fy + fh).min(PICTURE_H) {
        let sy = ((dy - fy) as f32 + 0.5) * h as f32 / fh as f32 - 0.5;
        let y0 = sy.floor().clamp(0.0, (h - 1) as f32) as usize;
        let y1 = (y0 + 1).min(h - 1);
        let ty = (sy - y0 as f32).clamp(0.0, 1.0);
        for dx in x0..x1 {
            let sx = ((dx - x0) as f32 + 0.5) * w as f32 / (x1 - x0) as f32 - 0.5;
            let xa = sx.floor().clamp(0.0, (w - 1) as f32) as usize;
            let xb = (xa + 1).min(w - 1);
            let tx = (sx - xa as f32).clamp(0.0, 1.0);
            let mut px = [0u8; 4];
            for (c, slot) in px.iter_mut().enumerate().take(3) {
                let top = sample(xa, y0, c) * (1.0 - tx) + sample(xb, y0, c) * tx;
                let bottom = sample(xa, y1, c) * (1.0 - tx) + sample(xb, y1, c) * tx;
                *slot = (top * (1.0 - ty) + bottom * ty).round().clamp(0.0, 255.0) as u8;
            }
            px[3] = 0xFF;
            out[dy * PICTURE_W + dx] = u32::from_le_bytes(px);
        }
    }
    out
}

fn load_still(path: &Path) -> Result<Vec<u32>, String> {
    let img = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
    let (w, h) = img.dimensions();
    Ok(picture_from_rgba(img.as_raw(), w as usize, h as usize))
}

/// Where ffmpeg is, if anywhere.
pub fn find_ffmpeg(media_root: &Path) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    if let Some(p) = std::env::var_os("COPPERLINE_FFMPEG").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    let mut candidates = vec![media_root.join(exe)];
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        candidates.push(dir.join(exe));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join(exe)));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// What a worker hands the card.
enum Message {
    /// A frame, with its sound when the source has some.
    Picture(Picture, Option<Sound>),
    NoSignal,
}

/// What an input's worker and its owner share: a file the input is held
/// to instead of its folder's contents, what it is playing, and whether
/// it may start (an input is not read before the switcher drives the
/// card).
#[derive(Default)]
struct Shared {
    pinned: Mutex<Option<PathBuf>>,
    playing: Mutex<Option<PathBuf>>,
    started: AtomicBool,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One input connector.
pub struct InputFeed {
    rx: Receiver<Message>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    current: Option<Picture>,
    /// The sound of the frame taken by the last `advance`, until taken.
    sound: Option<Sound>,
    dir: PathBuf,
    shared: Arc<Shared>,
}

impl InputFeed {
    /// An input watching `dir` once [`Self::start`] is called.
    pub fn spawn(index: usize, dir: PathBuf, ffmpeg: Option<PathBuf>) -> Self {
        // Eight frames (a quarter second) of buffer: a decoder or disk
        // hiccup on the host must not leave a frame without its picture and
        // sound, and the worker still only runs as far ahead as this.
        let (tx, rx) = sync_channel(8);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let shared = Arc::new(Shared::default());
        let worker_shared = Arc::clone(&shared);
        let worker_dir = dir.clone();
        let worker = std::thread::Builder::new()
            .name(format!("toaster-input-{}", index + 1))
            .spawn(move || {
                Worker::new(index, worker_dir, ffmpeg, tx, worker_stop, worker_shared).run()
            })
            .ok();
        Self {
            rx,
            stop,
            worker,
            current: None,
            sound: None,
            dir,
            shared,
        }
    }

    /// Start reading the folder: until then the worker waits, and the
    /// input has no signal.
    pub fn start(&self) {
        self.shared.started.store(true, Ordering::Relaxed);
    }

    /// Whether [`Self::start`] was called.
    pub fn started(&self) -> bool {
        self.shared.started.load(Ordering::Relaxed)
    }

    /// The input's folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The playable files in the folder, in the order they play.
    pub fn files(&self) -> Vec<PathBuf> {
        scan(&self.dir)
    }

    /// Hold the input to one file (anywhere on the host) instead of its
    /// folder, or with `None` go back to the folder. The change lands
    /// within a rescan.
    pub fn pin(&self, file: Option<PathBuf>) {
        *locked(&self.shared.pinned) = file;
    }

    pub fn pinned(&self) -> Option<PathBuf> {
        locked(&self.shared.pinned).clone()
    }

    /// The file on the input now, if any.
    pub fn playing(&self) -> Option<PathBuf> {
        locked(&self.shared.playing).clone()
    }

    /// Take the next frame, if the source has one ready: call once per
    /// emulated video frame. A still or a stalled decoder keeps the
    /// previous frame.
    pub fn advance(&mut self) {
        match self.rx.try_recv() {
            Ok(Message::Picture(p, sound)) => {
                self.current = Some(p);
                self.sound = sound;
            }
            Ok(Message::NoSignal) => {
                self.current = None;
                self.sound = None;
            }
            // No new frame: the picture holds, the sound does not.
            Err(_) => self.sound = None,
        }
    }

    /// The current frame; `None` with nothing plugged in.
    pub fn picture(&self) -> Option<&Picture> {
        self.current.as_ref()
    }

    /// The sound that goes with the frame the last `advance` took (none for
    /// a still, a silent source, or when no new frame came).
    pub fn sound(&self) -> Option<&Sound> {
        self.sound.as_ref()
    }
}

impl Drop for InputFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Unblock a worker waiting to send.
        while self.rx.try_recv().is_ok() {}
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Worker {
    index: usize,
    dir: PathBuf,
    ffmpeg: Option<PathBuf>,
    tx: SyncSender<Message>,
    stop: Arc<AtomicBool>,
    shared: Arc<Shared>,
    warned_no_ffmpeg: bool,
}

impl Worker {
    fn new(
        index: usize,
        dir: PathBuf,
        ffmpeg: Option<PathBuf>,
        tx: SyncSender<Message>,
        stop: Arc<AtomicBool>,
        shared: Arc<Shared>,
    ) -> Self {
        Self {
            index,
            dir,
            ffmpeg,
            tx,
            stop,
            shared,
            warned_no_ffmpeg: false,
        }
    }

    /// What the input plays: the pinned file while it exists, else the
    /// folder's files.
    fn sources(&self) -> Vec<PathBuf> {
        if let Some(file) = locked(&self.shared.pinned).clone() {
            if file.is_file() && classify(&file).is_some() {
                return vec![file];
            }
        }
        scan(&self.dir)
    }

    fn set_playing(&self, file: Option<&Path>) {
        *locked(&self.shared.playing) = file.map(Path::to_path_buf);
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Send, waiting for room; `false` when stopping.
    fn send(&self, mut msg: Message) -> bool {
        loop {
            if self.stopped() {
                return false;
            }
            match self.tx.try_send(msg) {
                Ok(()) => return true,
                Err(TrySendError::Full(m)) => {
                    msg = m;
                    std::thread::sleep(Duration::from_millis(4));
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
    }

    /// Sleep up to `d`, waking early to stop.
    fn nap(&self, d: Duration) -> bool {
        let until = Instant::now() + d;
        while Instant::now() < until {
            if self.stopped() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        !self.stopped()
    }

    fn run(mut self) {
        while !self.shared.started.load(Ordering::Relaxed) {
            if !self.nap(Duration::from_millis(100)) {
                return;
            }
        }
        let mut files: Vec<PathBuf> = Vec::new();
        let mut next = 0usize;
        let mut signal = false;
        while !self.stopped() {
            let now = self.sources();
            if now != files {
                if !now.is_empty() {
                    log::info!(
                        "toaster: input {} plays {} file(s) from {}",
                        self.index + 1,
                        now.len(),
                        self.dir.display()
                    );
                }
                files = now;
                next = 0;
            }
            if files.is_empty() {
                self.set_playing(None);
                if signal {
                    signal = false;
                    if !self.send(Message::NoSignal) {
                        return;
                    }
                }
                if !self.nap(RESCAN) {
                    return;
                }
                continue;
            }
            let file = files[next % files.len()].clone();
            next = next.wrapping_add(1);
            self.set_playing(Some(&file));
            let played = if classify(&file) == Some(true) {
                self.play_video(&file, &files)
            } else {
                self.play_still(&file, &files)
            };
            match played {
                Played::Stop => return,
                Played::Shown => signal = true,
                Played::Failed => {
                    // Nothing worth showing; do not spin on a bad file.
                    if !self.nap(RESCAN) {
                        return;
                    }
                }
            }
        }
    }

    fn play_still(&mut self, file: &Path, files: &[PathBuf]) -> Played {
        let picture = match load_still(file) {
            Ok(p) => Arc::new(p),
            Err(e) => {
                log::warn!("toaster: input {}: {}: {e}", self.index + 1, file.display());
                return Played::Failed;
            }
        };
        if !self.send(Message::Picture(picture, None)) {
            return Played::Stop;
        }
        // Alone, a still holds until the folder changes; one of several,
        // it holds for a while and the next one comes up.
        let started = Instant::now();
        loop {
            if !self.nap(RESCAN.min(STILL_HOLD)) {
                return Played::Stop;
            }
            if self.sources() != files {
                return Played::Shown;
            }
            if files.len() > 1 && started.elapsed() >= STILL_HOLD {
                return Played::Shown;
            }
        }
    }

    fn play_video(&mut self, file: &Path, files: &[PathBuf]) -> Played {
        let Some(ffmpeg) = self.ffmpeg.clone() else {
            if !self.warned_no_ffmpeg {
                self.warned_no_ffmpeg = true;
                log::warn!(
                    "toaster: input {}: {} needs ffmpeg, which was not found \
                     (put ffmpeg.exe in the Toaster files folder or on the PATH)",
                    self.index + 1,
                    file.display()
                );
            }
            return Played::Failed;
        };
        let filter = format!(
            "scale={SQUARE_W}:{PICTURE_H}:force_original_aspect_ratio=decrease,\
             pad={SQUARE_W}:{PICTURE_H}:(ow-iw)/2:(oh-ih)/2:black,\
             scale={PICTURE_W}:{PICTURE_H},fps=30000/1001"
        );
        let mut cmd = ffmpeg_command(&ffmpeg, file);
        cmd.args([
            "-an", "-vf", &filter, "-pix_fmt", "rgba", "-f", "rawvideo", "-",
        ]);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("toaster: input {}: starting ffmpeg: {e}", self.index + 1);
                return Played::Failed;
            }
        };
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            return Played::Failed;
        };
        // The sound comes from a second ffmpeg on the same file, read a
        // frame's worth at a time beside each picture, so the two stay
        // together whatever the host's pace. A file without sound plays
        // silent.
        let mut sound = SoundReader::start(&ffmpeg, file);
        let mut buf = vec![0u8; PICTURE_W * PICTURE_H * 4];
        let mut frames = 0u64;
        let mut last_scan = Instant::now();
        let result = loop {
            if self.stopped() {
                break Played::Stop;
            }
            if std::io::Read::read_exact(&mut stdout, &mut buf).is_err() {
                break if frames > 0 {
                    Played::Shown
                } else {
                    log::warn!(
                        "toaster: input {}: ffmpeg could not play {}",
                        self.index + 1,
                        file.display()
                    );
                    Played::Failed
                };
            }
            frames += 1;
            let picture: Vec<u32> = buf
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], 0xFF]))
                .collect();
            let frame_sound = sound.as_mut().and_then(SoundReader::frame);
            if frame_sound.is_none() {
                sound = None;
            }
            if !self.send(Message::Picture(Arc::new(picture), frame_sound)) {
                break Played::Stop;
            }
            if last_scan.elapsed() >= RESCAN {
                last_scan = Instant::now();
                if self.sources() != files {
                    break Played::Shown;
                }
            }
        };
        let _ = child.kill();
        let _ = child.wait();
        result
    }
}

/// ffmpeg reading `file`, its output on a pipe, no console window.
fn ffmpeg_command(ffmpeg: &Path, file: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.args(["-nostdin", "-loglevel", "error", "-i"])
        .arg(file)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashing up per clip.
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// A file's sound as stereo at the mixer rate, a video frame at a time.
struct SoundReader {
    child: std::process::Child,
    out: std::process::ChildStdout,
    carry: f64,
    buf: Vec<u8>,
}

impl SoundReader {
    fn start(ffmpeg: &Path, file: &Path) -> Option<Self> {
        let rate = crate::audio::MIX_SAMPLE_RATE.to_string();
        let mut cmd = ffmpeg_command(ffmpeg, file);
        cmd.args(["-vn", "-ac", "2", "-ar", &rate, "-f", "s16le", "-"]);
        let mut child = cmd.spawn().ok()?;
        let out = child.stdout.take()?;
        Some(Self {
            child,
            out,
            carry: 0.0,
            buf: Vec::new(),
        })
    }

    /// The next frame's samples; `None` once the sound has ended.
    fn frame(&mut self) -> Option<Sound> {
        self.carry += SAMPLES_PER_FRAME;
        let n = self.carry.floor() as usize;
        self.carry -= n as f64;
        self.buf.resize(n * 4, 0);
        std::io::Read::read_exact(&mut self.out, &mut self.buf).ok()?;
        Some(Arc::new(
            self.buf
                .chunks_exact(4)
                .map(|c| {
                    (
                        f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0,
                        f32::from(i16::from_le_bytes([c[2], c[3]])) / 32768.0,
                    )
                })
                .collect(),
        ))
    }
}

impl Drop for SoundReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

enum Played {
    Shown,
    Failed,
    Stop,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_video_file_brings_its_sound_frame_by_frame() {
        // Needs ffmpeg; skipped where there is none.
        let dir = tempfile::tempdir().unwrap();
        let Some(ffmpeg) = find_ffmpeg(dir.path()) else {
            return;
        };
        let clip = dir.path().join("tone.mp4");
        let made = std::process::Command::new(&ffmpeg)
            .args(["-nostdin", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("testsrc=size=160x120:rate=30000/1001:duration=1")
            .args(["-f", "lavfi", "-i", "sine=frequency=441:duration=1"])
            .args(["-shortest", "-c:v", "mpeg4", "-c:a", "aac"])
            .arg(&clip)
            .status();
        if !made.is_ok_and(|s| s.success()) {
            return;
        }
        let mut reader = SoundReader::start(&ffmpeg, &clip).expect("ffmpeg starts");
        let mut lengths = Vec::new();
        let mut peak = 0.0f32;
        for _ in 0..10 {
            let frame = reader.frame().expect("a frame of sound");
            lengths.push(frame.len());
            peak = frame.iter().fold(peak, |p, (l, _)| p.max(l.abs()));
        }
        // 44100 / 29.97: 1471 and 1472 samples, and a tone in them.
        assert!(
            lengths.iter().all(|&n| n == 1471 || n == 1472),
            "{lengths:?}"
        );
        assert!(lengths.iter().sum::<usize>().abs_diff(14715) <= 1);
        assert!(peak > 0.1, "peak {peak}");
    }

    #[test]
    fn fit_letterboxes_and_pillarboxes() {
        assert_eq!(fit(640, 480), (0, 0, 640, 480));
        assert_eq!(fit(1920, 1080), (0, 60, 640, 360));
        assert_eq!(fit(480, 480), (80, 0, 480, 480));
    }

    #[test]
    fn a_still_fills_the_picture() {
        let red = [255u8, 0, 0, 255].repeat(4 * 3);
        let p = picture_from_rgba(&red, 4, 3);
        assert_eq!(p.len(), PICTURE_W * PICTURE_H);
        assert_eq!(p[0], u32::from_le_bytes([255, 0, 0, 255]));
        assert_eq!(
            p[PICTURE_W * PICTURE_H - 1],
            u32::from_le_bytes([255, 0, 0, 255])
        );
        // 16:9 is letterboxed.
        let wide = [0u8, 255, 0, 255].repeat(16 * 9);
        let p = picture_from_rgba(&wide, 16, 9);
        assert_eq!(p[0], u32::from_le_bytes([0, 0, 0, 255]));
        assert_eq!(
            p[240 * PICTURE_W + 376],
            u32::from_le_bytes([0, 255, 0, 255])
        );
    }

    #[test]
    fn folders_are_laid_out() {
        let dir = tempfile::tempdir().unwrap();
        let m = MediaFolders::new(dir.path().join("Amiga Toaster Files"));
        m.prepare().unwrap();
        for n in 0..INPUTS {
            assert!(m.input(n).is_dir());
        }
        assert!(m.output().is_dir() && m.flyer().is_dir());
        assert!(m.root().join("README.txt").is_file());
        std::fs::write(m.input(0).join("b.png"), b"").unwrap();
        std::fs::write(m.input(0).join("a.MP4"), b"").unwrap();
        std::fs::write(m.input(0).join("notes.txt"), b"").unwrap();
        let files = scan(&m.input(0));
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["a.MP4", "b.png"]);
    }
}
