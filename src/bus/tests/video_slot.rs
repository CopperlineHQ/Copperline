// Video-slot tap: completed-field delivery, the live in-progress view, and
// the card's drive of the parallel data pins.
use super::*;
use crate::videoslot::{BeamPos, FieldPixels, LiveField, VideoSlotDevice};
use std::sync::{Arc, Mutex};

const RED: u16 = 0x0F00;
const GREEN: u16 = 0x00F0;
/// A colour clock well inside the standard picture.
const PROBE_HPOS: u32 = 0x80;

fn advance_to(bus: &mut Bus, vpos: u32, hpos: u32) {
    let start_frame = bus.emulated_frames();
    // At most one wrap: the target is either later on this field's beam or
    // on the next field.
    while !(bus.agnus.vpos == vpos && bus.agnus.hpos >= hpos)
        && bus.emulated_frames() <= start_frame + 1
    {
        bus.advance_devices(1);
    }
    assert_eq!(bus.agnus.vpos, vpos, "beam reached the target line");
}

fn advance_to_next_frame(bus: &mut Bus) {
    let frame = bus.emulated_frames();
    while bus.emulated_frames() == frame {
        bus.advance_devices(16);
    }
}

fn set_color0(bus: &mut Bus, rgb: u16) {
    assert!(!bus.custom_write(0x180, 2, u64::from(rgb)));
}

#[derive(Default)]
struct Seen {
    fields: Vec<(usize, u32, Vec<Option<u16>>)>,
    reads: Vec<(BeamPos, Option<u16>, usize)>,
}

struct Probe {
    seen: Arc<Mutex<Seen>>,
    /// Lines whose border colour a completed field is sampled at.
    sample_lines: Vec<u32>,
    /// Sample this many lines above the beam on a parallel read.
    read_lag: u32,
    wants_fields: bool,
}

impl VideoSlotDevice for Probe {
    fn name(&self) -> &'static str {
        "probe"
    }

    fn wants_completed_fields(&self) -> bool {
        self.wants_fields
    }

    fn field_complete(&mut self, field: &FieldPixels<'_>, _at: BeamPos) {
        let samples = self
            .sample_lines
            .iter()
            .map(|&v| field.rgb12_at_beam(v, PROBE_HPOS, 0))
            .collect();
        self.seen
            .lock()
            .unwrap()
            .fields
            .push((field.rows, field.visible_start_vpos, samples));
    }

    fn parallel_read(&mut self, live: &mut LiveField<'_>, _ddrb: u8) -> Option<u8> {
        let beam = live.beam();
        let field = live.pixels();
        let sample = field.rgb12_at_beam(beam.vpos - self.read_lag, PROBE_HPOS, 0);
        self.seen
            .lock()
            .unwrap()
            .reads
            .push((beam, sample, field.valid_rows));
        // Answer with the red and green nibbles of the sampled pixel.
        sample.map(|rgb| ((rgb >> 4) & 0xFF) as u8)
    }
}

fn probe(
    bus: &mut Bus,
    sample_lines: Vec<u32>,
    read_lag: u32,
    wants_fields: bool,
) -> Arc<Mutex<Seen>> {
    let seen = Arc::new(Mutex::new(Seen::default()));
    bus.attach_video_slot(Box::new(Probe {
        seen: Arc::clone(&seen),
        sample_lines,
        read_lag,
        wants_fields,
    }));
    seen
}

#[test]
fn completed_fields_show_beam_timed_border_colours() {
    let mut bus = empty_bus();
    let seen = probe(&mut bus, vec![0x50, 0x70, 0xA0], 0, true);
    advance_to_next_frame(&mut bus);
    seen.lock().unwrap().fields.clear();

    // One field: black down to $60, red to $90, green below.
    set_color0(&mut bus, 0);
    advance_to(&mut bus, 0x60, 0x10);
    set_color0(&mut bus, RED);
    advance_to(&mut bus, 0x90, 0x10);
    set_color0(&mut bus, GREEN);
    advance_to_next_frame(&mut bus);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.fields.len(), 1, "one completed field per wrap");
    let (rows, start, samples) = &seen.fields[0];
    assert!(*rows > 0x100, "a full field was rendered: {rows} rows");
    assert!(*start < 0x50);
    assert_eq!(samples, &vec![Some(0), Some(RED), Some(GREEN)]);
}

