// SPDX-License-Identifier: GPL-3.0-or-later

//! NewTek Video Toaster Flyer clips: container, VTASC video codec, audio.
//!
//! Transcribed from NewTek's 1995-98 sources (flyer.library and the Flyer
//! firmware); see `flyer-clip-format.md` for the format with citations.
//!
//! ```no_run
//! # let bytes: Vec<u8> = vec![];
//! let clip = flyerclip::Clip::read(&bytes).unwrap();
//! for n in 0..clip.field_count() {
//!     let lines = clip.decode_field(n).unwrap(); // 242 x 752 composite samples
//! #   let _ = lines;
//! }
//! let (channels, pcm) = clip.audio(); // interleaved i16, 735 per field
//! # let _ = (channels, pcm);
//! ```

pub mod audio;
pub mod clip;
pub mod composite;
pub mod cut;
pub mod header;
pub mod vtasc;
pub mod writer;

pub use clip::{Clip, Error, Frame};
pub use header::{ClipHeader, FieldInfo, FrameHeader, TailHeader, BLOCK};
pub use vtasc::{decode_field, decode_field_wide, encode_field, Algo, FieldParams, FirSet};
pub use writer::{ClipWriter, FieldStats, FrameVideo, Grade};
