// SPDX-License-Identifier: GPL-3.0-or-later

//! MP4 recordings: H.264 video and AAC sound, made from the recorder's
//! lossless AVI by the host's ffmpeg (`crate::ffmpeg`) once a recording
//! stops.
//!
//! Copperline carries no H.264 or AAC encoder of its own (there is no
//! pure-Rust one, and the C ones bring a C toolchain and codec licensing
//! into every build), so a recording is captured exactly as an AVI one is
//! -- cheaply, on the emulated timeline -- and encoded afterwards, in the
//! background, at a lowered priority so the emulation keeps the host.
//! The AVI is removed once the MP4 is written and kept whenever it is not.

use anyhow::{anyhow, Context, Result};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

/// The window's filter chain: every pixel doubled with nearest-neighbour
/// sampling, so each picture pixel keeps chroma of its own through 4:2:0
/// subsampling (a hires pixel at 1x shares its colour with its
/// neighbour) and the TV-aspect picture's odd 537 rows become even, as
/// 4:2:0 requires. The RGB-to-YUV matrix is BT.709 and the stream says
/// so: players assume BT.709 for an untagged picture of this size, and a
/// BT.601 conversion read that way shifts every colour.
const DOUBLED_FILTER: &str = "scale=iw*2:ih*2:flags=neighbor:out_color_matrix=bt709:out_range=tv,\
     format=yuv420p,setsar=1,\
     setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv";

/// Whether `path` names an MP4 (or QuickTime) file. A recording to one is
/// captured into [`avi_beside`] it and made into it when it stops.
pub fn is_mp4(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mp4" | "m4v" | "mov"))
}

/// The AVI a recording to `mp4` is captured into until it stops.
pub fn avi_beside(mp4: &Path) -> PathBuf {
    mp4.with_extension("recording.avi")
}

/// How an AVI becomes an MP4.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mp4Options {
    /// ffmpeg's video filter chain ahead of the H.264 encoder. It must
    /// leave even dimensions: 4:2:0 chroma needs them.
    pub video_filter: String,
    /// libx264's constant rate factor (lower is better and larger).
    pub crf: u8,
    /// AAC bit rate, kb/s.
    pub audio_kbps: u32,
}

impl Mp4Options {
    /// The window's recordings: the captured picture at twice its size
    /// (see [`DOUBLED_FILTER`]), CRF 18 and 192 kb/s.
    pub fn doubled() -> Self {
        Self {
            video_filter: DOUBLED_FILTER.into(),
            crf: 18,
            audio_kbps: 192,
        }
    }

    /// Scaled to `width` x `height` square pixels with Lanczos: the
    /// Video Toaster's Program output, at CRF 18 and 192 kb/s, or CRF 12
    /// and 320 kb/s when `high`.
    pub fn scaled(width: u32, height: u32, high: bool) -> Self {
        Self {
            video_filter: format!("scale={width}:{height}:flags=lanczos,setsar=1"),
            crf: if high { 12 } else { 18 },
            audio_kbps: if high { 320 } else { 192 },
        }
    }

    /// ffmpeg's command line for `avi` to `mp4`.
    fn args(&self, avi: &Path, mp4: &Path) -> Vec<OsString> {
        // The container follows the name; ffmpeg is told rather than left
        // to guess, so the choice is this function's.
        let muxer = match mp4.extension().and_then(|e| e.to_str()) {
            Some(e) if e.eq_ignore_ascii_case("mov") => "mov",
            _ => "mp4",
        };
        let mut args: Vec<OsString> = [
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostats",
            "-y",
            "-i",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        args.push(avi.into());
        args.extend(
            [
                "-vf",
                &self.video_filter,
                "-c:v",
                "libx264",
                "-preset",
                "medium",
                "-crf",
                &self.crf.to_string(),
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                &format!("{}k", self.audio_kbps),
                // The index up front, so a browser or a chat client can
                // start playing before the whole file has arrived.
                "-movflags",
                "+faststart",
                "-f",
                muxer,
            ]
            .iter()
            .map(OsString::from),
        );
        args.push(mp4.into());
        args
    }
}

/// Make `mp4` from `avi` now, on this thread: [`Conversion::start`] and
/// [`Conversion::wait`].
pub fn convert(ffmpeg: &Path, avi: &Path, mp4: &Path, options: &Mp4Options) -> Result<()> {
    Conversion::start(ffmpeg, avi, mp4, options)?.wait()
}

/// An MP4 being made from a finished AVI by an ffmpeg running in the
/// background.
///
/// Once ffmpeg succeeds and the MP4 is there, the AVI is removed; if it
/// fails, whatever it wrote is removed and the AVI stays. Dropping a
/// conversion that is still running (the program quitting, say) leaves
/// ffmpeg to finish on its own: the MP4 still appears, and the AVI stays
/// beside it, as nobody is left to remove it.
pub struct Conversion {
    child: Option<Child>,
    /// Collects ffmpeg's error output so a failure can say what it was
    /// (and so ffmpeg never blocks on a full pipe).
    stderr: Option<std::thread::JoinHandle<String>>,
    avi: PathBuf,
    mp4: PathBuf,
}

impl Conversion {
    /// Start `ffmpeg` making `mp4` from `avi`.
    pub fn start(ffmpeg: &Path, avi: &Path, mp4: &Path, options: &Mp4Options) -> Result<Self> {
        let mut cmd = Command::new(ffmpeg);
        cmd.args(options.args(avi, mp4))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        lower_priority(&mut cmd);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("running {}", ffmpeg.display()))?;
        let stderr = match child.stderr.take() {
            Some(mut pipe) => Some(
                std::thread::Builder::new()
                    .name("ffmpeg-stderr".into())
                    .spawn(move || {
                        let mut text = String::new();
                        let _ = pipe.read_to_string(&mut text);
                        text
                    })
                    .context("starting the ffmpeg output reader")?,
            ),
            None => None,
        };
        Ok(Self {
            child: Some(child),
            stderr,
            avi: avi.to_path_buf(),
            mp4: mp4.to_path_buf(),
        })
    }

