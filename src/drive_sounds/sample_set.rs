// SPDX-License-Identifier: GPL-3.0-or-later

//! WinUAE-format floppy drive sample sets: the sound packs WinUAE and
//! FS-UAE play in place of their built-in drive noises.
//!
//! A set is up to five WAV files sharing a name, `drive_<kind>_<NAME>.wav`
//! (the `.wav` is optional, and names match case-insensitively so a set
//! copied off a Windows disk works on a case-sensitive host):
//!
//! - `click`: the head step. The file may hold several clicks back to
//!   back. Each one starts at a peak above 0x6FF0 (16-bit full scale), and
//!   the n-th is the click for a step onto cylinder n, the last standing in
//!   for every cylinder past the count. A file with no such peak is one
//!   click for every cylinder.
//! - `spin` / `spinnd`: the platter turning with / without a disk, looped
//!   while the motor runs.
//! - `startup`: the motor winding up from a standstill.
//! - `snatch`: the hub grabbing a freshly inserted disk, played instead of
//!   `startup` on the first spin-up after an insert.
//!
//! Any of the five may be missing, but a set needs at least one. Copperline
//! ships none: a set is the user's own files, looked up by name in
//! [`crate::paths::floppy_sounds_dir`] or named by path.
//!
//! The slicing constants (the 0x6FF0 marker level, the 128-sample lead-in,
//! the 2800-sample click and the 3000-sample marker hold-off) are WinUAE's,
//! in the file's own sample rate, and so is playing the first half of each
//! slice: sets are authored against that player, so a set sounds here as it
//! does there. Everything is converted to mono at the mixer rate when the set
//! loads, so playback is a table lookup.

use anyhow::{anyhow, bail, Context, Result};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use crate::audio::MIX_SAMPLE_RATE;

/// Cylinders with a click of their own: WinUAE's `CLICK_TRACKS`.
pub const CLICK_SLOTS: usize = 84;

/// A click starts where a sample first rises above this level.
const CLICK_MARKER_LEVEL: f32 = 0x6FF0 as f32 / 32768.0;
/// How far before its marker peak a click starts, in source samples.
const CLICK_LEAD_IN: usize = 128;
/// A click's length when the file holds more than one, in source samples.
const CLICK_LENGTH: usize = 2800;
/// After a marker, the scan for the next one resumes this far on.
const CLICK_HOLD_OFF: usize = 3000;

/// A WAV larger than this is not a drive sound.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Nor is one longer than this. The file-size cap alone does not bound the
/// resampled output, which grows as the declared rate shrinks: a small file
/// claiming 1 Hz would resample to trillions of samples. With the rate
/// range below, this caps each converted member at a minute of mixer-rate
/// samples (about 10 MiB).
const MAX_SECONDS: u64 = 60;
/// Sample rates a drive sound may declare. Anything outside is a broken or
/// hostile header, not a recording.
const SAMPLE_RATES: std::ops::RangeInclusive<u32> = 1_000..=192_000;

/// The five files of a set, in the order WinUAE loads them.
const KINDS: [&str; 5] = ["click", "spin", "spinnd", "startup", "snatch"];

/// How a motor start is voiced before the spin loop takes over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LeadIn {
    /// Straight into the spin loop: the platter was still turning.
    #[default]
    None,
    /// The motor winding up from a standstill.
    Startup,
    /// The hub grabbing a disk inserted since the last spin-up.
    Snatch,
}

/// A loaded set, mono at [`MIX_SAMPLE_RATE`], normalised to [-1, 1].
pub struct SampleSet {
    name: String,
    click: Vec<f32>,
    /// Per cylinder, the `(start, length)` of its click within `click`.
    click_slices: [(usize, usize); CLICK_SLOTS],
    spin: Vec<f32>,
    spin_empty: Vec<f32>,
    startup: Vec<f32>,
    snatch: Vec<f32>,
}

