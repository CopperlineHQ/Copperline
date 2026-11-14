// SPDX-License-Identifier: GPL-3.0-or-later

//! Unit tests for the Video Toaster card: split out of `toaster/mod.rs`
//! for size, they are the same `toaster::tests` module and keep full
//! access to the parent's private items via `super::`.

use super::*;
use crate::videoslot::{SLOT_FIRST_CCK, SLOT_LINE_CCK};

const WIDTH: usize = SLOT_LINE_CCK * 4;

/// A one-line slot canvas for beam line `vpos`.
struct Line {
    pixels: Vec<u32>,
    vpos: u32,
}

#[test]
fn the_sound_follows_how_much_of_each_input_program_shows() {
    let mut shares = Inputs::default();
    let mut a = Inputs::default();
    a.0[0] = 1.0;
    let mut b = Inputs::default();
    b.0[1] = 1.0;
    let k = video::Key::Flat(64).mean() / 255.0;
    shares.add(&a, 1.0 - k);
    shares.add(&b, k);
    assert!((shares.0[0] - 0.749).abs() < 0.01 && (shares.0[1] - 0.251).abs() < 0.01);
    let map = video::Key::Map(&[0, 255, 255, 255]);
    assert!((map.mean() - 191.25).abs() < 0.01);
    // An input pinned to nothing is silent: no sound without frames.
    let mut card = Toaster::new();
    assert!(card
        .card()
        .mix_sound(&shares, 1471)
        .iter()
        .all(|&(l, r)| l == 0.0 && r == 0.0));
}

/// A capture at an emulated instant waits for the compose thread:
/// after `settle_outputs` the outputs hold the fields handed over,
/// with no call that would advance or read the card.
#[test]
fn settling_the_outputs_finishes_the_fields_handed_over() {
    let mut card = Toaster::new();
    let blank = Line::new(30);
    for _ in 0..2 {
        card.field_complete(&blank.field(), BeamPos::default());
    }
    VideoSlotDevice::settle_outputs(&mut card);
    let outputs = card.video_outputs();
    assert!(outputs.iter().all(|o| o.snapshot().is_some()));
}

#[test]
fn a_routed_flyer_channel_is_input_3_and_its_sound_joins_program() {
    let link = FlyerLink::new();
    let mut card = Toaster::new();
    card.set_flyer_link(Arc::clone(&link));
    let white = u32::from_le_bytes([255, 255, 255, 255]);
    link.show(0, FieldSource::Matte(white));
    let line = Line::new(30);
    let mut out = vec![0u32; video::PICTURE_W * video::PICTURE_H];
    // What the Flyer did reaches the card with the next field.
    let picture = |card: &mut Toaster, n: usize, out: &mut [u32]| {
        card.field_complete(&line.field(), BeamPos::default());
        card.card()
            .source_picture(regs::Mux::Vid(n), video::Role::Fader, &line.field(), out);
    };
    // Not routed: input 3 is the card's own (here unplugged) input.
    picture(&mut card, 2, &mut out);
    assert!(out.iter().all(|&p| p == video::black()));
    link.set_routing(true, false);
    picture(&mut card, 2, &mut out);
    assert!(out.iter().all(|&p| p == white));
    picture(&mut card, 3, &mut out);
    assert!(out.iter().all(|&p| p == video::black()));
    // The Flyer's two fields of sound reach the card with the next
    // field (the fourth here completes a frame of idle outputs, which
    // drop what came before) and fill the frame's.
    card.field_complete(&line.field(), BeamPos::default());
    link.push_sound(&[(0.5, 0.25); 1470]);
    card.field_complete(&line.field(), BeamPos::default());
    assert!(link.take_sound().is_empty());
    assert_eq!(card.card().flyer_sound.len(), 1470);
    let mut program = vec![(0.0f32, 0.0f32); 1485];
    let peak = card.card().add_flyer_sound(&mut program);
    assert_eq!(peak, Some((0.0, 0.0)));
    assert!(program.iter().all(|&(l, r)| l == 0.5 && r == 0.25));
    assert!(card.card().flyer_sound.is_empty());
    assert_eq!(card.card().status_value()["flyer"]["input3"], true);
}

