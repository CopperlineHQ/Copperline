// SPDX-License-Identifier: GPL-3.0-or-later

// A window, not a console program, on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

//! `flyer-convert`: a window, in the Video Toaster's own look, that turns
//! video files, still pictures and numbered picture sequences into Video
//! Flyer clips on the Flyer's drives (the `Flyer/FA0`... folders of the
//! Toaster files folder), with their crouton icons.
//!
//! ```text
//! flyer-convert [--media DIR] [FILE|FOLDER...]
//! ```
//!
//! Drop files or folders on the window (a folder of numbered pictures is
//! one sequence; any other folder adds the media in it), set the drive,
//! quality, motion, fit and sound, and Convert. Copperline starts it
//! beside a machine that has a Flyer (`[video_slot] converter`), passing
//! the card's folder as `--media`; it runs on its own, and a second copy
//! started while one runs just exits. The conversion is
//! `copperline::flyer_convert` (ffmpeg decodes the sources).

use copperline::flyer_convert::{self as fc, Fit, ImportOptions, Input, Motion, Probe};
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, ColorImage, FontId, Painter, Pos2, Rect, Sense, Stroke,
    TextureHandle, TextureOptions, Vec2,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

/// The window, in logical pixels.
const WIN: Vec2 = vec2(1000.0, 866.0);
/// The port held while a converter runs: a second one exits.
const INSTANCE_PORT: u16 = 47_611;
/// A Flyer drive's size (the emulated drive's 2^24 blocks).
const DRIVE_BYTES: u64 = 0x100_0000 * 512;

// The palette: the Toaster 4000's greys, its gold for what is selected,
// and a green for levels.
const BG: Color32 = Color32::from_rgb(0x8E, 0x8E, 0x8E);
const FACE: Color32 = Color32::from_rgb(0xAA, 0xAA, 0xAA);
const FACE_DOWN: Color32 = Color32::from_rgb(0x96, 0x96, 0x96);
const LIGHT: Color32 = Color32::from_rgb(0xF0, 0xF0, 0xF0);
const SHADE: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x2C);
const INSET: Color32 = Color32::from_rgb(0x9C, 0x9C, 0x9C);
const WELL: Color32 = Color32::from_rgb(0x52, 0x50, 0x50);
const INK: Color32 = Color32::from_rgb(0x12, 0x12, 0x12);
const DIM: Color32 = Color32::from_rgb(0x5A, 0x5A, 0x5A);
const GOLD: Color32 = Color32::from_rgb(0xE2, 0xC4, 0x5A);
const GREEN: Color32 = Color32::from_rgb(0x46, 0xD2, 0x3E);
const RED: Color32 = Color32::from_rgb(0xC8, 0x30, 0x28);
const SELECT: Color32 = Color32::from_rgb(0xC6, 0xC6, 0xC6);

const QUALITY: [&str; 3] = ["Standard Play", "High Quality 5", "High Quality 6"];
const MOTION: [&str; 2] = ["Fields  59.94/s", "Frames  29.97/s"];
const FIT: [&str; 3] = ["Letterbox", "Fill (crop)", "Stretch"];
const AUDIO: [&str; 3] = ["From Source (Stereo)", "From Source (Mono)", "None"];
const STILL: [(f64, &str); 7] = [
    (2.0, "Stills 2 s"),
    (3.0, "Stills 3 s"),
    (5.0, "Stills 5 s"),
    (10.0, "Stills 10 s"),
    (15.0, "Stills 15 s"),
    (30.0, "Stills 30 s"),
    (60.0, "Stills 60 s"),
];
const SEQ: [(f64, &str); 6] = [
    (24000.0 / 1001.0, "Seq 23.976"),
    (24.0, "Seq 24 fps"),
    (25.0, "Seq 25 fps"),
    (30000.0 / 1001.0, "Seq 29.97"),
    (30.0, "Seq 30 fps"),
    (60000.0 / 1001.0, "Seq 59.94"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Video,
    Still,
    Sequence,
    /// Already a Flyer clip: copied onto the drive as it is.
    Clip,
}

#[derive(Clone, PartialEq)]
enum Status {
    Queued,
    Working,
    Done(String),
    Failed(String),
}

struct Item {
    input: Input,
    kind: Kind,
    /// The clip's name on the Flyer (at most 31 characters).
    name: String,
    probe: Option<Probe>,
    status: Status,
}

impl Item {
    fn seconds(&self, options: &ImportOptions) -> Option<f64> {
        if let Some(s) = self.input.known_seconds(options) {
            return Some(s);
        }
        let s = self.probe.as_ref()?.seconds?;
        Some(match options.start_seconds {
            Some(t) => (s - t).max(0.0),
            None => s,
        })
    }
}

/// What the worker threads report.
enum Event {
    Probed(PathBuf, Probe),
    Preview(u64, Result<Vec<u32>, String>),
    Started(usize),
    Progress(usize, usize, Option<Vec<u32>>),
    Finished(usize, Result<String, String>),
    AllDone,
}

/// A conversion run.
struct Job {
    cancel: Arc<AtomicBool>,
    started: Instant,
    /// The item being converted, and colour frames done of it.
    current: Option<usize>,
    frames: usize,
    /// Seconds of the queue done before the current item, and in all.
    done_seconds: f64,
    total_seconds: f64,
}

struct App {
    media: PathBuf,
    ffmpeg: Option<PathBuf>,
    drives: Vec<(String, String)>,
    drive: usize,
    items: Vec<Item>,
    selected: Option<usize>,
    scroll: f32,
    quality: usize,
    motion: usize,
    fit: usize,
    audio: usize,
    still: usize,
    seq: usize,
    gain: [f32; 2],
    icons: bool,
    replace: bool,
    preview: Option<TextureHandle>,
    preview_key: u64,
    preview_seconds: f64,
    preview_wanted: u64,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    job: Option<Job>,
    message: String,
    /// Bytes used on the selected drive, and when it was last counted.
    used: (u64, Option<Instant>),
}

fn main() -> eframe::Result {
    let mut media = None;
    let mut files = Vec::new();
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        if a == "--media" {
            media = args.next().map(PathBuf::from);
        } else if a == "-h" || a == "--help" {
            println!("usage: flyer-convert [--media DIR] [FILE|FOLDER...]");
            return Ok(());
        } else {
            files.push(PathBuf::from(a));
        }
    }
    // One converter at a time: Copperline starts one with every machine.
    let Ok(_instance) = std::net::TcpListener::bind(("127.0.0.1", INSTANCE_PORT)) else {
        eprintln!("flyer-convert: already running");
        return Ok(());
    };
    let settings = Settings::load();
    let media = media
        .or_else(|| settings.media.clone())
        .unwrap_or_else(copperline::paths::toaster_files_dir);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Convert Media to Flyer Clips")
            .with_inner_size(WIN)
            .with_resizable(false)
            .with_decorations(false)
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "flyer-convert",
        options,
        Box::new(move |cc| {
            let mut app = App::new(media, &settings);
            app.style(&cc.egui_ctx);
            app.add_paths(&files);
            Ok(Box::new(app))
        }),
    )
}

/// What is remembered between runs (beside the program).
#[derive(Default)]
struct Settings {
    media: Option<PathBuf>,
    values: serde_json::Map<String, serde_json::Value>,
}

impl Settings {
    fn path() -> Option<PathBuf> {
        Some(
            std::env::current_exe()
                .ok()?
                .with_file_name("flyer-convert.json"),
        )
    }

    fn load() -> Self {
        let Some(text) = Self::path().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return Self::default();
        };
        let Ok(serde_json::Value::Object(values)) = serde_json::from_str(&text) else {
            return Self::default();
        };
        let media = values
            .get("media")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .filter(|p| p.is_dir());
        Self { media, values }
    }

    fn get(&self, key: &str, default: usize) -> usize {
        self.values
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .map_or(default, |v| v as usize)
    }
}