impl std::fmt::Debug for SampleSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SampleSet")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl SampleSet {
    /// Load the set a configuration names: a bare name is looked up in the
    /// floppy sounds folder, anything else is a path (see [`locate`]).
    pub fn open(value: &str) -> Result<Self> {
        let (dir, name) = locate(value)?;
        Self::load(&dir, &name)
    }

    /// Load set `name` from `dir`.
    pub fn load(dir: &Path, name: &str) -> Result<Self> {
        let entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("floppy sound set folder {}", dir.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();
        let mut pcm: [Vec<f32>; 5] = Default::default();
        let mut click_slices = [(0, 0); CLICK_SLOTS];
        let mut found = 0;
        for (slot, kind) in KINDS.iter().enumerate() {
            let Some(path) = find_member(&entries, kind, name) else {
                continue;
            };
            let (samples, rate) = read_wav_mono(&path)?;
            if slot == 0 {
                let ratio = f64::from(MIX_SAMPLE_RATE) / f64::from(rate);
                let at_mix_rate = |n: usize| (n as f64 * ratio).round() as usize;
                for (dst, (start, len)) in click_slices.iter_mut().zip(click_slices_of(&samples)) {
                    *dst = (at_mix_rate(start), at_mix_rate(len));
                }
            }
            pcm[slot] = resample_to_mix_rate(&samples, rate);
            found += 1;
        }
        if found == 0 {
            bail!(
                "no floppy sound set {name:?} in {}: expected files named drive_click_{name}.wav, \
                 drive_spin_{name}.wav, ...",
                dir.display()
            );
        }
        let [click, spin, spin_empty, startup, snatch] = pcm;
        // Clamp each slice to the converted buffer: rounding, or a marker in
        // the last few samples, can run a slice past the end.
        for (start, len) in &mut click_slices {
            *start = (*start).min(click.len());
            *len = (*len).min(click.len() - *start);
        }
        Ok(Self {
            name: name.to_string(),
            click,
            click_slices,
            spin,
            spin_empty,
            startup,
            snatch,
        })
    }

    /// A set from samples already at the mixer rate, for tests that drive
    /// the player without files.
    #[cfg(test)]
    pub(crate) fn from_samples(
        click: Vec<f32>,
        spin: Vec<f32>,
        spin_empty: Vec<f32>,
        startup: Vec<f32>,
        snatch: Vec<f32>,
    ) -> Self {
        Self {
            name: "test".to_string(),
            click_slices: click_slices_of(&click),
            click,
            spin,
            spin_empty,
            startup,
            snatch,
        }
    }

    /// The set's name: the `NAME` in its file names.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sample `age` of the click for a step onto `cylinder`, or `None` once
    /// that click has finished (or the set has none).
    pub(super) fn click_sample(&self, cylinder: u8, age: u32) -> Option<f32> {
        let (start, len) = self.click_slices[usize::from(cylinder).min(CLICK_SLOTS - 1)];
        let age = age as usize;
        (age < len).then(|| self.click[start + age])
    }

    /// Sample `age` of a running motor: the lead-in, then the spin loop for
    /// a drive with or without a disk. Zero when the set has neither.
    pub(super) fn motor_sample(&self, lead_in: LeadIn, disk: bool, age: u64) -> f32 {
        let lead: &[f32] = match lead_in {
            LeadIn::None => &[],
            LeadIn::Startup => &self.startup,
            LeadIn::Snatch => &self.snatch,
        };
        if let Some(&sample) = usize::try_from(age).ok().and_then(|age| lead.get(age)) {
            return sample;
        }
        let spin = if disk { &self.spin } else { &self.spin_empty };
        if spin.is_empty() {
            return 0.0;
        }
        let into_loop = age - lead.len() as u64;
        spin[(into_loop % spin.len() as u64) as usize]
    }
}