#[test]
fn the_outputs_are_black_and_silent_until_the_switcher_drives_the_card() {
    let link = FlyerLink::new();
    let mut card = Toaster::new();
    card.set_flyer_link(Arc::clone(&link));
    let white = u32::from_le_bytes([255, 255, 255, 255]);
    link.set_routing(true, false);
    link.show(0, FieldSource::Matte(white));
    // Both buses on input 3, which the Flyer shows white.
    card.card().elh[ELH_EH0 + 2].r = 0xA200;
    card.card().elh[ELH_EH0 + 2].b = 0x0200;
    let program = |card: &Toaster| {
        let outputs = card.video_outputs();
        let (_, _, pixels) = outputs[1].snapshot().expect("a frame");
        pixels
    };
    let frame = |card: &mut Toaster| {
        let blank = Line::new(30);
        card.field_complete(&blank.field(), BeamPos::default());
        card.field_complete(&blank.field(), BeamPos::default());
        let mut sound = Vec::new();
        card.take_audio(&mut sound);
        sound
    };
    link.push_sound(&[(0.5, 0.25); 1470]);
    let sound = frame(&mut card);
    assert!(!card.card().active);
    assert!(program(&card).iter().all(|&p| p == video::black()));
    assert!(!sound.is_empty() && sound.iter().all(|&(l, r)| l == 0.0 && r == 0.0));
    // The Flyer's sound from before is dropped, not played late.
    assert!(link.take_sound().is_empty());
    // The switcher's first header: from then on the outputs follow it.
    let mut eh0 = Line::new(23);
    eh0.header(Header {
        r: 0x8000,
        g: 0x0100,
        b: 0xF000,
        i: 0x0F00,
    });
    card.field_complete(&eh0.field(), BeamPos::default());
    assert!(card.card().active);
    card.card().elh[ELH_EH0 + 2].r = 0xA200;
    card.card().elh[ELH_EH0 + 2].b = 0x0200;
    link.push_sound(&[(0.5, 0.25); 1470]);
    let blank = Line::new(30);
    card.field_complete(&blank.field(), BeamPos::default());
    let mut sound = Vec::new();
    card.take_audio(&mut sound);
    assert!(program(&card).iter().all(|&p| p == white));
    assert!(sound.iter().any(|&(l, _)| l != 0.0));
}

#[test]
fn a_latch_effect_picks_each_pixel_source_from_the_amiga_colour() {
    // Picture row 1 is Amiga line AMIGA_LINE0 + 1.
    let mut line = Line::new(video::AMIGA_LINE0 + 1);
    let x0 = Card::pixel0_column(&line.field());
    // Picture column c is card pixel PICTURE_X0 + c.
    let at = |c: usize| x0 as usize + video::PICTURE_X0 + c;
    // Transparent (T) pixels latch: DV1 (code 4, r1) from picture
    // column 10, input 3 (code 2, r2) from 100; opaque pixels keep it.
    let latch = |r: u8, b: u8| u32::from_le_bytes([r, 0, b, 0]);
    line.pixels[at(10)] = latch(0x02, 0);
    line.pixels[at(100)] = latch(0x04, 0);
    line.pixels[at(200)] = latch(0x00, 0x04);
    let codes = video::latch_codes(&line.field(), x0);
    let row = &codes[video::PICTURE_W..2 * video::PICTURE_W];
    assert_eq!(row[5], None);
    assert_eq!(row[10], Some(4));
    assert_eq!(row[99], Some(4));
    assert_eq!(row[100], Some(2));
    assert_eq!(row[200], Some(1));
    assert_eq!(row[700], Some(1));
}