    /// The AVI being encoded.
    pub fn avi(&self) -> &Path {
        &self.avi
    }

    /// The MP4 being made.
    pub fn mp4(&self) -> &Path {
        &self.mp4
    }

    /// The outcome, once: `None` while ffmpeg runs (and after the outcome
    /// has been returned), `Some` the first time it is seen to have
    /// finished.
    pub fn poll(&mut self) -> Option<Result<()>> {
        let status = match self.child.as_mut()?.try_wait() {
            Ok(None) => return None,
            Ok(Some(status)) => Ok(status),
            Err(e) => Err(e),
        };
        self.child = None;
        Some(self.settle(status))
    }

    /// Wait for ffmpeg to finish, and the outcome.
    pub fn wait(mut self) -> Result<()> {
        let status = self
            .child
            .take()
            .ok_or_else(|| anyhow!("the conversion has already finished"))?
            .wait();
        self.settle(status)
    }

    /// Tidy up after ffmpeg: on success the AVI goes, otherwise whatever
    /// was written of the MP4 does.
    fn settle(&mut self, status: std::io::Result<ExitStatus>) -> Result<()> {
        let output = self
            .stderr
            .take()
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        let output = output.trim();
        let outcome = match status {
            Err(e) => Err(anyhow::Error::from(e).context("waiting for ffmpeg")),
            Ok(status) if !status.success() => Err(if output.is_empty() {
                anyhow!("ffmpeg failed ({status})")
            } else {
                anyhow!("ffmpeg failed ({status}): {output}")
            }),
            // Never trade the AVI for nothing: a success that left no MP4
            // behind is a failure.
            Ok(_) if !std::fs::metadata(&self.mp4).is_ok_and(|m| m.len() > 0) => {
                Err(anyhow!("ffmpeg wrote no {}", self.mp4.display()))
            }
            Ok(_) => Ok(()),
        };
        match outcome {
            Ok(()) => {
                if let Err(e) = std::fs::remove_file(&self.avi) {
                    log::warn!("removing {} after its MP4: {e}", self.avi.display());
                }
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::remove_file(&self.mp4);
                Err(e)
            }
        }
    }
}

impl Drop for Conversion {
    fn drop(&mut self) {
        if self.child.is_some() {
            log::info!(
                "ffmpeg is still making {}: it finishes on its own, and {} stays",
                self.mp4.display(),
                self.avi.display()
            );
        }
    }
}