/// Split a configured set into the folder it lives in and its name.
///
/// A bare name (`A500`) is a set in [`crate::paths::floppy_sounds_dir`]. A
/// value with a path separator is a path: either to one of the set's own
/// files (`.../drive_click_A500.wav`, which is what a file dialog picks), or
/// to the folder joined with the name (`.../floppysounds/A500`).
pub fn locate(value: &str) -> Result<(PathBuf, String)> {
    let value = value.trim();
    if value.is_empty() {
        bail!("empty floppy sound set name");
    }
    if !value.contains(['/', '\\']) {
        let dir = crate::paths::floppy_sounds_dir()
            .ok_or_else(|| anyhow!("no host data folder to look for floppy sound sets in"))?;
        return Ok((dir, value.to_string()));
    }
    let path = Path::new(value);
    let file = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("floppy sound set path {value:?} names no set"))?;
    let name = set_name_of_member(file).unwrap_or_else(|| file.to_string());
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };
    Ok((dir, name))
}

/// The name a configured set goes by in a picker: its `NAME`.
pub fn display_name(value: &str) -> String {
    locate(value).map_or_else(|_| value.trim().to_string(), |(_, name)| name)
}

/// Every set in `dir`, sorted case-insensitively. A set is found by any of
/// its member files, not only its click: [`SampleSet::load`] takes any
/// non-empty combination, so a motor-only set is a set too. (WinUAE lists
/// sets by their click files alone.)
pub fn list_sets(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter_map(|file| set_name_of_member(&file))
        .collect();
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
}

/// The set name in a member file's name, `drive_<kind>_<NAME>[.wav]`.
fn set_name_of_member(file: &str) -> Option<String> {
    let lower = file.to_ascii_lowercase();
    // Longest kind first: "spinnd" also starts with "spin".
    let mut kinds = KINDS;
    kinds.sort_by_key(|kind| std::cmp::Reverse(kind.len()));
    let prefix_len = kinds.iter().find_map(|kind| {
        let prefix = format!("drive_{kind}_");
        lower.starts_with(&prefix).then_some(prefix.len())
    })?;
    let rest = &file[prefix_len..];
    let name = if rest.to_ascii_lowercase().ends_with(".wav") {
        &rest[..rest.len() - 4]
    } else {
        rest
    };
    (!name.is_empty()).then(|| name.to_string())
}

/// The file in `entries` that is member `kind` of set `name`. An exact
/// spelling wins over a case-insensitive one, and the bare name WinUAE
/// tries first wins over the one with `.wav`.
fn find_member(entries: &[PathBuf], kind: &str, name: &str) -> Option<PathBuf> {
    let bare = format!("drive_{kind}_{name}");
    let wav = format!("{bare}.wav");
    let file_name = |path: &PathBuf| path.file_name().and_then(|n| n.to_str()).map(str::to_owned);
    for want in [&bare, &wav] {
        if let Some(path) = entries
            .iter()
            .find(|path| path.is_file() && file_name(path).as_deref() == Some(want.as_str()))
        {
            return Some(path.clone());
        }
    }
    for want in [&bare, &wav] {
        if let Some(path) = entries.iter().find(|path| {
            path.is_file() && file_name(path).is_some_and(|file| file.eq_ignore_ascii_case(want))
        }) {
            return Some(path.clone());
        }
    }
    None
}

/// Read a WAV file as mono samples in [-1, 1] at its own rate. Any layout
/// `hound` reads is taken -- 8/16/24/32-bit integer or 32-bit float, any
/// channel count, which is averaged down to one.
fn read_wav_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("{}", path.display()))?
        .len();
    if size > MAX_FILE_BYTES {
        bail!(
            "{}: {size} bytes is too large for a drive sound",
            path.display()
        );
    }
    let bytes = std::fs::read(path).with_context(|| format!("{}", path.display()))?;
    decode_wav_mono(&bytes).with_context(|| format!("{}", path.display()))
}