/// The fields of a short switcher session: a header line, framestore
/// writes, a bank flip and blank fields, each as a one-line canvas.
fn session_fields() -> Vec<Line> {
    let mut fields = Vec::new();
    let mut eh0 = Line::new(23);
    eh0.header(Header {
        r: 0x8000,
        g: 0x0100,
        b: 0xF000,
        i: 0x0F00,
    });
    fields.push(eh0);
    let bytes: Vec<u8> = (0..300).map(|k| 71 + (k % 129) as u8).collect();
    for (vpos, g) in [(24, 0xC000u16), (25, 0x8000), (26, 0xE000), (27, 0xA000)] {
        let mut data = Line::new(vpos);
        data.header(Header {
            r: 0xF000,
            g,
            b: (3u16 & 0xF) << 12,
            i: 0,
        });
        for (k, &byte) in bytes.iter().enumerate() {
            let nibble = if g & 0x4000 != 0 {
                byte >> 4
            } else {
                byte & 0xF
            };
            data.card_pixel(12 + k, nibble);
        }
        fields.push(data);
    }
    for vpos in [30, 31, 40, 41] {
        fields.push(Line::new(vpos));
    }
    fields
}

/// A host looking at the card (a capture, a status query) waits for
/// the compose thread, but it is no part of the emulated timeline: the
/// frames' sound still reaches Paula at the fields it does on a run
/// nobody looked at.
#[test]
fn a_host_query_does_not_move_when_the_sound_is_taken() {
    let fields = session_fields();
    let mut quiet = Toaster::new();
    let mut watched = Toaster::new();
    for (k, line) in fields.iter().enumerate() {
        let at = BeamPos {
            frame: k as u64,
            ..BeamPos::default()
        };
        let (mut unseen, mut seen) = (Vec::new(), Vec::new());
        quiet.submit(FieldInput::Owned(OwnedField::copy_of(&line.field())), at);
        quiet.take_audio(&mut unseen);
        watched.submit(FieldInput::Owned(OwnedField::copy_of(&line.field())), at);
        watched.take_audio(&mut seen);
        assert_eq!(seen.len(), unseen.len(), "field {k}");
        watched.settle_outputs();
        let _ = watched.control_status();
    }
}

/// The card's part of a save state: a card that takes a state saved
/// part-way through a session goes on exactly as the card it was
/// saved from, and saving leaves that one as it was.
#[test]
fn a_card_loaded_from_a_state_goes_on_as_the_saved_one_does() {
    let fields = session_fields();
    let feed = |card: &mut Toaster, k: usize, sound: &mut Vec<(f32, f32)>| {
        let at = BeamPos {
            frame: k as u64,
            ..BeamPos::default()
        };
        let field = OwnedField::copy_of(&fields[k].field());
        card.submit(FieldInput::Owned(field), at);
        let before = sound.len();
        card.take_audio(sound);
        sound.len() - before
    };
    let state = |card: &Toaster| VideoSlotDevice::save_state(card).expect("a state");
    for cut in [1, 2, 5, 6, 8] {
        let mut untouched = Toaster::new();
        let mut saved = Toaster::new();
        let (mut a, mut b, mut c) = (Vec::new(), Vec::new(), Vec::new());
        for k in 0..cut {
            feed(&mut untouched, k, &mut a);
            feed(&mut saved, k, &mut b);
        }
        let at_cut = state(&saved);
        // The card that loads it has been somewhere else entirely.
        let mut loaded = Toaster::new();
        for line in fields.iter().rev() {
            loaded.field_complete(&line.field(), BeamPos::default());
        }
        VideoSlotDevice::load_state(&mut loaded, Some(&at_cut));
        assert_eq!(
            state(&loaded),
            at_cut,
            "cut {cut}: a loaded state saves the same"
        );
        for k in cut..fields.len() {
            let n = feed(&mut untouched, k, &mut a);
            assert_eq!(
                feed(&mut saved, k, &mut b),
                n,
                "cut {cut} field {k}: saving"
            );
            assert_eq!(
                feed(&mut loaded, k, &mut c),
                n,
                "cut {cut} field {k}: loading"
            );
        }
        assert_eq!(a, b, "cut {cut}");
        assert_eq!(a[a.len() - c.len()..], c[..], "cut {cut}");
        let end = state(&untouched);
        assert!(state(&saved) == end, "cut {cut}: saving changed the card");
        assert!(
            state(&loaded) == end,
            "cut {cut}: the loaded card went elsewhere"
        );
        assert_eq!(untouched.stats(), loaded.stats());
        assert_eq!(untouched.card().elh, loaded.card().elh);
        assert_eq!(
            untouched.card().frame_store.data,
            loaded.card().frame_store.data
        );
    }
}

