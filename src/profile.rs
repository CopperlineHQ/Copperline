// SPDX-License-Identifier: GPL-3.0-or-later

//! Per-frame profile export for external profiler views.
//!
//! `profile.start` over the control protocol brackets a capture the way
//! `trace.start` and `waveform.start` do: while it runs, every committed
//! emulated frame appends one JSON line to `profile.jsonl` in the capture
//! directory -- chip-bus ownership totals and blit records from the frame
//! analyzer's trace (armed for the whole session), the guest's uaelib idle
//! markers, the retired-instruction delta, optionally one PC sample, the
//! per-colour-clock owner grid (RLE over the vAmiga-compatible owner
//! codes), and optionally a screenshot per frame through the same
//! side-effect-free renderer `capture.screenshot` uses. `profile.stop`
//! writes a `profile.json` summary (machine, options, owner names, the
//! registered uaelib resources); the streamed `profile.jsonl` survives a
//! crash without it.
//!
//! The collector is host-side state on the [`crate::emulator::Emulator`],
//! never serialized; reverse steps and state loads appear in the stream as
//! `{"marker":"reposition"}` lines rather than corrupt deltas. Arming the
//! frame analyzer blocks run-ahead for the session (the analyzer's
//! existing rule).

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Bounds on `profile.start {"frames"}`: about ten seconds of PAL by
/// default, and a hard cap so a typo cannot fill a disk.
pub const DEFAULT_PROFILE_FRAMES: u64 = 500;
pub const MAX_PROFILE_FRAMES: u64 = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotMode {
    None,
    Every,
    Last,
}

impl ScreenshotMode {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "none" => Some(Self::None),
            "every" => Some(Self::Every),
            "last" => Some(Self::Last),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Every => "every",
            Self::Last => "last",
        }
    }
}

/// Stalled-PC rows each `profile.jsonl` record lists under `cpu.stall_pcs`.
pub const PROFILE_STALL_PCS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileOptions {
    /// The capture directory; `profile.jsonl`, `profile.json` and any
    /// screenshots land inside it.
    pub path: PathBuf,
    pub frames: u64,
    /// Also write the per-colour-clock owner grid, RLE'd per row.
    pub slots: bool,
    pub screenshots: ScreenshotMode,
    /// Sample the PC once per frame (frame-boundary sample; no
    /// precise-loop cost, unlike a per-instruction histogram).
    pub pc_samples: bool,
}

/// A running capture: the streamed record file plus the bookkeeping the
/// per-frame poll needs.
pub struct ProfileCapture {
    opts: ProfileOptions,
    jsonl: BufWriter<File>,
    started: (u64, f64),
    frames_written: u64,
    last_frame_seen: Option<u64>,
    last_retired: u64,
    /// Whether this capture armed the frame analyzer (and must disarm it),
    /// or adopted one the analyzer pane handed over on close.
    armed_analyzer: bool,
    done: bool,
}

impl ProfileCapture {
    pub fn create(
        opts: ProfileOptions,
        frame: u64,
        seconds: f64,
        retired: u64,
        armed_analyzer: bool,
    ) -> io::Result<Self> {
        crate::paths::ensure_parent(&opts.path)?;
        std::fs::create_dir_all(&opts.path)?;
        let jsonl = BufWriter::new(File::create(opts.path.join("profile.jsonl"))?);
        Ok(Self {
            opts,
            jsonl,
            started: (frame, seconds),
            frames_written: 0,
            last_frame_seen: Some(frame),
            last_retired: retired,
            armed_analyzer,
            done: false,
        })
    }

    pub fn options(&self) -> &ProfileOptions {
        &self.opts
    }

    pub fn dir(&self) -> &Path {
        &self.opts.path
    }

    pub fn done(&self) -> bool {
        self.done
    }

    pub fn armed_analyzer(&self) -> bool {
        self.armed_analyzer
    }

    /// The frame analyzer pane closed while this capture runs: its arming
    /// is adopted, so the capture disarms it at stop.
    pub fn adopt_frame_analyzer(&mut self) {
        self.armed_analyzer = true;
    }

    pub fn last_frame_seen(&self) -> Option<u64> {
        self.last_frame_seen
    }

    /// Retired instructions since the last take, rebaselining.
    pub fn take_retired_delta(&mut self, retired_now: u64) -> u64 {
        let delta = retired_now.saturating_sub(self.last_retired);
        self.last_retired = retired_now;
        delta
    }

    /// The timeline moved backwards (a reverse step, a state load): mark
    /// the stream and rebaseline rather than emitting a corrupt delta.
    pub fn note_reposition(&mut self, frame: u64, retired: u64) -> io::Result<()> {
        writeln!(
            self.jsonl,
            "{}",
            json!({ "marker": "reposition", "frame": frame })
        )?;
        self.last_frame_seen = Some(frame);
        self.last_retired = retired;
        Ok(())
    }