impl App {
    fn new(media: PathBuf, settings: &Settings) -> Self {
        let (tx, rx) = channel();
        let ffmpeg = fc::find_ffmpeg(Some(&media));
        let mut app = Self {
            ffmpeg,
            drives: Vec::new(),
            drive: settings.get("drive", 0),
            items: Vec::new(),
            selected: None,
            scroll: 0.0,
            quality: settings.get("quality", 0).min(2),
            motion: settings.get("motion", 0).min(1),
            fit: settings.get("fit", 0).min(2),
            audio: settings.get("audio", 0).min(2),
            still: settings.get("still", 2).min(STILL.len() - 1),
            seq: settings.get("seq", 3).min(SEQ.len() - 1),
            gain: [
                settings
                    .values
                    .get("gain_l")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0) as f32,
                settings
                    .values
                    .get("gain_r")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0) as f32,
            ],
            icons: settings.get("icons", 1) != 0,
            replace: settings.get("replace", 0) != 0,
            preview: None,
            preview_key: 0,
            preview_seconds: 1.0,
            preview_wanted: 0,
            tx,
            rx,
            job: None,
            message: String::new(),
            used: (0, None),
            media,
        };
        app.scan_drives();
        if app.ffmpeg.is_none() {
            app.message = "ffmpeg not found: put ffmpeg.exe in the Toaster files folder".into();
        }
        app
    }

    fn save_settings(&self) {
        let Some(path) = Settings::path() else {
            return;
        };
        let v = serde_json::json!({
            "media": self.media.display().to_string(),
            "drive": self.drive,
            "quality": self.quality,
            "motion": self.motion,
            "fit": self.fit,
            "audio": self.audio,
            "still": self.still,
            "seq": self.seq,
            "gain_l": self.gain[0],
            "gain_r": self.gain[1],
            "icons": u8::from(self.icons),
            "replace": u8::from(self.replace),
        });
        let _ = std::fs::write(path, serde_json::to_string_pretty(&v).unwrap_or_default());
    }

    fn flyer_folder(&self) -> PathBuf {
        self.media.join("Flyer")
    }

    /// The Flyer's drives: `F<bus><target>` folders, with their volume
    /// names. With none, FA0 is offered (and made on the first convert).
    fn scan_drives(&mut self) {
        let flyer = self.flyer_folder();
        let mut drives = Vec::new();
        if let Ok(dir) = std::fs::read_dir(&flyer) {
            for e in dir.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let b = name.as_bytes();
                if e.path().is_dir()
                    && b.len() == 3
                    && b[0] == b'F'
                    && (b'A'..=b'C').contains(&b[1])
                    && (b'0'..=b'6').contains(&b[2])
                {
                    let volume = std::fs::read_to_string(e.path().join(".flyervol"))
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| format!("Flyer{}{}", b[1] as char, b[2] as char));
                    drives.push((name, volume));
                }
            }
        }
        drives.sort();
        if drives.is_empty() {
            drives.push(("FA0".into(), "FlyerA0".into()));
        }
        self.drive = self.drive.min(drives.len() - 1);
        self.drives = drives;
        self.used.1 = None;
    }

    fn drive_folder(&self) -> PathBuf {
        self.flyer_folder().join(&self.drives[self.drive].0)
    }

    fn options(&self) -> ImportOptions {
        ImportOptions {
            grade: match self.quality {
                1 => flyerclip::Grade::Hq5,
                2 => flyerclip::Grade::Hq6,
                _ => flyerclip::Grade::Std,
            },
            audio: self.audio != 2,
            mono: self.audio == 1,
            gain_db: self.gain,
            motion: if self.motion == 0 {
                Motion::Fields
            } else {
                Motion::Frames
            },
            fit: [Fit::Letterbox, Fit::Fill, Fit::Stretch][self.fit],
            still_seconds: STILL[self.still].0,
            sequence_fps: SEQ[self.seq].0,
            icon: self.icons,
            ..ImportOptions::default()
        }
    }

    /// Add files and folders: a folder of numbered pictures is one
    /// sequence, any other folder adds the media files in it.
    fn add_paths(&mut self, paths: &[PathBuf]) {
        let mut added = 0;
        for p in paths {
            if p.is_dir() {
                if let Some(seq) = fc::find_sequence(p) {
                    added += self.add_input(seq, Kind::Sequence);
                    continue;
                }
                let mut files: Vec<PathBuf> = std::fs::read_dir(p)
                    .map(|d| {
                        d.flatten()
                            .map(|e| e.path())
                            .filter(|p| p.is_file())
                            .collect()
                    })
                    .unwrap_or_default();
                files.sort();
                for f in files {
                    added += self.add_file(&f, false);
                }
            } else {
                added += self.add_file(p, true);
            }
        }
        if added > 0 && self.selected.is_none() {
            self.selected = Some(self.items.len() - added);
        }
        if added == 0 && !paths.is_empty() {
            self.message = "Nothing there the Flyer can take".into();
        }
    }

    fn add_file(&mut self, path: &Path, any: bool) -> usize {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if is_clip(path) {
            return self.add_input(Input::Video(path.to_path_buf()), Kind::Clip);
        }
        if fc::STILL_EXTENSIONS.contains(&ext.as_str()) {
            return self.add_input(Input::Still(path.to_path_buf()), Kind::Still);
        }
        if fc::VIDEO_EXTENSIONS.contains(&ext.as_str()) || (any && !ext.is_empty()) {
            return self.add_input(Input::Video(path.to_path_buf()), Kind::Video);
        }
        0
    }

    fn add_input(&mut self, input: Input, kind: Kind) -> usize {
        if self.items.iter().any(|i| i.input == input) {
            return 0;
        }
        let name = clip_name(&input.stem());
        if kind == Kind::Video {
            if let Some(ffmpeg) = self.ffmpeg.clone() {
                let tx = self.tx.clone();
                let path = input.path().to_path_buf();
                std::thread::spawn(move || {
                    if let Ok(p) = fc::probe(&ffmpeg, &path) {
                        let _ = tx.send(Event::Probed(path, p));
                    }
                });
            }
        }
        self.items.push(Item {
            input,
            kind,
            name,
            probe: None,
            status: Status::Queued,
        });
        1
    }

    fn pick_files(&mut self) {
        let dialog = rfd::FileDialog::new()
            .set_title("Add media")
            .add_filter(
                "Video and pictures",
                &[fc::VIDEO_EXTENSIONS, fc::STILL_EXTENSIONS, &["clip"]].concat(),
            )
            .add_filter("All files", &["*"]);
        if let Some(files) = dialog.pick_files() {
            self.add_paths(&files);
        }
    }

    fn pick_folder(&mut self) {
        if let Some(dir) = rfd::FileDialog::new()
            .set_title("Add a folder (numbered pictures make one sequence)")
            .pick_folder()
        {
            self.add_paths(&[dir]);
        }
    }

    fn pick_media(&mut self) {
        if let Some(dir) = rfd::FileDialog::new()
            .set_title("The Toaster files folder (the one holding Flyer)")
            .set_directory(&self.media)
            .pick_folder()
        {
            self.media = dir;
            self.ffmpeg = self
                .ffmpeg
                .clone()
                .or_else(|| fc::find_ffmpeg(Some(&self.media)));
            self.scan_drives();
        }
    }

    /// The clip a queued item becomes.
    fn target(&self, item: &Item) -> PathBuf {
        let dir = self.drive_folder();
        let path = dir.join(&item.name);
        if self.replace {
            path
        } else {
            fc::unused_path(&path)
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        let Some(ffmpeg) = self.ffmpeg.clone() else {
            self.message = "ffmpeg is needed to convert".into();
            return;
        };
        let queue: Vec<usize> = (0..self.items.len())
            .filter(|&i| !matches!(self.items[i].status, Status::Done(_)))
            .collect();
        if queue.is_empty() {
            self.message = "Nothing to convert: add some media".into();
            return;
        }
        let dir = self.drive_folder();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.message = format!("{}: {e}", dir.display());
            return;
        }
        self.save_settings();
        let options = self.options();
        let cancel = Arc::new(AtomicBool::new(false));
        let jobs: Vec<(usize, Input, Kind, PathBuf)> = queue
            .iter()
            .map(|&i| {
                let it = &self.items[i];
                (i, it.input.clone(), it.kind, self.target(it))
            })
            .collect();
        let total_seconds = queue
            .iter()
            .map(|&i| self.items[i].seconds(&options).unwrap_or(0.0))
            .sum();
        for &i in &queue {
            self.items[i].status = Status::Queued;
        }
        self.job = Some(Job {
            cancel: Arc::clone(&cancel),
            started: Instant::now(),
            current: None,
            frames: 0,
            done_seconds: 0.0,
            total_seconds,
        });
        self.message.clear();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let mut options = options;
        options.cancel = Some(Arc::clone(&cancel));
        std::thread::spawn(move || {
            for (i, input, kind, out) in jobs {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let _ = tx.send(Event::Started(i));
                ctx.request_repaint();
                let result = if kind == Kind::Clip {
                    std::fs::copy(input.path(), &out)
                        .map(|_| format!("copied to {}", out.display()))
                        .map_err(|e| e.to_string())
                } else {
                    let mut last = Instant::now();
                    fc::import_input(&ffmpeg, &input, &out, &options, |p| {
                        if last.elapsed().as_millis() >= 120 {
                            last = Instant::now();
                            let _ = tx.send(Event::Progress(
                                i,
                                p.frames,
                                p.picture.map(<[u32]>::to_vec),
                            ));
                            ctx.request_repaint();
                        }
                    })
                    .map(|r| {
                        let mut s = format!(
                            "{} {:.1} s, {:.1} MB",
                            out.file_name().unwrap_or_default().to_string_lossy(),
                            r.seconds,
                            r.bytes as f64 / 1e6
                        );
                        if r.truncated_fields > 0 {
                            s += &format!(", {} fields over budget", r.truncated_fields);
                        }
                        s
                    })
                    .map_err(|e| format!("{e:#}"))
                };
                let _ = tx.send(Event::Finished(i, result));
                ctx.request_repaint();
            }
            let _ = tx.send(Event::AllDone);
            ctx.request_repaint();
        });
    }

    fn events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Probed(path, probe) => {
                    for it in &mut self.items {
                        if it.input.path() == path {
                            it.probe = Some(probe.clone());
                        }
                    }
                }
                Event::Preview(key, result) => {
                    if key == self.preview_wanted && self.job.is_none() {
                        match result {
                            Ok(p) => self.set_preview(ctx, &p),
                            Err(_) => self.preview = None,
                        }
                    }
                }
                Event::Started(i) => {
                    if let Some(job) = self.job.as_mut() {
                        job.current = Some(i);
                        job.frames = 0;
                    }
                    if let Some(it) = self.items.get_mut(i) {
                        it.status = Status::Working;
                    }
                    self.selected = Some(i);
                }
                Event::Progress(i, frames, picture) => {
                    if let Some(job) = self.job.as_mut() {
                        if job.current == Some(i) {
                            job.frames = frames;
                        }
                    }
                    if let Some(p) = picture {
                        self.set_preview(ctx, &p);
                    }
                }
                Event::Finished(i, result) => {
                    let options = self.options();
                    if let Some(job) = self.job.as_mut() {
                        if let Some(it) = self.items.get(i) {
                            job.done_seconds += it.seconds(&options).unwrap_or(0.0);
                        }
                        job.frames = 0;
                        job.current = None;
                    }
                    if let Some(it) = self.items.get_mut(i) {
                        it.status = match result {
                            Ok(s) => Status::Done(s),
                            Err(e) => Status::Failed(e),
                        };
                    }
                    self.used.1 = None;
                }
                Event::AllDone => {
                    let stopped = self
                        .job
                        .as_ref()
                        .is_some_and(|j| j.cancel.load(Ordering::Relaxed));
                    let done = self
                        .items
                        .iter()
                        .filter(|i| matches!(i.status, Status::Done(_)))
                        .count();
                    let failed = self
                        .items
                        .iter()
                        .filter(|i| matches!(i.status, Status::Failed(_)))
                        .count();
                    for it in &mut self.items {
                        if it.status == Status::Working {
                            it.status = Status::Queued;
                        }
                    }
                    self.message = if stopped {
                        "Stopped".into()
                    } else if failed > 0 {
                        format!("{done} done, {failed} failed")
                    } else {
                        format!("{done} clip(s) on {}:", self.drives[self.drive].1)
                    };
                    self.job = None;
                    self.preview_key = 0;
                }
            }
        }
    }

    fn set_preview(&mut self, ctx: &egui::Context, picture: &[u32]) {
        let bytes: Vec<u8> = picture.iter().flat_map(|p| p.to_le_bytes()).collect();
        let image = ColorImage::from_rgba_unmultiplied([752, 480], &bytes);
        match &mut self.preview {
            Some(t) => t.set(image, TextureOptions::LINEAR),
            None => self.preview = Some(ctx.load_texture("preview", image, TextureOptions::LINEAR)),
        }
    }

    /// Ask for the preview picture of the selected item when what it
    /// depends on changed.
    fn want_preview(&mut self, ctx: &egui::Context) {
        if self.job.is_some() {
            return;
        }
        let Some(i) = self.selected.filter(|&i| i < self.items.len()) else {
            self.preview = None;
            return;
        };
        let item = &self.items[i];
        if item.kind == Kind::Clip {
            self.preview = None;
            return;
        }
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            item.input.path().hash(&mut h);
            self.fit.hash(&mut h);
            ((self.preview_seconds * 10.0) as i64).hash(&mut h);
            h.finish() | 1
        };
        if key == self.preview_key {
            return;
        }
        self.preview_key = key;
        self.preview_wanted = key;
        let Some(ffmpeg) = self.ffmpeg.clone() else {
            return;
        };
        let input = item.input.clone();
        let options = self.options();
        let seconds = self.preview_seconds;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = fc::preview(&ffmpeg, &input, seconds, &options).map_err(|e| e.to_string());
            let _ = tx.send(Event::Preview(key, r));
            ctx.request_repaint();
        });
    }

    /// Bytes in the selected drive's folder, recounted now and then.
    fn drive_used(&mut self) -> u64 {
        if self.used.1.is_none_or(|t| t.elapsed().as_secs() >= 5) {
            let total = std::fs::read_dir(self.drive_folder())
                .map(|d| {
                    d.flatten()
                        .filter_map(|e| e.metadata().ok())
                        .filter(|m| m.is_file())
                        .map(|m| m.len().div_ceil(512) * 512)
                        .sum()
                })
                .unwrap_or(0);
            self.used = (total, Some(Instant::now()));
        }
        self.used.0
    }

    fn style(&self, ctx: &egui::Context) {
        ctx.set_theme(egui::Theme::Light);
        ctx.all_styles_mut(|s| {
            s.visuals = egui::Visuals::light();
            s.visuals.panel_fill = BG;
            s.visuals.window_fill = BG;
            s.visuals.extreme_bg_color = INSET;
            s.visuals.override_text_color = Some(INK);
            s.visuals.selection.bg_fill = GOLD;
            s.visuals.selection.stroke = Stroke::new(1.0, INK);
            s.visuals.text_cursor.stroke = Stroke::new(2.0, INK);
            s.visuals.widgets.inactive.bg_stroke = Stroke::NONE;
            s.visuals.widgets.hovered.bg_stroke = Stroke::NONE;
            s.visuals.widgets.active.bg_stroke = Stroke::NONE;
            s.visuals.widgets.inactive.corner_radius = 0.0.into();
            s.visuals.widgets.hovered.corner_radius = 0.0.into();
            s.visuals.widgets.active.corner_radius = 0.0.into();
            s.visuals.widgets.noninteractive.corner_radius = 0.0.into();
            s.text_styles
                .insert(egui::TextStyle::Body, FontId::proportional(17.0));
        });
    }
}