/// A save takes in every field handed over. If the compose thread
/// has died, the fields left in its queue are run by the save itself,
/// as the card's own next use runs them: the saved card and the live
/// one must not part ways.
#[test]
fn a_save_runs_the_fields_a_dead_compose_thread_left_queued() {
    let fields = session_fields();
    let field = || FieldInput::Owned(OwnedField::copy_of(&fields[0].field()));
    let mut reference = Toaster::new();
    reference.submit(field(), BeamPos::default());

    let mut card = Toaster::new();
    card.pipe.lock().stop = true;
    card.pipe.changed.notify_all();
    let worker = card.worker.as_ref().expect("a compose thread");
    for _ in 0..2000 {
        if worker.is_finished() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(worker.is_finished());
    card.submitted = 1;
    card.pipe.lock().queue.push_back(Job {
        id: 1,
        input: field(),
        at: BeamPos::default(),
        first_unscanned: 0,
        flyer: None,
    });
    assert!(
        VideoSlotDevice::save_state(&card) == VideoSlotDevice::save_state(&reference),
        "the queued field is missing from the saved card"
    );
    // And the live card is where the saved one says it is.
    assert_eq!(card.stats(), reference.stats());
}

/// A state that holds no card (it was saved without one, or before
/// cards were saved), or bytes that are no state, put the card back
/// as it powers on: nothing of the session that was left stays.
#[test]
fn a_state_without_a_card_leaves_the_card_as_at_power_on() {
    let power_on = VideoSlotDevice::save_state(&Toaster::new());
    for state in [None, Some(&b"not a state"[..])] {
        let mut card = Toaster::new();
        for line in &session_fields() {
            card.field_complete(&line.field(), BeamPos::default());
        }
        assert!(card.card().active);
        assert!(VideoSlotDevice::save_state(&card) != power_on);
        VideoSlotDevice::load_state(&mut card, state);
        assert!(!card.card().active);
        assert!(VideoSlotDevice::save_state(&card) == power_on);
    }
}

#[test]
fn fields_handed_to_the_compose_thread_give_the_outputs_of_fields_taken_at_once() {
    let fields = session_fields();
    // One card takes each field in before the next; the other is
    // handed them all with the thread free to run behind.
    let mut direct = Toaster::new();
    let mut piped = Toaster::new();
    let mut direct_sound = Vec::new();
    let mut piped_sound = Vec::new();
    for (k, line) in fields.iter().enumerate() {
        let at = BeamPos {
            frame: k as u64,
            ..BeamPos::default()
        };
        direct.field_complete(&line.field(), at);
        direct.take_audio(&mut direct_sound);
        piped.submit(FieldInput::Owned(OwnedField::copy_of(&line.field())), at);
        piped.take_audio(&mut piped_sound);
    }
    // Only frames whose fields are known finished have been taken.
    assert!(piped_sound.len() < direct_sound.len());
    piped.drain();
    piped.take_audio(&mut piped_sound);
    assert_eq!(direct_sound, piped_sound);
    assert_eq!(direct.stats(), piped.stats());
    assert_eq!(direct.card().elh, piped.card().elh);
    assert_eq!(
        direct.card().frame_store.data,
        piped.card().frame_store.data
    );
    let program = |card: &Toaster| card.video_outputs()[1].snapshot().expect("a frame");
    assert_eq!(program(&direct), program(&piped));
    // The written line is in both.
    assert_eq!(direct.frame_store_byte(0, false, 7, 12), 71);
    assert_eq!(piped.frame_store_byte(0, false, 7, 12), 71);
}

#[test]
fn a_digitised_picture_is_encoded_when_the_bank_is_read() {
    let mut toaster = Toaster::new();
    let line = Line::new(30);
    let mut card = toaster.card();
    // IS = input 1 (unplugged: black), OBR clear: bank 0 records.
    card.elh[ELH_EH0 + 1].r = 0x4000;
    card.elh[ELH_EH0].r = 0x8000;
    card.record_input(&line.field());
    card.record_input(&line.field());
    assert_eq!(card.stats.grabs_encoded, 0);
    // Reading the bank encodes the latest picture, once.
    let byte = card.frame_store_byte(0, false, 1, 12);
    assert_eq!(card.stats.grabs_encoded, 1);
    let mut n = [0u8; 752];
    let mut c = [0u8; 752];
    composite::encode_line(&[video::black(); 752], 0, &mut n, &mut c);
    assert_eq!(byte, n[0]);
    assert_eq!(card.frame_store_byte(0, true, 480, 700), c[688]);
    assert_eq!(card.stats.grabs_encoded, 1);
    // The other bank was not touched.
    assert_eq!(card.frame_store_byte(1, false, 1, 12), 0);
    // A write to the bank lands over the digitised picture, as it would
    // have had the picture been encoded at once: the written line is
    // the Amiga's, the others the picture's.
    card.record_input(&line.field());
    drop(card);
    write_line(&mut toaster, false, 1, &[9, 8, 7]);
    assert_eq!(toaster.frame_store_byte(0, false, 1, 12), 9);
    assert_eq!(toaster.frame_store_byte(0, false, 3, 15), n[3]);
    assert_eq!(toaster.stats().grabs_encoded, 2);
}

impl Line {
    fn new(vpos: u32) -> Self {
        Self {
            pixels: vec![u32::from_le_bytes([0, 0, 0, 0xFF]); WIDTH],
            vpos,
        }
    }

    fn field(&self) -> FieldPixels<'_> {
        FieldPixels {
            pixels: &self.pixels,
            width: WIDTH,
            rows: 1,
            visible_start_vpos: self.vpos,
            first_cck: SLOT_FIRST_CCK,
            canvas_scale: 1,
            long_frame: true,
            interlaced: false,
            valid_rows: 1,
        }
    }

    /// A pixel the card's four lines read as `v` (R, G<<1, B<<2, I<<3).
    fn wire(v: u8) -> u32 {
        let r = if v & 1 != 0 { 0x02 } else { 0 } | if v & 4 != 0 { 0x01 } else { 0 };
        let g = if v & 2 != 0 { 0x02 } else { 0 } | if v & 8 != 0 { 0x01 } else { 0 };
        u32::from_le_bytes([r, g, 0, 0xFF])
    }

    /// Draw the header sprite: sixteen lo-res pixels from `$5C`.
    fn header(&mut self, h: Header) {
        let x0 = self
            .field()
            .column_for_lores(HEADER_LORES + crate::bus::SPRITE_OUTPUT_DELAY_LORES);
        for k in 0..16 {
            let bit = 15 - k;
            let v = ((h.r >> bit) & 1)
                | ((h.g >> bit) & 1) << 1
                | ((h.b >> bit) & 1) << 2
                | ((h.i >> bit) & 1) << 3;
            let col = (x0 + 2 * k) as usize;
            self.pixels[col] = Self::wire(v as u8);
            self.pixels[col + 1] = Self::wire(v as u8);
        }
    }

    /// Put nibble `v` on card pixel `p`.
    fn card_pixel(&mut self, p: usize, v: u8) {
        let col = (Card::pixel0_column(&self.field()) + p as i64) as usize;
        self.pixels[col] = Self::wire(v);
    }

    /// The read bitmap: quads from 3 labelled `label`, 18 quads each.
    fn labels(&mut self, first_label: u8) {
        for quad in 3..192usize {
            let label = first_label + ((quad - 3) / 18) as u8;
            for j in 0..4 {
                let bit = 3 - j;
                let v = ((label >> 4 >> bit) & 1) << 3 | ((label >> bit) & 1) << 2;
                self.card_pixel(quad * 4 + j, v);
            }
        }
    }
}