/// Run the encoder below the emulator: it is batch work, and on a host
/// with few cores an ffmpeg at normal priority takes the time the
/// emulation (and its audio) needs to keep up.
fn lower_priority(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: the closure runs in the forked child before exec and
        // makes one system call, touching no memory or locks it shares
        // with the parent. A failure leaves ffmpeg at normal priority.
        unsafe {
            cmd.pre_exec(|| {
                libc::setpriority(libc::PRIO_PROCESS, 0, 10);
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window flashing up, and below the emulator.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = cmd;
}

/// The picture size an MP4's H.264 sample entry declares: the `avc1`
/// entry of the first `stsd` box holding one (`avc1` alone also turns up
/// among the `ftyp` brands).
#[cfg(test)]
pub(crate) fn h264_dimensions(mp4: &[u8]) -> Option<(u16, u16)> {
    let mut from = 0;
    while let Some(at) = mp4[from..].windows(4).position(|w| w == b"stsd") {
        // 'stsd', version and flags, entry count, then the first entry's
        // size and type; its width and height follow 24 bytes of fields.
        let stsd = from + at;
        if mp4.get(stsd + 16..stsd + 20) == Some(b"avc1".as_slice()) {
            let field = |ofs: usize| {
                mp4.get(ofs..ofs + 2)
                    .map(|b| u16::from_be_bytes([b[0], b[1]]))
            };
            return Some((field(stsd + 44)?, field(stsd + 46)?));
        }
        from = stsd + 4;
    }
    None
}

/// A stand-in ffmpeg for tests: a shell script in `dir` running `body`
/// with ffmpeg's arguments, returned once it can be run. On Linux a
/// fork made by another test while the script was being written holds
/// a copy of its write handle until that child execs, and running the
/// script before then fails with ETXTBSY ("Text file busy"). The
/// script's own handle is closed before the first run is tried, so once
/// one run gets past exec no copy is left to stop a later one.
#[cfg(all(test, unix))]
pub(crate) fn fake_ffmpeg(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    const READY: &str = "--fake-ffmpeg-ready";
    let path = dir.join("ffmpeg");
    std::fs::write(
        &path,
        format!("#!/bin/sh\n[ \"$1\" = {READY} ] && exit 0\n{body}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match Command::new(&path).arg(READY).status() {
            Ok(status) => {
                assert!(status.success(), "the fake ffmpeg's ready run: {status}");
                return path;
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => panic!("running the fake ffmpeg {}: {e}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(options: &Mp4Options, mp4: &str) -> Vec<String> {
        options
            .args(Path::new("in.recording.avi"), Path::new(mp4))
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect()
    }

    fn value_after<'a>(args: &'a [String], flag: &str) -> &'a str {
        let at = args.iter().position(|a| a == flag).unwrap();
        &args[at + 1]
    }

    #[test]
    fn recording_names() {
        assert!(is_mp4(Path::new("a/clip.mp4")));
        assert!(is_mp4(Path::new("clip.MOV")));
        assert!(is_mp4(Path::new("clip.m4v")));
        assert!(!is_mp4(Path::new("clip.avi")));
        assert!(!is_mp4(Path::new("clip")));
        assert_eq!(
            avi_beside(Path::new("rec/copperline-video-1.mp4")),
            Path::new("rec/copperline-video-1.recording.avi")
        );
    }

    #[test]
    fn doubled_options_keep_square_pixels_and_tag_bt709() {
        let args = args_of(&Mp4Options::doubled(), "out.mp4");
        assert_eq!(value_after(&args, "-i"), "in.recording.avi");
        let vf = value_after(&args, "-vf");
        assert!(vf.starts_with("scale=iw*2:ih*2:flags=neighbor"), "{vf}");
        assert!(vf.contains("out_color_matrix=bt709"), "{vf}");
        assert!(vf.contains("setsar=1"), "{vf}");
        assert!(vf.contains("colorspace=bt709"), "{vf}");
        assert_eq!(value_after(&args, "-c:v"), "libx264");
        assert_eq!(value_after(&args, "-crf"), "18");
        assert_eq!(value_after(&args, "-pix_fmt"), "yuv420p");
        assert_eq!(value_after(&args, "-c:a"), "aac");
        assert_eq!(value_after(&args, "-b:a"), "192k");
        assert_eq!(value_after(&args, "-f"), "mp4");
        assert_eq!(args.last().map(String::as_str), Some("out.mp4"));
    }

    #[test]
    fn scaled_options_and_the_muxer_follow_the_name() {
        let args = args_of(&Mp4Options::scaled(640, 480, true), "out.MOV");
        assert_eq!(
            value_after(&args, "-vf"),
            "scale=640:480:flags=lanczos,setsar=1"
        );
        assert_eq!(value_after(&args, "-crf"), "12");
        assert_eq!(value_after(&args, "-b:a"), "320k");
        assert_eq!(value_after(&args, "-f"), "mov");
        let args = args_of(&Mp4Options::scaled(640, 480, false), "out.m4v");
        assert_eq!(value_after(&args, "-crf"), "18");
        assert_eq!(value_after(&args, "-b:a"), "192k");
        assert_eq!(value_after(&args, "-f"), "mp4");
    }

    /// [`Conversion::start`] with the options a recording uses.
    #[cfg(unix)]
    fn start(ffmpeg: &Path, avi: &Path, mp4: &Path) -> Conversion {
        Conversion::start(ffmpeg, avi, mp4, &Mp4Options::doubled())
            .expect("starting the fake ffmpeg")
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_conversion_keeps_the_avi_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let avi = dir.path().join("rec.recording.avi");
        let mp4 = dir.path().join("rec.mp4");
        std::fs::write(&avi, b"RIFF").unwrap();
        // ffmpeg got as far as starting the file before it failed.
        let ffmpeg = fake_ffmpeg(
            dir.path(),
            &format!(
                "echo partial > '{}'; echo 'Unknown encoder libx264' >&2; exit 1",
                mp4.display()
            ),
        );
        let err = start(&ffmpeg, &avi, &mp4).wait().unwrap_err();
        assert!(
            format!("{err:#}").contains("Unknown encoder libx264"),
            "{err:#}"
        );
        assert!(avi.is_file(), "the AVI stays");
        assert!(!mp4.exists(), "the partial MP4 is removed");
    }

    #[cfg(unix)]
    #[test]
    fn a_success_that_writes_nothing_keeps_the_avi() {
        let dir = tempfile::tempdir().unwrap();
        let avi = dir.path().join("rec.recording.avi");
        let mp4 = dir.path().join("rec.mp4");
        std::fs::write(&avi, b"RIFF").unwrap();
        let ffmpeg = fake_ffmpeg(dir.path(), "exit 0");
        let err = start(&ffmpeg, &avi, &mp4).wait().unwrap_err();
        assert!(format!("{err:#}").contains("wrote no"), "{err:#}");
        assert!(avi.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_finished_conversion_replaces_the_avi_and_is_reported_once() {
        let dir = tempfile::tempdir().unwrap();
        let avi = dir.path().join("rec.recording.avi");
        let mp4 = dir.path().join("rec.mp4");
        std::fs::write(&avi, b"RIFF").unwrap();
        // The output is ffmpeg's last argument.
        let ffmpeg = fake_ffmpeg(dir.path(), "for last; do :; done; echo mp4 > \"$last\"");
        let mut conversion = start(&ffmpeg, &avi, &mp4);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let outcome = loop {
            if let Some(outcome) = conversion.poll() {
                break outcome;
            }
            assert!(std::time::Instant::now() < deadline, "the fake ffmpeg hung");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        outcome.unwrap();
        assert!(conversion.poll().is_none(), "the outcome is reported once");
        assert!(mp4.is_file());
        assert!(!avi.exists(), "the AVI is removed once the MP4 is there");
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_running_conversion_lets_ffmpeg_finish() {
        let dir = tempfile::tempdir().unwrap();
        let avi = dir.path().join("rec.recording.avi");
        let mp4 = dir.path().join("rec.mp4");
        std::fs::write(&avi, b"RIFF").unwrap();
        let ffmpeg = fake_ffmpeg(
            dir.path(),
            "sleep 0.2; for last; do :; done; echo mp4 > \"$last\"",
        );
        drop(start(&ffmpeg, &avi, &mp4));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !mp4.is_file() {
            assert!(
                std::time::Instant::now() < deadline,
                "ffmpeg was stopped with its conversion"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(avi.is_file(), "nobody is left to remove the AVI");
    }

    /// The real thing, when the host has an ffmpeg: a recorder AVI with an
    /// odd height (as the TV-aspect picture's 537 rows are) becomes a
    /// playable MP4.
    #[test]
    fn the_hosts_ffmpeg_makes_an_mp4_from_a_recorder_avi() {
        let Some(ffmpeg) = crate::ffmpeg::find(None, &[]) else {
            eprintln!("no ffmpeg on this host; skipping");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mp4 = dir.path().join("rec.mp4");
        let avi = avi_beside(&mp4);
        let (w, h) = (43, 21);
        let mut rec = crate::recorder::VideoRecorder::create(&avi, w, h).unwrap();
        let sound = vec![(0.25f32, -0.25f32); 882];
        for i in 0..10u32 {
            let frame: Vec<u32> = (0..(w * h) as u32)
                .map(|p| u32::from_le_bytes([(p * 7 + i) as u8, (p * 3) as u8, 0x40, 0xFF]))
                .collect();
            rec.push_audio(&sound);
            rec.push_frame(&frame).unwrap();
        }
        rec.finish().unwrap();
        drop(rec);
        if let Err(e) = convert(&ffmpeg, &avi, &mp4, &Mp4Options::doubled()) {
            // An ffmpeg built without libx264 cannot make one; that is the
            // host's, not this code's.
            if format!("{e:#}").contains("libx264") {
                eprintln!("this host's ffmpeg has no libx264; skipping: {e:#}");
                assert!(avi.is_file());
                return;
            }
            panic!("converting with {}: {e:#}", ffmpeg.display());
        }
        let bytes = std::fs::read(&mp4).unwrap();
        assert_eq!(&bytes[4..8], b"ftyp");
        assert_eq!(
            h264_dimensions(&bytes),
            Some((2 * w as u16, 2 * h as u16)),
            "H.264 at twice the captured size"
        );
        assert!(bytes.windows(4).any(|w| w == b"mp4a"), "the sound is AAC");
        assert!(!avi.exists());
    }
}
