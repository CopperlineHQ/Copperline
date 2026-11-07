// SPDX-License-Identifier: GPL-3.0-or-later

//! Container tests: header layout, writer -> reader, index/back pointers,
//! audio, and a skeletal audio clip built exactly as MakeAudClip.c does.

use flyerclip::audio::{chunk_offset, SAMPLES_PER_FIELD};
use flyerclip::composite::line_from_yiq;
use flyerclip::header::*;
use flyerclip::vtasc::{self, Algo, SAMPLES_PER_LINE, TOASTER_LINES};
use flyerclip::{Clip, ClipWriter, FrameVideo, Grade};

#[test]
fn clip_header_layout_and_round_trip() {
    let mut h = ClipHeader {
        length: 0x0102_0304,
        vid_grade: GRADE_HQ5,
        fields: 1234,
        data_start: 1,
        data_end: 0x5555,
        tail: 0x5555,
        reserved2: 0x11,
        audio_length: 3,
        vid_flag: 1,
        audio_chans: 2,
        pedestal: 60,
        fir_set: 2,
        cust_coefs: [1, 2, 3, 4, 5, 6, 7, 0x3FF, 0x200],
        cust_inv_coefs: [9, 8, 7, 6, 5, 4, 3, 2, 0x200],
        fir_rom: 7,
        fir_ram: 9,
        ..ClipHeader::default()
    };
    h.reserved4[437] = 0xAB;
    let b = h.to_bytes();
    assert_eq!(&b[0..4], b"CLIP");
    assert_eq!(&b[4..8], &[1, 2, 3, 4]);
    assert_eq!(b[8], 4); // Version
    assert_eq!(b[9], 2); // VidCmpVer
    assert_eq!(b[10], 2); // AudVer
    assert_eq!(b[11], GRADE_HQ5);
    assert_eq!(b[29], 3); // AudioLength
    assert_eq!(b[31], 2); // AudioChans
    assert_eq!(b[32], 60); // Pedestal
    assert_eq!(&b[48..52], &[0x03, 0xFF, 0x02, 0x00]); // CustCoefs[7], [8]
    assert_eq!(&b[70..72], &[0x02, 0x00]); // LogBlkSize
    assert_eq!((b[72], b[73]), (7, 9));
    assert_eq!(b[511], 0xAB);
    assert_eq!(ClipHeader::parse(&b).unwrap(), h);
}

#[test]
fn frame_header_layout_and_round_trip() {
    let mut f = FrameHeader {
        length: 300,
        prev_frame: -300,
        next_frame: 300,
        frame_number: 77,
        audio_length: 3,
        audio_chans: 2,
        phase: 0x1234,
        pragma: 0xBEEF,
        ..FrameHeader::default()
    };
    f.back_ptrs[14] = -1;
    f.fields[3] = FieldInfo {
        vid_start: 25,
        vid_length: 0x70,
        tolerance: 1,
        rnd_size: 1,
        rnd_freq: 122,
        rnd_seed: 254,
        flags: FIF_SUBNYQ,
        fir_preset: 2,
        serial_len: 3,
        ..FieldInfo::default()
    };
    f.fields[3].serial[35] = 0x5A;
    let b = f.to_bytes();
    assert_eq!(&b[0..4], b"CFRM");
    assert_eq!(&b[12..16], &(-300i32).to_be_bytes());
    assert_eq!(&b[104..108], &[0xFF; 4]); // back_ptrs[14]
    assert_eq!(&b[218..220], &[0xBE, 0xEF]);
    let fo = 224 + 3 * FIELD_INFO_SIZE;
    assert_eq!(&b[fo..fo + 8], &[0, 25, 0, 0x70, 1, 1, 122, 254]);
    assert_eq!(b[fo + 12], FIF_SUBNYQ);
    assert_eq!(b[fo + 32], 2);
    assert_eq!(b[fo + 71], 0x5A);
    assert_eq!(fo + 72, 512);
    assert_eq!(FrameHeader::parse(&b).unwrap(), f);
}

#[test]
fn tail_header_layout() {
    let t = TailHeader::new(1, 3);
    let b = t.to_bytes();
    assert_eq!(&b[0..4], b"TAIL");
    assert_eq!(get(&b, 4), 1 + 1 + 2 + 40);
    assert_eq!(get(&b, 8), 1);
    assert_eq!(&b[12..16], &[0, 1, 0, 3]);
    assert_eq!(get(&b, 36), 2); // HeadList
    assert_eq!(&b[40..42], &[0, 2]);
    assert_eq!(get(&b, 74), 4); // UserArea
    assert_eq!(&b[78..80], &[0, 40]);
    assert_eq!(TailHeader::parse(&b).unwrap(), t);
}