/// Framestore line `line` written with `bytes` from card pixel 12 by
/// the four passes the switcher makes, with `OBR` as given.
fn write_line(card: &mut Toaster, obr: bool, line: usize, bytes: &[u8]) {
    write_line_pass(card, obr, line, bytes, true, 0);
}

/// One write of `line`, with EH0's `LUST` as given and the bitmap
/// drawn `left` pixels left of card pixel 12 (a zipper's second pass).
fn write_line_pass(
    card: &mut Toaster,
    obr: bool,
    line: usize,
    bytes: &[u8],
    lust: bool,
    left: usize,
) {
    let mut eh0 = Line::new(23);
    eh0.header(Header {
        r: 0x8000 | if obr { EH0R_OBR } else { 0 },
        g: 0x0100,
        b: 0xF000,
        i: if lust { 0x0F00 } else { 0x0E00 },
    });
    card.field_complete(&eh0.field(), BeamPos::default());
    let l = (line / 2) as u16;
    let field_ii = line.is_multiple_of(2);
    for (vpos, g) in [(24, 0xC000u16), (25, 0x8000), (26, 0xE000), (27, 0xA000)] {
        let mut data = Line::new(vpos);
        data.header(Header {
            r: 0xF000,
            g: g | if field_ii { 0x1000 } else { 0 },
            b: (l & 0xF) << 12,
            i: (l >> 4) << 12,
        });
        for (k, &byte) in bytes.iter().enumerate().skip(left) {
            let nibble = if g & 0x4000 != 0 {
                byte >> 4
            } else {
                byte & 0xF
            };
            data.card_pixel(12 + k - left, nibble);
        }
        card.field_complete(&data.field(), BeamPos::default());
    }
}

