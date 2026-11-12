// SPDX-License-Identifier: GPL-3.0-or-later

//! A Flyer clip on a host drive, read field by field for playback.
//!
//! A clip file is the clip's blocks verbatim (`crates/flyerclip/FORMAT.md`):
//! the CLIP header, then a chain of colour frames, each a CFRM header with
//! four fields of VTASC video and, per mono audio channel, four fields of
//! 735 samples. Playback needs one field at a time, so the file is not read
//! whole: opening it walks the frame headers (one block each) and keeps
//! them, and a field's video or audio is read from its blocks when it is
//! played. [`flyerclip`] does the decoding. A colour frame's sound (all its
//! fields and channels, one run of blocks) is read in one go and kept for
//! a few frames, so a field of sound is a copy out of memory; the decoder
//! threads that read pictures ahead (see [`super::link`]) read the sound
//! ahead the same way.

use flyerclip::header::{ClipHeader, FrameHeader, BLOCK};
use flyerclip::vtasc::{self, FieldParams};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Samples per channel per field (the DSP's `AY0 = 735`).
pub const SAMPLES_PER_FIELD: usize = flyerclip::audio::SAMPLES_PER_FIELD;

/// Colour frames of sound kept in memory per clip.
const AUDIO_FRAMES_KEPT: usize = 24;

/// An open clip.
pub struct ClipFile {
    /// Tells this opening of a file from any other (decoded pictures are
    /// cached by it).
    id: u64,
    path: PathBuf,
    file: Mutex<File>,
    pub header: ClipHeader,
    /// Each colour frame's block (from the CLIP header) and its header.
    frames: Vec<(u32, FrameHeader)>,
    /// The sound blocks of recently played colour frames.
    audio: Mutex<VecDeque<(u32, Arc<Vec<u8>>)>>,
}

impl std::fmt::Debug for ClipFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipFile")
            .field("path", &self.path)
            .field("fields", &self.field_count())
            .finish()
    }
}

impl ClipFile {
    /// Open a clip and find its frames, following each CFRM header's link
    /// to the next as the firmware's player does, up to `DataEnd` or the
    /// end of the file.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        let mut block = [0u8; BLOCK];
        read_block(&mut file, 0, &mut block)?;
        let header = ClipHeader::parse(&block).map_err(|e| invalid(path, &format!("{e:?}")))?;
        let blocks = len / BLOCK as u64;
        let end = if header.data_end > header.data_start {
            u64::from(header.data_end)
        } else {
            blocks
        };
        let mut frames = Vec::new();
        let mut at = u64::from(header.data_start);
        while at < end.min(blocks) {
            read_block(&mut file, at, &mut block)?;
            let Ok(frame) = FrameHeader::parse(&block) else {
                break;
            };
            let next = frame.next_frame;
            frames.push((at as u32, frame));
            if next <= 0 {
                break;
            }
            at += next as u64;
        }
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Ok(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            path: path.to_path_buf(),
            file: Mutex::new(file),
            header,
            frames,
            audio: Mutex::new(VecDeque::new()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// This opening's identity.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Fields the clip holds: its header's count, as far as its frames go.
    pub fn field_count(&self) -> u32 {
        self.header
            .fields
            .min((self.frames.len() as u32).saturating_mul(4))
    }

    pub fn has_video(&self) -> bool {
        self.header.vid_flag != 0
    }

    /// Mono audio channels.
    pub fn audio_channels(&self) -> u8 {
        self.header.audio_chans
    }

    /// Decode video field `n`: 242 lines of 752 composite samples, field
    /// `n % 4` of its colour frame. `None` for a field with no video or
    /// one that cannot be read.
    pub fn decode_field(&self, n: u32) -> Option<Vec<Vec<u8>>> {
        let (block, frame) = self.frames.get((n / 4) as usize)?;
        if n >= self.field_count() || frame.vid_flag == 0 {
            return None;
        }
        let fi = &frame.fields[(n % 4) as usize];
        if fi.vid_length == 0 {
            return None;
        }
        let mut data = vec![0u8; usize::from(fi.vid_length) * BLOCK];
        let at = u64::from(*block + u32::from(fi.vid_start)) * BLOCK as u64;
        let got = self.read_at(at, &mut data).ok()?;
        data.truncate(got);
        if data.is_empty() {
            return None;
        }
        let params =
            FieldParams::from_field_info(fi, frame.vid_cmp_ver, n, Some(&self.header), true);
        Some(vtasc::decode_field(&data, &params))
    }

    /// The 735 samples of mono channel `chan` in field `n`; silence where
    /// the clip has none.
    pub fn audio_field(&self, n: u32, chan: u8) -> [i16; SAMPLES_PER_FIELD] {
        let mut out = [0i16; SAMPLES_PER_FIELD];
        let Some((_, frame)) = self.frames.get((n / 4) as usize) else {
            return out;
        };
        if n >= self.field_count() || chan >= frame.audio_chans || frame.audio_length == 0 {
            return out;
        }
        let Some(blocks) = self.frame_audio(n / 4) else {
            return out;
        };
        // Chunks are counted from the block after the CFRM header, where
        // the frame's sound starts.
        let offset = flyerclip::audio::chunk_offset(
            frame.aud_ver,
            frame.audio_length,
            frame.audio_chans,
            chan,
            (n % 4) as u8,
        ) as usize
            - 1;
        let raw = blocks
            .get(offset * BLOCK..)
            .map_or(&[][..], |r| &r[..r.len().min(SAMPLES_PER_FIELD * 2)]);
        for (s, b) in out.iter_mut().zip(raw.chunks_exact(2)) {
            *s = i16::from_be_bytes([b[0], b[1]]);
        }
        out
    }

    /// The sound blocks of colour frame `f` (every field and channel), read
    /// once and kept while it is among the last few frames used. `None`
    /// for a frame with no sound.
    pub fn frame_audio(&self, f: u32) -> Option<Arc<Vec<u8>>> {
        let (block, frame) = self.frames.get(f as usize)?;
        if frame.audio_chans == 0 || frame.audio_length == 0 {
            return None;
        }
        {
            let kept = self.audio.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, b)) = kept.iter().find(|(k, _)| *k == f) {
                return Some(Arc::clone(b));
            }
        }
        let len = 4 * usize::from(frame.audio_chans) * usize::from(frame.audio_length) * BLOCK;
        let mut data = vec![0u8; len];
        let got = self
            .read_at(u64::from(*block + 1) * BLOCK as u64, &mut data)
            .unwrap_or(0);
        data.truncate(got);
        let data = Arc::new(data);
        let mut kept = self.audio.lock().unwrap_or_else(|e| e.into_inner());
        if !kept.iter().any(|(k, _)| *k == f) {
            if kept.len() >= AUDIO_FRAMES_KEPT {
                kept.pop_front();
            }
            kept.push_back((f, Arc::clone(&data)));
        }
        Some(data)
    }

