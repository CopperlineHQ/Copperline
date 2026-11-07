// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading a clip: CLIP header, the CFRM frame chain, the TAIL and its index.

use crate::audio;
use crate::header::*;
use crate::vtasc::{self, FieldParams};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Header(HeaderError),
    /// A block offset points outside the data.
    OutOfRange {
        block: u64,
    },
    /// Field number beyond the clip.
    NoSuchField(u32),
    /// The field has no video (audio-only clip, dropped field).
    NoVideo(u32),
}

impl From<HeaderError> for Error {
    fn from(e: HeaderError) -> Self {
        Error::Header(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// One colour frame: its header and where it sits in the clip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Block offset of the CFRM header from the CLIP header.
    pub block: u32,
    pub header: FrameHeader,
}

/// A parsed clip borrowing its bytes (a clip file is the clip's blocks
/// verbatim, starting with the CLIP header).
#[derive(Clone, Debug)]
pub struct Clip<'a> {
    data: &'a [u8],
    pub header: ClipHeader,
    pub frames: Vec<Frame>,
    pub tail: Option<TailHeader>,
    /// Index table entries (frame offsets relative to the TAIL block).
    pub index: Vec<i32>,
}

fn block_at(data: &[u8], block: u64) -> Result<&[u8], Error> {
    let o = block as usize * BLOCK;
    if block > usize::MAX as u64 / BLOCK as u64 || o + BLOCK > data.len() {
        return Err(Error::OutOfRange { block });
    }
    Ok(&data[o..o + BLOCK])
}

impl<'a> Clip<'a> {
    /// Parse a clip. Frames are found by following `NextFrame` from
    /// `DataStart` (as PlayVidMachine does, vid.c:1995), stopping at
    /// `NextFrame == 0`, at `DataEnd`, or at the end of the data.
    pub fn read(data: &'a [u8]) -> Result<Clip<'a>, Error> {
        let header = ClipHeader::parse(block_at(data, 0)?)?;
        let mut frames = Vec::new();
        let mut b = header.data_start as u64;
        let end = if header.data_end > header.data_start {
            header.data_end as u64
        } else {
            u64::MAX
        };
        while b < end && (b as usize + 1) * BLOCK <= data.len() {
            let fh = match FrameHeader::parse(block_at(data, b)?) {
                Ok(f) => f,
                Err(_) if !frames.is_empty() => break,
                Err(e) => return Err(e.into()),
            };
            let next = fh.next_frame;
            frames.push(Frame {
                block: b as u32,
                header: fh,
            });
            if next <= 0 {
                break;
            }
            b += next as u64;
        }
        let mut tail = None;
        let mut index = Vec::new();
        if header.tail != 0 {
            if let Ok(tb) = block_at(data, header.tail as u64) {
                if let Ok(t) = TailHeader::parse(tb) {
                    for i in 0..t.ndx_count as usize {
                        let blk = header.tail as u64 + t.table as u64 + (i / STONESPERBLK) as u64;
                        if let Ok(ib) = block_at(data, blk) {
                            index.push(get_u32(ib, (i % STONESPERBLK) * 4) as i32);
                        }
                    }
                    tail = Some(t);
                }
            }
        }
        Ok(Clip {
            data,
            header,
            frames,
            tail,
            index,
        })
    }

    /// The clip's bytes.
    pub fn bytes(&self) -> &'a [u8] {
        self.data
    }

    /// Number of fields (`ClipHeader.Fields`, capped by the frames present).
    pub fn field_count(&self) -> u32 {
        self.header.fields.min(self.frames.len() as u32 * 4)
    }

    /// Frame header and field record for field `n` (frame n/4, field n%4).
    pub fn field_info(&self, n: u32) -> Result<(&Frame, &FieldInfo), Error> {
        if n >= self.field_count() {
            return Err(Error::NoSuchField(n));
        }
        let fr = &self.frames[(n / 4) as usize];
        Ok((fr, &fr.header.fields[(n % 4) as usize]))
    }

    /// The coded VTASC bytes of field `n` (`VidLength` blocks at
    /// frame + `VidStart`), clamped to the data.
    pub fn field_data(&self, n: u32) -> Result<&'a [u8], Error> {
        let (fr, fi) = self.field_info(n)?;
        if fi.vid_length == 0 || fr.header.vid_flag == 0 {
            return Err(Error::NoVideo(n));
        }
        let start = (fr.block as usize + fi.vid_start as usize) * BLOCK;
        let end = (start + fi.vid_length as usize * BLOCK).min(self.data.len());
        if start >= end {
            return Err(Error::OutOfRange {
                block: (start / BLOCK) as u64,
            });
        }
        Ok(&self.data[start..end])
    }

    /// Codec parameters for field `n` (`post`: decode with the post FIR).
    pub fn field_params(&self, n: u32, post: bool) -> Result<FieldParams, Error> {
        let (fr, fi) = self.field_info(n)?;
        Ok(FieldParams::from_field_info(
            fi,
            fr.header.vid_cmp_ver,
            n,
            Some(&self.header),
            post,
        ))
    }

    /// Decode field `n` to 242 lines of 752 composite samples.
    pub fn decode_field(&self, n: u32) -> Result<Vec<Vec<u8>>, Error> {
        let data = self.field_data(n)?;
        Ok(vtasc::decode_field(data, &self.field_params(n, true)?))
    }

    /// All audio as interleaved 16-bit samples, 735 per channel per field.
    /// Returns (channels, samples).
    pub fn audio(&self) -> (u8, Vec<i16>) {
        audio::extract(self)
    }
}