fn read_request(line: usize, pixel: u8, complemented: bool) -> Header {
    let l = (line / 2) as u16;
    let field_ii = line.is_multiple_of(2);
    Header {
        r: [0x0000, 0x2000, 0xC000, 0xE000][usize::from(pixel)],
        g: if complemented { 0x2000 } else { 0 } | if field_ii { 0x1000 } else { 0 },
        b: (l & 0xF) << 12,
        i: (l >> 4) << 12,
    }
}

#[test]
fn a_zipper_writes_second_pass_lands_where_the_first_did() {
    // The switcher's AA framestore load: the whole line with LUST set,
    // then again with LUST clear and the bitmap scrolled 16 pixels
    // left. Both passes must agree on every sample.
    let mut card = Toaster::new();
    let bytes: Vec<u8> = (0..756).map(|k| 71 + (k % 129) as u8).collect();
    write_line_pass(&mut card, false, 100, &bytes, true, 0);
    write_line_pass(&mut card, false, 100, &bytes, false, 16);
    let card = card.card();
    let stored = card.frame_store.line(0, false, 100);
    for k in [0usize, 4, 15, 16, 300, 731, 739] {
        assert_eq!(stored[12 + k], bytes[k], "sample {}", 12 + k);
    }
}

#[test]
fn a_written_line_reads_back_by_quad_label() {
    let mut card = Toaster::new();
    let ramp: Vec<u8> = (71..=200).collect();
    write_line(&mut card, false, 504, &ramp);
    assert!(card.card().active);
    assert_eq!(card.frame_store_byte(0, false, 504, 12), 71);
    assert_eq!(card.frame_store_byte(0, true, 504, 13), 72);

    // The switcher reads bank 0 with OBR set.
    let mut eh0 = Line::new(23);
    eh0.header(Header {
        r: 0x8000 | EH0R_OBR,
        g: 0x0100,
        b: 0xF000,
        i: 0x0F00,
    });
    card.field_complete(&eh0.field(), BeamPos::default());
    let mut read = Line::new(24);
    read.header(read_request(504, 0, false));
    read.labels(3);
    let at = |hpos| BeamPos {
        vpos: 24,
        hpos,
        ..BeamPos::default()
    };
    // HS.default's ten read positions land in labels 3..=12.
    let expected: Vec<u8> = (0..10).map(|i| 71 + 4 * i).collect();
    let got: Vec<u8> = [58, 75, 93, 111, 129, 147, 166, 183, 201, 216]
        .into_iter()
        .map(|h| card.card().port_byte(&read.field(), at(h)).unwrap())
        .collect();
    assert_eq!(got, expected);
    // Five clocks later (the CIA access's E-clock slip) still reads
    // the same labels.
    let late: Vec<u8> = [63, 80, 98, 116, 134, 152, 171, 188, 206, 221]
        .into_iter()
        .map(|h| card.card().port_byte(&read.field(), at(h)).unwrap())
        .collect();
    assert_eq!(late, expected);
    // Pixel 1 of each quad, from the complemented copy.
    let mut read = Line::new(25);
    read.header(read_request(504, 1, true));
    read.labels(3);
    assert_eq!(
        card.card().port_byte(
            &read.field(),
            BeamPos {
                vpos: 25,
                hpos: 75,
                ..BeamPos::default()
            }
        ),
        Some(72 + 4)
    );
}

