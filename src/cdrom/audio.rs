// SPDX-License-Identifier: GPL-3.0-or-later

//! Audio-file tracks of a cue sheet (`FILE ... WAVE`, `FILE ... MP3`)
//! served as CD-DA sectors.
//!
//! A packaged disc keeps each audio track as a WAV or MP3 file instead
//! of raw 2352-byte frames in the BIN. This module presents such a file
//! as a run of CD-DA sectors -- 588 stereo 16-bit little-endian sample
//! frames per sector at 44.1 kHz, the last sector zero-padded -- so the
//! layout and sector-read code in the parent module treats the file
//! exactly like a BINARY one. A source at another sample rate (a 48 kHz
//! WAV, an MPEG-2 low-rate MP3) is resampled to 44.1 kHz by linear
//! interpolation in integer arithmetic, so a sector reads as the same
//! bytes on every host; a mono source plays on both channels.
//!
//! The file is decoded on demand, sector by sector, not up front: a WAV
//! is random access (`wav.rs`), and an MP3 keeps a decode cursor that
//! follows sequential playback and re-seeks, with a warm-up, on a jump
//! (`mp3.rs`). Loading a disc with an hour of MP3 audio therefore costs
//! a header scan rather than a full decode, and the emulator never holds
//! a disc's worth of PCM in memory.

use super::{wav::WavPcm, RAW_SECTOR_BYTES};
use anyhow::{bail, Context, Result};
use std::path::Path;

/// CD-DA sample rate.
pub(super) const CDDA_RATE: u32 = 44_100;
/// Stereo sample frames per CD-DA sector (2352 bytes / 4).
pub(super) const FRAMES_PER_SECTOR: usize = RAW_SECTOR_BYTES / 4;

/// How a cue sheet `FILE` stores its tracks: the type word on the `FILE`
/// line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FileFormat {
    /// Raw sector bytes (`BINARY`).
    Binary,
    /// PCM audio in a RIFF WAVE file (`WAVE`).
    Wave,
    /// An MPEG-1/2/2.5 Layer III stream (`MP3`).
    Mp3,
}

impl FileFormat {
    /// The format a cue sheet `FILE` type word names, if it is one
    /// Copperline reads. `MOTOROLA` (big-endian raw) and `AIFF` are
    /// recognised cue types but not supported, so they map to `None`
    /// like any other word.
    pub fn from_cue_type(word: &str) -> Option<Self> {
        if word.eq_ignore_ascii_case("BINARY") {
            Some(FileFormat::Binary)
        } else if word.eq_ignore_ascii_case("WAVE") {
            Some(FileFormat::Wave)
        } else if word.eq_ignore_ascii_case("MP3") {
            Some(FileFormat::Mp3)
        } else {
            None
        }
    }

    /// Whether `word` is a cue sheet file type at all (supported or not).
    pub fn is_cue_type(word: &str) -> bool {
        Self::from_cue_type(word).is_some()
            || word.eq_ignore_ascii_case("MOTOROLA")
            || word.eq_ignore_ascii_case("AIFF")
    }

    pub fn cue_type(self) -> &'static str {
        match self {
            FileFormat::Binary => "BINARY",
            FileFormat::Wave => "WAVE",
            FileFormat::Mp3 => "MP3",
        }
    }
}

/// A decoded-audio source: stereo 16-bit sample frames at the source's
/// own rate, filled by range.
enum Pcm {
    Wav(WavPcm),
    #[cfg(feature = "cd-mp3")]
    Mp3(Box<super::mp3::Mp3Pcm>),
}

impl Pcm {
    /// Decode source frames `[first, first + out.len())` into `out`. The
    /// caller keeps the range inside the source.
    fn fill(&mut self, first: u64, out: &mut [(i16, i16)]) -> Result<()> {
        match self {
            Pcm::Wav(wav) => wav.fill(first, out),
            #[cfg(feature = "cd-mp3")]
            Pcm::Mp3(mp3) => mp3.fill(first, out),
        }
    }
}

/// One audio file of a cue sheet presented as CD-DA sectors.
pub(super) struct AudioSource {
    pcm: Pcm,
    /// Source sample rate in Hz.
    rate: u32,
    /// Stereo sample frames in the source.
    src_frames: u64,
    /// CD-DA sample frames the source yields after rate conversion.
    out_frames: u64,
    /// Whole CD-DA sectors, the last one zero-padded.
    sectors: u32,
    /// Scratch for the source frames one sector needs.
    src_buf: Vec<(i16, i16)>,
}

impl std::fmt::Debug for AudioSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioSource")
            .field("rate", &self.rate)
            .field("src_frames", &self.src_frames)
            .field("out_frames", &self.out_frames)
            .field("sectors", &self.sectors)
            .finish_non_exhaustive()
    }
}

