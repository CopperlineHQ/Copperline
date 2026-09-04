// SPDX-License-Identifier: GPL-3.0-or-later

//! Per-frame profile export for external profiler views.
//!
//! `profile.start` over the control protocol brackets a capture the way
//! `trace.start` and `waveform.start` do: while it runs, every committed
//! emulated frame appends one JSON line to `profile.jsonl` in the capture
//! directory -- chip-bus ownership totals and blit records from the frame
//! analyzer's trace (armed for the whole session), the guest's uaelib idle
//! markers, the retired-instruction delta, optionally one PC sample, precise
//! instruction/call-stack sidecars, the per-colour-clock owner grid (RLE over
//! the vAmiga-compatible owner codes), and optionally a screenshot per frame
//! through the same side-effect-free renderer `capture.screenshot` uses. `profile.stop`
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

#[cfg(feature = "dap")]
pub mod report;
pub mod samples;

/// Bounds on `profile.start {"frames"}`: about ten seconds of PAL by
/// default, and a hard cap so a typo cannot fill a disk.
pub const DEFAULT_PROFILE_FRAMES: u64 = 500;
pub const MAX_PROFILE_FRAMES: u64 = 100_000;

/// Deferred start condition for a profile capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileTrigger {
    Frame(u64),
    BusyCckOver(u64),
}

impl ProfileTrigger {
    pub fn value(self) -> Value {
        match self {
            Self::Frame(frame) => json!({"frame": frame}),
            Self::BusyCckOver(cck) => json!({"busy_cck_over": cck}),
        }
    }
}

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
    /// Dump chip and slow RAM once at capture start for offline replay.
    pub memory: bool,
    pub screenshots: ScreenshotMode,
    /// Sample the PC once per frame (frame-boundary sample; no
    /// precise-loop cost, unlike a per-instruction histogram).
    pub pc_samples: bool,
    /// Record every retired instruction in a Bartman/WinUAE-compatible
    /// binary stream, split into one file per emulated frame.
    pub samples: bool,
    /// Append D0-D7, A0-A7 and SR to every precise sample.
    pub registers: bool,
    /// Optional program text base plus compact DWARF-derived unwind rows.
    pub unwind: Option<samples::CompactUnwindTable>,
    /// Runtime bases of every hunk, for offline source relocation.
    pub relocation_bases: Vec<u32>,
    /// Runtime ranges of executable hunks, for compact-unwind boundaries.
    pub code_ranges: Vec<(u32, u32)>,
    /// Keep the capture armed but write nothing until this condition matches.
    pub trigger: Option<ProfileTrigger>,
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
    /// Whether this capture promoted an existing cheap trace to full records.
    armed_full: bool,
    done: bool,
    triggered: bool,
    triggered_at: Option<u64>,
    samples_total: u64,
    irq_cck: u64,
    sample_sequence: u64,
    cck_per_cpu_cycle: f64,
    stack_bounds: Option<crate::amigaos::StackBounds>,
}