    /// Append one committed frame's record. Self-stops (like the
    /// instruction trace's line cap) once the frame budget is spent.
    pub fn record(&mut self, frame: u64, record: &Value) -> io::Result<()> {
        if self.done {
            return Ok(());
        }
        writeln!(self.jsonl, "{record}")?;
        self.last_frame_seen = Some(frame);
        self.frames_written += 1;
        if self.frames_written >= self.opts.frames {
            self.done = true;
            self.jsonl.flush()?;
        }
        Ok(())
    }

    pub fn status_value(&self, active: bool) -> Value {
        json!({
            "active": active,
            "path": self.opts.path.display().to_string(),
            "frames_written": self.frames_written,
            "frames_limit": self.opts.frames,
            "slots": self.opts.slots,
            "screenshots": self.opts.screenshots.name(),
            "pc_samples": self.opts.pc_samples,
            "done": self.done,
        })
    }

    /// Close the capture: flush the stream, write the `profile.json`
    /// summary, and return the final status.
    pub fn finish(
        &mut self,
        machine: Value,
        resources: Value,
        frame: u64,
        seconds: f64,
    ) -> io::Result<Value> {
        self.jsonl.flush()?;
        let summary = json!({
            "version": 1,
            "machine": machine,
            "options": {
                "frames": self.opts.frames,
                "slots": self.opts.slots,
                "screenshots": self.opts.screenshots.name(),
                "pc_samples": self.opts.pc_samples,
            },
            "owners": crate::bus::CHIP_BUS_OWNER_NAMES,
            "cpu_wait_classes": crate::bus::CPU_WAIT_CLASS_NAMES,
            "started": { "frame": self.started.0, "seconds": self.started.1 },
            "ended": { "frame": frame, "seconds": seconds },
            "frames_written": self.frames_written,
            "resources": resources,
        });
        std::fs::write(
            self.opts.path.join("profile.json"),
            serde_json::to_string_pretty(&summary).unwrap_or_default(),
        )?;
        let mut status = self.status_value(false);
        status["summary"] = Value::from(self.opts.path.join("profile.json").display().to_string());
        Ok(status)
    }
}

/// One trace row's owner codes, run-length encoded as `<count><code>`
/// runs -- `"12R3B497."` -- over the same single-char codes vAmiga's DMA
/// debugger uses, so a consumer can share its legend.
pub fn rle_owner_row(row: &[u8]) -> String {
    let mut out = String::new();
    let mut iter = row.iter().copied().peekable();
    while let Some(code) = iter.next() {
        let mut run = 1usize;
        while iter.peek() == Some(&code) {
            iter.next();
            run += 1;
        }
        out.push_str(&run.to_string());
        out.push(code as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rle_encodes_owner_rows_compactly() {
        assert_eq!(rle_owner_row(b""), "");
        assert_eq!(rle_owner_row(b"R"), "1R");
        let mut row = Vec::new();
        row.extend(std::iter::repeat_n(b'R', 12));
        row.extend(std::iter::repeat_n(b'B', 3));
        row.extend(std::iter::repeat_n(b'.', 497));
        assert_eq!(rle_owner_row(&row), "12R3B497.");
    }

    #[test]
    fn capture_self_stops_at_the_frame_cap_and_summarises() {
        let dir = std::env::temp_dir().join(format!(
            "copperline-profile-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let opts = ProfileOptions {
            path: dir.clone(),
            frames: 2,
            slots: false,
            screenshots: ScreenshotMode::None,
            pc_samples: false,
        };
        let mut capture = ProfileCapture::create(opts, 100, 2.0, 1000, true).unwrap();
        assert_eq!(capture.take_retired_delta(1500), 500);
        capture.record(101, &json!({"frame": 101})).unwrap();
        assert!(!capture.done());
        capture.record(102, &json!({"frame": 102})).unwrap();
        assert!(capture.done());
        // A record past the cap is dropped, not written.
        capture.record(103, &json!({"frame": 103})).unwrap();
        capture.note_reposition(50, 100).unwrap();
        let status = capture.finish(Value::Null, json!([]), 103, 2.06).unwrap();
        assert_eq!(status["frames_written"], 2);
        assert_eq!(status["done"], true);

        let jsonl = std::fs::read_to_string(dir.join("profile.jsonl")).unwrap();
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 3, "two records plus the reposition marker");
        let summary: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("profile.json")).unwrap())
                .unwrap();
        assert_eq!(summary["version"], 1);
        assert_eq!(summary["owners"][6], "blitter");
        assert_eq!(summary["started"]["frame"], 100);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
