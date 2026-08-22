// SPDX-License-Identifier: GPL-3.0-or-later

//! `FILE ... MP3` tracks: an MPEG Layer III stream decoded on demand.
//!
//! Loading indexes the file once without decoding it: ID3v2 tags are
//! skipped; every Layer III frame is located (a header is believed when
//! it sits exactly where the previous frame ended, or else only when the
//! frame it describes is followed by another header of the same stream
//! or by the end of the file, so a header-shaped word inside tag or junk
//! bytes is not taken for audio); a leading Xing/Info/VBRI frame
//! is dropped as the metadata it is; and a LAME tag's encoder delay and
//! padding are trimmed the way gapless players do, so the track is
//! sample-exact against the WAV it was encoded from. What remains is the
//! frame table plus the stream's fixed samples-per-frame, which map any
//! PCM position to a frame arithmetically.
//!
//! Reading keeps a cursor: sequential reads (CD audio playing through a
//! track) decode one frame after another, holding only the last decoded
//! frame. A jump re-seeks: a fresh decoder is warmed up on the frames
//! before the target (see `Mp3Pcm::seek`), enough of them to refill the
//! Layer III bit reservoir -- `main_data_begin` reaches back up to 511
//! bytes (255 for MPEG-2/2.5) of earlier frames' main data, which is a
//! couple of frames at 128 kbps but hundreds at the bottom of the MPEG-2
//! range, where a 24-byte frame carries a single main-data byte -- and
//! the overlap-add and synthesis-filter state (one frame deep), so the
//! samples a position decodes to do not depend on how the cursor got
//! there. That is what keeps a run resumed from a save state
//! byte-identical to one that played the track from its start;
//! `mp3_seek_decode_matches_linear_decode` in the parent module's tests
//! holds it to that, down to 8 kbps MPEG-2 streams.
//!
//! The decoder is Symphonia's pure-Rust `MpaDecoder`, the one the MHI
//! board uses (see `Cargo.toml` on why); it is packet-based, so the frame
//! table doubles as the packetizer.

use crate::audio::mpeg::{parse_frame_header, FrameHeader, HEADER_LEN};
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use symphonia_bundle_mp3::MpaDecoder;
use symphonia_core::audio::{Audio, GenericAudioBufferRef};
use symphonia_core::codecs::audio::well_known::CODEC_ID_MP3;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia_core::packet::PacketRef;
use symphonia_core::units::{Duration, Timestamp};

/// Frames decoded and discarded ahead of the reservoir refill a seek
/// needs (see `Mp3Pcm::seek`): margin against the warm-up's own
/// arithmetic, not a requirement of the format.
const WARMUP_MARGIN_FRAMES: usize = 1;

/// The Layer III decoder's own delay in samples, which a LAME tag's
/// encoder-delay field does not include (the convention gapless players
/// and Symphonia's demuxer apply).
const DECODER_DELAY: u32 = 529;

/// Where one audio frame lies in the file, and how much main data it
/// feeds the bit reservoir.
#[derive(Debug, Clone, Copy)]
struct FrameSpan {
    offset: u64,
    len: u16,
    main_data: u16,
}

/// Samples a LAME tag says to drop at each end of the decoded stream.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Trim {
    delay: u32,
    padding: u32,
}

pub(super) struct Mp3Pcm {
    file: File,
    path: PathBuf,
    frames: Vec<FrameSpan>,
    /// Samples per channel each frame decodes to, fixed for the stream.
    spf: u32,
    rate: u32,
    mono: bool,
    /// Bytes of earlier main data a frame can reach back for.
    reservoir: usize,
    /// Decoded samples dropped at the front: encoder plus decoder delay.
    delay: u64,
    /// Stereo sample frames the stream yields after trimming.
    out_frames: u64,
    decoder: MpaDecoder,
    /// Index of the frame the decoder expects next; `carry` holds the
    /// one before it.
    next: usize,
    /// The most recently decoded frame, and the absolute (untrimmed)
    /// sample index of `carry[0]`.
    carry: Vec<(i16, i16)>,
    carry_start: u64,
    read_buf: Vec<u8>,
}

impl std::fmt::Debug for Mp3Pcm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mp3Pcm")
            .field("path", &self.path)
            .field("frames", &self.frames.len())
            .field("rate", &self.rate)
            .field("mono", &self.mono)
            .field("out_frames", &self.out_frames)
            .finish_non_exhaustive()
    }
}

