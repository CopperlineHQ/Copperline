// SPDX-License-Identifier: GPL-3.0-or-later

//! Building a clip the way the Flyer's record path (`DoRecMachine`,
//! flyer/src/vid.c:2280-2725) lays one out: CLIP header, the CFRM frames
//! back to back, then TAIL, index table, two HEAD blocks and 40 user blocks.

use crate::audio::{self, BLOCKS_PER_FIELD, SAMPLES_PER_FIELD};
use crate::header::*;
use crate::vtasc::{self, fir_preset, Algo, FieldParams, DEFAULT_FIR_SET};

/// Video grade: sets the per-field size budget, recorded in `VidGrade`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grade {
    Std,
    Hq5,
    Hq6,
}

impl Grade {
    pub fn code(self) -> u8 {
        match self {
            Grade::Std => GRADE_STD,
            Grade::Hq5 => GRADE_HQ5,
            Grade::Hq6 => GRADE_HQ6,
        }
    }
    /// Nominal blocks per field (`MODE_*_NOMSIZE`, vid.c:158-167, as
    /// `GetLegalVidSize` returns them for software field writing).
    pub fn field_limit(self) -> u16 {
        match self {
            Grade::Std => 0x70,
            Grade::Hq5 => 0x92,
            Grade::Hq6 => 0xBF - 0x0A,
        }
    }
}

/// What the writer did with one field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldStats {
    pub algo: Algo,
    pub tolerance: u8,
    pub blocks: u16,
    /// Nothing on the ladder fitted; the field was cut at the limit (its
    /// bottom lines decode as garbage), as `EmptyBuffer` does (asm:1453-1463).
    pub truncated: bool,
}

/// Fields of one colour frame: 4 fields of up to 242 lines x 752 samples.
pub type FrameVideo = [Vec<Vec<u8>>; 4];

pub struct ClipWriter {
    grade: Grade,
    audio_chans: u8,
    video: bool,
    limit: u16,
    fir_set: Option<u8>,
    rnd_freq: u8,
    ladder: Vec<(Algo, u8)>,
    frames: Vec<(FrameHeader, Vec<u8>)>,
    /// The clip header a copied clip inherits (coder version, pedestal,
    /// FIR set and custom coefficients) from the clip its frames come from.
    template: Option<ClipHeader>,
}