/// Whether a file is already a Flyer clip.
fn is_clip(path: &Path) -> bool {
    std::fs::File::open(path)
        .and_then(|mut f| {
            let mut id = [0u8; 4];
            std::io::Read::read_exact(&mut f, &mut id)?;
            Ok(&id == b"CLIP")
        })
        .unwrap_or(false)
}

/// Characters in a clip's name: a Flyer name holds 31, and its icon's
/// name is the clip's and `.I`.
const NAME_MAX: usize = 29;

/// A clip name the Flyer accepts: at most [`NAME_MAX`] characters, no
/// `/` or `:`.
fn clip_name(stem: &str) -> String {
    let name: String = stem
        .chars()
        .map(|c| {
            if matches!(c, '/' | ':' | '\\') {
                '_'
            } else {
                c
            }
        })
        .take(NAME_MAX)
        .collect();
    if name.trim().is_empty() {
        "Clip".into()
    } else {
        name
    }
}

/// NTSC time code, HH:MM:SS:FF (30 frame labels a second).
fn timecode(seconds: f64) -> String {
    let frames = (seconds * 30000.0 / 1001.0).round() as u64;
    let (f, s) = (frames % 30, frames / 30);
    format!("{:02}:{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60, f)
}

fn hms(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn megabytes(bytes: f64) -> String {
    if bytes >= 1e9 {
        format!("{:.2} GB", bytes / 1e9)
    } else {
        format!("{:.1} MB", bytes / 1e6)
    }
}

// ---- Drawing: bevels, text and gadgets in the Toaster's style ----

fn bevel(p: &Painter, r: Rect, raised: bool) {
    let (tl, br) = if raised {
        (LIGHT, SHADE)
    } else {
        (SHADE, LIGHT)
    };
    let w = 2.0;
    p.rect_filled(
        Rect::from_min_max(r.min, pos2(r.max.x, r.min.y + w)),
        0.0,
        tl,
    );
    p.rect_filled(
        Rect::from_min_max(r.min, pos2(r.min.x + w, r.max.y)),
        0.0,
        tl,
    );
    p.rect_filled(
        Rect::from_min_max(pos2(r.min.x, r.max.y - w), r.max),
        0.0,
        br,
    );
    p.rect_filled(
        Rect::from_min_max(pos2(r.max.x - w, r.min.y), r.max),
        0.0,
        br,
    );
}

/// Text, a little heavier than the font (drawn twice).
fn text(p: &Painter, pos: Pos2, align: Align2, s: &str, size: f32, color: Color32) -> Rect {
    let font = FontId::proportional(size);
    p.text(pos + vec2(0.6, 0.0), align, s, font.clone(), color);
    p.text(pos, align, s, font, color)
}

/// A raised button that sinks while pressed; gold when `lit`.
fn button(ui: &mut egui::Ui, r: Rect, label: &str, enabled: bool, lit: bool) -> egui::Response {
    let id = ui
        .id()
        .with(("button", label, r.min.x as i32, r.min.y as i32));
    let resp = ui.interact(
        r,
        id,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let p = ui.painter();
    let down = resp.is_pointer_button_down_on();
    let face = if lit {
        GOLD
    } else if down {
        FACE_DOWN
    } else {
        FACE
    };
    p.rect_filled(r, 0.0, face);
    bevel(p, r, !down);
    let ink = if enabled { INK } else { DIM };
    text(
        p,
        r.center() + vec2(0.0, if down { 1.0 } else { 0.0 }),
        Align2::CENTER_CENTER,
        label,
        19.0,
        ink,
    );
    resp
}

/// The Toaster's cycle gadget: a raised button with up/down arrows on
/// its right. Left click steps forward, right click back.
fn cycle(ui: &mut egui::Ui, r: Rect, options: &[&str], index: &mut usize, enabled: bool) -> bool {
    let id = ui.id().with(("cycle", r.min.x as i32, r.min.y as i32));
    let resp = ui.interact(
        r,
        id,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let p = ui.painter();
    p.rect_filled(
        r,
        0.0,
        if resp.is_pointer_button_down_on() {
            FACE_DOWN
        } else {
            FACE
        },
    );
    bevel(p, r, true);
    let ink = if enabled { INK } else { DIM };
    let text_r = Rect::from_min_max(r.min, pos2(r.max.x - 28.0, r.max.y));
    text(
        p,
        text_r.center(),
        Align2::CENTER_CENTER,
        options[*index],
        18.0,
        ink,
    );
    // The arrows.
    let ax = r.max.x - 15.0;
    let cy = r.center().y;
    for (dy, up) in [(-5.0, true), (5.0, false)] {
        let y = cy + dy;
        let pts = if up {
            vec![
                pos2(ax - 5.0, y + 3.0),
                pos2(ax + 5.0, y + 3.0),
                pos2(ax, y - 3.0),
            ]
        } else {
            vec![
                pos2(ax - 5.0, y - 3.0),
                pos2(ax + 5.0, y - 3.0),
                pos2(ax, y + 3.0),
            ]
        };
        p.add(egui::Shape::convex_polygon(pts, ink, Stroke::NONE));
    }
    p.line_segment(
        [
            pos2(r.max.x - 28.0, r.min.y + 5.0),
            pos2(r.max.x - 28.0, r.max.y - 5.0),
        ],
        Stroke::new(1.0, SHADE),
    );
    p.line_segment(
        [
            pos2(r.max.x - 27.0, r.min.y + 5.0),
            pos2(r.max.x - 27.0, r.max.y - 5.0),
        ],
        Stroke::new(1.0, LIGHT),
    );
    let n = options.len();
    if resp.clicked() {
        *index = (*index + 1) % n;
        return true;
    }
    if resp.secondary_clicked() {
        *index = (*index + n - 1) % n;
        return true;
    }
    false
}

/// A sunken label/value well.
fn well(p: &Painter, r: Rect) {
    p.rect_filled(r, 0.0, INSET);
    bevel(p, r, false);
}

/// A groove across the window.
fn divider(p: &Painter, y: f32) {
    p.line_segment(
        [pos2(18.0, y), pos2(WIN.x - 18.0, y)],
        Stroke::new(1.0, SHADE),
    );
    p.line_segment(
        [pos2(18.0, y + 1.0), pos2(WIN.x - 18.0, y + 1.0)],
        Stroke::new(1.0, LIGHT),
    );
}

fn checkbox(ui: &mut egui::Ui, at: Pos2, label: &str, value: &mut bool, enabled: bool) {
    let r = Rect::from_min_size(at, vec2(28.0, 28.0));
    let label_r = Rect::from_min_size(at, vec2(36.0 + label.len() as f32 * 9.0, 28.0));
    let id = ui.id().with(("check", label));
    let resp = ui.interact(
        label_r,
        id,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if resp.clicked() {
        *value = !*value;
    }
    let p = ui.painter();
    p.rect_filled(r, 0.0, FACE);
    bevel(p, r, true);
    if *value {
        let pts = [
            pos2(at.x + 7.0, at.y + 14.0),
            pos2(at.x + 12.0, at.y + 21.0),
            pos2(at.x + 22.0, at.y + 6.0),
        ];
        p.line_segment([pts[0], pts[1]], Stroke::new(3.5, INK));
        p.line_segment([pts[1], pts[2]], Stroke::new(3.5, INK));
    }
    text(
        p,
        pos2(at.x + 40.0, at.y + 14.0),
        Align2::LEFT_CENTER,
        label,
        19.0,
        INK,
    );
}

/// A gain slider in dB, -24 to +12: its label and a level bar (green,
/// gold past unity) above, the prop gadget and step arrows below. 330
/// wide.
fn gain(ui: &mut egui::Ui, at: Pos2, label: &str, db: &mut f32, enabled: bool) {
    let p = ui.painter().clone();
    let ink = if enabled { INK } else { DIM };
    text(
        &p,
        pos2(at.x, at.y + 10.0),
        Align2::LEFT_CENTER,
        label,
        19.0,
        ink,
    );
    let (lo, hi) = (-24.0f32, 12.0f32);
    let bar = Rect::from_min_size(pos2(at.x + 140.0, at.y + 1.0), vec2(190.0, 18.0));
    well(&p, bar);
    let f = (*db - lo) / (hi - lo);
    let fill = Rect::from_min_size(
        bar.min + vec2(2.0, 2.0),
        vec2((bar.width() - 4.0) * f, bar.height() - 4.0),
    );
    p.rect_filled(fill, 0.0, if *db > 0.0 { GOLD } else { GREEN });
    let unity = bar.min.x + 2.0 + (bar.width() - 4.0) * (-lo / (hi - lo));
    p.line_segment(
        [pos2(unity, bar.min.y), pos2(unity, bar.max.y)],
        Stroke::new(1.0, INK),
    );
    text(
        &p,
        bar.center(),
        Align2::CENTER_CENTER,
        &format!("{db:+.1} dB"),
        14.0,
        INK,
    );
    // The prop gadget.
    let track = Rect::from_min_size(pos2(at.x, at.y + 26.0), vec2(262.0, 30.0));
    well(&p, track);
    let id = ui.id().with(("gain", label));
    let resp = ui.interact(
        track,
        id,
        if enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    let knob_w = 46.0;
    if let Some(pos) = resp.interact_pointer_pos() {
        if resp.is_pointer_button_down_on() {
            let f =
                ((pos.x - track.min.x - knob_w / 2.0) / (track.width() - knob_w)).clamp(0.0, 1.0);
            *db = ((lo + f * (hi - lo)) * 2.0).round() / 2.0;
        }
    }
    let f = (*db - lo) / (hi - lo);
    let kx = track.min.x + 2.0 + f * (track.width() - knob_w - 4.0);
    let knob = Rect::from_min_size(
        pos2(kx, track.min.y + 3.0),
        vec2(knob_w, track.height() - 6.0),
    );
    p.rect_filled(knob, 0.0, FACE);
    bevel(&p, knob, true);
    for k in 0..6 {
        let x = knob.min.x + 10.0 + k as f32 * 5.0;
        p.line_segment(
            [pos2(x, knob.min.y + 5.0), pos2(x, knob.max.y - 5.0)],
            Stroke::new(1.5, SHADE),
        );
    }
    let left = Rect::from_min_size(pos2(track.max.x + 4.0, track.min.y), vec2(31.0, 30.0));
    let right = Rect::from_min_size(pos2(left.max.x + 1.0, track.min.y), vec2(31.0, 30.0));
    if button(ui, left, "<", enabled, false).clicked() {
        *db = (*db - 0.5).max(lo);
    }
    if button(ui, right, ">", enabled, false).clicked() {
        *db = (*db + 0.5).min(hi);
    }
}

fn icon_film(p: &Painter, r: Rect, ink: Color32) {
    p.rect_filled(r, 0.0, ink);
    let holes = 4;
    for k in 0..holes {
        let y = r.min.y + 2.0 + k as f32 * (r.height() - 4.0) / holes as f32;
        for x in [r.min.x + 2.0, r.max.x - 5.0] {
            p.rect_filled(Rect::from_min_size(pos2(x, y), vec2(3.0, 3.0)), 0.0, FACE);
        }
    }
    let inner = r.shrink2(vec2(7.0, 3.0));
    p.rect_filled(inner, 0.0, Color32::from_rgb(0x70, 0x70, 0x70));
}

fn icon_picture(p: &Painter, r: Rect, ink: Color32) {
    p.rect_filled(r, 0.0, ink);
    let inner = r.shrink(2.0);
    p.rect_filled(inner, 0.0, LIGHT);
    let base = inner.max.y - 2.0;
    p.add(egui::Shape::convex_polygon(
        vec![
            pos2(inner.min.x + 2.0, base),
            pos2(inner.min.x + 10.0, inner.min.y + 8.0),
            pos2(inner.min.x + 18.0, base),
        ],
        ink,
        Stroke::NONE,
    ));
    p.add(egui::Shape::convex_polygon(
        vec![
            pos2(inner.min.x + 12.0, base),
            pos2(inner.max.x - 8.0, inner.min.y + 10.0),
            pos2(inner.max.x - 2.0, base),
        ],
        ink,
        Stroke::NONE,
    ));
    p.circle_filled(pos2(inner.max.x - 7.0, inner.min.y + 6.0), 3.0, ink);
}

fn icon_for(p: &Painter, r: Rect, kind: Kind) {
    match kind {
        Kind::Video => icon_film(p, r, INK),
        Kind::Still => icon_picture(p, r, INK),
        Kind::Sequence => {
            icon_picture(p, r.translate(vec2(4.0, -4.0)), DIM);
            icon_picture(p, r, INK);
        }
        Kind::Clip => {
            icon_film(p, r, Color32::from_rgb(0x30, 0x30, 0x80));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.events(&ctx);
        self.want_preview(&ctx);
        // Files dropped on the window.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() && self.job.is_none() {
            self.add_paths(&dropped);
        }
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let busy = self.job.is_some();
        let options = self.options();

        let full = Rect::from_min_size(Pos2::ZERO, WIN);
        let p = ui.painter().clone();
        p.rect_filled(full, 0.0, BG);
        bevel(&p, full, true);

        // The title bar: close gadget, title, depth gadgets; drag to move.
        let bar = Rect::from_min_max(pos2(2.0, 2.0), pos2(WIN.x - 2.0, 40.0));
        p.rect_filled(bar, 0.0, FACE);
        bevel(&p, bar, true);
        let close = Rect::from_min_size(pos2(8.0, 7.0), vec2(30.0, 28.0));
        let close_resp = ui.interact(close, ui.id().with("close"), Sense::click());
        p.rect_filled(
            close,
            0.0,
            if close_resp.is_pointer_button_down_on() {
                FACE_DOWN
            } else {
                FACE
            },
        );
        bevel(&p, close, true);
        p.rect_stroke(
            close.shrink(8.0),
            0.0,
            Stroke::new(2.0, INK),
            egui::StrokeKind::Inside,
        );
        text(
            &p,
            bar.center(),
            Align2::CENTER_CENTER,
            "Convert Media to Flyer Clips",
            25.0,
            INK,
        );
        let depth = Rect::from_min_size(pos2(WIN.x - 76.0, 7.0), vec2(30.0, 28.0));
        let mini = Rect::from_min_size(pos2(WIN.x - 40.0, 7.0), vec2(30.0, 28.0));
        let depth_resp = ui.interact(depth, ui.id().with("depth"), Sense::click());
        let mini_resp = ui.interact(mini, ui.id().with("mini"), Sense::click());
        for (r, resp) in [(depth, &depth_resp), (mini, &mini_resp)] {
            p.rect_filled(
                r,
                0.0,
                if resp.is_pointer_button_down_on() {
                    FACE_DOWN
                } else {
                    FACE
                },
            );
            bevel(&p, r, true);
        }
        p.rect_stroke(
            Rect::from_min_size(depth.min + vec2(6.0, 6.0), vec2(12.0, 10.0)),
            0.0,
            Stroke::new(2.0, INK),
            egui::StrokeKind::Inside,
        );
        p.rect_filled(
            Rect::from_min_size(depth.min + vec2(12.0, 11.0), vec2(12.0, 10.0)),
            0.0,
            INK,
        );
        p.rect_filled(
            Rect::from_min_size(mini.min + vec2(7.0, 18.0), vec2(16.0, 3.0)),
            0.0,
            INK,
        );
        let drag = ui.interact(
            Rect::from_min_max(pos2(42.0, 2.0), pos2(WIN.x - 80.0, 40.0)),
            ui.id().with("drag"),
            Sense::click_and_drag(),
        );
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if close_resp.clicked() {
            if let Some(job) = &self.job {
                job.cancel.store(true, Ordering::Relaxed);
            }
            self.save_settings();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if mini_resp.clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if depth_resp.clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        // ---- Sources: the drop area, the list, its buttons ----
        let drop = Rect::from_min_max(pos2(20.0, 56.0), pos2(472.0, 326.0));
        let drop_resp = ui.interact(drop, ui.id().with("drop"), Sense::click());
        p.rect_filled(drop, 0.0, if hovering { SELECT } else { BG });
        let dash = Stroke::new(2.0, if hovering { INK } else { LIGHT });
        for (a, b) in [
            (drop.left_top(), drop.right_top()),
            (drop.right_top(), drop.right_bottom()),
            (drop.right_bottom(), drop.left_bottom()),
            (drop.left_bottom(), drop.left_top()),
        ] {
            p.extend(egui::Shape::dashed_line(&[a, b], dash, 9.0, 6.0));
        }
        let c = drop.center() + vec2(0.0, -40.0);
        let film = Rect::from_center_size(c + vec2(-75.0, 0.0), vec2(70.0, 84.0));
        icon_film(&p, film, Color32::from_rgb(0x3A, 0x3A, 0x3A));
        p.add(egui::Shape::convex_polygon(
            vec![
                pos2(c.x - 30.0, c.y - 8.0),
                pos2(c.x + 2.0, c.y - 8.0),
                pos2(c.x + 2.0, c.y - 18.0),
                pos2(c.x + 22.0, c.y),
                pos2(c.x + 2.0, c.y + 18.0),
                pos2(c.x + 2.0, c.y + 8.0),
                pos2(c.x - 30.0, c.y + 8.0),
            ],
            FACE,
            Stroke::new(2.0, SHADE),
        ));
        for k in 0..3 {
            let r = Rect::from_center_size(
                c + vec2(85.0 - k as f32 * 7.0, 8.0 - k as f32 * 7.0),
                vec2(80.0, 62.0),
            );
            icon_picture(&p, r, Color32::from_rgb(0x3A, 0x3A, 0x3A));
        }
        text(
            &p,
            pos2(drop.center().x, drop.max.y - 60.0),
            Align2::CENTER_CENTER,
            "Drop MP4, MOV, AVI, PNG, JPG,",
            19.0,
            Color32::from_rgb(0x30, 0x30, 0x30),
        );
        text(
            &p,
            pos2(drop.center().x, drop.max.y - 34.0),
            Align2::CENTER_CENTER,
            "image sequences or folders here",
            19.0,
            Color32::from_rgb(0x30, 0x30, 0x30),
        );
        if drop_resp.clicked() && !busy {
            self.pick_files();
        }

        text(
            &p,
            pos2(494.0, 66.0),
            Align2::LEFT_CENTER,
            &format!(
                "Source Media ({} item{})",
                self.items.len(),
                if self.items.len() == 1 { "" } else { "s" }
            ),
            20.0,
            INK,
        );
        let list = Rect::from_min_max(pos2(492.0, 84.0), pos2(790.0, 326.0));
        well(&p, list);
        let row_h = 40.0;
        let rows = ((list.height() - 8.0) / row_h).floor() as usize;
        let max_scroll = self.items.len().saturating_sub(rows) as f32;
        let list_resp = ui.interact(list, ui.id().with("list"), Sense::click());
        if list_resp.hovered() {
            let dy = ctx.input(|i| i.smooth_scroll_delta.y);
            self.scroll = (self.scroll - dy / row_h).clamp(0.0, max_scroll);
        }
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        let first = self.scroll.round() as usize;
        let lp = p.with_clip_rect(list.shrink(3.0));
        for (k, i) in (first..self.items.len()).take(rows).enumerate() {
            let it = &self.items[i];
            let r = Rect::from_min_size(
                pos2(list.min.x + 4.0, list.min.y + 4.0 + k as f32 * row_h),
                vec2(list.width() - 8.0, row_h - 2.0),
            );
            if self.selected == Some(i) {
                lp.rect_filled(r, 0.0, SELECT);
                bevel(&lp, r, true);
            }
            icon_for(
                &lp,
                Rect::from_min_size(r.min + vec2(8.0, 7.0), vec2(32.0, 24.0)),
                it.kind,
            );
            let color = match it.status {
                Status::Failed(_) => RED,
                _ => INK,
            };
            let mut label = it.name.clone();
            if label.chars().count() > 20 {
                label = label.chars().take(19).collect::<String>() + "…";
            }
            text(
                &lp,
                pos2(r.min.x + 52.0, r.center().y),
                Align2::LEFT_CENTER,
                &label,
                19.0,
                color,
            );
            let tag = match &it.status {
                Status::Working => "....".to_string(),
                Status::Done(_) => "done".to_string(),
                Status::Failed(_) => "failed".to_string(),
                Status::Queued => it
                    .seconds(&options)
                    .map(|s| {
                        if s < 10.0 {
                            format!("{s:.1}s")
                        } else {
                            format!("{s:.0}s")
                        }
                    })
                    .unwrap_or_default(),
            };
            text(
                &lp,
                pos2(r.max.x - 6.0, r.center().y),
                Align2::RIGHT_CENTER,
                &tag,
                16.0,
                DIM,
            );
            let resp = ui.interact(r.intersect(list), ui.id().with(("row", i)), Sense::click());
            if resp.clicked() {
                self.selected = Some(i);
            }
            if let Status::Done(s) | Status::Failed(s) = &it.status {
                resp.on_hover_text(s.clone());
            }
        }
        if self.items.is_empty() {
            text(
                &p,
                list.center(),
                Align2::CENTER_CENTER,
                "(nothing yet)",
                18.0,
                DIM,
            );
        }
        // The list's scroller: arrows and a prop gadget.
        let sb = Rect::from_min_max(pos2(792.0, 84.0), pos2(816.0, 326.0));
        p.rect_filled(sb, 0.0, INSET);
        bevel(&p, sb, false);
        let up = Rect::from_min_size(sb.min, vec2(24.0, 26.0));
        let down = Rect::from_min_size(pos2(sb.min.x, sb.max.y - 26.0), vec2(24.0, 26.0));
        if button(ui, up, "^", true, false).clicked() {
            self.scroll = (self.scroll - 1.0).max(0.0);
        }
        if button(ui, down, "v", true, false).clicked() {
            self.scroll = (self.scroll + 1.0).min(max_scroll);
        }
        let track = Rect::from_min_max(
            pos2(sb.min.x + 2.0, up.max.y + 2.0),
            pos2(sb.max.x - 2.0, down.min.y - 2.0),
        );
        let n = self.items.len().max(1) as f32;
        let visible = (rows as f32 / n).min(1.0);
        let th = (track.height() * visible).max(24.0);
        let ty = track.min.y
            + (track.height() - th)
                * if max_scroll > 0.0 {
                    self.scroll / max_scroll
                } else {
                    0.0
                };
        let thumb = Rect::from_min_size(pos2(track.min.x, ty), vec2(track.width(), th));
        p.rect_filled(thumb, 0.0, FACE);
        bevel(&p, thumb, true);
        for k in 0..4 {
            let y = thumb.center().y - 6.0 + k as f32 * 4.0;
            p.line_segment(
                [pos2(thumb.min.x + 5.0, y), pos2(thumb.max.x - 5.0, y)],
                Stroke::new(1.5, SHADE),
            );
        }
        let tresp = ui.interact(track, ui.id().with("thumb"), Sense::drag());
        if let Some(pos) = tresp.interact_pointer_pos() {
            if tresp.dragged() && max_scroll > 0.0 {
                let f = ((pos.y - track.min.y - th / 2.0) / (track.height() - th)).clamp(0.0, 1.0);
                self.scroll = f * max_scroll;
            }
        }

        let bx = 830.0;
        let bw = 154.0;
        if button(
            ui,
            Rect::from_min_size(pos2(bx, 56.0), vec2(bw, 40.0)),
            "Add Files...",
            !busy,
            false,
        )
        .clicked()
        {
            self.pick_files();
        }
        if button(
            ui,
            Rect::from_min_size(pos2(bx, 104.0), vec2(bw, 40.0)),
            "Add Folder...",
            !busy,
            false,
        )
        .clicked()
        {
            self.pick_folder();
        }
        let can_remove = !busy && self.selected.is_some_and(|i| i < self.items.len());
        if button(
            ui,
            Rect::from_min_size(pos2(bx, 152.0), vec2(bw, 40.0)),
            "Remove",
            can_remove,
            false,
        )
        .clicked()
        {
            if let Some(i) = self.selected {
                self.items.remove(i);
                self.selected = if self.items.is_empty() {
                    None
                } else {
                    Some(i.min(self.items.len() - 1))
                };
                self.preview_key = 0;
            }
        }
        if button(
            ui,
            Rect::from_min_size(pos2(bx, 200.0), vec2(bw, 40.0)),
            "Clear",
            !busy && !self.items.is_empty(),
            false,
        )
        .clicked()
        {
            self.items.clear();
            self.selected = None;
            self.preview = None;
            self.preview_key = 0;
        }
        if button(
            ui,
            Rect::from_min_size(pos2(bx, 286.0), vec2(bw, 40.0)),
            "Files Folder...",
            !busy,
            false,
        )
        .clicked()
        {
            self.pick_media();
        }
        let ff = if self.ffmpeg.is_some() {
            "ffmpeg ready"
        } else {
            "no ffmpeg!"
        };
        text(
            &p,
            pos2(bx + bw / 2.0, 264.0),
            Align2::CENTER_CENTER,
            ff,
            16.0,
            if self.ffmpeg.is_some() { DIM } else { RED },
        );

        divider(&p, 340.0);

        // ---- Settings ----
        let lx = 24.0;
        let row = |y: f32, s: &str| {
            text(&p, pos2(lx, y + 18.0), Align2::LEFT_CENTER, s, 20.0, INK);
        };
        row(352.0, "Output / Flyer Drive");
        let drive_names: Vec<String> = self.drives.iter().map(|d| format!("{}:", d.1)).collect();
        let drive_refs: Vec<&str> = drive_names.iter().map(String::as_str).collect();
        if cycle(
            ui,
            Rect::from_min_size(pos2(236.0, 352.0), vec2(200.0, 38.0)),
            &drive_refs,
            &mut self.drive,
            !busy,
        ) {
            self.used.1 = None;
        }
        text(
            &p,
            pos2(456.0, 370.0),
            Align2::LEFT_CENTER,
            "Clip Name",
            20.0,
            INK,
        );
        let name_r = Rect::from_min_size(pos2(556.0, 352.0), vec2(150.0, 38.0));
        well(&p, name_r);
        if let Some(i) = self.selected.filter(|&i| i < self.items.len()) {
            let edit = egui::TextEdit::singleline(&mut self.items[i].name)
                .char_limit(NAME_MAX)
                .vertical_align(egui::Align::Center)
                .frame(egui::Frame::NONE)
                .font(FontId::proportional(18.0))
                .text_color(INK)
                .margin(vec2(6.0, 0.0));
            ui.add_enabled_ui(!busy, |ui| {
                ui.put(name_r.shrink(2.0), edit);
            });
        }
        divider(&p, 400.0);

        row(410.0, "Conversion Quality");
        cycle(
            ui,
            Rect::from_min_size(pos2(236.0, 410.0), vec2(200.0, 38.0)),
            &QUALITY,
            &mut self.quality,
            !busy,
        );
        text(
            &p,
            pos2(456.0, 428.0),
            Align2::LEFT_CENTER,
            "Motion",
            20.0,
            INK,
        );
        cycle(
            ui,
            Rect::from_min_size(pos2(536.0, 410.0), vec2(170.0, 38.0)),
            &MOTION,
            &mut self.motion,
            !busy,
        );

        row(456.0, "Picture Fit");
        cycle(
            ui,
            Rect::from_min_size(pos2(236.0, 456.0), vec2(200.0, 38.0)),
            &FIT,
            &mut self.fit,
            !busy,
        );
        let still_labels: Vec<&str> = STILL.iter().map(|s| s.1).collect();
        let seq_labels: Vec<&str> = SEQ.iter().map(|s| s.1).collect();
        cycle(
            ui,
            Rect::from_min_size(pos2(456.0, 456.0), vec2(124.0, 38.0)),
            &still_labels,
            &mut self.still,
            !busy,
        );
        cycle(
            ui,
            Rect::from_min_size(pos2(586.0, 456.0), vec2(120.0, 38.0)),
            &seq_labels,
            &mut self.seq,
            !busy,
        );

        row(502.0, "Resolution / Fielding");
        let res = Rect::from_min_size(pos2(236.0, 502.0), vec2(470.0, 38.0));
        well(&p, res);
        text(
            &p,
            res.center(),
            Align2::CENTER_CENTER,
            "752 x 484  NTSC 4fsc composite, interlaced",
            18.0,
            INK,
        );
        divider(&p, 550.0);

        row(560.0, "Audio Source");
        cycle(
            ui,
            Rect::from_min_size(pos2(236.0, 560.0), vec2(250.0, 38.0)),
            &AUDIO,
            &mut self.audio,
            !busy,
        );
        let audio_on = self.audio != 2 && !busy;
        gain(
            ui,
            pos2(24.0, 610.0),
            "Audio Gain L",
            &mut self.gain[0],
            audio_on,
        );
        gain(
            ui,
            pos2(376.0, 610.0),
            "Audio Gain R",
            &mut self.gain[1],
            audio_on,
        );

        // ---- Preview ----
        let pv = Rect::from_min_max(pos2(724.0, 352.0), pos2(984.0, 668.0));
        p.rect_filled(pv, 0.0, BG);
        bevel(&p, pv, false);
        let pt = Rect::from_min_max(
            pv.min + vec2(3.0, 3.0),
            pos2(pv.max.x - 3.0, pv.min.y + 28.0),
        );
        p.rect_filled(pt, 0.0, FACE);
        bevel(&p, pt, true);
        text(
            &p,
            pos2(pt.min.x + 10.0, pt.center().y),
            Align2::LEFT_CENTER,
            "Preview",
            18.0,
            INK,
        );
        let screen = Rect::from_min_size(pos2(pv.min.x + 10.0, pt.max.y + 8.0), vec2(240.0, 180.0));
        p.rect_filled(screen.expand(3.0), 0.0, WELL);
        bevel(&p, screen.expand(3.0), false);
        match &self.preview {
            Some(t) => {
                p.image(
                    t.id(),
                    screen,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                p.rect_filled(screen, 0.0, Color32::from_rgb(0x20, 0x20, 0x28));
                // Colour bars stand in when there is no picture.
                let bars = [
                    (0xC0, 0xC0, 0xC0),
                    (0xC0, 0xC0, 0x00),
                    (0x00, 0xC0, 0xC0),
                    (0x00, 0xC0, 0x00),
                    (0xC0, 0x00, 0xC0),
                    (0xC0, 0x00, 0x00),
                    (0x00, 0x00, 0xC0),
                ];
                let w = screen.width() / 7.0;
                for (k, (r, g, b)) in bars.iter().enumerate() {
                    p.rect_filled(
                        Rect::from_min_size(
                            pos2(screen.min.x + k as f32 * w, screen.min.y),
                            vec2(w + 0.5, screen.height() * 0.66),
                        ),
                        0.0,
                        Color32::from_rgb(*r, *g, *b),
                    );
                }
                text(
                    &p,
                    screen.center() + vec2(0.0, 52.0),
                    Align2::CENTER_CENTER,
                    "FLYER",
                    26.0,
                    LIGHT,
                );
            }
        }
        // Where in the source the preview looks.
        let ty = screen.max.y + 12.0;
        let tw = 56.0;
        let labels = ["|<", "<", ">", ">|"];
        for (k, l) in labels.iter().enumerate() {
            let r = Rect::from_min_size(
                pos2(pv.min.x + 10.0 + k as f32 * (tw + 5.0), ty),
                vec2(tw, 34.0),
            );
            if button(ui, r, l, !busy && self.selected.is_some(), false).clicked() {
                let len = self
                    .selected
                    .and_then(|i| self.items.get(i))
                    .and_then(|it| it.seconds(&options))
                    .unwrap_or(10.0);
                self.preview_seconds = match k {
                    0 => 0.0,
                    1 => (self.preview_seconds - 5.0).max(0.0),
                    2 => (self.preview_seconds + 5.0).min((len - 0.1).max(0.0)),
                    _ => (len - 0.5).max(0.0),
                };
            }
        }
        text(
            &p,
            pos2(pv.center().x, ty + 48.0),
            Align2::CENTER_CENTER,
            &timecode(self.preview_seconds),
            17.0,
            INK,
        );

        divider(&p, 680.0);
        checkbox(
            ui,
            pos2(24.0, 690.0),
            "Make crouton icons (.I)",
            &mut self.icons,
            !busy,
        );
        checkbox(
            ui,
            pos2(420.0, 690.0),
            "Replace clips of the same name",
            &mut self.replace,
            !busy,
        );
        divider(&p, 728.0);

        // ---- Totals and progress ----
        let total: f64 = self
            .items
            .iter()
            .filter(|i| !matches!(i.status, Status::Done(_)))
            .filter_map(|i| i.seconds(&options))
            .sum::<f64>()
            .max(0.0);
        let chans = if options.audio { 2 } else { 0 };
        let bytes = fc::bytes_per_second(options.grade, chans) * total;
        let used = self.drive_used();
        let free = DRIVE_BYTES.saturating_sub(used) as f64;
        let space = format!("Free on {}:", self.drives[self.drive].1);
        let info = [
            ("Approx. Output Length:", timecode(total)),
            (
                "Estimated Disk Usage:",
                format!("up to {}", megabytes(bytes)),
            ),
            (space.as_str(), megabytes(free)),
        ];
        for (k, (a, b)) in info.iter().enumerate() {
            let y = 748.0 + k as f32 * 26.0;
            text(&p, pos2(24.0, y), Align2::LEFT_CENTER, a, 18.0, INK);
            text(
                &p,
                pos2(250.0, y),
                Align2::LEFT_CENTER,
                b,
                18.0,
                if k == 2 && bytes > free { RED } else { INK },
            );
        }
        p.line_segment(
            [pos2(470.0, 738.0), pos2(470.0, 806.0)],
            Stroke::new(1.0, SHADE),
        );
        p.line_segment(
            [pos2(471.0, 738.0), pos2(471.0, 806.0)],
            Stroke::new(1.0, LIGHT),
        );
        let bar = Rect::from_min_size(pos2(488.0, 762.0), vec2(496.0, 22.0));
        well(&p, bar);
        let (status, fraction, remain) = match &self.job {
            Some(job) => {
                let item = job.current.and_then(|i| self.items.get(i));
                let item_seconds = item.and_then(|it| it.seconds(&options)).unwrap_or(0.0);
                let frame_total = (item_seconds * 30000.0 / 1001.0).ceil().max(1.0) as usize;
                let frame = (job.frames * 2).min(frame_total);
                let done = job.done_seconds + job.frames as f64 * 2.0 * 1001.0 / 30000.0;
                let f = if job.total_seconds > 0.0 {
                    (done / job.total_seconds).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let elapsed = job.started.elapsed().as_secs_f64();
                let remain = if f > 0.02 {
                    elapsed / f * (1.0 - f)
                } else {
                    f64::NAN
                };
                let s = match item {
                    Some(it) if it.kind == Kind::Clip => format!("Copying {}", it.name),
                    Some(it) => format!(
                        "{}: frame {} of {}",
                        truncate(&it.name, 18),
                        frame,
                        frame_total
                    ),
                    None => "Starting...".into(),
                };
                (s, f as f32, remain)
            }
            None => {
                let s = if !self.message.is_empty() {
                    self.message.clone()
                } else if let Some(Status::Done(s) | Status::Failed(s)) = self
                    .selected
                    .and_then(|i| self.items.get(i))
                    .map(|i| &i.status)
                {
                    s.clone()
                } else {
                    "Ready".into()
                };
                (s, 0.0, f64::NAN)
            }
        };
        text(
            &p,
            pos2(488.0, 746.0),
            Align2::LEFT_CENTER,
            &truncate(&status, 46),
            18.0,
            INK,
        );
        if fraction > 0.0 {
            let fill = Rect::from_min_size(
                bar.min + vec2(2.0, 2.0),
                vec2((bar.width() - 4.0) * fraction, bar.height() - 4.0),
            );
            p.rect_filled(fill, 0.0, GREEN);
        }
        if self.job.is_some() {
            let r = if remain.is_finite() {
                format!(" (Est. {} Remaining)", hms(remain))
            } else {
                String::new()
            };
            text(
                &p,
                pos2(488.0, 798.0),
                Align2::LEFT_CENTER,
                &format!("{:.0}% Complete{r}", fraction * 100.0),
                18.0,
                INK,
            );
        }
        divider(&p, 816.0);

        // ---- Actions ----
        if button(
            ui,
            Rect::from_min_size(pos2(20.0, 824.0), vec2(220.0, 36.0)),
            "Open Drive Folder",
            true,
            false,
        )
        .clicked()
        {
            let dir = self.drive_folder();
            let _ = std::fs::create_dir_all(&dir);
            open_folder(&dir);
        }
        if busy {
            if button(
                ui,
                Rect::from_min_size(pos2(560.0, 824.0), vec2(200.0, 36.0)),
                "Stop",
                true,
                true,
            )
            .clicked()
            {
                if let Some(job) = &self.job {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            }
        } else if button(
            ui,
            Rect::from_min_size(pos2(560.0, 824.0), vec2(200.0, 36.0)),
            "Convert",
            !self.items.is_empty() && self.ffmpeg.is_some(),
            false,
        )
        .clicked()
        {
            self.start(&ctx);
        }
        if button(
            ui,
            Rect::from_min_size(pos2(780.0, 824.0), vec2(200.0, 36.0)),
            "Close",
            true,
            false,
        )
        .clicked()
        {
            if let Some(job) = &self.job {
                job.cancel.store(true, Ordering::Relaxed);
            }
            self.save_settings();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn on_exit(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
        self.save_settings();
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}

fn open_folder(dir: &Path) {
    #[cfg(windows)]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let cmd = "xdg-open";
    let _ = std::process::Command::new(cmd).arg(dir).spawn();
}