fn new_decoder() -> MpaDecoder {
    let mut params = AudioCodecParameters::new();
    params.for_codec(CODEC_ID_MP3);
    // Cannot fail: MP3 support is compiled into symphonia-bundle-mp3 by
    // this crate's dependency declaration, and `try_new` checks nothing
    // beyond the codec ID.
    MpaDecoder::try_new(&params, &AudioDecoderOptions::default())
        .expect("symphonia-bundle-mp3 is built with its mp3 feature")
}

impl Mp3Pcm {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let (frames, spec, trim) = index_frames(&data)
            .with_context(|| format!("{}: not an MP3 stream", path.display()))?;
        drop(data);
        let spf = spec.samples();
        let total = frames.len() as u64 * u64::from(spf);
        let out_frames = total.saturating_sub(u64::from(trim.delay) + u64::from(trim.padding));
        if out_frames == 0 {
            bail!("{}: decodes to no audio", path.display());
        }
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            frames,
            spf,
            rate: spec.rate,
            mono: spec.mono,
            reservoir: spec.reservoir_reach(),
            delay: u64::from(trim.delay),
            out_frames,
            decoder: new_decoder(),
            next: 0,
            carry: Vec::new(),
            carry_start: 0,
            read_buf: Vec::new(),
        })
    }

    pub(super) fn rate(&self) -> u32 {
        self.rate
    }

    pub(super) fn frames(&self) -> u64 {
        self.out_frames
    }

    /// Decode sample frames `[first, first + out.len())` of the trimmed
    /// stream into `out`. The caller keeps the range inside the stream.
    pub(super) fn fill(&mut self, first: u64, out: &mut [(i16, i16)]) -> Result<()> {
        let total = self.frames.len() as u64 * u64::from(self.spf);
        let mut pos = first + self.delay;
        let mut done = 0;
        while done < out.len() {
            if pos >= total {
                out[done..].fill((0, 0));
                break;
            }
            let carry_end = self.carry_start + self.carry.len() as u64;
            if pos >= self.carry_start && pos < carry_end {
                let i = (pos - self.carry_start) as usize;
                let n = (out.len() - done).min(self.carry.len() - i);
                out[done..done + n].copy_from_slice(&self.carry[i..i + n]);
                done += n;
                pos += n as u64;
            } else if pos == carry_end && self.next < self.frames.len() {
                // Sequential: the next frame continues the carry.
                self.decode_next()?;
            } else {
                self.seek(pos)?;
            }
        }
        Ok(())
    }

    /// Start over at the frame holding absolute sample `pos`, warmed up
    /// on the frames before it (see the module doc comment).
    ///
    /// The frame before the target supplies the overlap-add and
    /// synthesis state the target's output starts from, so it must
    /// itself decode from a full reservoir: the frames before *it* have
    /// to hold at least `reservoir` bytes of main data, the furthest its
    /// `main_data_begin` can reach. Symphonia appends every warm-up
    /// frame's main data whether or not it could decode the frame, so
    /// once that many bytes are in, the reservoir holds exactly what a
    /// linear decode's would.
    fn seek(&mut self, pos: u64) -> Result<()> {
        let target = (pos / u64::from(self.spf)) as usize;
        let mut start = target.saturating_sub(1);
        let mut bytes = 0usize;
        while start > 0 && bytes < self.reservoir {
            start -= 1;
            bytes += usize::from(self.frames[start].main_data);
        }
        let start = start.saturating_sub(WARMUP_MARGIN_FRAMES);
        self.decoder = new_decoder();
        self.next = start;
        for _ in start..=target {
            self.decode_next()?;
        }
        Ok(())
    }

    /// Decode frame `self.next` into `carry`. A frame the decoder rejects
    /// plays as silence, so the frame table and the PCM position never
    /// drift apart.
    fn decode_next(&mut self) -> Result<()> {
        let index = self.next;
        let span = self.frames[index];
        self.read_buf.resize(usize::from(span.len), 0);
        self.file.seek(SeekFrom::Start(span.offset))?;
        self.file
            .read_exact(&mut self.read_buf)
            .with_context(|| format!("{}: reading frame {index}", self.path.display()))?;
        self.carry_start = index as u64 * u64::from(self.spf);
        self.carry.clear();
        let packet = PacketRef::new(0, Timestamp::ZERO, Duration::ZERO, &self.read_buf);
        if let Ok(GenericAudioBufferRef::F32(buf)) = self.decoder.decode_ref(&packet) {
            if self.mono {
                if let Some(plane) = buf.plane(0) {
                    self.carry.extend(plane.iter().map(|&v| {
                        let s = to_i16(v);
                        (s, s)
                    }));
                }
            } else if let Some((l, r)) = buf.plane_pair(0, 1) {
                self.carry
                    .extend(l.iter().zip(r).map(|(&l, &r)| (to_i16(l), to_i16(r))));
            }
        }
        self.carry.resize(self.spf as usize, (0, 0));
        self.next = index + 1;
        Ok(())
    }
}

