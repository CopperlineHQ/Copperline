// SPDX-License-Identifier: GPL-3.0-or-later

//! `FILE ... WAVE` tracks: PCM read straight out of a RIFF WAVE file.
//!
//! Any PCM layout `hound` reads is accepted -- 8/16/24/32-bit integer or
//! 32-bit float samples, any channel count (the first two play; a mono
//! file plays on both), any sample rate (`audio.rs` resamples) -- and
//! converted to the CD-DA 16-bit range on the way out. The file is never
//! read ahead of the sector being served: a WAV is random access, so
//! the reader just seeks when playback jumps.

use anyhow::{anyhow, bail, Context, Result};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

pub(super) struct WavPcm {
    reader: hound::WavReader<BufReader<File>>,
    path: PathBuf,
    channels: usize,
    bits: u16,
    float: bool,
    rate: u32,
    /// Sample frames (per channel) in the file.
    frames: u64,
    /// The frame the reader is positioned at; `u64::MAX` forces a seek.
    next: u64,
}

impl std::fmt::Debug for WavPcm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WavPcm")
            .field("path", &self.path)
            .field("rate", &self.rate)
            .field("frames", &self.frames)
            .finish_non_exhaustive()
    }
}

impl WavPcm {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let reader = hound::WavReader::open(path)
            .map_err(|e| anyhow!("{}: not a readable WAVE file: {e}", path.display()))?;
        let spec = reader.spec();
        match (spec.sample_format, spec.bits_per_sample) {
            (hound::SampleFormat::Int, 8 | 16 | 24 | 32) | (hound::SampleFormat::Float, 32) => {}
            (format, bits) => bail!(
                "{}: {bits}-bit {format:?} WAVE samples are not supported \
                 (8/16/24/32-bit PCM or 32-bit float)",
                path.display()
            ),
        }
        if spec.channels == 0 || spec.sample_rate == 0 {
            bail!(
                "{}: WAVE header claims {} channels at {} Hz",
                path.display(),
                spec.channels,
                spec.sample_rate
            );
        }
        Ok(Self {
            frames: u64::from(reader.duration()),
            reader,
            path: path.to_path_buf(),
            channels: usize::from(spec.channels),
            bits: spec.bits_per_sample,
            float: spec.sample_format == hound::SampleFormat::Float,
            rate: spec.sample_rate,
            next: 0,
        })
    }

    pub(super) fn rate(&self) -> u32 {
        self.rate
    }

    pub(super) fn frames(&self) -> u64 {
        self.frames
    }

    /// Read sample frames `[first, first + out.len())` as 16-bit stereo.
    /// The caller keeps the range inside the file.
    pub(super) fn fill(&mut self, first: u64, out: &mut [(i16, i16)]) -> Result<()> {
        if first != self.next {
            // hound addresses frames as u32: a WAV file is under 4 GB.
            let frame = u32::try_from(first).with_context(|| {
                format!(
                    "{}: frame {first} is beyond a WAVE file",
                    self.path.display()
                )
            })?;
            self.reader
                .seek(frame)
                .with_context(|| format!("{}: seeking to frame {first}", self.path.display()))?;
        }
        // Until the read completes the reader's position is unknown.
        self.next = u64::MAX;
        let channels = self.channels;
        if self.float {
            let samples = self.reader.samples::<f32>();
            read_frames(samples, channels, out, |v| {
                (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
            })
        } else {
            let bits = self.bits;
            let samples = self.reader.samples::<i32>();
            read_frames(samples, channels, out, |v| match bits {
                8 => (v << 8) as i16,
                16 => v as i16,
                24 => (v >> 8) as i16,
                _ => (v >> 16) as i16,
            })
        }
        .with_context(|| format!("{}: reading sample data", self.path.display()))?;
        self.next = first + out.len() as u64;
        Ok(())
    }
}

/// Pull `out.len()` frames of `channels` samples each from `samples`,
/// converting with `to_i16`; the first two channels are kept, a single
/// channel is duplicated.
fn read_frames<S, I>(
    mut samples: I,
    channels: usize,
    out: &mut [(i16, i16)],
    to_i16: impl Fn(S) -> i16,
) -> Result<()>
where
    I: Iterator<Item = hound::Result<S>>,
{
    for frame in out.iter_mut() {
        let mut lr = [0i16; 2];
        for c in 0..channels {
            let v = match samples.next() {
                Some(Ok(v)) => v,
                Some(Err(e)) => return Err(e.into()),
                None => bail!("sample data ends before the header says it does"),
            };
            if c < 2 {
                lr[c] = to_i16(v);
            }
        }
        *frame = if channels == 1 {
            (lr[0], lr[0])
        } else {
            (lr[0], lr[1])
        };
    }
    Ok(())
}