fn decode_wav_mono(bytes: &[u8]) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::new(Cursor::new(bytes))
        .map_err(|e| anyhow!("not a readable WAVE file: {e}"))?;
    let spec = reader.spec();
    if spec.channels == 0 || !SAMPLE_RATES.contains(&spec.sample_rate) {
        bail!(
            "WAVE header claims {} channels at {} Hz (a drive sound needs at least one \
             channel at {}-{} Hz)",
            spec.channels,
            spec.sample_rate,
            SAMPLE_RATES.start(),
            SAMPLE_RATES.end()
        );
    }
    // Checked from the header, before anything is decoded or resampled.
    let frames = u64::from(reader.duration());
    if frames > u64::from(spec.sample_rate) * MAX_SECONDS {
        bail!(
            "{} s long; a drive sound may be at most {MAX_SECONDS} s",
            frames / u64::from(spec.sample_rate)
        );
    }
    let interleaved: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .map(|v| v.map(|v| v.clamp(-1.0, 1.0)))
            .collect::<hound::Result<_>>()?,
        (hound::SampleFormat::Int, bits @ (8 | 16 | 24 | 32)) => {
            let full_scale = (1u64 << (bits - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|v| v.map(|v| v as f32 / full_scale))
                .collect::<hound::Result<_>>()?
        }
        (format, bits) => bail!(
            "{bits}-bit {format:?} WAVE samples are not supported \
             (8/16/24/32-bit PCM or 32-bit float)"
        ),
    };
    let channels = usize::from(spec.channels);
    let mono = interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}

/// WinUAE's click slicing (`processclicks`), in source samples: per
/// cylinder, the `(start, length)` actually played -- the first half of the
/// slice WinUAE cuts, which is what its player sounds.
fn click_slices_of(click: &[f32]) -> [(usize, usize); CLICK_SLOTS] {
    let mut starts = Vec::new();
    let mut n = 0;
    while n < click.len() {
        if click[n] > CLICK_MARKER_LEVEL && starts.len() < CLICK_SLOTS {
            starts.push(n.saturating_sub(CLICK_LEAD_IN));
            n += CLICK_HOLD_OFF;
        }
        n += 1;
    }
    let mut slices = [(0, click.len()); CLICK_SLOTS];
    match starts.as_slice() {
        [] => {}
        // One click: it runs to the end of the file.
        &[start] => slices = [(start, click.len() - start); CLICK_SLOTS],
        _ => {
            for (slot, &start) in starts.iter().enumerate() {
                slices[slot] = (start, CLICK_LENGTH);
            }
            let last = slices[starts.len() - 1];
            for slice in &mut slices[starts.len()..] {
                *slice = last;
            }
        }
    }
    slices.map(|(start, len)| (start, len / 2))
}