impl AudioSource {
    /// Open an audio file and size it in CD-DA sectors. Decodes nothing
    /// beyond the headers.
    pub(super) fn open(path: &Path, format: FileFormat) -> Result<Self> {
        let (pcm, rate, src_frames) = match format {
            FileFormat::Binary => bail!("{}: BINARY is not an audio format", path.display()),
            FileFormat::Wave => {
                let wav = WavPcm::open(path)?;
                let (rate, frames) = (wav.rate(), wav.frames());
                (Pcm::Wav(wav), rate, frames)
            }
            #[cfg(feature = "cd-mp3")]
            FileFormat::Mp3 => {
                let mp3 = super::mp3::Mp3Pcm::open(path)?;
                let (rate, frames) = (mp3.rate(), mp3.frames());
                (Pcm::Mp3(Box::new(mp3)), rate, frames)
            }
            #[cfg(not(feature = "cd-mp3"))]
            FileFormat::Mp3 => bail!(
                "{}: MP3 audio tracks need a Copperline built with the `cd-mp3` feature",
                path.display()
            ),
        };
        let out_frames = if rate == CDDA_RATE {
            src_frames
        } else {
            src_frames * u64::from(CDDA_RATE) / u64::from(rate)
        };
        if out_frames == 0 {
            bail!("{}: holds no audio", path.display());
        }
        let sectors = u32::try_from(out_frames.div_ceil(FRAMES_PER_SECTOR as u64))
            .with_context(|| format!("{}: too long for a CD", path.display()))?;
        Ok(Self {
            pcm,
            rate,
            src_frames,
            out_frames,
            sectors,
            src_buf: Vec::new(),
        })
    }

    /// The file's length in CD-DA bytes: what the cue layout sees.
    pub(super) fn byte_len(&self) -> u64 {
        u64::from(self.sectors) * RAW_SECTOR_BYTES as u64
    }

    /// Read CD-DA sector `sector` of the file into `buf` (2352 bytes in
    /// disc byte order). Sectors past the audio read as silence.
    pub(super) fn read_sector(&mut self, sector: u32, buf: &mut [u8]) -> Result<()> {
        debug_assert_eq!(buf.len(), RAW_SECTOR_BYTES);
        let out0 = u64::from(sector) * FRAMES_PER_SECTOR as u64;
        if self.rate == CDDA_RATE {
            let have = self
                .src_frames
                .saturating_sub(out0)
                .min(FRAMES_PER_SECTOR as u64) as usize;
            self.src_buf.clear();
            self.src_buf.resize(FRAMES_PER_SECTOR, (0, 0));
            if have > 0 {
                self.pcm.fill(out0, &mut self.src_buf[..have])?;
            }
            for (&(l, r), bytes) in self.src_buf.iter().zip(buf.chunks_exact_mut(4)) {
                bytes[..2].copy_from_slice(&l.to_le_bytes());
                bytes[2..].copy_from_slice(&r.to_le_bytes());
            }
        } else {
            // The source frames the sector's output frames fall on, plus
            // the right-hand neighbour each interpolation reaches for.
            let rate = u64::from(self.rate);
            let cdda = u64::from(CDDA_RATE);
            let src0 = out0 * rate / cdda;
            let src_end = (out0 + FRAMES_PER_SECTOR as u64) * rate / cdda + 2;
            let span = (src_end - src0) as usize;
            let have = self.src_frames.saturating_sub(src0).min(span as u64) as usize;
            self.src_buf.clear();
            self.src_buf.resize(span, (0, 0));
            if have > 0 {
                self.pcm.fill(src0, &mut self.src_buf[..have])?;
            }
            for (f, bytes) in buf.chunks_exact_mut(4).enumerate() {
                let frame = out0 + f as u64;
                if frame >= self.out_frames {
                    // Padding past the declared length: silence, not the
                    // last real sample fading into the zero fill.
                    bytes.fill(0);
                    continue;
                }
                let pos = frame * rate;
                let i = (pos / cdda - src0) as usize;
                let frac = (pos % cdda) as i64;
                let (a, b) = (self.src_buf[i], self.src_buf[i + 1]);
                let l = lerp(a.0, b.0, frac, cdda as i64);
                let r = lerp(a.1, b.1, frac, cdda as i64);
                bytes[..2].copy_from_slice(&l.to_le_bytes());
                bytes[2..].copy_from_slice(&r.to_le_bytes());
            }
        }
        Ok(())
    }
}

/// `a + (b - a) * num / den` in integer arithmetic: exact and the same on
/// every host, which a float resampler would not guarantee.
fn lerp(a: i16, b: i16, num: i64, den: i64) -> i16 {
    (i64::from(a) + (i64::from(b) - i64::from(a)) * num / den) as i16
}