fn get(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn bars_frame(level: f32) -> FrameVideo {
    std::array::from_fn(|f| {
        (0..TOASTER_LINES)
            .map(|y| {
                line_from_yiq(
                    |x| {
                        let bar = x * 4 / SAMPLES_PER_LINE;
                        (
                            70.0 + level * 30.0 * bar as f32,
                            [0.0, 30.0, -20.0, 10.0][bar],
                            [0.0, -10.0, 25.0, 15.0][bar],
                        )
                    },
                    f as u32,
                    y,
                )
            })
            .collect()
    })
}

/// FindFrame (flyer/src/subs.c:410-541): index entry ceil(F/16), then
/// that frame's back pointer [16 - F%16 - 1].
fn find_frame(clip: &Clip, frame: u32) -> u32 {
    let tail = clip.header.tail as i64;
    let back = frame & 15;
    let entry = (frame >> 4) + (back != 0) as u32;
    let mut target = (tail + clip.index[entry as usize] as i64) as u32;
    if back != 0 {
        let fr = clip
            .frames
            .iter()
            .find(|f| f.block == target)
            .expect("index points at a frame");
        target = (target as i64 + fr.header.back_ptrs[(16 - back - 1) as usize] as i64) as u32;
    }
    target
}

#[test]
fn writer_reader_round_trip_with_index_and_audio() {
    let nframes = 18; // crosses a 16-frame milestone
    let mut w = ClipWriter::new(Grade::Std, 2);
    let mut pcm = Vec::new();
    let mut inputs = Vec::new();
    for i in 0..nframes {
        let video = bars_frame(i as f32 / nframes as f32);
        let mut a = Vec::with_capacity(4 * SAMPLES_PER_FIELD * 2);
        for k in 0..4 * SAMPLES_PER_FIELD {
            let t = (i * 4 * SAMPLES_PER_FIELD + k) as i32;
            a.push((t * 100) as i16); // left: sawtooth
            a.push(((t * 37) as i16).wrapping_neg()); // right
        }
        let stats = w.push_frame(Some(&video), Some(&a));
        for s in stats.iter().flatten() {
            assert_eq!((s.algo, s.tolerance, s.truncated), (Algo::D2, 0, false));
            assert!(s.blocks <= Grade::Std.field_limit());
        }
        pcm.extend_from_slice(&a);
        inputs.push(video);
    }
    let bytes = w.finish();
    let clip = Clip::read(&bytes).unwrap();
    let h = &clip.header;
    assert_eq!((h.version, h.vid_cmp_ver, h.aud_ver), (4, 2, 2));
    assert_eq!(h.fields, 4 * nframes as u32);
    assert_eq!(h.length as usize * BLOCK, bytes.len());
    assert_eq!(h.tail, h.data_end);
    assert_eq!(
        (h.audio_length, h.audio_chans, h.vid_flag, h.pedestal),
        (3, 2, 1, 60)
    );
    assert_eq!(clip.frames.len(), nframes);
    let tail = clip.tail.as_ref().unwrap();
    assert_eq!(tail.ndx_count, 3); // milestones 0 and 16, then the last frame
    assert_eq!(tail.chunk_len, 1 + 1 + 2 + 40);
    assert_eq!(&bytes[(h.tail as usize + 2) * BLOCK..][..4], b"HEAD");

    // Frame chain and layout.
    for (i, fr) in clip.frames.iter().enumerate() {
        let fh = &fr.header;
        assert_eq!(fh.frame_number, i as u32);
        assert_eq!(fh.audio_chans, 2);
        if i > 0 {
            let prev = &clip.frames[i - 1];
            assert_eq!(fh.prev_frame, prev.block as i32 - fr.block as i32);
            assert_eq!(prev.header.next_frame, prev.header.length as i32);
        }
        // Audio first (2 ch x 4 fields x 3 blocks), then the 4 fields back to back.
        let mut at = 1 + 24;
        for fi in &fh.fields {
            assert_eq!(fi.vid_start, at);
            at += fi.vid_length;
        }
        assert_eq!(at as u32, fh.length);
        assert_eq!(chunk_offset(2, 3, 2, 1, 2), 1 + (4 + 2) * 3);
    }
    assert_eq!(clip.frames.last().unwrap().header.next_frame, 0);

    // The firmware's frame finder reaches every frame.
    for f in 0..nframes as u32 {
        assert_eq!(
            find_frame(&clip, f),
            clip.frames[f as usize].block,
            "frame {f}"
        );
    }

    // Audio comes back bit-exact.
    let (ch, got) = clip.audio();
    assert_eq!(ch, 2);
    assert_eq!(got, pcm);

    // Video decodes within the FIR round-trip error of the input.
    for n in [0u32, 1, 6, 71] {
        let out = clip.decode_field(n).unwrap();
        let src = &inputs[(n / 4) as usize][(n % 4) as usize];
        let mean: f64 = src
            .iter()
            .zip(&out)
            .map(|(a, b)| {
                a[12..740]
                    .iter()
                    .zip(&b[12..740])
                    .map(|(p, q)| p.abs_diff(*q) as f64)
                    .sum::<f64>()
            })
            .sum::<f64>()
            / (242.0 * 728.0);
        assert!(mean < 4.0, "field {n}: mean err {mean}");
        let p = clip.field_params(n, true).unwrap();
        assert_eq!(p.rnd_seed, (n % 255) as u8);
    }
}

#[test]
fn writer_falls_down_the_ladder_when_over_budget() {
    // Noise does not fit 0x70 blocks at D2/0; a tight budget forces SN.
    let mut s = 1u32;
    let noisy: FrameVideo = std::array::from_fn(|_| {
        (0..TOASTER_LINES)
            .map(|_| {
                (0..SAMPLES_PER_LINE)
                    .map(|_| {
                        s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
                        100 + ((s >> 16) % 24) as u8
                    })
                    .collect()
            })
            .collect()
    });
    let mut w = ClipWriter::new(Grade::Std, 0);
    let stats = w.push_frame(Some(&noisy), None);
    for st in stats.iter().flatten() {
        assert!(st.blocks <= 0x70);
        assert_ne!((st.algo, st.tolerance), (Algo::D2, 0), "{st:?}");
    }
    let bytes = w.finish();
    let clip = Clip::read(&bytes).unwrap();
    for n in 0..4 {
        let (_, fi) = clip.field_info(n).unwrap();
        let st = stats[n as usize].unwrap();
        assert_eq!(fi.is_sub_nyquist(), st.algo == Algo::SubNyquist);
        assert_eq!(fi.tolerance, st.tolerance);
        clip.decode_field(n).unwrap();
    }
}

/// A clip written block by block as MakeAudClip.c:84-170 does (skeletal:
/// no tail, audio only), read back.
#[test]
fn reads_makeaudclip_skeletal_clip() {
    const FRAMES: u32 = 16;
    let mut audio = [0u16; 768];
    let mut j = 0u16;
    for s in audio.iter_mut().take(735) {
        j = j.wrapping_add(100);
        *s = j;
    }
    let mut bytes = Vec::new();
    let ch = ClipHeader {
        length: 1 + FRAMES * (1 + 3 * 2 * 4),
        fields: FRAMES * 4,
        data_start: 1,
        data_end: FRAMES * (1 + 3 * 2 * 4),
        tail: 0,
        audio_length: 3,
        vid_flag: 0,
        audio_chans: 2,
        fir_set: 0,
        ..ClipHeader::default()
    };
    bytes.extend_from_slice(&ch.to_bytes());
    for frm in 0..FRAMES {
        let len = 1 + 3 * 2 * 4;
        let fh = FrameHeader {
            length: len,
            prev_frame: if frm == 0 { 0 } else { -(len as i32) },
            next_frame: if frm + 1 >= FRAMES { 0 } else { len as i32 },
            frame_number: frm,
            audio_length: 3,
            vid_flag: 0,
            audio_chans: 2,
            ..FrameHeader::default()
        };
        bytes.extend_from_slice(&fh.to_bytes());
        for _ in 0..8 {
            for s in &audio {
                bytes.extend_from_slice(&s.to_be_bytes());
            }
        }
    }
    let clip = Clip::read(&bytes).unwrap();
    assert!(clip.tail.is_none());
    assert_eq!(clip.frames.len(), FRAMES as usize);
    assert_eq!(clip.field_count(), FRAMES * 4);
    assert!(matches!(
        clip.decode_field(0),
        Err(flyerclip::Error::NoVideo(0))
    ));
    let (ch, pcm) = clip.audio();
    assert_eq!(ch, 2);
    assert_eq!(pcm.len(), (FRAMES * 4) as usize * 735 * 2);
    for i in 0..735 {
        assert_eq!(pcm[2 * i] as u16, audio[i]);
        assert_eq!(pcm[2 * i + 1] as u16, audio[i]);
    }
}

#[test]
fn audio_only_writer() {
    let mut w = ClipWriter::audio_only(1);
    let a: Vec<i16> = (0..4 * 735).map(|i| (i * 7) as i16).collect();
    w.push_frame(None, Some(&a));
    let bytes = w.finish();
    let clip = Clip::read(&bytes).unwrap();
    assert_eq!(clip.header.vid_flag, 0);
    assert_eq!(clip.frames[0].header.length, 1 + 12);
    assert_eq!(clip.audio(), (1, a));
}

#[test]
fn decoded_field_matches_direct_codec() {
    // Clip::decode_field is the codec applied with the stored parameters.
    let mut w = ClipWriter::new(Grade::Hq5, 0);
    w.push_frame(Some(&bars_frame(0.5)), None);
    let bytes = w.finish();
    let clip = Clip::read(&bytes).unwrap();
    for n in 0..4 {
        let p = clip.field_params(n, true).unwrap();
        assert_eq!(p.fir, vtasc::fir_preset(2, true));
        assert_eq!(
            clip.decode_field(n).unwrap(),
            vtasc::decode_field(clip.field_data(n).unwrap(), &p)
        );
    }
    assert_eq!(clip.header.vid_grade, GRADE_HQ5);
}