impl ClipWriter {
    /// A video clip with `audio_chans` mono audio channels (0-2).
    pub fn new(grade: Grade, audio_chans: u8) -> Self {
        ClipWriter {
            grade,
            audio_chans,
            video: true,
            limit: grade.field_limit(),
            fir_set: Some(DEFAULT_FIR_SET),
            rnd_freq: 122,
            // FlyerProc.c:341-349 tries D2/0, D2/1, SN/0, SN/1; the record
            // defaults reach SN/2 (vid.c:537-538).
            ladder: vec![
                (Algo::D2, 0),
                (Algo::D2, 1),
                (Algo::SubNyquist, 0),
                (Algo::SubNyquist, 1),
                (Algo::SubNyquist, 2),
            ],
            frames: Vec::new(),
            template: None,
        }
    }
    /// A clip made of frames already coded in another clip, whose header
    /// is `master` ([`Self::push_coded_frame`]): what the firmware's
    /// cutting room builds (`MakeSubClips`, clip.c:1914).
    pub fn copy_of(master: &ClipHeader, video: bool, audio_chans: u8) -> Self {
        let grade = match master.vid_grade {
            GRADE_HQ5 => Grade::Hq5,
            GRADE_HQ6 => Grade::Hq6,
            _ => Grade::Std,
        };
        let mut w = ClipWriter::new(grade, audio_chans);
        w.video = video;
        w.fir_set = Some(master.fir_set);
        w.template = Some(master.clone());
        w
    }
    /// An audio-only clip (`VidFlag` 0), like MakeAudClip.c.
    pub fn audio_only(audio_chans: u8) -> Self {
        let mut w = ClipWriter::new(Grade::Std, audio_chans);
        w.video = false;
        w
    }
    pub fn set_field_limit(&mut self, blocks: u16) {
        self.limit = blocks;
    }
    /// FIR precompensation preset (1-4) or `None` for `FWF_NOFIR`.
    pub fn set_fir(&mut self, set: Option<u8>) {
        self.fir_set = set;
    }
    /// Sequence of (algorithm, tolerance) tried until a field fits.
    pub fn set_ladder(&mut self, ladder: &[(Algo, u8)]) {
        assert!(!ladder.is_empty());
        self.ladder = ladder.to_vec();
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Append a colour frame. `audio` is interleaved, 4 x 735 samples per
    /// channel (short input is zero-padded).
    pub fn push_frame(
        &mut self,
        video: Option<&FrameVideo>,
        audio: Option<&[i16]>,
    ) -> [Option<FieldStats>; 4] {
        let i = self.frames.len() as u32;
        let chans = self.audio_chans;
        let mut hdr = FrameHeader {
            frame_number: i,
            vid_flag: self.video as u8,
            audio_chans: chans,
            audio_length: if chans > 0 { BLOCKS_PER_FIELD as u8 } else { 0 },
            ..FrameHeader::default()
        };
        let mut body = Vec::new();
        for c in 0..chans as usize {
            for f in 0..4 {
                let mut s = [0i16; SAMPLES_PER_FIELD];
                if let Some(a) = audio {
                    for (k, v) in s.iter_mut().enumerate() {
                        *v = a
                            .get((f * SAMPLES_PER_FIELD + k) * chans as usize + c)
                            .copied()
                            .unwrap_or(0);
                    }
                }
                body.extend_from_slice(&audio::chunk_bytes(&s));
            }
        }
        let mut stats = [None; 4];
        if self.video {
            let empty: Vec<Vec<u8>> = Vec::new();
            for f in 0..4 {
                let n = i * 4 + f as u32;
                let lines = video.map(|v| &v[f]).unwrap_or(&empty);
                let (data, st) = self.encode_fitting(lines, n);
                let fi = &mut hdr.fields[f];
                fi.vid_start = (1 + body.len() / BLOCK) as u16;
                fi.vid_length = (data.len() / BLOCK) as u16;
                fi.tolerance = st.tolerance;
                fi.rnd_size = st.tolerance; // CloseField, clip.c:676
                fi.rnd_freq = self.rnd_freq;
                fi.rnd_seed = (n % 255) as u8; // clip.c:678
                fi.flags = if st.algo == Algo::SubNyquist {
                    FIF_SUBNYQ
                } else {
                    0
                };
                fi.fir_preset = self.fir_set.unwrap_or(DEFAULT_FIR_SET);
                body.extend_from_slice(&data);
                stats[f] = Some(st);
            }
        }
        hdr.length = 1 + (body.len() / BLOCK) as u32;
        self.frames.push((hdr, body));
        stats
    }

    /// Append a colour frame already coded: its header (numbering and
    /// links are redone by [`Self::finish`]) and the blocks after it.
    pub fn push_coded_frame(&mut self, mut header: FrameHeader, body: Vec<u8>) {
        debug_assert_eq!(body.len() % BLOCK, 0);
        header.frame_number = self.frames.len() as u32;
        header.length = 1 + (body.len() / BLOCK) as u32;
        self.frames.push((header, body));
    }

    fn encode_fitting(&self, lines: &[Vec<u8>], n: u32) -> (Vec<u8>, FieldStats) {
        let fir = self.fir_set.and_then(|s| fir_preset(s, false));
        let mut last = None;
        for &(algo, tolerance) in &self.ladder {
            let p = FieldParams {
                field: n,
                algo,
                tolerance,
                rnd_freq: self.rnd_freq,
                rnd_seed: (n % 255) as u8,
                fir,
            };
            let data = vtasc::encode_field(lines, &p);
            let blocks = (data.len() / BLOCK) as u16;
            if blocks <= self.limit {
                return (
                    data,
                    FieldStats {
                        algo,
                        tolerance,
                        blocks,
                        truncated: false,
                    },
                );
            }
            last = Some((data, algo, tolerance));
        }
        let (mut data, algo, tolerance) = last.unwrap();
        data.truncate(self.limit as usize * BLOCK);
        let blocks = (data.len() / BLOCK) as u16;
        (
            data,
            FieldStats {
                algo,
                tolerance,
                blocks,
                truncated: true,
            },
        )
    }

    /// Lay out the clip and return its bytes.
    pub fn finish(mut self) -> Vec<u8> {
        let nfr = self.frames.len();
        // Frame positions (blocks from the CLIP header).
        let mut lba = Vec::with_capacity(nfr);
        let mut b = 1u32;
        for (h, _) in &self.frames {
            lba.push(b);
            b += h.length;
        }
        let tail_lba = b;
        // Links, back pointers and index, as vid.c:2354-2412 and 2593-2600.
        let rel = |ptrs: &[u32; 15], at: u32| {
            ptrs.map(|p| if p == 0 { 0 } else { p.wrapping_sub(at) as i32 })
        };
        let mut back = [0u32; 15];
        let mut ndx_ctr = 0usize;
        let mut index: Vec<u32> = Vec::new();
        for (i, (h, _)) in self.frames.iter_mut().enumerate() {
            let at = lba[i];
            h.prev_frame = if i == 0 {
                0
            } else {
                lba[i - 1] as i32 - at as i32
            };
            h.next_frame = if i + 1 == nfr { 0 } else { h.length as i32 };
            if ndx_ctr == 0 {
                h.back_ptrs = rel(&back, at);
                index.push(at);
                back = [0; 15];
                ndx_ctr = 15;
            } else {
                back[ndx_ctr - 1] = at;
                ndx_ctr -= 1;
                h.back_ptrs = rel(&back, at);
            }
        }
        if let Some(&last) = lba.last() {
            index.push(last);
        }
        let ndx_blks = index.len().div_ceil(STONESPERBLK).max(1) as u16;
        let tail = TailHeader::new(ndx_blks, index.len() as u16);
        let chans = self.audio_chans;
        let base = self.template.take().unwrap_or_default();
        let ch = ClipHeader {
            id: ID_CLIP,
            length: tail_lba + tail.chunk_len,
            vid_grade: if self.video {
                self.grade.code()
            } else {
                GRADE_STD
            },
            fields: nfr as u32 * 4,
            data_start: 1,
            data_end: tail_lba,
            tail: tail_lba,
            audio_length: if chans > 0 { BLOCKS_PER_FIELD as u8 } else { 0 },
            vid_flag: self.video as u8,
            audio_chans: chans,
            fir_set: self.fir_set.unwrap_or(DEFAULT_FIR_SET),
            ..base
        };
        let mut out = Vec::with_capacity(ch.length as usize * BLOCK);
        out.extend_from_slice(&ch.to_bytes());
        for (h, body) in &self.frames {
            out.extend_from_slice(&h.to_bytes());
            out.extend_from_slice(body);
        }
        out.extend_from_slice(&tail.to_bytes());
        let mut ib = vec![0u8; ndx_blks as usize * BLOCK];
        for (k, &e) in index.iter().enumerate() {
            put_u32(&mut ib, k * 4, e.wrapping_sub(tail_lba));
        }
        out.extend_from_slice(&ib);
        out.extend_from_slice(&head_block(false));
        out.extend_from_slice(&head_block(true));
        out.resize(out.len() + USERAREASIZE as usize * BLOCK, 0);
        debug_assert_eq!(out.len(), ch.length as usize * BLOCK);
        out
    }
}
