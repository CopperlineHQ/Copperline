// SPDX-License-Identifier: GPL-3.0-or-later

//! Sub-clips: a run of colour frames copied, still coded, out of a clip,
//! as the firmware's cutting room makes them (`MakeSubClips`,
//! flyer/src/clip.c:1914): the frames whole (`CopyData`), or their sound
//! or their picture alone (`StripAudioRaw` / `StripVideoRaw`), then a new
//! CLIP header, TAIL and index (`ConstructIndex`).

use crate::clip::{Clip, Error};
use crate::header::BLOCK;
use crate::writer::ClipWriter;

/// What a sub-clip keeps of its frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parts {
    pub video: bool,
    pub audio: bool,
}

/// The sub-clip of `master` from field `start` for `fields` fields. It
/// starts at the colour frame holding `start` and runs whole colour
/// frames (`Fields2Frames(n, TRUE)` rounds up), clipped to the master's
/// end. Parts the master lacks are left out; a sub-clip with nothing left
/// is an error.
pub fn sub_clip(master: &Clip, start: u32, fields: u32, parts: Parts) -> Result<Vec<u8>, Error> {
    let total = master.field_count();
    if start >= total || fields == 0 {
        return Err(Error::NoSuchField(start));
    }
    let first = (start / 4) as usize;
    let count = fields.div_ceil(4) as usize;
    let last = (first + count).min(master.frames.len());
    let has_video = master.header.vid_flag != 0;
    let chans = master.header.audio_chans;
    let video = parts.video && has_video;
    let audio = parts.audio && chans > 0;
    if !video && !audio {
        return Err(Error::NoVideo(start));
    }
    let mut writer = ClipWriter::copy_of(&master.header, video, if audio { chans } else { 0 });
    let data = master.bytes();
    for frame in &master.frames[first..last] {
        let mut header = frame.header.clone();
        let from = (frame.block as usize + 1) * BLOCK;
        let to = ((frame.block + header.length) as usize * BLOCK).min(data.len());
        if from > to {
            return Err(Error::OutOfRange {
                block: frame.block as u64,
            });
        }
        let mut body = data[from..to].to_vec();
        body.resize(body.len().div_ceil(BLOCK) * BLOCK, 0);
        // The sound comes first in a frame: each channel's four fields.
        let audio_blocks = usize::from(header.audio_length) * usize::from(header.audio_chans) * 4;
        let audio_bytes = (audio_blocks * BLOCK).min(body.len());
        if !audio && audio_blocks > 0 {
            body.drain(..audio_bytes);
            for f in header.fields.iter_mut().filter(|f| f.vid_length > 0) {
                f.vid_start = f.vid_start.saturating_sub(audio_blocks as u16);
            }
            header.audio_chans = 0;
            header.audio_length = 0;
        }
        if !video {
            body.truncate(audio_bytes);
            header.vid_flag = 0;
            for f in header.fields.iter_mut() {
                f.vid_start = 0;
                f.vid_length = 0;
            }
        }
        writer.push_coded_frame(header, body);
    }
    Ok(writer.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::Grade;

    fn test_clip(frames: u32) -> Vec<u8> {
        let mut w = ClipWriter::new(Grade::Std, 2);
        for i in 0..frames {
            let line = vec![(40 + i * 10) as u8; 752];
            let field: Vec<Vec<u8>> = vec![line; 242];
            let video = [field.clone(), field.clone(), field.clone(), field];
            let sound: Vec<i16> = (0..4 * 735 * 2).map(|k| (i * 1000 + k) as i16).collect();
            w.push_frame(Some(&video), Some(&sound));
        }
        w.finish()
    }

    #[test]
    fn a_sub_clip_is_whole_colour_frames_copied_as_they_were() {
        let bytes = test_clip(6);
        let master = Clip::read(&bytes).unwrap();
        // Fields 9-17 lie in frames 2-4.
        let cut = sub_clip(
            &master,
            9,
            9,
            Parts {
                video: true,
                audio: true,
            },
        )
        .unwrap();
        let sub = Clip::read(&cut).unwrap();
        assert_eq!(sub.frames.len(), 3);
        assert_eq!(sub.field_count(), 12);
        assert_eq!(
            sub.decode_field(0).unwrap(),
            master.decode_field(8).unwrap()
        );
        assert_eq!(
            sub.decode_field(11).unwrap(),
            master.decode_field(19).unwrap()
        );
        let (c, a) = sub.audio();
        let (_, m) = master.audio();
        assert_eq!(c, 2);
        assert_eq!(a[..], m[8 * 735 * 2..20 * 735 * 2]);
        assert!(!sub.index.is_empty());
    }

    #[test]
    fn a_sub_clip_can_keep_only_its_picture_or_only_its_sound() {
        let bytes = test_clip(3);
        let master = Clip::read(&bytes).unwrap();
        let pic = sub_clip(
            &master,
            0,
            12,
            Parts {
                video: true,
                audio: false,
            },
        )
        .unwrap();
        let pic = Clip::read(&pic).unwrap();
        assert_eq!(pic.header.audio_chans, 0);
        assert_eq!(pic.audio().1.len(), 0);
        assert_eq!(
            pic.decode_field(5).unwrap(),
            master.decode_field(5).unwrap()
        );
        let snd = sub_clip(
            &master,
            4,
            4,
            Parts {
                video: false,
                audio: true,
            },
        )
        .unwrap();
        let snd = Clip::read(&snd).unwrap();
        assert_eq!(snd.header.vid_flag, 0);
        assert!(snd.decode_field(0).is_err());
        assert_eq!(
            snd.audio().1[..],
            master.audio().1[4 * 735 * 2..8 * 735 * 2]
        );
    }

    #[test]
    fn a_sub_clip_stops_at_the_end_of_its_master() {
        let bytes = test_clip(2);
        let master = Clip::read(&bytes).unwrap();
        let cut = sub_clip(
            &master,
            4,
            400,
            Parts {
                video: true,
                audio: true,
            },
        )
        .unwrap();
        assert_eq!(Clip::read(&cut).unwrap().frames.len(), 1);
        assert!(sub_clip(
            &master,
            8,
            4,
            Parts {
                video: true,
                audio: true
            }
        )
        .is_err());
    }
}
