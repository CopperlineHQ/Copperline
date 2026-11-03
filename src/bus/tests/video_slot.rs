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

/// One grabbed row: the field's start line, row stride, first colour clock
/// and the pixels.
type GrabbedRow = (u32, usize, i32, Vec<u32>);

/// Collects one row of every completed field.
struct RowGrab {
    rows: Arc<Mutex<Vec<GrabbedRow>>>,
    vpos: u32,
}

impl VideoSlotDevice for RowGrab {
    fn name(&self) -> &'static str {
        "rowgrab"
    }

    fn field_complete(&mut self, field: &FieldPixels<'_>, _at: BeamPos) {
        if let Some(row) = field.row(self.vpos) {
            self.rows.lock().unwrap().push((
                field.visible_start_vpos,
                field.width,
                field.first_cck,
                row.to_vec(),
            ));
        }
    }
}

fn red_columns(row: &[u32]) -> Vec<usize> {
    row.iter()
        .enumerate()
        .filter(|(_, p)| (**p & 0xFF) >= 0xF0 && ((**p >> 8) & 0xFF) < 0x10)
        .map(|(i, _)| i)
        .collect()
}

/// The Toaster's per-line header is sprite 0 at HSTART $5B, seven lo-res
/// pixels left of the display framebuffer's first column: the slot canvas
/// starts further left and shows it.
#[test]
fn slot_canvas_shows_a_sprite_left_of_the_display_capture() {
    let mut bus = empty_bus();
    let rows = Arc::new(Mutex::new(Vec::new()));
    bus.attach_video_slot(Box::new(RowGrab {
        rows: Arc::clone(&rows),
        vpos: 0x40,
    }));
    advance_to_next_frame(&mut bus);
    set_color0(&mut bus, 0);
    assert!(!bus.custom_write(0x08E, 2, 0x2C40)); // DIWSTRT
    assert!(!bus.custom_write(0x090, 2, 0x2CD1)); // DIWSTOP
    assert!(!bus.custom_write(0x100, 2, 0x0200)); // BPLCON0: colour on
    assert!(!bus.custom_write(0x1A2, 2, 0x0F00)); // COLOR17 red
    advance_to(&mut bus, 0x3F, 0x10);
    // Manual sprite 0: vstart $40, vstop $41, hstart $5B.
    assert!(!bus.custom_write(0x140, 2, 0x402D));
    assert!(!bus.custom_write(0x142, 2, 0x4101));
    assert!(!bus.custom_write(0x146, 2, 0x0000));
    assert!(!bus.custom_write(0x144, 2, 0x8001)); // leftmost and rightmost pixel
                                                  // Denise shows a line's sprites only once its first BPL1DAT access
                                                  // has happened (the Toaster opens its display window on the sprite
                                                  // table's first line for this reason); a manual write stands in.
    advance_to(&mut bus, 0x40, 0x08);
    assert!(!bus.custom_write(0x110, 2, 0x0000));
    advance_to_next_frame(&mut bus);

    let rows = rows.lock().unwrap();
    let (_, width, first_cck, row) = rows.last().expect("a completed field");
    let field = FieldPixels {
        pixels: row,
        width: *width,
        rows: 1,
        visible_start_vpos: 0x40,
        first_cck: *first_cck,
        canvas_scale: 1,
        long_frame: false,
        interlaced: false,
        valid_rows: 1,
    };
    let red = red_columns(row);
    let left = field.column_for_lores(0x5B);
    let display_left = field.column_for_lores(0x62);
    assert!(
        left < display_left,
        "the header starts left of the display capture"
    );
    assert_eq!(
        red.len(),
        4,
        "two lo-res pixels of sprite, two hi-res columns each: {red:?}"
    );
    // The serializer's first pixel lands one lo-res pixel after the
    // comparator match (SPRITE_OUTPUT_DELAY_LORES).
    let delay = 2 * crate::bus::SPRITE_OUTPUT_DELAY_LORES as usize;
    assert_eq!(red[0], left as usize + delay);
    assert_eq!(red[2], left as usize + delay + 30);
}

/// A colour change late on the line, past the display framebuffer's right
/// edge, still reaches the slot canvas.
#[test]
fn slot_canvas_extends_past_the_display_captures_right_edge() {
    let mut bus = empty_bus();
    let rows = Arc::new(Mutex::new(Vec::new()));
    bus.attach_video_slot(Box::new(RowGrab {
        rows: Arc::clone(&rows),
        vpos: 0x60,
    }));
    advance_to_next_frame(&mut bus);
    set_color0(&mut bus, 0);
    advance_to(&mut bus, 0x60, 0xE0);
    set_color0(&mut bus, RED);
    advance_to(&mut bus, 0x61, 0x10);
    set_color0(&mut bus, 0);
    advance_to_next_frame(&mut bus);

    let rows = rows.lock().unwrap();
    let (_, width, first_cck, row) = rows.last().expect("a completed field");
    let field = FieldPixels {
        pixels: row,
        width: *width,
        rows: 1,
        visible_start_vpos: 0x60,
        first_cck: *first_cck,
        canvas_scale: 1,
        long_frame: false,
        interlaced: false,
        valid_rows: 1,
    };
    assert!(
        *width as i64 > field.column_for_hpos(0xE8),
        "the canvas reaches clock $E8"
    );
    let red = red_columns(row);
    eprintln!(
        "width {} red {:?}..{:?} n={}",
        width,
        red.first(),
        red.last(),
        red.len()
    );
    assert_eq!(field.rgb12_at_beam(0x60, 0xD0, 0), Some(0));
    assert_eq!(field.rgb12_at_beam(0x60, 0xF0, 0), Some(RED));
}

/// A fitted card moves the capture up to the first line sprite DMA runs
/// on, so the line headers software puts there are captured.
#[test]
fn fitting_a_card_captures_from_the_first_sprite_line() {
    let mut bus = empty_bus();
    bus.set_video_standard(crate::chipset::agnus::VideoStandard::Ntsc);
    advance_to_next_frame(&mut bus);
    advance_to_next_frame(&mut bus);
    assert_eq!(
        bus.frame_visible_start_vpos(),
        0x1C,
        "deep-overscan default"
    );

    let rows = Arc::new(Mutex::new(Vec::new()));
    bus.attach_video_slot(Box::new(RowGrab {
        rows: Arc::clone(&rows),
        vpos: 0x16,
    }));
    advance_to_next_frame(&mut bus);
    advance_to_next_frame(&mut bus);
    assert_eq!(bus.frame_visible_start_vpos(), 0x14);
    let rows = rows.lock().unwrap();
    assert_eq!(
        rows.last().map(|r| r.0),
        Some(0x14),
        "the field reports its start"
    );

    bus.detach_video_slot();
    advance_to_next_frame(&mut bus);
    advance_to_next_frame(&mut bus);
    assert_eq!(bus.frame_visible_start_vpos(), 0x1C);
}