impl ProfileCapture {
    pub fn create(
        opts: ProfileOptions,
        frame: u64,
        seconds: f64,
        retired: u64,
        armed_analyzer: bool,
        armed_full: bool,
        cpu_clocks_per_cck: u32,
    ) -> io::Result<Self> {
        crate::paths::ensure_parent(&opts.path)?;
        std::fs::create_dir_all(&opts.path)?;
        let jsonl = BufWriter::new(File::create(opts.path.join("profile.jsonl"))?);
        let triggered = opts.trigger.is_none();
        Ok(Self {
            opts,
            jsonl,
            started: (frame, seconds),
            frames_written: 0,
            last_frame_seen: Some(frame),
            last_retired: retired,
            armed_analyzer,
            armed_full,
            done: false,
            triggered,
            triggered_at: triggered.then_some(frame),
            samples_total: 0,
            irq_cck: 0,
            sample_sequence: 0,
            cck_per_cpu_cycle: 1.0 / f64::from(cpu_clocks_per_cck.max(1)),
            stack_bounds: None,
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

    pub fn armed_full(&self) -> bool {
        self.armed_full
    }

    /// The frame analyzer pane closed while this capture runs: its arming
    /// is adopted, so the capture disarms it at stop.
    pub fn adopt_frame_analyzer(&mut self) {
        self.armed_analyzer = true;
    }

    pub fn set_stack_bounds(&mut self, bounds: Option<crate::amigaos::StackBounds>) {
        self.stack_bounds = bounds;
    }

    pub fn last_frame_seen(&self) -> Option<u64> {
        self.last_frame_seen
    }

    /// Decide whether this frame is part of the capture. Skipped frames
    /// rebaseline both the frame and retired-instruction counters.
    pub fn should_record(&mut self, frame: u64, busy_cck: u64, retired: u64) -> bool {
        if self.triggered {
            return true;
        }
        let fired = match self.opts.trigger {
            Some(ProfileTrigger::Frame(target)) => frame >= target,
            Some(ProfileTrigger::BusyCckOver(limit)) => busy_cck > limit,
            None => true,
        };
        if fired {
            self.triggered = true;
            self.triggered_at = Some(frame);
        } else {
            self.last_frame_seen = Some(frame);
            self.last_retired = retired;
        }
        fired
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
            "memory": self.opts.memory,
            "screenshots": self.opts.screenshots.name(),
            "pc_samples": self.opts.pc_samples,
            "samples": self.opts.samples,
            "registers": self.opts.registers,
            "trigger": self.opts.trigger.map(ProfileTrigger::value),
            "triggered": self.triggered,
            "triggered_at": self.triggered_at,
            "samples_total": self.samples_total,
            "irq_cck": self.irq_cck,
            "done": self.done,
        })
    }

    /// Write one frame's WinUAE-compatible samples and Copperline's parallel
    /// timing metadata. Long samples are split so `~0 - cck` remains in the
    /// marker range Bartman's parser reserves for cycle counts.
    pub fn write_samples(
        &mut self,
        frame: u64,
        samples: &[samples::InstructionSample],
    ) -> io::Result<SampleFrameStats> {
        let sequence = self.sample_sequence;
        self.sample_sequence = self.sample_sequence.saturating_add(1);
        let samples_name = format!("samples-{sequence:06}-frame-{frame:06}.bin");
        let metadata_name = format!("samples-{sequence:06}-frame-{frame:06}.meta");
        let mut stream = BufWriter::new(File::create(self.opts.path.join(&samples_name))?);
        let mut metadata = BufWriter::new(File::create(self.opts.path.join(&metadata_name))?);
        metadata.write_all(b"CLSM")?;
        metadata.write_all(&1u32.to_le_bytes())?;
        metadata.write_all(&0u32.to_le_bytes())?; // patched count after encoding

        let mut count = 0u32;
        let mut irq_cck = 0u64;
        for sample in samples {
            let mut remaining = sample.total_cck.max(1);
            let total = sample.total_cck.max(1);
            let mut remaining_instruction = sample.instruction_cck;
            while remaining != 0 {
                let chunk = remaining.min(u16::MAX.into());
                let instruction_cck = if remaining == chunk {
                    remaining_instruction.min(chunk)
                } else {
                    ((u64::from(sample.instruction_cck) * u64::from(chunk)) / u64::from(total))
                        as u32
                };
                let bus_wait_cck = chunk.saturating_sub(instruction_cck);
                for pc in &sample.callstack[..sample.callstack_depth] {
                    stream.write_all(&pc.to_le_bytes())?;
                }
                stream.write_all(&(u32::MAX - chunk).to_le_bytes())?;
                if let Some(registers) = &sample.registers {
                    for register in registers {
                        stream.write_all(&register.to_le_bytes())?;
                    }
                }
                metadata.write_all(&chunk.to_le_bytes())?;
                metadata.write_all(&instruction_cck.to_le_bytes())?;
                metadata.write_all(&bus_wait_cck.to_le_bytes())?;
                metadata.write_all(
                    &sample
                        .irq
                        .map_or(u32::MAX, |irq| u32::from(irq.level))
                        .to_le_bytes(),
                )?;
                metadata.write_all(
                    &sample
                        .irq
                        .map_or(u32::MAX, |irq| u32::from(irq.vector))
                        .to_le_bytes(),
                )?;
                if sample.irq.is_some() {
                    irq_cck = irq_cck.saturating_add(u64::from(chunk));
                }
                count = count.saturating_add(1);
                remaining_instruction = remaining_instruction.saturating_sub(instruction_cck);
                remaining -= chunk;
            }
        }
        stream.flush()?;
        metadata.flush()?;
        drop(metadata);
        let mut metadata = std::fs::OpenOptions::new()
            .write(true)
            .open(self.opts.path.join(&metadata_name))?;
        use std::io::Seek;
        metadata.seek(std::io::SeekFrom::Start(8))?;
        metadata.write_all(&count.to_le_bytes())?;

        self.samples_total = self.samples_total.saturating_add(u64::from(count));
        self.irq_cck = self.irq_cck.saturating_add(irq_cck);
        Ok(SampleFrameStats {
            samples_name,
            metadata_name,
            count,
            irq_cck,
            samples_total: self.samples_total,
        })
    }

    /// Write the full 24-byte little-endian DMA records for one frame.
    pub fn write_slots(
        &self,
        frame: u64,
        records: &[crate::bus::BusSlotRecord],
    ) -> io::Result<String> {
        let name = format!("slots-{frame:06}.bin");
        let mut out = BufWriter::new(File::create(self.opts.path.join(&name))?);
        for record in records {
            record.write_to(&mut out)?;
        }
        out.flush()?;
        Ok(name)
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
                "memory": self.opts.memory,
                "screenshots": self.opts.screenshots.name(),
                "pc_samples": self.opts.pc_samples,
                "samples": self.opts.samples,
                "registers": self.opts.registers,
                "trigger": self.opts.trigger.map(ProfileTrigger::value),
            },
            "owners": crate::bus::CHIP_BUS_OWNER_NAMES,
            "cpu_wait_classes": crate::bus::CPU_WAIT_CLASS_NAMES,
            "started": { "frame": self.started.0, "seconds": self.started.1 },
            "ended": { "frame": frame, "seconds": seconds },
            "frames_written": self.frames_written,
            "samples_total": self.samples_total,
            "irq_cck": self.irq_cck,
            "sampling": self.opts.samples.then(|| json!({
                "clock_unit": "cck",
                "cck_per_cpu_cycle": self.cck_per_cpu_cycle,
                "stream": "little-endian u32: callstack PCs, ~0-cck, optional D0-D7/A0-A7/SR",
                "metadata": "CLSM v1: count, then total_cck/instruction_cck/bus_wait_cck/irq_level/vector as little-endian u32",
                "unwind_base": self.opts.unwind.as_ref().map(samples::CompactUnwindTable::base),
                "unwind_text_size": self.opts.unwind.as_ref().map(samples::CompactUnwindTable::text_size),
                "relocation_bases": self.opts.relocation_bases,
                "code_ranges": self.opts.code_ranges.iter().map(|(base, size)| json!({"base": base, "size": size})).collect::<Vec<_>>(),
            })),
            "triggered_at": self.triggered_at,
            "resources": resources,
            "systemStackLower": self.stack_bounds.map(|bounds| bounds.system_lower),
            "systemStackUpper": self.stack_bounds.map(|bounds| bounds.system_upper),
            "stackLower": self.stack_bounds.map(|bounds| bounds.task_lower),
            "stackUpper": self.stack_bounds.map(|bounds| bounds.task_upper),
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

pub struct SampleFrameStats {
    pub samples_name: String,
    pub metadata_name: String,
    pub count: u32,
    pub irq_cck: u64,
    pub samples_total: u64,
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
            memory: false,
            screenshots: ScreenshotMode::None,
            pc_samples: false,
            samples: false,
            registers: false,
            unwind: None,
            relocation_bases: Vec::new(),
            code_ranges: Vec::new(),
            trigger: None,
        };
        let mut capture = ProfileCapture::create(opts, 100, 2.0, 1000, true, false, 2).unwrap();
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

    #[test]
    fn deferred_triggers_arm_without_recording_early_frames() {
        let dir =
            std::env::temp_dir().join(format!("copperline-profile-trigger-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let opts = ProfileOptions {
            path: dir.clone(),
            frames: 2,
            slots: false,
            memory: false,
            screenshots: ScreenshotMode::None,
            pc_samples: false,
            samples: false,
            registers: false,
            unwind: None,
            relocation_bases: Vec::new(),
            code_ranges: Vec::new(),
            trigger: Some(ProfileTrigger::BusyCckOver(100)),
        };
        let mut capture = ProfileCapture::create(opts, 5, 0.1, 10, false, false, 2).unwrap();
        assert!(!capture.should_record(6, 100, 20));
        assert!(capture.should_record(7, 101, 30));
        assert_eq!(capture.take_retired_delta(30), 10);
        let status = capture.status_value(true);
        assert_eq!(status["triggered"], true);
        assert_eq!(status["triggered_at"], 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_frame_samples_use_monotonic_sidecar_names() {
        let dir =
            std::env::temp_dir().join(format!("copperline-profile-repeat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let opts = ProfileOptions {
            path: dir.clone(),
            frames: 2,
            slots: false,
            memory: false,
            screenshots: ScreenshotMode::None,
            pc_samples: false,
            samples: true,
            registers: false,
            unwind: None,
            relocation_bases: Vec::new(),
            code_ranges: Vec::new(),
            trigger: None,
        };
        let mut capture = ProfileCapture::create(opts, 10, 0.1, 0, false, false, 2).unwrap();
        let first = capture.write_samples(11, &[]).unwrap();
        capture.note_reposition(10, 0).unwrap();
        let second = capture.write_samples(11, &[]).unwrap();
        assert_ne!(first.samples_name, second.samples_name);
        assert_ne!(first.metadata_name, second.metadata_name);
        assert!(dir.join(first.samples_name).is_file());
        assert!(dir.join(second.samples_name).is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_slot_sidecar_uses_the_documented_little_endian_record() {
        let dir =
            std::env::temp_dir().join(format!("copperline-profile-slots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let opts = ProfileOptions {
            path: dir.clone(),
            frames: 1,
            slots: true,
            memory: false,
            screenshots: ScreenshotMode::None,
            pc_samples: false,
            samples: false,
            registers: false,
            unwind: None,
            relocation_bases: Vec::new(),
            code_ranges: Vec::new(),
            trigger: None,
        };
        let capture = ProfileCapture::create(opts, 0, 0.0, 0, true, true, 2).unwrap();
        let record = crate::bus::BusSlotRecord {
            reg: 0x0100,
            kind: crate::bus::BUS_RECORD_CPU,
            subtype: 1,
            size: 4,
            ipl: 3,
            flags: 1,
            addr: 0x0012_3456,
            data: 0x89AB_CDEF,
            events: crate::bus::BUS_EVENT_CPU_IRQ,
        };
        let name = capture.write_slots(42, &[record]).unwrap();
        assert_eq!(name, "slots-000042.bin");
        let bytes = std::fs::read(dir.join(name)).unwrap();
        assert_eq!(bytes.len(), crate::bus::BusSlotRecord::BYTE_SIZE);
        let mut expected = Vec::new();
        record.write_to(&mut expected).unwrap();
        assert_eq!(bytes, expected);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