/// Linear-interpolation resample to the mixer rate. Drive sounds are
/// recorded at or near it, so nothing finer is called for.
fn resample_to_mix_rate(samples: &[f32], rate: u32) -> Vec<f32> {
    if rate == MIX_SAMPLE_RATE || samples.is_empty() {
        return samples.to_vec();
    }
    let out_len = (samples.len() as u64 * u64::from(MIX_SAMPLE_RATE) / u64::from(rate)) as usize;
    let step = f64::from(rate) / f64::from(MIX_SAMPLE_RATE);
    let last = samples.len() - 1;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * step;
            let idx = (pos as usize).min(last);
            let frac = (pos - idx as f64) as f32;
            let next = samples[(idx + 1).min(last)];
            samples[idx] + (next - samples[idx]) * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A fresh, empty folder under the system temp dir.
    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "copperline-sample-set-{}-{}",
            std::process::id(),
            DIR_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[i16]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &s in samples {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    }

    /// A click file with `count` marked clicks 4000 samples apart, each a
    /// full-scale spike followed by a short tail at a level unique to it.
    fn multi_click(count: usize) -> Vec<i16> {
        let mut out = vec![0i16; 200 + count * 4000];
        for click in 0..count {
            let at = 200 + click * 4000;
            out[at] = 0x7FFF;
            for s in &mut out[at + 1..at + 50] {
                *s = (click as i16 + 1) * 100;
            }
        }
        out
    }

    #[test]
    fn a_marked_click_file_gives_each_cylinder_its_own_click() {
        let samples: Vec<f32> = multi_click(3)
            .iter()
            .map(|&s| f32::from(s) / 32768.0)
            .collect();
        let slices = click_slices_of(&samples);
        // Each click starts 128 samples ahead of its marker and plays half
        // of WinUAE's 2800-sample slice.
        assert_eq!(slices[0], (200 - 128, 1400));
        assert_eq!(slices[1], (4200 - 128, 1400));
        assert_eq!(slices[2], (8200 - 128, 1400));
        // Cylinders past the last click reuse it.
        assert_eq!(slices[3], slices[2]);
        assert_eq!(slices[CLICK_SLOTS - 1], slices[2]);
    }

    #[test]
    fn an_unmarked_click_file_is_one_click_for_every_cylinder() {
        let samples = vec![0.25f32; 1000];
        let slices = click_slices_of(&samples);
        assert!(slices.iter().all(|&slice| slice == (0, 500)));
        // A single marker: from just before it to the end of the file.
        let mut one = vec![0.0f32; 1000];
        one[300] = 0.99;
        let slices = click_slices_of(&one);
        assert!(slices.iter().all(|&slice| slice == (172, (1000 - 172) / 2)));
    }

    #[test]
    fn a_marker_in_the_lead_in_starts_the_click_at_the_top() {
        let mut samples = vec![0.0f32; 8000];
        samples[10] = 0.99;
        samples[5000] = 0.99;
        let slices = click_slices_of(&samples);
        assert_eq!(slices[0].0, 0);
        assert_eq!(slices[1].0, 5000 - 128);
    }

    #[test]
    fn set_loads_by_name_case_insensitively_and_tolerates_missing_members() -> Result<()> {
        let dir = temp_dir();
        write_wav(
            &dir.join("DRIVE_CLICK_Test.WAV"),
            MIX_SAMPLE_RATE,
            1,
            &multi_click(2),
        );
        write_wav(
            &dir.join("drive_spin_Test.wav"),
            MIX_SAMPLE_RATE,
            1,
            &[1000; 64],
        );
        // An unrelated set beside it is not picked up.
        write_wav(
            &dir.join("drive_spin_Other.wav"),
            MIX_SAMPLE_RATE,
            1,
            &[-1; 8],
        );
        let set = SampleSet::load(&dir, "Test")?;
        assert_eq!(set.name(), "Test");
        // Cylinder 1's click starts 128 samples ahead of its own marker.
        assert_eq!(
            set.click_sample(1, 128),
            Some(f32::from(0x7FFFi16) / 32768.0)
        );
        assert_eq!(set.click_sample(1, 129), Some(200.0 / 32768.0));
        assert_eq!(set.click_sample(1, 1400), None);
        // The spin loop repeats; with no spinnd file an empty drive's
        // motor is silent, and with no startup the loop starts at once.
        assert_eq!(set.motor_sample(LeadIn::Startup, true, 0), 1000.0 / 32768.0);
        assert_eq!(
            set.motor_sample(LeadIn::None, true, 64 * 3 + 5),
            1000.0 / 32768.0
        );
        assert_eq!(set.motor_sample(LeadIn::None, false, 0), 0.0);
        assert_eq!(list_sets(&dir), ["Other", "Test"]);
        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn a_motor_only_set_is_listed() {
        let dir = temp_dir();
        write_wav(
            &dir.join("drive_spin_Quiet.wav"),
            MIX_SAMPLE_RATE,
            1,
            &[0; 8],
        );
        write_wav(
            &dir.join("drive_snatch_quiet.wav"),
            MIX_SAMPLE_RATE,
            1,
            &[0; 8],
        );
        write_wav(
            &dir.join("drive_click_A500.wav"),
            MIX_SAMPLE_RATE,
            1,
            &[0; 8],
        );
        std::fs::write(dir.join("readme.txt"), "not a set").unwrap();
        assert_eq!(list_sets(&dir), ["A500", "Quiet"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn implausible_rates_and_lengths_are_refused_before_decoding() {
        let dir = temp_dir();
        // A tiny file claiming 1 Hz would otherwise resample to an
        // allocation of tens of millions of samples per input sample.
        write_wav(&dir.join("drive_spin_Low.wav"), 1, 1, &[0; 64]);
        let err = SampleSet::load(&dir, "Low").expect_err("1 Hz");
        assert!(format!("{err:#}").contains("1 Hz"), "{err:#}");
        // In range, but 61 s long at 1 kHz.
        write_wav(&dir.join("drive_spin_Long.wav"), 1_000, 1, &[0; 61_000]);
        let err = SampleSet::load(&dir, "Long").expect_err("too long");
        assert!(format!("{err:#}").contains("61 s"), "{err:#}");
        // A minute exactly is fine.
        write_wav(&dir.join("drive_spin_Minute.wav"), 1_000, 1, &[0; 60_000]);
        assert!(SampleSet::load(&dir, "Minute").is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_set_with_no_files_is_an_error() {
        let dir = temp_dir();
        let err = SampleSet::load(&dir, "Missing").expect_err("no files");
        assert!(err.to_string().contains("drive_click_Missing"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lead_in_plays_before_the_spin_loop() -> Result<()> {
        let dir = temp_dir();
        write_wav(
            &dir.join("drive_startup_S.wav"),
            MIX_SAMPLE_RATE,
            1,
            &[3000; 10],
        );
        write_wav(
            &dir.join("drive_snatch_S.wav"),
            MIX_SAMPLE_RATE,
            1,
            &[5000; 20],
        );
        write_wav(
            &dir.join("drive_spinnd_S.wav"),
            MIX_SAMPLE_RATE,
            1,
            &[700; 4],
        );
        let set = SampleSet::load(&dir, "S")?;
        let level = |v: i16| f32::from(v) / 32768.0;
        assert_eq!(set.motor_sample(LeadIn::Startup, false, 9), level(3000));
        assert_eq!(set.motor_sample(LeadIn::Startup, false, 10), level(700));
        assert_eq!(set.motor_sample(LeadIn::Snatch, false, 19), level(5000));
        assert_eq!(set.motor_sample(LeadIn::Snatch, false, 20), level(700));
        // No click file: no click at all.
        assert_eq!(set.click_sample(0, 0), None);
        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn stereo_and_other_rates_load_as_mono_at_the_mixer_rate() -> Result<()> {
        let dir = temp_dir();
        // 22.05 kHz stereo, left and right differing: one second of it.
        let frames = 22_050;
        let stereo: Vec<i16> = (0..frames).flat_map(|_| [2000i16, 4000]).collect();
        write_wav(&dir.join("drive_spin_R.wav"), 22_050, 2, &stereo);
        let set = SampleSet::load(&dir, "R")?;
        assert_eq!(set.spin.len(), MIX_SAMPLE_RATE as usize);
        assert!(set
            .spin
            .iter()
            .all(|&s| (s - 3000.0 / 32768.0).abs() < 1e-6));
        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn locate_takes_a_name_a_member_file_or_a_folder_and_name() -> Result<()> {
        let (dir, name) = locate("/sets/floppysounds/drive_click_A500Real.wav")?;
        assert_eq!(dir, PathBuf::from("/sets/floppysounds"));
        assert_eq!(name, "A500Real");
        let (dir, name) = locate("/sets/floppysounds/drive_spinnd_LOUD")?;
        assert_eq!(dir, PathBuf::from("/sets/floppysounds"));
        assert_eq!(name, "LOUD");
        let (dir, name) = locate("sets/ST")?;
        assert_eq!(dir, PathBuf::from("sets"));
        assert_eq!(name, "ST");
        assert_eq!(display_name("/x/drive_snatch_A500.wav"), "A500");
        if let Some(home) = crate::paths::floppy_sounds_dir() {
            assert_eq!(locate(" A500 ")?, (home, "A500".to_string()));
        }
        assert!(locate("  ").is_err());
        Ok(())
    }

    #[test]
    fn member_names_parse_the_longest_kind_first() {
        assert_eq!(
            set_name_of_member("drive_spinnd_A500.wav").as_deref(),
            Some("A500")
        );
        assert_eq!(
            set_name_of_member("drive_spin_A500.wav").as_deref(),
            Some("A500")
        );
        assert_eq!(set_name_of_member("DRIVE_CLICK_x").as_deref(), Some("x"));
        assert_eq!(set_name_of_member("drive_click_.wav"), None);
        assert_eq!(set_name_of_member("readme.txt"), None);
    }
}