#[test]
fn banks_follow_the_obr_bit() {
    let mut card = Toaster::new();
    write_line(&mut card, true, 100, &[1, 2, 3]);
    assert_eq!(card.frame_store_byte(1, false, 100, 12), 1);
    assert_eq!(card.frame_store_byte(0, false, 100, 12), 0);
    write_line(&mut card, false, 100, &[9]);
    assert_eq!(card.frame_store_byte(0, false, 100, 12), 9);
    // Reads come from the other bank than OBR names: the last EH0 above
    // cleared OBR, so bank 1 is read.
    let mut read = Line::new(24);
    read.header(read_request(100, 0, false));
    read.labels(3);
    assert_eq!(
        card.card().port_byte(
            &read.field(),
            BeamPos {
                vpos: 24,
                hpos: 58,
                ..BeamPos::default()
            }
        ),
        Some(1)
    );
}

#[test]
fn input_lines_follow_the_wired_rgb_bits() {
    let px = |r: u8, g: u8, b: u8| u32::from_le_bytes([r, g, b, 0xFF]);
    // The switcher's write palette: one line per bitplane.
    assert_eq!(lines_of(px(0x04, 0, 0)), 1);
    assert_eq!(lines_of(px(0, 0, 0x04)), 2);
    assert_eq!(lines_of(px(0x01, 0, 0)), 4);
    assert_eq!(lines_of(px(0, 0x01, 0)), 8);
    // The alternate bits of each pair, and the sprite palette's pairs.
    assert_eq!(lines_of(px(0x02, 0, 0)), 1);
    assert_eq!(lines_of(px(0, 0x02, 0)), 2);
    assert_eq!(lines_of(px(0, 0, 0x01)), 4);
    assert_eq!(lines_of(px(0, 0, 0x02)), 8);
    assert_eq!(lines_of(px(0x66, 0, 0)), 1);
    assert_eq!(lines_of(px(0, 0x22, 0x44)), 2);
    // High bits of the guns carry nothing.
    assert_eq!(lines_of(px(0xF8, 0xFC, 0xF8)), 0);
}

#[test]
fn header_kinds_decode() {
    let h = |r, g, b, i| Header { r, g, b, i };
    // DoSafeWrite's filler: no ELH, no write bit, line number 255.
    assert_eq!(h(0xF000, 0x0000, 0xF000, 0xF000).kind(), LineKind::Dummy);
    // The read self-test's line 504 (L = 252) decodes like any other.
    assert_eq!(
        h(0xF000, 0xD000, 0xC000, 0xF000).kind(),
        LineKind::Write {
            line: 504,
            high_nibble: true,
            complemented: false
        }
    );
    // tblI/tblII write codes for toaster line L = $21: field I line 67.
    assert_eq!(
        h(0xF000, 0xC000, 0x1000, 0x2000).kind(),
        LineKind::Write {
            line: 67,
            high_nibble: true,
            complemented: false
        }
    );
    assert_eq!(
        h(0xF000, 0xB000, 0x1000, 0x2000).kind(),
        LineKind::Write {
            line: 66,
            high_nibble: false,
            complemented: true
        }
    );
    // Read request, pixel 2 of each quad, complemented, field II.
    assert_eq!(
        h(0xC000, 0x3000, 0x1000, 0x2000).kind(),
        LineKind::Read {
            line: 66,
            pixel: 2,
            complemented: true
        }
    );
    assert_eq!(
        h(0x8100, 0x0500, 0xFF00, 0xFF00).kind(),
        LineKind::Elh { index: 0 }
    );
    // EH6's page is in the B word: EH63.
    assert_eq!(h(0xB000, 0, 0x0300, 0).kind(), LineKind::Elh { index: 9 });
    assert_eq!(h(0x7000, 0, 0, 0).kind(), LineKind::Elh { index: ELH_EH7 });
}