fn to_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

fn same_stream(a: &FrameHeader, b: &FrameHeader) -> bool {
    a.rate == b.rate && a.mpeg1 == b.mpeg1 && a.mono == b.mono
}

/// Locate every audio frame in `data` (see the module doc comment).
/// Returns the frame table, the stream's header parameters, and the
/// trim a leading LAME tag asks for.
fn index_frames(data: &[u8]) -> Result<(Vec<FrameSpan>, FrameHeader, Trim)> {
    let mut pos = skip_id3v2(data);
    let mut spans = Vec::new();
    let mut spec: Option<FrameHeader> = None;
    let mut trim = Trim::default();
    // Where the next frame starts while the scan is in sync with the
    // stream (the end of the last accepted frame).
    let mut expected: Option<usize> = None;
    while pos + HEADER_LEN <= data.len() {
        let Some(header) =
            parse_frame_header([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
        else {
            pos += 1;
            continue;
        };
        let end = pos + header.len;
        if end > data.len() {
            // A truncated trailing frame, or a fake sync word whose
            // declared length overshoots the file: not a frame.
            pos += 1;
            continue;
        }
        // In sync, the header is where the stream said it would be; out
        // of sync (the start, or after junk), it must be backed by the
        // next frame's header or the end of the file.
        if expected != Some(pos) && end + HEADER_LEN <= data.len() {
            let next = parse_frame_header([data[end], data[end + 1], data[end + 2], data[end + 3]]);
            if !next.is_some_and(|next| same_stream(&next, &header)) {
                pos += 1;
                continue;
            }
        }
        expected = Some(end);
        match spec {
            None => {
                spec = Some(header);
                if let Some(tag) = info_tag(&data[pos..end], &header) {
                    trim = tag;
                    pos = end;
                    continue;
                }
            }
            Some(first) if !same_stream(&header, &first) => bail!(
                "sample rate or channel layout changes at byte {pos}; \
                 a track must be one stream"
            ),
            Some(_) => {}
        }
        spans.push(FrameSpan {
            offset: pos as u64,
            len: header.len as u16,
            main_data: header.main_data_len() as u16,
        });
        pos = end;
    }
    match spec {
        Some(spec) if !spans.is_empty() => Ok((spans, spec, trim)),
        _ => bail!("no MPEG Layer III audio frames found"),
    }
}

/// Bytes of ID3v2 tag(s) at the front of `data`.
fn skip_id3v2(data: &[u8]) -> usize {
    let mut pos = 0;
    while data.len() >= pos + 10 && &data[pos..pos + 3] == b"ID3" {
        let flags = data[pos + 5];
        let size = data[pos + 6..pos + 10]
            .iter()
            .fold(0usize, |acc, &b| (acc << 7) | usize::from(b & 0x7F));
        let footer = if flags & 0x10 != 0 { 10 } else { 0 };
        pos += 10 + size + footer;
    }
    pos.min(data.len())
}

/// If `frame` (the first frame of the stream) is a Xing/Info or VBRI
/// metadata frame rather than audio, the trim it carries (zero without a
/// LAME extension). The layout follows LAME's tag specification as
/// Symphonia's demuxer reads it: the tag sits right after the side
/// information; the LAME extension's encoder string is followed by 12
/// bytes of revision, lowpass, ReplayGain, and flag fields before the
/// 12-bit delay and padding pair.
fn info_tag(frame: &[u8], header: &FrameHeader) -> Option<Trim> {
    if frame.len() >= 40 && &frame[36..40] == b"VBRI" {
        return Some(Trim::default());
    }
    let off = HEADER_LEN + header.side_info_len();
    if frame.len() < off + 8 {
        return None;
    }
    let id = &frame[off..off + 4];
    if id != b"Xing" && id != b"Info" {
        return None;
    }
    let flags = u32::from_be_bytes([
        frame[off + 4],
        frame[off + 5],
        frame[off + 6],
        frame[off + 7],
    ]);
    let mut p = off + 8;
    for (bit, len) in [(1, 4), (2, 4), (4, 100), (8, 4)] {
        if flags & bit != 0 {
            p += len;
        }
    }
    let lame = frame.len() >= p + 24
        && [&b"LAME"[..], b"Lavf", b"Lavc"]
            .iter()
            .any(|tag| frame[p..].starts_with(tag));
    if !lame {
        return Some(Trim::default());
    }
    let packed = u32::from_be_bytes([0, frame[p + 21], frame[p + 22], frame[p + 23]]);
    Some(Trim {
        delay: DECODER_DELAY + (packed >> 12),
        padding: (packed & 0xFFF).saturating_sub(DECODER_DELAY),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A silent MPEG-1 Layer III stereo frame at 128 kbps/44.1 kHz: a
    /// header followed by all-zero side information and main data.
    fn silent_frame() -> Vec<u8> {
        let mut f = vec![0u8; 417];
        f[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x40]);
        f
    }

    #[test]
    fn id3v2_tag_is_skipped_by_its_syncsafe_size() {
        // A 300-byte tag body (syncsafe 0x00 0x00 0x02 0x2C) with the
        // footer flag, followed by two frames.
        let mut data = vec![b'I', b'D', b'3', 4, 0, 0x10, 0, 0, 0x02, 0x2C];
        data.extend(std::iter::repeat_n(0xFFu8, 300 + 10));
        let tag_len = data.len();
        data.extend(silent_frame());
        data.extend(silent_frame());
        assert_eq!(skip_id3v2(&data), tag_len);
        let (spans, spec, trim) = index_frames(&data).unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].offset, tag_len as u64);
        assert_eq!(spec.rate, 44_100);
        assert_eq!(trim, Trim::default());
    }

    #[test]
    fn unconfirmed_sync_words_in_junk_are_not_frames() {
        // A header-shaped word inside junk that is not followed by
        // another frame at its declared length, then two real frames and
        // an ID3v1 tag.
        let mut data = vec![0x11u8; 50];
        data.extend([0xFF, 0xFB, 0x90, 0x40]);
        data.extend(std::iter::repeat_n(0x22u8, 100));
        let first = data.len();
        data.extend(silent_frame());
        data.extend(silent_frame());
        data.extend(b"TAG");
        data.extend(std::iter::repeat_n(0u8, 125));
        let (spans, _, _) = index_frames(&data).unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].offset, first as u64);
        assert_eq!(spans[1].offset, (first + 417) as u64);
    }

    #[test]
    fn truncated_trailing_frame_is_dropped() {
        let mut data = silent_frame();
        data.extend(silent_frame());
        data.extend(&silent_frame()[..200]);
        let (spans, _, _) = index_frames(&data).unwrap();
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn info_frame_with_lame_tag_is_metadata_not_audio() {
        let mut info = silent_frame();
        // Stereo MPEG-1 side info is 32 bytes: the tag sits at +36.
        let off = 36;
        info[off..off + 4].copy_from_slice(b"Info");
        info[off + 4..off + 8].copy_from_slice(&[0, 0, 0, 0x0F]); // all four fields
        let lame = off + 8 + 4 + 4 + 100 + 4;
        info[lame..lame + 9].copy_from_slice(b"LAME3.100");
        // Encoder delay 576, padding 1344.
        info[lame + 21] = 0x24;
        info[lame + 22] = 0x05;
        info[lame + 23] = 0x40;
        let mut data = info.clone();
        data.extend(silent_frame());
        data.extend(silent_frame());
        let (spans, _, trim) = index_frames(&data).unwrap();
        assert_eq!(spans.len(), 2, "the Info frame is not an audio frame");
        assert_eq!(spans[0].offset, 417);
        assert_eq!(
            trim,
            Trim {
                delay: 529 + 576,
                padding: 1344 - 529
            }
        );
        // Without a LAME extension the frame is still skipped, untrimmed.
        let mut bare = silent_frame();
        bare[off..off + 4].copy_from_slice(b"Xing");
        let mut data = bare;
        data.extend(silent_frame());
        let (spans, _, trim) = index_frames(&data).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(trim, Trim::default());
    }

    #[test]
    fn a_stream_that_changes_sample_rate_is_rejected() {
        let mut data = silent_frame();
        data.extend(silent_frame());
        // MPEG-2 22.05 kHz stereo frame (64 kbps): a different stream.
        let mut other = vec![0u8; 72 * 64_000 / 22_050];
        other[..4].copy_from_slice(&[0xFF, 0xF3, 0x80, 0x40]);
        data.extend(other.clone());
        data.extend(other);
        let err = index_frames(&data).unwrap_err();
        assert!(err.to_string().contains("changes at byte"), "{err}");
    }

    #[test]
    fn junk_only_input_has_no_frames() {
        let err = index_frames(&[0u8; 5000]).unwrap_err();
        assert!(err.to_string().contains("no MPEG Layer III"), "{err}");
    }
}