#[test]
fn opting_out_of_completed_fields_skips_the_frame_render() {
    let mut bus = empty_bus();
    let seen = probe(&mut bus, vec![0x50], 0, false);
    advance_to_next_frame(&mut bus);
    advance_to_next_frame(&mut bus);
    assert!(seen.lock().unwrap().fields.is_empty());
}

#[test]
fn parallel_read_sees_the_field_in_progress() {
    let mut bus = empty_bus();
    let seen = probe(&mut bus, Vec::new(), 4, false);
    bus.cia_a.write(REG_DDRB, 0x00); // all data pins are inputs
    advance_to_next_frame(&mut bus);

    set_color0(&mut bus, 0);
    advance_to(&mut bus, 0x60, 0x10);
    set_color0(&mut bus, RED);

    // Four lines below the colour change the card samples line $66 of this
    // same field, which is already red.
    advance_to(&mut bus, 0x6A, 0x20);
    let v = bus.cia_a_read(0xBFE101, 1) as u8;
    assert_eq!(v, 0xF0, "red nibble F, green nibble 0");
    // A second read on the same line reuses the live render.
    let v2 = bus.cia_a_read(0xBFE101, 1) as u8;
    assert_eq!(v2, 0xF0);
    assert_eq!(bus.video_slot_live_renders(), 1);

    set_color0(&mut bus, GREEN);
    advance_to(&mut bus, 0x96, 0x20);
    let v3 = bus.cia_a_read(0xBFE101, 1) as u8;
    assert_eq!(v3, 0x0F, "line $92 is green");
    assert_eq!(bus.video_slot_live_renders(), 2);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.reads.len(), 3);
    let (beam, _, valid_rows) = seen.reads[2];
    assert_eq!(beam.vpos, 0x96);
    // Only rows above the beam are final.
    assert!(valid_rows <= beam.vpos as usize);
}

#[test]
fn card_cannot_override_output_pins() {
    let mut bus = empty_bus();
    let _seen = probe(&mut bus, Vec::new(), 2, false);
    advance_to_next_frame(&mut bus);
    set_color0(&mut bus, RED | GREEN); // card answers $FF
    advance_to(&mut bus, 0x80, 0x20);

    bus.cia_a.write(REG_DDRB, 0xF0);
    bus.cia_a.write(REG_PRB, 0x50);
    let v = bus.cia_a_read(0xBFE101, 1) as u8;
    assert_eq!(v, 0x5F, "high nibble is the CIA's output, low the card's");
}

#[test]
fn video_slot_card_blocks_runahead() {
    let mut bus = empty_bus();
    // No other host-backed blocker in the way.
    bus.rtc_present = false;
    assert_eq!(bus.runahead_host_block_reason(), None);
    let _seen = probe(&mut bus, Vec::new(), 0, false);
    assert_eq!(bus.runahead_host_block_reason(), Some("video-slot card"));
    bus.detach_video_slot();
    assert_eq!(bus.runahead_host_block_reason(), None);
}

struct SyncCard;

impl VideoSlotDevice for SyncCard {
    fn name(&self) -> &'static str {
        "sync"
    }

    fn wants_completed_fields(&self) -> bool {
        false
    }

    fn supplies_sync(&self) -> bool {
        true
    }
}

fn vhposr(bus: &mut Bus) -> u16 {
    bus.custom_read(0xDFF006, 2) as u16
}

#[test]
fn a_card_supplying_sync_keeps_ersy_beam_live() {
    let mut bus = empty_bus();
    bus.attach_video_slot(Box::new(SyncCard));
    assert!(!bus.custom_write(0x100, 2, 0x0202)); // BPLCON0: COLOR + ERSY
    let a = vhposr(&mut bus);
    bus.advance_devices(64);
    assert_ne!(vhposr(&mut bus), a, "beam slaved to the card's sync");

    // Without the card (and no configured genlock) ERSY freezes again.
    bus.detach_video_slot();
    assert!(!bus.custom_write(0x100, 2, 0x0200));
    assert!(!bus.custom_write(0x100, 2, 0x0202));
    let a = vhposr(&mut bus);
    bus.advance_devices(64);
    assert_eq!(vhposr(&mut bus), a);
}

#[test]
fn configured_genlock_outlives_a_sync_card() {
    let mut bus = empty_bus();
    bus.set_genlock(true);
    bus.attach_video_slot(Box::new(SyncCard));
    bus.detach_video_slot();
    assert!(!bus.custom_write(0x100, 2, 0x0202));
    let a = vhposr(&mut bus);
    bus.advance_devices(64);
    assert_ne!(vhposr(&mut bus), a);
}