    fn read_at(&self, offset: u64, out: &mut [u8]) -> std::io::Result<usize> {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        file.seek(SeekFrom::Start(offset))?;
        let mut got = 0;
        while got < out.len() {
            match file.read(&mut out[got..])? {
                0 => break,
                n => got += n,
            }
        }
        Ok(got)
    }
}

fn read_block(file: &mut File, block: u64, out: &mut [u8; BLOCK]) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(block * BLOCK as u64))?;
    file.read_exact(out)
}

fn invalid(path: &Path, why: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("{}: not a Flyer clip ({why})", path.display()),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use flyerclip::{ClipWriter, FrameVideo, Grade};

    /// A short clip: `frames` colour frames of flat grey fields whose level
    /// steps with the field number, and stereo sound whose samples carry
    /// the field number (left) and its negative (right).
    pub fn write_test_clip(path: &Path, frames: u32) {
        let mut writer = ClipWriter::new(Grade::Std, 2);
        for f in 0..frames {
            let mut video: FrameVideo = Default::default();
            for (k, field) in video.iter_mut().enumerate() {
                let level = 80 + ((4 * f + k as u32) * 7 % 100) as u8;
                *field = vec![vec![level; vtasc::SAMPLES_PER_LINE]; vtasc::TOASTER_LINES];
            }
            let mut sound = Vec::new();
            for k in 0..4u32 {
                let n = (4 * f + k) as i16;
                for _ in 0..SAMPLES_PER_FIELD {
                    sound.push(100 * n);
                    sound.push(-100 * n);
                }
            }
            writer.push_frame(Some(&video), Some(&sound));
        }
        std::fs::write(path, writer.finish()).unwrap();
    }

    #[test]
    fn fields_and_sound_read_back_one_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Clip");
        write_test_clip(&path, 3);
        let clip = ClipFile::open(&path).unwrap();
        assert_eq!(clip.field_count(), 12);
        assert!(clip.has_video());
        assert_eq!(clip.audio_channels(), 2);
        let lines = clip.decode_field(5).unwrap();
        assert_eq!(lines.len(), vtasc::TOASTER_LINES);
        // write_test_clip's level for field 5.
        let level = 80 + 5 * 7;
        let mid = i32::from(lines[100][376]);
        assert!((mid - level).abs() <= 3, "{mid} vs {level}");
        assert!(clip.decode_field(12).is_none());
        let left = clip.audio_field(6, 0);
        let right = clip.audio_field(6, 1);
        assert!(left.iter().all(|&s| s == 600));
        assert!(right.iter().all(|&s| s == -600));
        assert!(clip.audio_field(6, 2).iter().all(|&s| s == 0));
        assert!(clip.audio_field(99, 0).iter().all(|&s| s == 0));
    }

    #[test]
    fn a_file_that_is_not_a_clip_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Notes");
        std::fs::write(&path, vec![0u8; 2048]).unwrap();
        assert!(ClipFile::open(&path).is_err());
        assert!(ClipFile::open(&dir.path().join("missing")).is_err());
    }
}
