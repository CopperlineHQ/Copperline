// SPDX-License-Identifier: GPL-3.0-or-later

//! Unit tests for the chip bus: split out of `bus.rs` for size, they
//! are the same `bus::tests` module and keep full access to the
//! parent's private items via `super::`.

use super::{
    bitplane_slot_plan_bplcon0_key, bitplane_words_per_row, clipped_display_rows_before_visible,
    display_window_contains_vpos, diw_h_start, diw_h_stop, diw_v_start, diw_v_stop,
    framebuffer_x_for_live_collision_hpos, live_bitplane_collision_bits, live_display_window_x,
    live_manual_sprite_collision_sources, live_sprite_playfield_collision_bits_in_range,
    live_sprite_sprite_collision_bits, sprite_hstart_for_fmode, visible_start_vpos_for_diw,
    BeamChipRamWrite, BeamRegisterWrite, BeamWriteSource, BitplaneBplcon0Delay, Bus,
    CapturedBitplaneRow, CapturedSpriteLine, ChipBusOwner, CpuBusAccessKind, DeviceClock,
    DisplaySpriteDmaState, DisplaySpriteLineData, FrameBusTrace, LiveCollisionControl,
    LiveCollisionLineReplay, LiveSpriteCollisionSource, PortDevice, RenderRegisterSnapshot,
    BLITTER_SLOWDOWN_CPU_MISS_LIMIT, BLTCON0_USE_A, BLTCON0_USE_C, BLTCON0_USE_D, BLTCON1_DOFF,
    BLTCON1_LINE, BPLCON0_ECSENA, BPLCON3_BRDRBLNK, BPLCON3_BRDSPRT, BPLCON3_SPRES_HIRES,
    DENISE_HPOS_LAG_CCK, DMACON_BLTEN, DMACON_BLTPRI, DMACON_BPLEN, DMACON_SPREN,
    PAL_SPRITE_DMA_FIRST_ACTIVE_VPOS, RENDER_COPPER_WAIT_HPOS_FB0, RENDER_DIW_HSTART_FB0,
    RENDER_MIN_OVERSCAN_START_VPOS, RENDER_VISIBLE_LINES, RENDER_VISIBLE_START_VPOS,
    SPRITE_DMA_SLOT1_HPOS, SPRITE_OUTPUT_DELAY_LORES,
};
use crate::audio::AudioSink;
use crate::chipset::agnus::{
    AgnusRevision, AgnusTick, VideoStandard, BEAMCON0_DUAL, BEAMCON0_HARDDIS, BEAMCON0_PAL,
    BEAMCON0_VARBEAMEN, COLORCLOCKS_PER_LINE, NTSC_LONG_COLORCLOCKS_PER_LINE, PAL_LINES,
};
use crate::chipset::cia::{
    REG_CRA, REG_CRB, REG_DDRA, REG_DDRB, REG_ICR, REG_PRA, REG_PRB, REG_TAHI, REG_TALO, REG_TBHI,
    REG_TBLO, REG_TODHI, REG_TODLO, REG_TODMID,
};
use crate::chipset::copper::{CopperWait, DMACON_COPEN};
use crate::chipset::denise::{rgb12_to_rgba8, DeniseRevision, DiwHigh, COLOR_TRANSPARENCY_BIT};
use crate::chipset::paula::{
    Paula, DMACON_DMAEN, INT_AUD0, INT_BLIT, INT_COPER, INT_EXTER, INT_MASTER, INT_PORTS,
    INT_VERTB, PAULA_CLOCK_HZ,
};
use crate::floppy::{FloppyController, ADF_SIZE};
use crate::memory::Memory;
use crate::serial::SerialSink;
use crate::video::beam::BeamEventIndex;
use crate::video::{bitplane, FB_HEIGHT, FB_PIXELS, FB_WIDTH};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const STANDARD_DIW_HSTART: i32 = 0x81;
const STANDARD_VISIBLE_X0: usize = ((STANDARD_DIW_HSTART - RENDER_DIW_HSTART_FB0) * 2) as usize;
// Mirrors `COLOR_WRITE_HPOS_FB0` in src/video/bitplane.rs. COLORxx writes
// reach Denise's final palette/output phase one lores pixel ahead of
// writes that feed delayed shifter/control paths.
const RENDER_COLOR_WRITE_HPOS_FB0: u32 = 0x35;
static BUS_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn frame_analyzer_records_owner_spans_and_blitter_wait_cck() {
    let mut trace = FrameBusTrace::default();
    trace.reset_for_frame(7, 0.140, 4, 16, 1, 2, false);

    trace.record(1, 2, 3, ChipBusOwner::Bitplane, false);
    trace.record(1, 5, 2, ChipBusOwner::Copper, true);
    trace.record(1, 7, 2, ChipBusOwner::Blitter, true);
    trace.record(8, 0, 4, ChipBusOwner::Cpu, true);
    trace.finish_window(2, 1);

    assert_eq!(trace.frame, 7);
    assert_eq!(trace.rows, 4);
    assert_eq!(trace.cols, 16);
    assert_eq!(trace.visible_start_vpos, 2);
    assert_eq!(trace.visible_lines, 1);
    assert!(trace.has_samples());

    let row = trace.owner_row(1).expect("recorded beam row");
    assert_eq!(row[1], b'.');
    assert_eq!(&row[2..5], &[b'B'; 3]);
    assert_eq!(&row[5..7], &[b'C'; 2]);
    assert_eq!(&row[7..9], &[b'L'; 2]);
    assert_eq!(trace.owner_code_at(3, 0), b'.');

    assert_eq!(
        trace.owner_cck[ChipBusOwner::Bitplane.accounting_index()],
        3
    );
    assert_eq!(trace.owner_cck[ChipBusOwner::Copper.accounting_index()], 2);
    assert_eq!(trace.owner_cck[ChipBusOwner::Blitter.accounting_index()], 2);
    assert_eq!(trace.owner_cck[ChipBusOwner::Cpu.accounting_index()], 0);
    assert_eq!(trace.blitter_busy_cck, 4);
    assert_eq!(
        trace.blitter_starve_cck[ChipBusOwner::Copper.accounting_index()],
        2
    );
    assert_eq!(
        trace.blitter_starve_cck[ChipBusOwner::Blitter.accounting_index()],
        0
    );
}

#[test]
fn bitplane_slot_plan_key_ignores_denise_only_bplcon0_bits() {
    let fetch_shape = 0x5000;
    let denise_only_bits = 0x0F9F;

    assert_eq!(
        bitplane_slot_plan_bplcon0_key(fetch_shape, false),
        bitplane_slot_plan_bplcon0_key(fetch_shape | denise_only_bits, false)
    );
}

#[test]
fn bitplane_slot_plan_key_tracks_fetch_shape_bplcon0_bits() {
    let base = 0x5000;

    assert_ne!(
        bitplane_slot_plan_bplcon0_key(base, false),
        bitplane_slot_plan_bplcon0_key(base ^ 0x1000, false),
        "plane-count changes alter Agnus bitplane fetch ownership"
    );
    assert_ne!(
        bitplane_slot_plan_bplcon0_key(base, false),
        bitplane_slot_plan_bplcon0_key(base | 0x8000, false),
        "hires changes alter bitplane fetch cadence"
    );
    assert_ne!(
        bitplane_slot_plan_bplcon0_key(base, false),
        bitplane_slot_plan_bplcon0_key(base | 0x0040, false),
        "shres changes alter bitplane fetch cadence"
    );

    assert_eq!(
        bitplane_slot_plan_bplcon0_key(base, false),
        bitplane_slot_plan_bplcon0_key(base | 0x0010, false),
        "OCS/ECS do not decode AGA BPU3"
    );
    assert_ne!(
        bitplane_slot_plan_bplcon0_key(base, true),
        bitplane_slot_plan_bplcon0_key(base | 0x0010, true),
        "AGA BPU3 changes bitplane fetch ownership"
    );
}

#[test]
fn bitplane_slot_plan_cache_keeps_recent_fetch_shapes() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x00D0;

    let shapes = [0x1000, 0x2000, 0x5000];
    for &bplcon0 in &shapes {
        assert!(bus.bitplane_slot_plan_for_bplcon0(bplcon0).is_some());
    }
    assert!(bus.bitplane_slot_plan_for_bplcon0(shapes[0]).is_some());

    let cache = bus.bitplane_slot_plan_cache.entries_snapshot();
    let keys = shapes.map(|bplcon0| bitplane_slot_plan_bplcon0_key(bplcon0, false));
    assert_eq!(
        bus.bitplane_slot_plan_cache
            .last_hit_entry()
            .map(|(key, _)| key.bplcon0),
        Some(keys[0]),
        "a cache hit should become the next lookup fast path without moving entries"
    );
    for key in keys {
        assert!(
            cache
                .iter()
                .any(|entry| matches!(entry, Some((cached, _)) if cached.bplcon0 == key)),
            "recent fetch shape {key:#06X} should remain cached"
        );
    }
}

#[test]
fn wide_bitplane_hot_line_matches_dynamic_slot_arbitration() {
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    bus.agnus.write_fmode(0x0003);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0030;
    bus.denise.ddfstop = 0x00D0;
    bus.denise.bplcon0 = 0x5000;

    for hpos in 0..bus.agnus.current_line_cck() {
        assert_eq!(
            bus.bitplane_slot_active_at(bus.agnus.vpos, hpos),
            bus.dynamic_bitplane_slot_active_at(bus.agnus.vpos, hpos),
            "wide-FMODE slot mismatch at hpos {hpos:#04x}"
        );
    }
    assert!(bus.wide_bitplane_hot_line.is_current(bus.agnus.vpos));

    bus.ddf_seq_invalidate_line();
    assert_eq!(
        bus.wide_bitplane_dynamic_vpos.get(),
        Some(bus.agnus.vpos),
        "a mid-line fetch-register change must select the dynamic fallback"
    );
    for hpos in 0..bus.agnus.current_line_cck() {
        assert_eq!(
            bus.bitplane_slot_active_at(bus.agnus.vpos, hpos),
            bus.dynamic_bitplane_slot_active_at(bus.agnus.vpos, hpos)
        );
    }
}

#[test]
fn wide_bitplane_line_stays_dynamic_when_control_delay_crosses_wrap() {
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    bus.agnus.write_fmode(0x0003);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = bus.agnus.current_line_cck() - 1;
    bus.denise.bplcon0 = 0x1000;

    assert!(!bus.write_custom_word_from(0x100, 0x5000, BeamWriteSource::Cpu));
    bus.advance_chipset(1);

    assert_eq!(bus.agnus.vpos, 0x2D);
    assert_eq!(
        bus.wide_bitplane_dynamic_vpos.get(),
        Some(0x2D),
        "the next line must follow the delayed BPLCON0 transition dynamically"
    );
}

#[test]
fn restored_wide_bitplane_line_reenters_dynamic_arbitration() {
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    bus.agnus.write_fmode(0x0003);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x35;
    bus.emulated_cck = u64::from(bus.agnus.hpos);
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0030;
    bus.denise.ddfstop = 0x00D0;
    bus.denise.bplcon0 = 0;
    bus.bitplane_bplcon0_delay = Some(BitplaneBplcon0Delay {
        previous: 0x5000,
        changed_at_cck: bus.emulated_cck - 3,
    });

    // Serde-skipped cache state defaults to an apparently static line. At
    // this point the coarse delay has expired, but the current fetch block
    // still owns the previous BPLCON0 shape.
    assert!(bus.bitplane_slot_plan_for_bplcon0(0).is_none());
    assert!(
        (bus.agnus.hpos..bus.agnus.current_line_cck())
            .any(|hpos| bus.dynamic_bitplane_slot_active_at(bus.agnus.vpos, hpos)),
        "the delayed previous-value fallback should own a remaining slot"
    );

    bus.reset_transient_video_after_state_load();

    assert_eq!(
        bus.wide_bitplane_dynamic_vpos.get(),
        Some(bus.agnus.vpos),
        "the restored partial line must not publish one static fetch shape"
    );
    assert!(!bus.wide_bitplane_hot_line.is_current(bus.agnus.vpos));
    for hpos in bus.agnus.hpos..bus.agnus.current_line_cck() {
        assert_eq!(
            bus.bitplane_slot_active_at(bus.agnus.vpos, hpos),
            bus.dynamic_bitplane_slot_active_at(bus.agnus.vpos, hpos)
        );
    }
}

fn render_color_write_x(hpos: u32) -> usize {
    hpos.saturating_sub(RENDER_COLOR_WRITE_HPOS_FB0)
        .saturating_mul(4)
        .min(FB_WIDTH as u32) as usize
}

struct NoopSerial;

impl SerialSink for NoopSerial {
    fn write_byte(&mut self, _b: u8, _at_cck: u64) {}
    fn flush(&mut self) {}
}

struct NoopAudio;

impl AudioSink for NoopAudio {
    fn push(&mut self, _left: f32, _right: f32) {}
    fn flush(&mut self) {}
}

type SharedFrames = Rc<RefCell<Vec<(f32, f32)>>>;

struct CollectAudio {
    frames: SharedFrames,
}

impl AudioSink for CollectAudio {
    fn push(&mut self, left: f32, right: f32) {
        self.frames.borrow_mut().push((left, right));
    }

    fn flush(&mut self) {}
}

pub(super) fn empty_bus() -> Bus {
    empty_bus_with_chip_ram(512 * 1024)
}

fn empty_bus_with_chip_ram(chip_ram_bytes: usize) -> Bus {
    Bus::new(
        Memory {
            chip_ram: vec![0; chip_ram_bytes],
            slow_ram: Vec::new(),
            mb_ram: Vec::new(),
            accel_ram: Vec::new(),
            rom: vec![0; 512 * 1024],
            overlay: true,
            zorro: crate::zorro::ZorroChain::default(),
            extended_rom: Vec::new(),
            extended_rom_base: 0,
            wcs: Vec::new(),
            wcs_write_protected: false,
        },
        Paula::new(Box::new(NoopSerial), Box::new(NoopAudio)),
        FloppyController::default(),
    )
}

fn empty_bus_with_collect_audio() -> (Bus, SharedFrames) {
    let frames = Rc::new(RefCell::new(Vec::new()));
    let bus = Bus::new(
        Memory {
            chip_ram: vec![0; 512 * 1024],
            slow_ram: Vec::new(),
            mb_ram: Vec::new(),
            accel_ram: Vec::new(),
            rom: vec![0; 512 * 1024],
            overlay: true,
            zorro: crate::zorro::ZorroChain::default(),
            extended_rom: Vec::new(),
            extended_rom_base: 0,
            wcs: Vec::new(),
            wcs_write_protected: false,
        },
        Paula::new(
            Box::new(NoopSerial),
            Box::new(CollectAudio {
                frames: Rc::clone(&frames),
            }),
        ),
        FloppyController::default(),
    );
    (bus, frames)
}

fn temp_bus_adf() -> std::path::PathBuf {
    let id = BUS_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("copperline-bus-{}-{id}.adf", std::process::id()))
}

/// Drive the Amiga-side KDAT handshake the way the boot ROM does:
/// SPMODE out (KDAT low), a pulse of device time, SPMODE back in.
fn keyboard_handshake(bus: &mut Bus) {
    let cra = (REG_CRA as u64) << 8;
    let _ = bus.cia_a_write(cra, 1, 0x40);
    bus.advance_devices(310);
    let _ = bus.cia_a_write(cra, 1, 0x00);
}

fn write_chip_word(bus: &mut Bus, off: usize, val: u16) {
    let bytes = val.to_be_bytes();
    bus.mem.chip_ram[off] = bytes[0];
    bus.mem.chip_ram[off + 1] = bytes[1];
}

/// Build a captured bitplane row sized to `words_per_row`, placing
/// `plane_words[p]` into plane `p` (each a list of (word_index,
/// value)). The render path only accepts a captured row whose
/// `words_per_row` matches the value it computes from the line's
/// display window, so beam-replay tests must size their injected
/// rows to that width rather than a single word.
fn captured_row(
    nplanes: usize,
    words_per_row: usize,
    plane_words: &[&[(usize, u16)]],
) -> CapturedBitplaneRow {
    let mut planes: [Vec<u16>; 8] = Default::default();
    for (p, plane) in planes.iter_mut().enumerate().take(nplanes) {
        *plane = vec![0u16; words_per_row];
        if let Some(words) = plane_words.get(p) {
            for &(idx, value) in *words {
                plane[idx] = value;
            }
        }
    }
    CapturedBitplaneRow {
        nplanes,
        words_per_row,
        fetch_origin_cck: None,
        planes,
    }
}

fn run_copper_moves_at(bus: &mut Bus, cop1: usize, vpos: u32, hpos: u32, moves: &[(u16, u16)]) {
    for (idx, &(register, value)) in moves.iter().enumerate() {
        let off = cop1 + idx * 4;
        write_chip_word(bus, off, register);
        write_chip_word(bus, off + 2, value);
    }
    let end = cop1 + moves.len() * 4;
    write_chip_word(bus, end, 0xFFFF);
    write_chip_word(bus, end + 2, 0xFFFE);

    bus.agnus.dmacon |= DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.vpos = vpos;
    bus.agnus.hpos = hpos;
    bus.copper.jump(cop1 as u32);
    // Each MOVE spans 4 color clocks (fetch, idle, fetch+write, idle), so
    // advance four per move plus a little slack for the trailing fetch.
    bus.advance_chipset((moves.len() * 4 + 2) as u32);
}

fn write_copper_wait_then_move(
    bus: &mut Bus,
    cop1: usize,
    wait_first: u16,
    wait_second: u16,
    move_register: u16,
    move_value: u16,
) {
    write_chip_word(bus, cop1, wait_first);
    write_chip_word(bus, cop1 + 2, wait_second);
    write_chip_word(bus, cop1 + 4, move_register);
    write_chip_word(bus, cop1 + 6, move_value);
    write_chip_word(bus, cop1 + 8, 0xFFFF);
    write_chip_word(bus, cop1 + 10, 0xFFFE);
}

fn run_copper_guarded_move(revision: AgnusRevision, register: u16, cdang: bool) -> Bus {
    let mut bus = empty_bus();
    bus.set_agnus_revision(revision);
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, register);
    write_chip_word(&mut bus, cop1 + 2, 0x1234);
    write_chip_word(&mut bus, cop1 + 4, 0x0182);
    write_chip_word(&mut bus, cop1 + 6, 0x0ABC);
    write_chip_word(&mut bus, cop1 + 8, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 10, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    if cdang {
        assert!(!bus.custom_write(0x02E, 2, 0x0002));
    }
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    bus.advance_chipset(8);
    bus
}

fn bus_with_pending_two_word_a_to_d_blit() -> Bus {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BLTEN;
    bus.agnus.hpos = 0x20;
    bus.blitter.bltcon0 = 0x09F0;
    bus.blitter.bltcon1 = 0;
    bus.blitter.bltafwm = 0xFFFF;
    bus.blitter.bltalwm = 0xFFFF;
    bus.blitter.bltapt = 0x10;
    bus.blitter.bltdpt = 0x20;
    write_chip_word(&mut bus, 0x10, 0x1111);
    write_chip_word(&mut bus, 0x12, 0x2222);
    write_chip_word(&mut bus, 0x14, 0x3333);

    assert!(!bus.custom_write(0x058, 2, ((1 << 6) | 2) as u64));
    assert!(bus.blitter.busy);
    assert_eq!(bus.next_blitter_completion_cck(), Some(10));
    bus
}

fn assert_busy_blitter_register_write_drains_current_blit<F>(
    off: u16,
    value: u16,
    assert_latched: F,
) where
    F: FnOnce(&Bus),
{
    let mut bus = bus_with_pending_two_word_a_to_d_blit();

    assert!(!bus.custom_write(off as u64, 2, value as u64));

    assert!(!bus.blitter.busy);
    assert_eq!(&bus.mem.chip_ram[0x20..0x24], &[0x11, 0x11, 0x22, 0x22]);
    assert_ne!(bus.paula.intreq & INT_BLIT, 0);
    assert_latched(&bus);
}

fn write_chip_word_wrapping(bus: &mut Bus, off: usize, val: u16) {
    let len = bus.mem.chip_ram.len();
    let bytes = val.to_be_bytes();
    bus.mem.chip_ram[off % len] = bytes[0];
    bus.mem.chip_ram[(off + 1) % len] = bytes[1];
}

/// POS/CTL words with `hstart` in the REGISTER domain (what the DMA
/// descriptor tests place in chip RAM and read back from captures).
fn sprite_control_words(vstart: u16, vstop: u16, hstart: u16) -> (u16, u16) {
    let pos = ((vstart & 0x00FF) << 8) | ((hstart >> 1) & 0x00FF);
    let ctl = ((vstop & 0x00FF) << 8)
        | ((vstart & 0x0100) >> 6)
        | ((vstop & 0x0100) >> 7)
        | (hstart & 0x0001);
    (pos, ctl)
}

/// POS/CTL words for a sprite whose FIRST OUTPUT PIXEL sits at lo-res
/// position `hstart`: the register value is one lo-res position lower
/// because Denise's serializer emits one lo-res pixel after the
/// comparator match (`SPRITE_OUTPUT_DELAY_LORES`). The collision tests
/// are written against output positions.
fn sprite_control_words_for_output(vstart: u16, vstop: u16, hstart: u16) -> (u16, u16) {
    sprite_control_words(vstart, vstop, hstart - SPRITE_OUTPUT_DELAY_LORES as u16)
}

/// Cross the vertical-blank reset line (PAL $19) over the whole sprite slot
/// band: the reset forces every channel's vstop to that line, so its slots
/// fetch the POS/CTL control words from SPRxPT the way hardware loads them
/// at the start of each field.
fn sprite_fetch_control_words_at_reset_line(bus: &mut Bus) {
    for sprite in 0..8 {
        let _ = bus.captured_sprite_line_at(sprite, 0x19);
    }
}

#[test]
fn realtime_clock_produces_paula_cck_from_elapsed_wall_time() {
    let mut clock = DeviceClock::default();
    clock.set_realtime_enabled(true);
    let start = Instant::now();
    clock.realtime_anchor = Some(start);

    assert_eq!(
        clock.realtime_cck_due(start + Duration::from_secs(1)),
        PAULA_CLOCK_HZ
    );
}

#[test]
fn realtime_clock_carries_fractional_cck() {
    let mut clock = DeviceClock::default();
    clock.set_realtime_enabled(true);
    let start = Instant::now();
    clock.realtime_anchor = Some(start);

    assert_eq!(clock.realtime_cck_due(start + Duration::from_nanos(1)), 0);
    assert_eq!(clock.realtime_cck_due(start + Duration::from_nanos(282)), 1);
}

#[test]
fn pending_vbi_collapses_into_intreq_latch() {
    let mut bus = empty_bus();
    bus.paula.intreq = INT_VERTB;
    bus.pending_vbi = 1;

    bus.flush_pending_vbi();
    assert_eq!(bus.paula.intreq, INT_VERTB);
    assert_eq!(bus.pending_vbi, 0);

    bus.paula.intreq = 0;
    bus.pending_vbi = 1;
    bus.flush_pending_vbi();
    assert_eq!(bus.paula.intreq, INT_VERTB);
    assert_eq!(bus.pending_vbi, 0);
}

#[test]
fn frame_event_deadline_tracks_next_vbi() {
    let mut bus = empty_bus();
    bus.agnus.vpos = crate::chipset::agnus::PAL_LINES - 1;
    bus.agnus.hpos = COLORCLOCKS_PER_LINE - 4;

    assert_eq!(bus.next_frame_event_cck(), 4);
    let tick = bus.advance_devices(4);
    assert_eq!(tick.new_frames, 1);
    assert_ne!(bus.paula.intreq & INT_VERTB, 0);
}

#[test]
fn display_start_deadline_tracks_snapshot_boundary() {
    let mut bus = empty_bus();
    bus.current_frame_visible_start_vpos = RENDER_MIN_OVERSCAN_START_VPOS;
    bus.refresh_frame_geometry_visible_start();
    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS - 1;
    bus.agnus.hpos = COLORCLOCKS_PER_LINE - 3;
    bus.mem.chip_ram[0] = 0x12;

    assert_eq!(bus.next_display_start_event_cck(), Some(3));
    let tick = bus.advance_chipset(3);
    assert_eq!(tick.new_lines, 1);
    assert!(bus.current_frame_display_snapshot_taken);
    assert_eq!(
        bus.current_frame_visible_start_vpos,
        RENDER_MIN_OVERSCAN_START_VPOS
    );
    assert_eq!(
        bus.current_frame_geometry.visible_start_vpos,
        RENDER_MIN_OVERSCAN_START_VPOS
    );
    assert_eq!(bus.current_frame_chip_ram[0], 0x12);
    assert_eq!(bus.next_display_start_event_cck(), None);
}

#[test]
fn display_start_deadline_tracks_early_vertical_overscan_diw() {
    let mut bus = empty_bus();
    bus.current_frame_visible_start_vpos = RENDER_MIN_OVERSCAN_START_VPOS;
    bus.refresh_frame_geometry_visible_start();
    bus.denise.diwstrt = 0x1C81;
    bus.agnus.vpos = RENDER_MIN_OVERSCAN_START_VPOS - 1;
    bus.agnus.hpos = COLORCLOCKS_PER_LINE - 3;
    bus.mem.chip_ram[0] = 0x12;

    assert_eq!(bus.next_display_start_event_cck(), Some(3));
    let tick = bus.advance_chipset(3);
    assert_eq!(tick.new_lines, 1);
    assert!(bus.current_frame_display_snapshot_taken);
    assert_eq!(
        bus.current_frame_visible_start_vpos,
        RENDER_MIN_OVERSCAN_START_VPOS
    );
    assert_eq!(bus.current_frame_chip_ram[0], 0x12);
    assert_eq!(bus.next_display_start_event_cck(), None);
}

#[test]
fn cia_b_tod_alarm_deadline_tracks_hsync() {
    let mut bus = empty_bus();
    bus.agnus.hpos = COLORCLOCKS_PER_LINE - 5;
    bus.cia_b.write(REG_TODHI, 0);
    bus.cia_b.write(REG_TODMID, 0);
    bus.cia_b.write(REG_TODLO, 0);
    bus.cia_b.write(REG_CRB, 0x80);
    bus.cia_b.write(REG_TODHI, 0);
    bus.cia_b.write(REG_TODMID, 0);
    bus.cia_b.write(REG_TODLO, 2);
    bus.cia_b.write(REG_CRB, 0);

    assert_eq!(
        bus.next_cia_b_tod_alarm_cck(),
        Some(5 + COLORCLOCKS_PER_LINE)
    );
    let tick = bus.advance_devices(5 + COLORCLOCKS_PER_LINE);
    assert_eq!(tick.new_lines, 2);
    assert_ne!(bus.cia_b.read(REG_ICR) & 0x04, 0);
}

#[test]
fn cia_b_mask_enable_propagates_latched_timer_interrupt_to_paula() {
    let mut bus = empty_bus();
    let addr = |reg: usize| (reg as u64) << 8;

    let _ = bus.cia_b_write(addr(REG_TALO), 1, 0);
    let _ = bus.cia_b_write(addr(REG_TAHI), 1, 0);
    let _ = bus.cia_b_write(addr(REG_CRA), 1, 0x01);

    assert!(!bus.cia_b.tick(1));
    assert_eq!(bus.paula.intreq & INT_EXTER, 0);

    let _ = bus.cia_b_write(addr(REG_ICR), 1, 0x80 | 0x01);

    // The pin (and so INTREQ) follows the mask write one E-cycle later.
    assert_eq!(bus.paula.intreq & INT_EXTER, 0);
    bus.advance_devices(8);
    assert_ne!(bus.paula.intreq & INT_EXTER, 0);
}

#[test]
fn cia_a_pc_strobe_reaches_parallel_port_and_ack_drives_cia_a_flag() {
    struct AckCapture(Arc<Mutex<Vec<(u8, u64)>>>);

    impl crate::parallel::ParallelPort for AckCapture {
        fn strobe(&mut self, data: u8, at_cck: u64) -> bool {
            self.0.lock().unwrap().push((data, at_cck));
            true
        }
    }

    let mut bus = empty_bus();
    let events = Arc::new(Mutex::new(Vec::new()));
    bus.attach_parallel_port(Box::new(AckCapture(Arc::clone(&events))));
    let addr = |reg: usize| (reg as u64) << 8;

    // FLAG is a delayed CIA source. Enable it, drive all eight printer data
    // pins on CIA-A port B, then access PRB: CIA-A pulses PC, the peripheral
    // accepts $A5, and its acknowledge falling edge latches CIA-A ICR.FLG.
    let _ = bus.cia_a_write(addr(REG_ICR), 1, 0x80 | 0x10);
    let _ = bus.cia_a_write(addr(REG_DDRB), 1, 0xFF);
    assert!(
        events.lock().unwrap().is_empty(),
        "DDRB sampling is not a strobe"
    );
    let _ = bus.cia_a_write(addr(REG_PRB), 1, 0xA5);

    assert_eq!(&*events.lock().unwrap(), &[(0xA5, 0)]);
    assert_ne!(bus.cia_a.debug_icr_data() & 0x10, 0);
    assert_eq!(bus.paula.intreq & INT_PORTS, 0, "FLAG pin has one-E lag");
    bus.advance_devices(5);
    assert_ne!(bus.paula.intreq & INT_PORTS, 0);

    // A guest PRB read generates the same hardware PC pulse, so it strobes the
    // peripheral again with the latched pin levels.
    assert_eq!(bus.cia_a_read(addr(REG_PRB), 1), 0xA5);
    assert_eq!(events.lock().unwrap().len(), 2);
}

#[test]
fn parallel_peripheral_drives_cia_b_centronics_status_inputs() {
    let mut bus = empty_bus();
    let addr = |reg: usize| (reg as u64) << 8;

    // An empty port: BUSY, POUT, and SEL float high on the pull-ups, like
    // the serial handshake lines on PA3-7. parallel.device reads this as a
    // busy offline printer and never sends a byte, as on a real machine.
    assert_eq!(bus.cia_b_read(addr(REG_PRA), 1), 0xFF);

    // A printer holds SEL high with BUSY and POUT low, so the guest reads
    // $FC: online, ready, paper loaded.
    let path = std::env::temp_dir().join(format!(
        "copperline-bus-parallel-status-{}.raw",
        std::process::id()
    ));
    let printer = crate::parallel::FileParallelPort::create(&path).unwrap();
    bus.attach_parallel_port(Box::new(printer));
    assert_eq!(bus.cia_b_read(addr(REG_PRA), 1), 0xFC);

    // Pins the guest switches to outputs stay CIA-driven: BUSY written high
    // as an output reads back high even with the printer attached.
    let _ = bus.cia_b_write(addr(REG_DDRA), 1, 0x01);
    let _ = bus.cia_b_write(addr(REG_PRA), 1, 0x01);
    assert_eq!(bus.cia_b_read(addr(REG_PRA), 1), 0xFD);

    let _ = std::fs::remove_file(path);
}

#[test]
fn floppy_index_flag_sync_delay_is_visible_to_cia_b_icr_polling() {
    let path = temp_bus_adf();
    std::fs::write(&path, vec![0u8; ADF_SIZE]).unwrap();
    let mut bus = empty_bus();
    bus.floppy.insert_disk_image(0, path.clone(), true).unwrap();
    bus.floppy.write_prb(0x77);

    let index_cck = bus
        .floppy
        .next_index_pulse_cck()
        .expect("motor-on selected disk should report the next index edge");
    assert!(index_cck > 1);
    assert_eq!(bus.cia_b.read(REG_ICR) & 0x10, 0);

    bus.advance_devices(index_cck - 1);
    assert_eq!(bus.cia_b.read(REG_ICR) & 0x10, 0);

    bus.advance_devices(1);
    assert_eq!(bus.cia_b.read(REG_ICR) & 0x10, 0);

    bus.advance_devices(1);
    assert_ne!(bus.cia_b.read(REG_ICR) & 0x10, 0);

    let _ = std::fs::remove_file(path);
}

#[test]
fn realtime_slice_bus_advance_ticks_shared_device_clock() {
    let mut bus = empty_bus();
    bus.device_clock.realtime_enabled = true;

    bus.record_slice_bus_advance(2, AgnusTick::default());
    bus.flush_timed_devices();

    assert_eq!(bus.device_clock.cia_tick_remainder_cck, 2);
}

#[test]
fn real_mode_cpu_bus_advance_ticks_cia_without_double_counting() {
    let mut bus = empty_bus();
    bus.cia_b.write(REG_TBLO, 4);
    bus.cia_b.write(REG_TBHI, 0);
    bus.cia_b.write(REG_CRB, 0x11);

    // Device ticks are deferred from the per-access advance and applied in
    // one batch at the next observation/boundary; flush to apply them here.
    bus.record_slice_bus_advance(10, AgnusTick::default());
    bus.flush_timed_devices();
    assert_eq!(bus.cia_b.tb_count, 2);

    // The cycle-exact model has no post-slice reconciliation: a slice whose
    // bus time fully covered its device time advances nothing afterwards.
    // (advance_devices flushes pending first, then ticks its own 0 cck.)
    bus.advance_devices(0);
    assert_eq!(bus.cia_b.tb_count, 2);
}

#[test]
fn audio_time_flushes_before_audio_register_write() {
    let (mut bus, frames) = empty_bus_with_collect_audio();
    bus.paula.set_led_filter_guest(false);
    bus.mem.chip_ram[0] = 0x7F;
    bus.mem.chip_ram[1] = 0x7F;
    bus.mem.chip_ram[2] = 0x7F;
    bus.mem.chip_ram[3] = 0x7F;
    bus.paula.write_audio_reg(0x00, 0, 0);
    bus.paula.write_audio_reg(0x02, 0, 0);
    bus.paula.write_audio_reg(0x04, 2, 0);
    bus.paula.write_audio_reg(0x06, 80, 0);
    bus.paula.write_audio_reg(0x08, 64, 0);
    assert!(!bus.custom_write(0xDFF096, 2, (0x8000 | DMACON_DMAEN | 0x0001) as u64));

    // Two scanlines for the two start-up fetches, then some output time.
    bus.advance_chipset(800);
    frames.borrow_mut().clear();

    let _ = bus.custom_write(0xDFF0A8, 2, 0);
    let frames = frames.borrow();
    assert!(
        frames.iter().any(|(left, _)| left.abs() > 0.5),
        "pending audio should be mixed with the old volume before AUD0VOL is changed: {frames:?}"
    );
}

#[test]
fn audio_irq_deadline_accounts_for_pending_audio_time() {
    let mut bus = empty_bus();
    // IRQ-mode output: a CPU AUDxDAT write starts direct playback, and every
    // word start raises the channel interrupt, so the deadline is the period
    // counter's word boundary (percnt + one period from the high byte).
    bus.paula.write_audio_reg(0x06, 10, 0);
    bus.paula.write_audio_reg(0x0A, 0x1122, 0);
    assert_eq!(bus.next_audio_irq_cck(), Some(20));

    bus.audio_pending_cck = 15;
    assert_eq!(bus.next_audio_irq_cck(), Some(5));
}

#[test]
fn enabled_audio_dma_reserves_only_actively_fetching_channel_slots() {
    let mut bus = empty_bus();
    // Channel 0 enabled but with no pending DMA request yet: its slot is NOT
    // reserved (a fixed audio slot is free for the CPU/blitter on lines the
    // channel does not fetch).
    bus.agnus.dmacon = DMACON_DMAEN | 0x0001;
    bus.agnus.hpos = 0x00D;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);

    // The DMA-enable edge posts the state machine's request; the line-end
    // transfer latches it into Agnus, and only then does the slot reserve.
    bus.paula.apply_audio_dmacon_edges(0, bus.agnus.dmacon);
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    bus.paula.transfer_audio_dma_requests();
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Audio);

    // The even gap after it (0x00E) is not an audio cycle, and the other
    // channels' slots (0x00F/0x011/0x013) are free -- those channels are
    // disabled -- so the CPU/blitter may use all of these.
    for hpos in [0x00E, 0x00F, 0x011, 0x013, 0x015] {
        bus.agnus.hpos = hpos;
        assert_eq!(
            bus.scheduled_dma_owner(false),
            ChipBusOwner::Idle,
            "hpos {hpos:#05X} should not be reserved for audio"
        );
    }

    // Servicing channel 0's start-up fetch posts the next request (the
    // state machine runs its two-fetch start-up); switching DMACON to
    // channels 1 and 3 then leaves that already-posted request latched --
    // the audio slot is NOT gated by the DMACON bits -- alongside the new
    // channels' first requests.
    let dmacon = bus.agnus.dmacon;
    let _ = bus.paula.grant_audio_dma(0, 0, dmacon);
    bus.agnus.dmacon = DMACON_DMAEN | 0x000A;
    bus.paula
        .apply_audio_dmacon_edges(DMACON_DMAEN | 0x0001, bus.agnus.dmacon);
    bus.paula.transfer_audio_dma_requests();
    for (hpos, owner) in [
        (0x00D, ChipBusOwner::Audio),
        (0x00F, ChipBusOwner::Audio),
        (0x011, ChipBusOwner::Idle),
        (0x013, ChipBusOwner::Audio),
    ] {
        bus.agnus.hpos = hpos;
        assert_eq!(bus.scheduled_dma_owner(false), owner, "hpos {hpos:#05X}");
    }
    // Once the outstanding fetch is serviced (with the channel bit off it
    // goes nowhere: the machine idles in state 101), channel 0's slot
    // frees for the CPU/blitter.
    let dmacon = bus.agnus.dmacon;
    let _ = bus.paula.grant_audio_dma(0, 0, dmacon);
    bus.paula.transfer_audio_dma_requests();
    bus.agnus.hpos = 0x00D;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);

    // No channels enabled: nothing reserved.
    bus.agnus.dmacon = DMACON_DMAEN;
    bus.agnus.hpos = 0x00D;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
}

#[test]
fn beamcon0_dual_warns_once_on_ecs() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    assert!(!bus.uhres_dual_warned);
    // PAL bit keeps the standard; DUAL trips the one-time UHRES warning.
    bus.custom_write(0xDFF1DC, 2, (BEAMCON0_PAL | BEAMCON0_DUAL) as u64);
    assert!(bus.uhres_dual_warned);
    // A second DUAL write must not re-arm (no per-write log spam).
    bus.custom_write(0xDFF1DC, 2, (BEAMCON0_PAL | BEAMCON0_DUAL) as u64);
    assert!(bus.uhres_dual_warned);
}

#[test]
fn harddis_widens_bitplane_arbitration_window() {
    // A 4-plane lores display with ddfstop past the 0xD8 hard stop. The
    // 22nd fetch word (hpos ~0xE0..0xE7) exists only when HARDDIS relaxes
    // the ceiling to 0xE0; without it, stop clamps to 0xD8 and the last
    // fetch hpos is 0xDF. hpos 0xE1 is the plane-4 fetch of that extra word.
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.bplcon0 = 0x4200; // 4 planes, lores
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x00E0;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0xF4C1;
    bus.agnus.vpos = 0x40;
    bus.agnus.hpos = 0x0E1;

    // PAL bit set, no HARDDIS: stop clamps to 0xD8 -> 0xE1 is past the window.
    assert!(!bus.custom_write(0xDFF1DC, 2, BEAMCON0_PAL as u64));
    assert_ne!(bus.scheduled_dma_owner(false), ChipBusOwner::Bitplane);

    // HARDDIS relaxes the stop to 0xE0, adding the 22nd fetch word.
    assert!(!bus.custom_write(0xDFF1DC, 2, (BEAMCON0_PAL | BEAMCON0_HARDDIS) as u64));
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Bitplane);
}

#[test]
fn audio_dma_enable_raises_intreq_when_first_startup_word_arrives() {
    let mut bus = empty_bus();
    bus.mem.chip_ram[0] = 0x12;
    bus.mem.chip_ram[1] = 0x34;

    let _ = bus.custom_write(0xDFF0A0, 2, 0);
    let _ = bus.custom_write(0xDFF0A2, 2, 0);
    let _ = bus.custom_write(0xDFF0A4, 2, 1);
    let _ = bus.custom_write(0xDFF0A6, 2, 8);
    let _ = bus.custom_write(0xDFF0A8, 2, 64);

    // The DMA-enable edge itself only posts the first fetch request; the
    // channel interrupt fires when that word arrives (line end transfers
    // the request, the next line's fixed slot services it).
    assert!(!bus.custom_write(0xDFF096, 2, (0x8000 | DMACON_DMAEN | 0x0001) as u64));
    assert_eq!(bus.paula.intreq & INT_AUD0, 0);

    bus.advance_chipset(2 * 227);
    assert_eq!(bus.paula.intreq & INT_AUD0, INT_AUD0);

    assert!(!bus.custom_write(0xDFF09C, 2, INT_AUD0 as u64));
    assert_eq!(bus.paula.intreq & INT_AUD0, 0);
}

#[test]
fn audio_dma_startup_fetches_stale_pointer_word_then_block_start() {
    let mut bus = empty_bus();
    bus.mem.chip_ram[0x04] = 0x12;
    bus.mem.chip_ram[0x05] = 0x34;
    bus.mem.chip_ram[0x06] = 0x56;
    bus.mem.chip_ram[0x07] = 0x78;
    bus.paula.write_audio_reg(0x00, 0, 0);
    bus.paula.write_audio_reg(0x02, 4, 0);
    bus.paula.write_audio_reg(0x04, 2, 0);
    bus.paula.write_audio_reg(0x06, 8, 0);
    // The DMA pointer is stale from previous playback; the enable edge
    // does not reload it.
    bus.paula.set_audio_dma_ptr_for_test(0, 0x20);
    bus.agnus.dmacon = DMACON_DMAEN | 0x0001;
    bus.paula.apply_audio_dmacon_edges(0, bus.agnus.dmacon);
    bus.paula.transfer_audio_dma_requests();
    bus.agnus.hpos = 0x00D;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Audio);

    // First slot: the start-up fetch reads the STALE pointer (its word is
    // never played), raises the channel interrupt, and resets the pointer
    // to AUDxLC for the real first word.
    bus.advance_chipset(1);
    assert_eq!(bus.last_chip_bus_owner(), ChipBusOwner::Audio);
    assert_eq!(bus.paula.intreq & INT_AUD0, INT_AUD0);
    assert_eq!(bus.paula.audio_current_sample_for_test(0), Some(0));
    assert_eq!(bus.paula.audio_dma_ptr_for_test(0), Some(4));

    // Second slot (next line): the word at AUDxLC starts playback.
    bus.paula.transfer_audio_dma_requests();
    bus.agnus.hpos = 0x00D;
    bus.advance_chipset(1);
    assert_eq!(bus.paula.audio_current_sample_for_test(0), Some(0x12));
    assert_eq!(bus.paula.audio_dma_ptr_for_test(0), Some(6));
}

#[test]
fn paula_interrupt_write_addresses_use_write_only_custom_bus_readback() {
    let mut bus = empty_bus();

    bus.paula.write_intena(0x8000 | INT_MASTER | INT_VERTB);
    bus.paula.write_intreq(0x8000 | INT_AUD0);

    assert_eq!(bus.custom_read(0x01C, 2), (INT_MASTER | INT_VERTB) as u64);
    assert_eq!(bus.custom_read(0x01E, 2), INT_AUD0 as u64);
    // The write addresses drive nothing on a read: they return the floating
    // chip data bus, never the INTENA/INTREQ latches.
    bus.data_bus = 0x1234;
    assert_eq!(bus.custom_read(0x09A, 2), 0x1234);
    assert_eq!(bus.custom_read(0x09C, 2), 0x1234);
}

#[test]
fn write_only_custom_register_reads_float_to_the_chip_data_bus() {
    // BPLCON3 ($106) and BPL1MOD ($108) drive nothing on a read, so the
    // CPU samples the residue of the last real chip-bus transfer, not a
    // fixed zero. Software that reads a write-only register back and ORs
    // the result into a fresh write must pick up the same garbage bits it
    // would on real silicon (a floating LOCT bit in a read-back BPLCON3
    // misroutes AGA palette writes into the low nibbles, collapsing the
    // palette toward dark).
    let mut bus = empty_bus();
    bus.data_bus = 0xA53C;

    assert_eq!(bus.custom_read(0x106, 2), 0xA53C);
    assert_eq!(bus.custom_read(0x108, 2), 0xA53C);
    // A 68000 byte read takes the high data-bus lane at even addresses
    // and the low lane at odd.
    assert_eq!(bus.custom_read(0x106, 1), 0xA5);
    assert_eq!(bus.custom_read(0x107, 1), 0x3C);
    // A longword read spans two undriven word cycles; the residue holds
    // across both.
    assert_eq!(bus.custom_read(0x106, 4), 0xA53C_A53C);

    // A readable register still drives the bus with its own value.
    bus.paula.write_intena(0x8000 | INT_MASTER | INT_VERTB);
    assert_eq!(bus.custom_read(0x01C, 2), (INT_MASTER | INT_VERTB) as u64);
}

#[test]
fn a_driven_word_cycle_recharges_the_bus_for_a_following_undriven_cycle() {
    // MOVE.L $DFF01E,Dn pairs a driven word with an undriven one: the
    // INTREQR cycle drives the chip data bus, so the following read of the
    // write-only DSKPTH ($020) floats to INTREQR's value, not to the
    // residue from before the transfer.
    let mut bus = empty_bus();
    bus.paula.write_intreq(0x8000 | INT_AUD0);
    bus.data_bus = 0x5A5A;

    let intreqr = INT_AUD0 as u64;
    assert_eq!(bus.custom_read(0x01E, 4), (intreqr << 16) | intreqr);
    // The driven cycle's word stays on the bus for later undriven reads.
    assert_eq!(bus.custom_read(0x106, 2), intreqr);
}

#[test]
fn dmacon_masks_stored_bits_and_dmaconr_derives_status() {
    let mut bus = empty_bus();

    bus.agnus.write_dmacon(0x8000 | 0x7FFF);

    assert_eq!(bus.agnus.dmacon, 0x07FF);
    bus.blitter.busy = true;
    bus.blitter.bbusy = true;
    bus.blitter.bzero = true;
    assert_eq!(bus.custom_read(0x002, 2), 0x67FF);
}

#[test]
fn dma_modulo_registers_are_word_aligned() {
    let mut bus = empty_bus();

    let _ = bus.custom_write(0x060, 2, 0x0001);
    let _ = bus.custom_write(0x062, 2, 0xFFFF);
    let _ = bus.custom_write(0x064, 2, 0x8001);
    let _ = bus.custom_write(0x066, 2, 0x7FFF);
    let _ = bus.custom_write(0x108, 2, 0xFFFF);
    let _ = bus.custom_write(0x10A, 2, 0x0001);

    assert_eq!(bus.blitter.bltcmod, 0);
    assert_eq!(bus.blitter.bltbmod, -2);
    assert_eq!(bus.blitter.bltamod, i16::MIN);
    assert_eq!(bus.blitter.bltdmod, 0x7FFE);
    assert_eq!(bus.denise.bpl1mod, -2);
    assert_eq!(bus.denise.bpl2mod, 0);
}

#[test]
fn custom_byte_write_drives_the_byte_onto_both_bus_halves() {
    // A 68000 byte write drives the byte on BOTH halves of the data bus,
    // and the custom chips latch the full 16-bit word (no byte lanes):
    // either byte address of a register receives $vvvv, never a merge
    // with the previously latched word. The vAmigaTS CIA/oldcnt
    // cnt1/cnt3/cnt5 ramps (move.b TALO,COLOR00+1 showing the mirrored
    // high nibble in red) photograph this on hardware.
    let mut bus = empty_bus();

    let _ = bus.custom_write(0xDFF074, 1, 0x80); // BLTADAT even byte
    assert_eq!(bus.blitter.bltadat, 0x8080);

    let _ = bus.custom_write(0xDFF075, 1, 0x01); // BLTADAT odd byte
    assert_eq!(bus.blitter.bltadat, 0x0101);

    // COLOR00: `move.b v,COLOR00+1` latches $vvvv, so the red nibble
    // mirrors the blue one ($AE -> RGB $EAE; bit 15 is the stored
    // transparency/genlock bit from the mirrored byte's bit 7).
    let _ = bus.custom_write(0xDFF181, 1, 0xAE);
    assert_eq!(bus.denise.palette[0], 0x8EAE);

    // COPCON's one-bit Agnus control latch sees the mirrored byte value,
    // so a 68000 byte bit operation such as `bset #1,COPCON` enables CDANG.
    let _ = bus.custom_write(0xDFF02E, 1, 0x02);
    assert_eq!(bus.agnus.copcon, 0x0002);
    let _ = bus.custom_write(0xDFF02E, 1, 0x00);
    assert_eq!(bus.agnus.copcon, 0x0000);

    // Paula latches the mirrored word too, so `move.b #v,AUDxPER/VOL`
    // lands the value in the low byte exactly as `move.w` would. (Magic
    // Pockets sets its echo voice volume with a byte write to AUDxVOL
    // and relies on this.)
    let _ = bus.custom_write(0xDFF0A6, 1, 0x12);
    assert_eq!(bus.paula.peek_audio_reg_latch(0x06), Some(0x1212));

    let _ = bus.custom_write(0xDFF0A7, 1, 0x34);
    assert_eq!(bus.paula.peek_audio_reg_latch(0x06), Some(0x3434));
}

#[test]
fn sync_strobes_are_documented_noops() {
    let mut bus = empty_bus();

    for off in [0x038, 0x03A, 0x03C, 0x03E] {
        assert!(!bus.custom_write(off, 2, 0xFFFF));
    }
}

#[test]
fn bplcon3_writes_require_ecs_denise_and_enbplcn3() {
    use crate::chipset::denise::BPLCON3_PF2OF_DEFAULT;

    // OCS Denise has no BPLCON3: writes never latch.
    let mut ocs = empty_bus();
    assert!(!ocs.custom_write(0x100, 2, 0x0001));
    assert!(!ocs.custom_write(0x106, 2, 0x1234));
    assert_eq!(ocs.denise.bplcon3, BPLCON3_PF2OF_DEFAULT);

    // ECS Denise drops BPLCON3 writes while ENBPLCN3 (BPLCON0 bit 0)
    // is clear and latches them while it is set.
    let mut ecs = empty_bus();
    ecs.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    assert!(!ecs.custom_write(0x106, 2, 0x1234));
    assert_eq!(ecs.denise.bplcon3, BPLCON3_PF2OF_DEFAULT);
    assert!(!ecs.custom_write(0x100, 2, 0x0001));
    assert!(!ecs.custom_write(0x106, 2, 0x5678));
    assert_eq!(ecs.denise.bplcon3, 0x5678);

    // Clearing ENBPLCN3 again keeps the last latched value but blocks
    // further writes.
    assert!(!ecs.custom_write(0x100, 2, 0x0000));
    assert!(!ecs.custom_write(0x106, 2, 0x9ABC));
    assert_eq!(ecs.denise.bplcon3, 0x5678);
}

#[test]
fn ddf_low_bits_are_masked_by_fetch_mode() {
    assert_eq!(
        bitplane_words_per_row(AgnusRevision::Ocs, 0x0000, 0, 0x0038, 0x0043, false),
        bitplane_words_per_row(AgnusRevision::Ocs, 0x0000, 0, 0x0038, 0x0040, false)
    );
    assert_eq!(
        bitplane_words_per_row(AgnusRevision::Ocs, 0x0000, 0, 0x003C, 0x0040, false),
        bitplane_words_per_row(AgnusRevision::Ocs, 0x0000, 0, 0x0038, 0x0040, false)
    );
    // OCS DDF registers keep 4-CCK precision. The low-res fetch
    // sequencer still completes whole 8-CCK units, so DDFSTRT bit 2 can
    // change the row length when DDFSTOP is aligned to the same unit.
    assert_eq!(
        bitplane_words_per_row(AgnusRevision::Ocs, 0x0000, 0, 0x004A, 0x00B6, false),
        15
    );
    assert_eq!(
        bitplane_words_per_row(AgnusRevision::Ocs, 0x0000, 0, 0x0064, 0x00A5, false),
        9
    );
    assert_eq!(
        bitplane_words_per_row(AgnusRevision::Ocs, 0x0000, 0, 0x0034, 0x00D4, false),
        21
    );
    assert_eq!(
        bitplane_words_per_row(AgnusRevision::Ocs, 0x0000, 0, 0x0028, 0x00D4, false),
        23
    );
    // Hires DDF has 4-cck granularity: the start's low bits shift the
    // window by half a fetch unit. The sequencer still runs whole 8-cck
    // units (the unit starting at-or-after DDFSTOP completes), so the
    // half-unit shift is visible in the word count when the stop lands
    // mid-unit.
    assert_eq!(
        bitplane_words_per_row(AgnusRevision::Ocs, 0x8000, 0, 0x003C, 0x0044, false),
        4
    );
    assert_eq!(
        bitplane_words_per_row(AgnusRevision::Ocs, 0x8000, 0, 0x0038, 0x0044, false),
        6
    );
    // CDTV extended-ROM trademark screen: hires $64/$A8 with $28 modulos
    // requires 20 words/row (10 whole units); truncating the partial
    // tail fetched 18 and sheared every row.
    assert_eq!(
        bitplane_words_per_row(AgnusRevision::Ocs, 0x8000, 0, 0x0064, 0x00A8, false),
        20
    );
}

#[test]
fn dma_pointer_registers_follow_configured_chip_ram_mask() {
    let mut ocs = empty_bus();

    assert!(!ocs.custom_write(0x080, 2, 0x001F));
    assert!(!ocs.custom_write(0x082, 2, 0xFFFE));
    assert_eq!(ocs.agnus.cop1lc, 0x07FFFE);

    assert!(!ocs.custom_write(0x020, 2, 0x001F));
    assert!(!ocs.custom_write(0x022, 2, 0xFFFE));
    assert_eq!(ocs.floppy.dskpt(), 0x07FFFE);

    assert!(!ocs.custom_write(0x050, 2, 0x001F));
    assert!(!ocs.custom_write(0x052, 2, 0xFFFE));
    assert_eq!(ocs.blitter.bltapt, 0x07FFFE);

    assert!(!ocs.custom_write(0x0E0, 2, 0x001F));
    assert!(!ocs.custom_write(0x0E2, 2, 0xFFFE));
    assert_eq!(ocs.denise.bplpt[0], 0x07FFFE);

    assert!(!ocs.custom_write(0x120, 2, 0x001F));
    assert!(!ocs.custom_write(0x122, 2, 0xFFFE));
    assert_eq!(ocs.denise.sprpt[0], 0x07FFFE);

    assert!(!ocs.custom_write(0x0A0, 2, 0x001F));
    assert!(!ocs.custom_write(0x0A2, 2, 0xFFFE));
    assert_eq!(ocs.paula.peek_audio_reg_latch(0x00), Some(0x0007));
    assert_eq!(ocs.paula.peek_audio_reg_latch(0x02), Some(0xFFFE));

    let mut ecs = empty_bus_with_chip_ram(2 * 1024 * 1024);
    ecs.set_agnus_revision(AgnusRevision::Ecs8375);
    assert!(!ecs.custom_write(0x080, 2, 0x001F));
    assert!(!ecs.custom_write(0x082, 2, 0xFFFE));
    assert_eq!(ecs.agnus.cop1lc, 0x1FFFFE);

    // The 8372A's address bus stops at 1 MB even with 2 MB installed,
    // and an OCS Agnus at 512 KiB.
    let mut ecs_1m = empty_bus_with_chip_ram(2 * 1024 * 1024);
    ecs_1m.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    assert!(!ecs_1m.custom_write(0x080, 2, 0x001F));
    assert!(!ecs_1m.custom_write(0x082, 2, 0xFFFE));
    assert_eq!(ecs_1m.agnus.cop1lc, 0x0FFFFE);

    let mut ocs_2m = empty_bus_with_chip_ram(2 * 1024 * 1024);
    assert!(!ocs_2m.custom_write(0x080, 2, 0x001F));
    assert!(!ocs_2m.custom_write(0x082, 2, 0xFFFE));
    assert_eq!(ocs_2m.agnus.cop1lc, 0x07FFFE);
}

/// Plan 1.1: walk every DMA pointer register and check the writable
/// high bits follow the Agnus revision's address-bus reach.
#[test]
fn dma_pointer_high_bits_follow_agnus_revision() {
    for (revision, mask) in [
        (AgnusRevision::Ocs, 0x0007_FFFEu32),
        (AgnusRevision::Ecs8372Rev4, 0x000F_FFFE),
        (AgnusRevision::Ecs8375, 0x001F_FFFE),
    ] {
        let mut bus = empty_bus_with_chip_ram(2 * 1024 * 1024);
        bus.set_agnus_revision(revision);
        let write_ptr = |bus: &mut Bus, high: u16| {
            assert!(!bus.custom_write(u64::from(high), 2, 0x001F));
            assert!(!bus.custom_write(u64::from(high) + 2, 2, 0xFFFE));
        };

        write_ptr(&mut bus, 0x020);
        assert_eq!(bus.floppy.dskpt(), mask, "{revision:?} DSKPT");

        for (idx, high) in [0x048u16, 0x04C, 0x050, 0x054].iter().enumerate() {
            write_ptr(&mut bus, *high);
            let got = match idx {
                0 => bus.blitter.bltcpt,
                1 => bus.blitter.bltbpt,
                2 => bus.blitter.bltapt,
                _ => bus.blitter.bltdpt,
            };
            assert_eq!(got, mask, "{revision:?} BLTxPT {high:#X}");
        }

        write_ptr(&mut bus, 0x080);
        assert_eq!(bus.agnus.cop1lc, mask, "{revision:?} COP1LC");
        write_ptr(&mut bus, 0x084);
        assert_eq!(bus.agnus.cop2lc, mask, "{revision:?} COP2LC");

        for ch in 0..4u16 {
            write_ptr(&mut bus, 0x0A0 + ch * 0x10);
            assert_eq!(
                bus.paula.peek_audio_reg_latch(ch * 0x10),
                Some(((mask >> 16) & 0x001F) as u16),
                "{revision:?} AUD{ch}LCH"
            );
            assert_eq!(
                bus.paula.peek_audio_reg_latch(ch * 0x10 + 2),
                Some((mask & 0xFFFE) as u16),
                "{revision:?} AUD{ch}LCL"
            );
        }

        for plane in 0..6u16 {
            write_ptr(&mut bus, 0x0E0 + plane * 4);
            assert_eq!(
                bus.denise.bplpt[plane as usize], mask,
                "{revision:?} BPL{plane}PT"
            );
        }

        for sprite in 0..8u16 {
            write_ptr(&mut bus, 0x120 + sprite * 4);
            assert_eq!(
                bus.denise.sprpt[sprite as usize], mask,
                "{revision:?} SPR{sprite}PT"
            );
        }
    }
}

#[test]
fn power_on_reset_keeps_copcon_cdang_clear() {
    let bus = empty_bus();

    assert_eq!(bus.agnus.copcon, 0);
    assert!(!bus.agnus.copper_danger_enabled());
}

#[test]
fn cold_power_on_reset_clears_chip_ram_and_restores_overlay() {
    let mut bus = empty_bus();
    bus.mem.chip_ram[0] = 0xAA;
    bus.mem.chip_ram[1024] = 0x55;
    bus.mem.overlay = false;

    bus.power_on_reset();

    assert!(
        bus.mem.chip_ram.iter().all(|&b| b == 0),
        "chip RAM must be zeroed on cold boot"
    );
    assert!(
        bus.mem.overlay,
        "ROM overlay must be re-enabled on cold boot"
    );
}

#[test]
fn cpu_reset_drives_external_floppy_reset_line() {
    let mut bus = empty_bus();
    bus.floppy.write_prb(0x6F);
    assert!(bus.floppy.activity_led_on());

    bus.reset_custom_chips_from_cpu_reset();

    assert!(!bus.floppy.activity_led_on());
}

#[test]
fn copper_jump_latches_bitplane_state_from_chip_ram() {
    let mut bus = empty_bus();
    let mut pc = 0x0100;
    for (reg, val) in [
        (0x0E0, 0x0000),
        (0x0E2, 0x6090),
        (0x100, 0x4200),
        (0x180, 0x0123),
        (0xFFFF, 0xFFFE),
    ] {
        write_chip_word(&mut bus, pc, reg);
        write_chip_word(&mut bus, pc + 2, val);
        pc += 4;
    }

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, 0x0100));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    // Four MOVEs at the 4-color-clock Copper cadence (last write at +14).
    bus.advance_chipset(18);

    assert_eq!(bus.denise.bplpt[0], 0x6090);
    assert_eq!(bus.denise.bplcon0, 0x4200);
    assert_eq!(bus.denise.palette[0], 0x0123);
}

#[test]
fn copper_can_fetch_list_from_chip_ram_base() {
    let mut bus = empty_bus();
    for (pc, word) in [
        (0x0000, 0x0180),
        (0x0002, 0x00F0),
        (0x0004, 0xFFFF),
        (0x0006, 0xFFFE),
    ] {
        write_chip_word(&mut bus, pc, word);
    }

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, 0x0000));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    bus.advance_chipset(6);

    assert_eq!(bus.denise.palette[0], 0x00F0);
}

#[test]
fn copper_interrupt_wait_fires_coper_at_programmed_line() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, 0xD421);
    write_chip_word(&mut bus, cop1 + 2, 0xFFFE);
    write_chip_word(&mut bus, cop1 + 4, 0x009C);
    write_chip_word(&mut bus, cop1 + 6, 0x8010);
    write_chip_word(&mut bus, cop1 + 8, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 10, 0xFFFE);
    bus.agnus.cop1lc = cop1 as u32;
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));

    let target_cck = 0xD4 * COLORCLOCKS_PER_LINE + 0x20;
    bus.advance_chipset(target_cck);
    assert_eq!(bus.paula.intreq & INT_COPER, 0);

    // The comparator's horizontal input runs two color clocks ahead of the
    // beam, so the wait wakes on the bus-free color clock $1E, the first
    // fetch of the INTREQ MOVE lands on the $20 target, and the write lands
    // with the second fetch at $22 (the vAmiga/interfere-photo landing).
    bus.advance_chipset(2);
    assert_eq!(bus.paula.intreq & INT_COPER, 0);

    bus.advance_chipset(1);
    assert_ne!(bus.paula.intreq & INT_COPER, 0);
}

#[test]
fn copper_coper_intreq_reaches_cpu_after_chipset_irq_delay() {
    let mut bus = empty_bus();
    bus.agnus.vpos = 0x40;
    bus.agnus.hpos = 0x20;

    assert!(bus.write_custom_word_from(0x09C, 0x8000 | INT_COPER, BeamWriteSource::Copper,));

    assert_ne!(bus.paula.intreq & INT_COPER, 0);
    assert_eq!(bus.cpu_visible_intreq() & INT_COPER, 0);
    assert_eq!(bus.pending_copper_irq_beam, Some((0x40, 0x20)));

    bus.advance_chipset(1);
    assert_eq!(bus.cpu_visible_intreq() & INT_COPER, 0);

    bus.advance_chipset(1);
    assert_ne!(bus.cpu_visible_intreq() & INT_COPER, 0);
}

#[test]
fn clearing_coper_intreq_cancels_pending_cpu_irq_delay() {
    let mut bus = empty_bus();
    assert!(bus.write_custom_word_from(0x09C, 0x8000 | INT_COPER, BeamWriteSource::Copper,));

    assert!(!bus.write_custom_word_from(0x09C, INT_COPER, BeamWriteSource::Cpu));

    assert_eq!(bus.paula.intreq & INT_COPER, 0);
    assert_eq!(bus.cpu_visible_intreq() & INT_COPER, 0);
    assert_eq!(bus.pending_copper_irq_beam, None);
}

#[test]
fn intena_unmasks_latched_ports_source_without_new_recognition_delay() {
    let mut bus = empty_bus();
    bus.irq_latency_setting = 65;
    bus.paula.intreq = INT_PORTS;

    assert!(!bus.custom_write(0x09A, 2, u64::from(0x8000 | INT_MASTER | INT_PORTS)));

    assert_ne!(bus.paula.intena & INT_MASTER, 0);
    assert_ne!(bus.cpu_visible_intreq() & INT_PORTS, 0);
    assert_eq!(bus.irq_latency_mask & INT_PORTS, 0);
    assert_eq!(bus.irq_latency_last_pending & INT_PORTS, INT_PORTS);
}

#[test]
fn intena_unmask_of_latched_exter_arms_recognition_delay() {
    let mut bus = empty_bus();
    bus.irq_latency_setting = 65;
    bus.paula.intreq = INT_EXTER;

    assert!(!bus.custom_write(0x09A, 2, u64::from(0x8000 | INT_MASTER | INT_EXTER)));

    assert_ne!(bus.paula.intena & INT_MASTER, 0);
    assert_eq!(bus.cpu_visible_intreq() & INT_EXTER, 0);
    assert_eq!(bus.irq_latency_mask & INT_EXTER, INT_EXTER);
    assert_eq!(bus.irq_latency_last_pending & INT_EXTER, INT_EXTER);

    bus.advance_chipset(65);
    assert_ne!(bus.cpu_visible_intreq() & INT_EXTER, 0);
}

#[test]
fn intena_unmask_of_latched_vertb_arms_recognition_delay() {
    let mut bus = empty_bus();
    bus.irq_latency_setting = 65;
    bus.paula.intreq = INT_VERTB;

    assert!(!bus.custom_write(0x09A, 2, u64::from(0x8000 | INT_MASTER | INT_VERTB)));

    assert_ne!(bus.paula.intena & INT_MASTER, 0);
    assert_eq!(bus.cpu_visible_intreq() & INT_VERTB, 0);
    assert_eq!(bus.irq_latency_mask & INT_VERTB, INT_VERTB);
    assert_eq!(bus.irq_latency_last_pending & INT_VERTB, INT_VERTB);

    bus.advance_chipset(65);
    assert_ne!(bus.cpu_visible_intreq() & INT_VERTB, 0);
}

#[test]
fn copper_jump_does_not_raise_delayed_coper_immediately() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, 0xD407);
    write_chip_word(&mut bus, cop1 + 2, 0xFFFE);
    write_chip_word(&mut bus, cop1 + 4, 0x009C);
    write_chip_word(&mut bus, cop1 + 6, 0x8010);
    write_chip_word(&mut bus, cop1 + 8, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 10, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));

    assert_eq!(bus.paula.intreq & INT_COPER, 0);
}

#[test]
fn copper_dma_enable_gates_instruction_execution() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, 0x0100);
    write_chip_word(&mut bus, cop1 + 2, 0x4200);
    write_chip_word(&mut bus, cop1 + 4, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 6, 0xFFFE);

    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    assert_eq!(bus.denise.bplcon0, 0);

    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x096, 2, (0x8000 | DMACON_DMAEN | DMACON_COPEN) as u64));
    bus.advance_chipset(4);
    assert_eq!(bus.denise.bplcon0, 0x4200);
}

#[test]
fn copper_lc_write_retargets_dormant_copper_pc() {
    // Real Agnus: while the Copper has NOT been active in the current field
    // (Copper DMA off at the vertical-blank strobe and no COPEN edge since),
    // writing its current list's location register moves the Copper PC
    // directly instead of only loading the latch. The vAmigaTS
    // Agnus/Copper/lc tests photograph exactly this: a VERTB handler that
    // rewrites COP1LC and then enables Copper DMA runs the NEW list.
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    let redirected = 0x0200usize;
    write_chip_word(&mut bus, cop1, 0x0180);
    write_chip_word(&mut bus, cop1 + 2, 0x0111);
    write_chip_word(&mut bus, cop1 + 4, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 6, 0xFFFE);
    write_chip_word(&mut bus, redirected, 0x0180);
    write_chip_word(&mut bus, redirected + 2, 0x0999);
    write_chip_word(&mut bus, redirected + 4, 0xFFFF);
    write_chip_word(&mut bus, redirected + 6, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN; // Copper DMA off: the Copper is dormant
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF)); // strobe: list 1, PC = cop1

    // Dormant retarget: the rewrite moves the PC, no strobe needed.
    assert!(!bus.custom_write(0x082, 2, redirected as u64));

    assert!(!bus.custom_write(0x096, 2, (0x8000 | DMACON_COPEN) as u64));
    bus.advance_chipset(4);
    assert_eq!(
        bus.denise.palette[0], 0x0999,
        "the redirected list must run without a COPJMP strobe"
    );
}

#[test]
fn copper_skipped_forbidden_move_still_stops_the_copper() {
    // A satisfied SKIP suppresses the following MOVE's write, but the MOVE
    // still passes through the illegal-register decode: a forbidden target
    // stops the Copper exactly as it would unskipped (real-Agnus behaviour,
    // photographed by vAmigaTS Copper/Skip/copskip4).
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, 0x0001); // SKIP: beam already past 0,0
    write_chip_word(&mut bus, cop1 + 2, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 4, 0x003E); // forbidden MOVE (no CDANG)
    write_chip_word(&mut bus, cop1 + 6, 0x0000);
    write_chip_word(&mut bus, cop1 + 8, 0x0180); // never reached
    write_chip_word(&mut bus, cop1 + 10, 0x0F00);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.vpos = 0x50;
    bus.agnus.hpos = 0x20;
    bus.copper.jump(cop1 as u32);
    bus.advance_chipset(16);

    assert!(
        !bus.copper.is_running(),
        "the skipped forbidden MOVE must halt"
    );
    assert_eq!(bus.denise.palette[0], 0, "the write after it must not run");
}

#[test]
fn copper_dma_enable_gates_current_pc_until_copjmp_strobe() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    let stale = 0x0200usize;
    write_chip_word(&mut bus, cop1, 0x0100);
    write_chip_word(&mut bus, cop1 + 2, 0x4200);
    write_chip_word(&mut bus, cop1 + 4, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 6, 0xFFFE);
    write_chip_word(&mut bus, stale, 0x0180);
    write_chip_word(&mut bus, stale + 2, 0x0999);
    write_chip_word(&mut bus, stale + 4, 0xFFFF);
    write_chip_word(&mut bus, stale + 6, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN;
    bus.copper.jump(stale as u32);
    // The park at `stale` implies the Copper ran this field: COPxLC writes
    // must only load the latch. (A DORMANT Copper - not active since the
    // vertical blank - has its PC retargeted by the write instead; see
    // copper_lc_written and the Copper/lc tests.)
    bus.copper_active_in_frame = true;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x096, 2, (0x8000 | DMACON_COPEN) as u64));
    bus.advance_chipset(4);

    assert_eq!(bus.denise.palette[0], 0x0999);
    assert_eq!(bus.denise.bplcon0, 0);

    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    bus.advance_chipset(4);
    assert_eq!(bus.denise.bplcon0, 0x4200);
}

#[test]
fn automatic_copper_restart_uses_live_cop1lc_at_frame_boundary() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    let cop2 = 0x0200usize;
    write_chip_word(&mut bus, cop1, 0x0180);
    write_chip_word(&mut bus, cop1 + 2, 0x0555);
    write_chip_word(&mut bus, cop1 + 4, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 6, 0xFFFE);
    write_chip_word(&mut bus, cop2, 0x0180);
    write_chip_word(&mut bus, cop2 + 2, 0x0666);
    write_chip_word(&mut bus, cop2 + 4, 0xFFFF);
    write_chip_word(&mut bus, cop2 + 6, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    bus.agnus.cop1lc = cop2 as u32;
    bus.agnus.vpos = crate::chipset::agnus::PAL_LINES - 1;
    bus.agnus.hpos = COLORCLOCKS_PER_LINE - 1;

    // The Copper restarts at the top of the frame (vpos 0), not at the end
    // of vblank, so the live COP1LC (cop2) is picked up immediately as the
    // beam wraps -- no delay until the end of vblank.
    bus.advance_chipset(5);
    assert_eq!(bus.pending_copper_frame_start, None);
    assert_eq!(bus.denise.palette[0], 0);

    // The restarted Copper's first instruction-word fetch lands on the hpos
    // 0x02 access cycle (COPPER_FRAME_START_HPOS, vAmiga-trace calibrated),
    // so the single MOVE's write lands at hpos 0x04.
    bus.advance_chipset(2);
    assert_eq!(bus.denise.palette[0], 0x0666);
}

#[test]
fn next_copper_wakeup_cck_tracks_wait_beam_position() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.vpos = 0x50;
    bus.agnus.hpos = 0x10;
    bus.copper.wait(CopperWait::new(0x5021, 0xFFFE));

    // The comparator's horizontal input runs two color clocks ahead of the
    // beam, so the wakeup lands at $1E, two before the masked $20 target.
    assert_eq!(bus.next_copper_wakeup_cck(), Some(0x0E));

    bus.agnus.hpos = 0x20;
    assert_eq!(bus.next_copper_wakeup_cck(), Some(0));

    bus.agnus.dmacon = DMACON_COPEN;
    assert_eq!(bus.next_copper_wakeup_cck(), None);
}

fn advance_chipset_quantum_reference(bus: &mut Bus, target_cck: u32) -> AgnusTick {
    let mut total = AgnusTick::default();
    let mut remaining = target_cck;
    while remaining != 0 {
        let (cck, tick) = bus.advance_one_chip_bus_quantum_limited(None, remaining);
        remaining = remaining.saturating_sub(cck);
        super::add_agnus_tick(&mut total, tick);
    }
    total
}

fn assert_wait_deadline_bus_equivalent(actual: &Bus, expected: &Bus) {
    assert_eq!(
        (actual.agnus.vpos, actual.agnus.hpos),
        (expected.agnus.vpos, expected.agnus.hpos)
    );
    assert_eq!(actual.copper.state_label(), expected.copper.state_label());
    assert_eq!(actual.copper.waiting(), expected.copper.waiting());
    assert_eq!(actual.copper.pc(), expected.copper.pc());
    assert_eq!(actual.denise.palette, expected.denise.palette);
    assert_eq!(actual.data_bus, expected.data_bus);
    assert_eq!(actual.last_chip_bus_owner, expected.last_chip_bus_owner);
    assert_eq!(actual.display_dma_bplpt, expected.display_dma_bplpt);
    assert_eq!(
        actual.frame_captured_bitplane_rows(),
        expected.frame_captured_bitplane_rows()
    );
    assert_eq!(actual.frame_render_events(), expected.frame_render_events());
    assert_eq!(actual.paula.intreq, expected.paula.intreq);
    assert_eq!(actual.emulated_cck, expected.emulated_cck);
    assert_eq!(actual.emulated_frames, expected.emulated_frames);
}

fn sleeping_copper_with_display_dma() -> Bus {
    let mut bus = empty_bus();
    assert!(!bus.custom_write(0x08E, 2, 0x2C90));
    assert!(!bus.custom_write(0x090, 2, 0xF4B0));
    assert!(!bus.custom_write(0x092, 2, 0x0038));
    assert!(!bus.custom_write(0x094, 2, 0x00D0));
    assert!(!bus.custom_write(0x100, 2, 0x6600));
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x64;
    bus.agnus.hpos = 0x20;
    bus.copper.jump(0x0100);
    write_chip_word(&mut bus, 0x0100, 0x0180);
    write_chip_word(&mut bus, 0x0102, 0x0456);
    bus.copper.wait(CopperWait::new(0x6441, 0xFFFE));
    bus
}

#[test]
fn sleeping_copper_deadline_matches_per_quantum_comparator_under_display_dma() {
    let mut optimized = sleeping_copper_with_display_dma();
    let mut reference = sleeping_copper_with_display_dma();

    let optimized_tick = optimized.advance_chipset_cpu_idle(0x40);
    let reference_tick = advance_chipset_quantum_reference(&mut reference, 0x40);

    assert_eq!(
        (optimized_tick.new_lines, optimized_tick.new_frames),
        (reference_tick.new_lines, reference_tick.new_frames)
    );
    assert_wait_deadline_bus_equivalent(&optimized, &reference);
    assert_eq!(optimized.denise.palette[0], 0x0456);
}

#[test]
fn sleeping_copper_deadline_matches_reference_across_random_beam_spans() {
    let mut random = 0xC0_77_E2_55u32;
    for case in 0..512 {
        random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let vpos = (random >> 12) % PAL_LINES;
        let hpos = (random & 0xFF) % COLORCLOCKS_PER_LINE;
        random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let target_vpos = (random >> 16) as u16;
        let target_hpos = ((random >> 7) & 0xFE) as u16;
        let wait = CopperWait::new((target_vpos << 8) | target_hpos | 1, 0xFFFE);
        let span = 1 + (random & 0x7FF);

        let make_bus = || {
            let mut bus = empty_bus();
            bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
            bus.agnus.vpos = vpos;
            bus.agnus.hpos = hpos;
            bus.copper.wait(wait);
            bus
        };
        let mut optimized = make_bus();
        let mut reference = make_bus();
        let optimized_tick = optimized.advance_chipset_cpu_idle(span);
        let reference_tick = advance_chipset_quantum_reference(&mut reference, span);

        assert_eq!(
            (optimized_tick.new_lines, optimized_tick.new_frames),
            (reference_tick.new_lines, reference_tick.new_frames),
            "case {case}"
        );
        assert_eq!(
            optimized.copper.state_label(),
            reference.copper.state_label(),
            "case {case}: start ({vpos:#x}, {hpos:#x}), wait ({target_vpos:#x}, {target_hpos:#x}), span {span:#x}"
        );
        assert_wait_deadline_bus_equivalent(&optimized, &reference);
    }
}

#[test]
fn next_copper_wakeup_cck_waits_for_vertical_low_byte_rollover_after_line_255() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.vpos = 0xFF;
    bus.agnus.hpos = 0xDF;
    bus.copper.wait(CopperWait::new(0x1B01, 0xFFFE));

    let expected = bus.agnus.cck_until_line_start(0x11B).unwrap();
    assert_eq!(
        expected,
        (COLORCLOCKS_PER_LINE - 0xDF) + 0x1B * COLORCLOCKS_PER_LINE
    );
    assert_eq!(bus.next_copper_wakeup_cck(), Some(expected));

    bus.agnus.vpos = 0x100;
    bus.agnus.hpos = 0;
    assert_eq!(
        bus.next_copper_wakeup_cck(),
        Some(0x1B * COLORCLOCKS_PER_LINE)
    );

    bus.agnus.vpos = 0x11B;
    assert_eq!(bus.next_copper_wakeup_cck(), Some(0));
}

#[test]
fn next_copper_wakeup_cck_keeps_high_half_full_mask_wait_satisfied_on_line_255() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.vpos = 0xFF;
    bus.agnus.hpos = COLORCLOCKS_PER_LINE - 1;
    bus.copper.wait(CopperWait::new(0xFC01, 0xFFFE));

    assert_eq!(bus.next_copper_wakeup_cck(), Some(0));
}

/// Run a Copper list that waits out the 8-bit vertical rollover (WAIT
/// vp=$FF hp=$DE, then WAIT vp=$36 for line $136/310) and then MOVEs
/// $0123 into COLOR00. Returns the beam line the write landed on.
fn copper_line_310_move_lands_at(bitplane_dma: bool) -> Option<u32> {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, 0xFFDF);
    write_chip_word(&mut bus, cop1 + 2, 0xFFFE);
    write_chip_word(&mut bus, cop1 + 4, 0x3601);
    write_chip_word(&mut bus, cop1 + 6, 0xFFFE);
    write_chip_word(&mut bus, cop1 + 8, 0x0180);
    write_chip_word(&mut bus, cop1 + 10, 0x0123);
    write_chip_word(&mut bus, cop1 + 12, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 14, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    if bitplane_dma {
        // Deep-overscan display: the last DDFSTOP=$D8 lores fetch unit
        // occupies hpos $D8..$DF, covering the wait's only releasable
        // color clock at $DE (later ccks fall in the line-end blackout).
        assert!(!bus.custom_write(0x08E, 2, 0x0571)); // DIWSTRT
        assert!(!bus.custom_write(0x090, 2, 0x40D1)); // DIWSTOP
        assert!(!bus.custom_write(0x092, 2, 0x0030)); // DDFSTRT
        assert!(!bus.custom_write(0x094, 2, 0x00D8)); // DDFSTOP
        assert!(!bus.custom_write(0x100, 2, 0x6200)); // BPLCON0 6 planes
        bus.agnus.dmacon |= DMACON_BPLEN;
    }
    bus.agnus.vpos = 0x18;
    bus.agnus.hpos = 0x30;
    bus.copper.jump(cop1 as u32);

    for _ in 0..(313 * 227) {
        bus.advance_chipset(1);
        if bus.denise.palette[0] == 0x0123 {
            return Some(bus.agnus.vpos);
        }
        if bus.agnus.vpos == 0 && bus.agnus.hpos == 0 {
            break;
        }
    }
    None
}

#[test]
fn copper_wait_past_vertical_rollover_releases_on_quiet_bus() {
    // CDTV extended-ROM boot list idiom: WAIT vp=$FF hp=$DE releases in
    // the last color clocks of line 255, then WAIT vp=$36 targets line
    // $136 (310) after the 8-bit rollover, then a MOVE turns the display
    // off. The MOVE must land at line 310.
    assert_eq!(copper_line_310_move_lands_at(false), Some(310));
}

#[test]
fn copper_wait_comparator_runs_under_fixed_bitplane_dma() {
    // Same list with overscan bitplane DMA fetching through hpos $DE:
    // the comparator is combinational on real Agnus and keeps running
    // while fixed DMA owns the bus, so the wait still releases on line
    // 255 and the display-off MOVE still lands at line 310. Regression:
    // the CDTV boot screen left bitplane DMA running through the bottom
    // vblank, fetching garbage rows past the image (noise band at the
    // bottom of the screen).
    assert_eq!(copper_line_310_move_lands_at(true), Some(310));
}

#[test]
fn copper_wait_wakeup_spends_a_free_slot_under_lores6_fetch() {
    // A sleeping WAIT whose comparator matches inside a 6-plane lores fetch
    // window. The lores plane slots sit at group offsets {1,2,3,5,6,7}, so
    // with DDFSTRT=$38 the only copper-usable color clocks around a WAIT
    // (v,$40) are hpos $40, $44, $48, ... The post-WAIT wake-up is a real
    // Copper cycle and must spend the first FREE access-parity color clock
    // (vAmiga's COP_REQ_DMA reschedules until the bus is free): wake-up at
    // $40, first fetch $44, second fetch + write $48. Without the free-slot
    // gate the wake-up collapses into the (plane-owned) release color clock
    // and the write lands one free slot early, at $44. Shadow of the Beast's
    // title band tucks per-line COLOR00 toggles against its display window
    // edge (DIWSTRT.H=$90, color clock $48) with exactly this list shape.
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_copper_wait_then_move(&mut bus, cop1, 0x6441, 0xFFFE, 0x0180, 0x0123);

    assert!(!bus.custom_write(0x08E, 2, 0x2C90)); // DIWSTRT (SotB title)
    assert!(!bus.custom_write(0x090, 2, 0xF4B0)); // DIWSTOP
    assert!(!bus.custom_write(0x092, 2, 0x0038)); // DDFSTRT
    assert!(!bus.custom_write(0x094, 2, 0x00D0)); // DDFSTOP
    assert!(!bus.custom_write(0x100, 2, 0x6600)); // BPLCON0 6 planes, dual pf
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x64;
    bus.agnus.hpos = 0x20;
    bus.copper.jump(cop1 as u32);

    let mut landed_hpos = None;
    for _ in 0..COLORCLOCKS_PER_LINE {
        bus.advance_chipset(1);
        if bus.denise.palette[0] == 0x0123 {
            landed_hpos = Some(bus.agnus.hpos);
            break;
        }
    }
    assert_eq!(landed_hpos, Some(0x49));
}

#[test]
fn next_copper_wakeup_cck_accounts_for_ntsc_long_lines() {
    let mut bus = empty_bus();
    bus.set_video_standard(VideoStandard::Ntsc);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.copper.wait(CopperWait::new(0x0201, 0xFFFE));

    assert_eq!(
        bus.next_copper_wakeup_cck(),
        Some(COLORCLOCKS_PER_LINE + NTSC_LONG_COLORCLOCKS_PER_LINE)
    );
}

#[test]
fn copper_wait_with_bfd_caps_to_blitter_completion_after_position_match() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN | DMACON_BLTEN;
    bus.agnus.vpos = 0x50;
    bus.agnus.hpos = 0x20;
    bus.copper.wait(CopperWait::new(0x5021, 0x7FFE));
    bus.blitter.bltcon0 = 0x0100;
    bus.blitter.start_scheduled((1 << 6) | 1, &bus.mem.chip_ram);

    assert_eq!(bus.next_blitter_completion_cck(), Some(8));
    assert_eq!(bus.next_copper_wakeup_cck(), Some(8));
}

#[test]
fn copper_wait_with_bfd_clear_resumes_after_busy_blitter_finishes() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_copper_wait_then_move(&mut bus, cop1, 0x5021, 0x7FFE, 0x0180, 0x0555);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN | DMACON_BLTEN;
    bus.agnus.vpos = 0x50;
    bus.agnus.hpos = 0x1E;
    bus.copper.jump(cop1 as u32);
    // The WAIT instruction's two-word fetch now spans 3 color clocks
    // (fetch, idle, fetch) before the Copper parks.
    bus.advance_chipset(3);
    assert!(bus.copper.waiting().is_some());

    bus.blitter.bltcon0 = 0x0100;
    bus.blitter.start_scheduled((1 << 6) | 1, &bus.mem.chip_ram);
    assert_eq!(bus.next_copper_wakeup_cck(), Some(8));

    bus.advance_chipset(7);
    assert!(bus.copper.waiting().is_some());
    assert_eq!(bus.denise.palette[0], 0);

    bus.advance_chipset(1);
    assert!(!bus.blitter.busy);
    assert!(bus.copper.waiting().is_some());

    bus.advance_chipset(2);
    assert_eq!(bus.denise.palette[0], 0);

    // Once the blitter frees the bus (the engine finishes one colour clock
    // earlier now that its startup models the hardware's poke+4 first body
    // cycle), the color clock where the released comparator is evaluated is
    // itself the bus-free wake-up cycle; the MOVE fetches land on the next
    // even copper slots and the write lands at hpos 0x2E (recorded at its
    // Denise-effective position, 0x32).
    bus.advance_chipset(8);
    assert_eq!(bus.denise.palette[0], 0x0555);
    assert_eq!(bus.current_render_events()[0].hpos, 0x32);
}

#[test]
fn copper_wait_with_bfd_set_ignores_busy_blitter_after_position_match() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_copper_wait_then_move(&mut bus, cop1, 0x5021, 0xFFFE, 0x0180, 0x0666);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN | DMACON_BLTEN;
    bus.agnus.vpos = 0x50;
    bus.agnus.hpos = 0x1E;
    bus.copper.jump(cop1 as u32);
    // The WAIT instruction's two-word fetch now spans 3 color clocks.
    bus.advance_chipset(3);
    assert!(bus.copper.waiting().is_some());

    bus.blitter.bltcon0 = 0x0100;
    bus.blitter.start_scheduled((1 << 6) | 1, &bus.mem.chip_ram);
    assert_eq!(bus.next_copper_wakeup_cck(), Some(0));

    bus.advance_chipset(2);
    assert_eq!(bus.denise.palette[0], 0);

    // BFD set: the Copper ignores the busy blitter. The WAIT pays its
    // WAIT1/WAIT2 tail through hpos 0x22, the already-true comparator is
    // seen at 0x23, the wake-up spends the 0x24 access cycle, and the MOVE
    // write lands at hpos 0x28 (fetch2+6 fetch, +2 write; recorded at its
    // Denise-effective position, 0x2C).
    bus.advance_chipset(6);
    assert_eq!(bus.denise.palette[0], 0x0666);
    assert_eq!(bus.current_render_events()[0].hpos, 0x2C);
    assert!(bus.blitter.busy);
}

#[test]
fn copper_wait_immediate_match_uses_free_cycle_before_next_fetch() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_copper_wait_then_move(&mut bus, cop1, 0x0021, 0xFFFE, 0x0180, 0x0777);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    bus.copper.jump(cop1 as u32);

    // WAIT fetch parks the Copper already past the target; it pays the
    // WAIT1/WAIT2 tail through hpos 0x24, sees the true comparator at 0x25,
    // wakes on the 0x26 access cycle, and the MOVE write lands at hpos 0x2A.
    bus.advance_chipset(6);
    assert_eq!(bus.denise.palette[0], 0);

    bus.advance_chipset(5);
    assert_eq!(bus.denise.palette[0], 0x0777);
    assert_eq!(bus.current_render_events()[0].hpos, 0x2E);
}

#[test]
fn copper_wait_wakeup_yields_free_cycle_after_late_match() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_copper_wait_then_move(&mut bus, cop1, 0x0025, 0xFFFE, 0x0180, 0x0888);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    bus.copper.jump(cop1 as u32);

    // The wait matches at hpos 0x24; the Copper yields that free cycle,
    // spends a dummy wake-up cycle, then writes at hpos 0x2A.
    bus.advance_chipset(8);
    assert_eq!(bus.denise.palette[0], 0);

    bus.advance_chipset(3);
    assert_eq!(bus.denise.palette[0], 0x0888);
    assert_eq!(bus.current_render_events()[0].hpos, 0x2E);
}

#[test]
fn copper_line_end_bus_lockout_defers_transfer_past_the_wrap() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    let start_vpos = 0x40;
    write_chip_word(&mut bus, cop1, 0x0180);
    write_chip_word(&mut bus, cop1 + 2, 0x0999);
    write_chip_word(&mut bus, cop1 + 4, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 6, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.vpos = start_vpos;
    bus.agnus.hpos = bus.copper_bus_lockout_hpos();
    bus.copper.jump(cop1 as u32);

    // $E0 on a PAL short line is the Copper's end-of-line bus lockout: the
    // slot is claimed (nobody else can use it, vAmiga's BLOCKED marker) but
    // no instruction word is fetched.
    bus.advance_chipset(1);
    assert_eq!(bus.copper.pc(), cop1 as u32);
    assert_eq!(bus.denise.palette[0], 0);

    // $E1 is the Copper's idle half and $E2 is the line-end refresh slot, so
    // the Copper stays parked on the first word until the beam wraps.
    bus.advance_chipset(2);
    assert_eq!(bus.copper.pc(), cop1 as u32);
    assert_eq!(bus.agnus.vpos, start_vpos + 1);
    assert_eq!(bus.agnus.hpos, 0);
    assert_eq!(bus.denise.palette[0], 0);

    // With refresh confined to odd slots plus line-end, hpos 0x00 is a free
    // access-parity color clock, so the Copper fetches the first word
    // immediately after the line wrap.
    bus.advance_chipset(1);
    assert_eq!(bus.copper.pc(), cop1 as u32 + 2);
    assert_eq!(bus.denise.palette[0], 0);

    // Hpos 0x01 is refresh; the next free access-parity clock is 0x02, where
    // the Copper fetches the second word and the MOVE write lands (recorded at
    // its Denise-effective position, 0x06).
    bus.advance_chipset(2);
    assert_eq!(bus.denise.palette[0], 0x0999);
    assert_eq!(bus.current_render_events()[0].vpos, start_vpos + 1);
    assert_eq!(bus.current_render_events()[0].hpos, 0x06);
}

#[test]
fn copper_skip_does_not_skip_wait_instruction() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, 0x0001);
    write_chip_word(&mut bus, cop1 + 2, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 4, 0x5021);
    write_chip_word(&mut bus, cop1 + 6, 0xFFFE);
    write_chip_word(&mut bus, cop1 + 8, 0x0180);
    write_chip_word(&mut bus, cop1 + 10, 0x0555);
    write_chip_word(&mut bus, cop1 + 12, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 14, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.vpos = 0x00;
    bus.agnus.hpos = 0x20;
    bus.copper.jump(cop1 as u32);
    // The SKIP runs the full hardware sequence: a 4-color-clock fetch plus
    // its bus-free WAITSKIP1/WAITSKIP2 tail on the two following access
    // cycles (next fetch at fetch2+6), then the WAIT spends its own
    // 4-color-clock fetch, so the Copper reaches and parks on the WAIT
    // after 11 color clocks.
    bus.advance_chipset(11);

    assert!(bus.copper.waiting().is_some());
    assert_eq!(bus.denise.palette[0], 0);
}

#[test]
fn copper_skip_over_move_consumes_move_fetch_slots_before_next_instruction() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, 0x0001);
    write_chip_word(&mut bus, cop1 + 2, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 4, 0x0180);
    write_chip_word(&mut bus, cop1 + 6, 0x0111);
    write_chip_word(&mut bus, cop1 + 8, 0x0182);
    write_chip_word(&mut bus, cop1 + 10, 0x0222);
    write_chip_word(&mut bus, cop1 + 12, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 14, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    bus.copper.jump(cop1 as u32);
    // SKIP fetch is 4 color clocks and is still in its tail cycles here, so
    // neither MOVE has run yet.
    bus.advance_chipset(4);

    assert_eq!(bus.denise.palette[0], 0);
    assert_eq!(bus.denise.palette[1], 0);

    // The SKIP's tail runs through the compare cycle at fetch2+4, the
    // skipped MOVE still spends its 4-color-clock fetch, then the second
    // MOVE spends its own before writing palette[1] at hpos 0x2E.
    bus.advance_chipset(11);

    assert_eq!(bus.denise.palette[0], 0);
    assert_eq!(bus.denise.palette[1], 0x0222);
}

#[test]
fn copper_wait_wakeup_keeps_vp7_loop_switch_on_scanline_boundary() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    let comb2 = cop1 + 4 + 42 * 4 + 4 + 4 + 8;
    let mut pc = cop1;

    write_chip_word(&mut bus, pc, 0x0033); // horizontal wait, VP7 low half
    write_chip_word(&mut bus, pc + 2, 0x80FE);
    pc += 4;
    for i in 0..42u16 {
        write_chip_word(&mut bus, pc, 0x0102); // BPLCON1
        write_chip_word(&mut bus, pc + 2, 0x0100 | i);
        pc += 4;
    }
    write_chip_word(&mut bus, pc, 0x7FE1); // skip COPJMP1 at vpos 127, hpos 224
    write_chip_word(&mut bus, pc + 2, 0xFFFF);
    pc += 4;
    write_chip_word(&mut bus, pc, 0x0088); // COPJMP1
    write_chip_word(&mut bus, pc + 2, 0x0000);
    pc += 4;
    write_chip_word(&mut bus, pc, 0x0080); // COP1LCH = comb2
    write_chip_word(&mut bus, pc + 2, ((comb2 >> 16) & 0x001F) as u16);
    pc += 4;
    write_chip_word(&mut bus, pc, 0x0082); // COP1LCL = comb2
    write_chip_word(&mut bus, pc + 2, (comb2 & 0xFFFE) as u16);

    pc = comb2;
    write_chip_word(&mut bus, pc, 0x8033); // horizontal wait, VP7 high half
    write_chip_word(&mut bus, pc + 2, 0x80FE);
    pc += 4;
    for i in 0..42u16 {
        write_chip_word(&mut bus, pc, 0x0102); // BPLCON1
        write_chip_word(&mut bus, pc + 2, 0x0200 | i);
        pc += 4;
    }
    write_chip_word(&mut bus, pc, 0xFFFF);
    write_chip_word(&mut bus, pc + 2, 0xFFFE);

    bus.agnus.cop1lc = cop1 as u32;
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.vpos = 126;
    bus.agnus.hpos = 0;
    bus.copper.jump(cop1 as u32);
    bus.advance_chipset(COLORCLOCKS_PER_LINE * 4);

    let bplcon1_writes_on = |vpos| {
        bus.current_render_events()
            .iter()
            .filter(|event| {
                event.source == BeamWriteSource::Copper
                    && event.offset == 0x0102
                    && event.vpos == vpos
            })
            .count()
    };
    assert_eq!(bplcon1_writes_on(126), 42);
    assert_eq!(bplcon1_writes_on(127), 42);
    assert_eq!(bplcon1_writes_on(128), 42);

    let line_128_hpos: Vec<_> = bus
        .current_render_events()
        .iter()
        .filter(|event| {
            event.source == BeamWriteSource::Copper && event.offset == 0x0102 && event.vpos == 128
        })
        .map(|event| event.hpos)
        .collect();
    // Each blast starts fetching at h=$32 (the WAIT's comparator lookahead
    // wakes it at $30), so the first BPLCON1 write lands at $34 and the
    // 42nd at $D8 -- the same positions on every line. The recorded render
    // events sit four colour clocks later, at the writes' Denise-effective
    // positions ($38..$DC).
    assert_eq!(line_128_hpos.first(), Some(&0x38));
    assert_eq!(line_128_hpos.last(), Some(&0xDC));
}

#[test]
fn copper_masked_end_of_list_stops_instead_of_waiting() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 2, 0xFFFC);
    write_chip_word(&mut bus, cop1 + 4, 0x0180);
    write_chip_word(&mut bus, cop1 + 6, 0x0555);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    bus.copper.jump(cop1 as u32);
    bus.advance_chipset(4);

    assert!(!bus.copper.is_running());
    assert!(bus.copper.waiting().is_none());
    assert_eq!(bus.denise.palette[0], 0);
}

#[test]
fn blitter_completion_deadline_skips_fixed_dma_slots() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BLTEN | DMACON_BPLEN;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x00D0;
    bus.agnus.vpos = 0x40; // inside the default vertical display window
    bus.agnus.hpos = 0x03A;
    bus.blitter.bltcon0 = 0x09F0; // A -> D copy: every A/D slot is a bus access
    bus.blitter.bltafwm = 0xFFFF;
    bus.blitter.bltalwm = 0xFFFF;
    bus.blitter.bltapt = 0x10;
    bus.blitter.bltdpt = 0x20;
    write_chip_word(&mut bus, 0x10, 0x1234);
    bus.blitter.start_scheduled((1 << 6) | 1, &bus.mem.chip_ram);

    assert_eq!(bus.blitter.scheduled_slots_remaining(), Some(8));
    // Four internal lead-in cycles elapse at 0x3A-0x3D regardless of DMA
    // (the BLTSIZE commit + startup extras plus StartDelay/Init), the A
    // fetch lands at 0x3E, then the idle D pipeline bubble must skip the
    // plane-1 bitplane fetch slot at 0x3F (idle phases stall through fixed
    // DMA), passing at 0x40; the internal E cycle runs at 0x41 and the
    // final F write lands at 0x42: 9 color clocks in all.
    assert_eq!(bus.next_blitter_completion_cck(), Some(9));

    // After the lead-in and the A fetch the idle D bubble is pending and
    // the bitplane fetch owns its fixed slot.
    bus.advance_chipset(5);
    assert!(bus.blitter.busy);
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Bitplane);

    bus.advance_chipset(3);
    assert!(bus.blitter.busy);
    bus.advance_chipset(1);
    assert!(!bus.blitter.busy);
    assert_eq!(&bus.mem.chip_ram[0x20..0x22], &[0x12, 0x34]);
    // INTREQ.BLIT is armed off the terminal BLTDONE cycle's first attempt
    // and is visible from the following colour clock on.
    assert_ne!(bus.paula.intreq & INT_BLIT, 0);
}

#[test]
fn blitter_completion_deadline_accounts_for_copper_dma_slots() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, 0x0180);
    write_chip_word(&mut bus, cop1 + 2, 0x0123);
    write_chip_word(&mut bus, cop1 + 4, 0x0182);
    write_chip_word(&mut bus, cop1 + 6, 0x0456);
    write_chip_word(&mut bus, cop1 + 8, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 10, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN | DMACON_BLTEN;
    bus.agnus.hpos = 0x20;
    bus.copper.jump(cop1 as u32);
    bus.blitter.bltcon0 = 0x09F0; // A -> D copy: every A/D slot is a bus access
    bus.blitter.bltafwm = 0xFFFF;
    bus.blitter.bltalwm = 0xFFFF;
    bus.blitter.bltapt = 0x10;
    bus.blitter.bltdpt = 0x20;
    write_chip_word(&mut bus, 0x10, 0x1234);
    bus.blitter.start_scheduled((1 << 6) | 1, &bus.mem.chip_ram);

    assert_eq!(bus.blitter.scheduled_slots_remaining(), Some(8));
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Copper);
    // Four internal lead-in cycles pass under the Copper's fetches at
    // 0x20-0x23, the A access takes the Copper's idle half at 0x25, the
    // idle D bubble passes under the fetch at 0x26 and the internal E
    // cycle at 0x27, and the final F write is blocked by the Copper's
    // fetch at 0x28, landing at 0x29: 10 color clocks in all.
    assert_eq!(bus.next_blitter_completion_cck(), Some(10));

    bus.advance_chipset(6);
    assert!(bus.blitter.busy);
    assert_eq!(bus.next_blitter_completion_cck(), Some(4));

    bus.advance_chipset(3);
    // The internal E cycle elapsed under the Copper's 0x28 fetch; the
    // terminal F write is pending for the Copper's idle half at 0x29.
    assert!(bus.blitter.busy);
    assert_eq!(bus.paula.intreq & INT_BLIT, 0);
    bus.advance_chipset(1);
    assert!(!bus.blitter.busy);
    assert_ne!(bus.paula.intreq & INT_BLIT, 0);
}

#[test]
fn copper_move_writes_visible_registers_on_second_dma_slot() {
    let mut bus = empty_bus();
    run_copper_moves_at(
        &mut bus,
        0x0100,
        RENDER_VISIBLE_START_VPOS,
        RENDER_COPPER_WAIT_HPOS_FB0,
        &[(0x0182, 0x00F0), (0x0092, 0x0040), (0x0100, 0x1200)],
    );

    let events = bus.current_render_events();
    assert_eq!(events.len(), 3);
    assert_eq!(
        events
            .iter()
            .map(|event| (event.offset, event.value, event.hpos, event.source))
            .collect::<Vec<_>>(),
        vec![
            // Writes land at +2/+6/+10 on the second-word fetches; the
            // recorded render events sit at the writes' Denise-effective
            // positions (+4).
            (
                0x0182,
                0x00F0,
                RENDER_COPPER_WAIT_HPOS_FB0 + 6,
                BeamWriteSource::Copper
            ),
            (
                0x0092,
                0x0040,
                RENDER_COPPER_WAIT_HPOS_FB0 + 10,
                BeamWriteSource::Copper
            ),
            (
                0x0100,
                0x1200,
                RENDER_COPPER_WAIT_HPOS_FB0 + 14,
                BeamWriteSource::Copper
            ),
        ]
    );
    assert_eq!(bus.denise.palette[1], 0x00F0);
    assert_eq!(bus.denise.ddfstrt, 0x0040);
    assert_eq!(bus.denise.bplcon0, 0x1200);
}

#[test]
fn copper_move_palette_write_affects_pixels_after_second_dma_slot() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.agnus.dmacon = DMACON_DMAEN;
    run_copper_moves_at(
        &mut bus,
        0x0100,
        RENDER_VISIBLE_START_VPOS,
        RENDER_COPPER_WAIT_HPOS_FB0 + 30,
        &[(0x0182, 0x00F0)],
    );

    let event_hpos = bus.current_render_events()[0].hpos;
    // MOVE write lands on its second-word fetch, two color clocks into the
    // 4-color-clock cadence from the start hpos (+30); the recorded event
    // sits at its Denise-effective position (+4).
    assert_eq!(event_hpos, RENDER_COPPER_WAIT_HPOS_FB0 + 36);
    let words_per_row = bitplane_words_per_row(
        bus.agnus.revision(),
        bus.denise.bplcon0,
        bus.agnus.fmode(),
        bus.denise.ddfstrt,
        bus.denise.ddfstop,
        bus.harddis_active(),
    );
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row,
        fetch_origin_cck: None,
        planes: [
            vec![0xFFFF; words_per_row],
            vec![0; words_per_row],
            vec![0; words_per_row],
            vec![0; words_per_row],
            vec![0; words_per_row],
            vec![0; words_per_row],
            Vec::new(),
            Vec::new(),
        ],
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    let event_x = render_color_write_x(event_hpos);
    assert!(event_x > STANDARD_VISIBLE_X0);
    assert_eq!(fb[event_x - 1], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[event_x], rgb12_to_rgba8(0x00F0));
}

#[test]
fn copper_write_restrictions_require_copcon_for_blitter_registers() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, 0x0040);
    write_chip_word(&mut bus, cop1 + 2, 0x1234);
    write_chip_word(&mut bus, cop1 + 4, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 6, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    bus.advance_chipset(4);
    assert_eq!(bus.blitter.bltcon0, 0);

    assert!(!bus.custom_write(0x02E, 2, 0x0002));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    bus.advance_chipset(4);
    assert_eq!(bus.blitter.bltcon0, 0x1234);
}

#[test]
fn copper_forbidden_move_ranges_match_copcon_cdang_on_ocs_and_ecs() {
    let cases_by_revision = [
        (
            AgnusRevision::Ocs,
            [
                (0x000, false, false),
                (0x000, true, false),
                (0x03E, true, false),
                (0x040, false, false),
                (0x040, true, true),
                (0x07E, false, false),
                (0x07E, true, true),
                (0x080, false, true),
                (0x180, false, true),
            ],
        ),
        (
            AgnusRevision::Ecs8372Rev4,
            [
                (0x000, false, false),
                (0x000, true, true),
                (0x03E, true, true),
                (0x040, false, false),
                (0x040, true, true),
                (0x07E, false, false),
                (0x07E, true, true),
                (0x080, false, true),
                (0x180, false, true),
            ],
        ),
    ];

    for (revision, cases) in cases_by_revision {
        for (register, cdang, should_continue) in cases {
            let bus = run_copper_guarded_move(revision, register, cdang);
            assert_eq!(
                bus.denise.palette[1] == 0x0ABC,
                should_continue,
                "revision={revision:?} register={register:#05X} cdang={cdang}"
            );
        }
    }
}

#[test]
fn ecs_copper_can_clear_copcon_then_loses_lower_register_access() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, 0x002E);
    write_chip_word(&mut bus, cop1 + 2, 0x0000);
    write_chip_word(&mut bus, cop1 + 4, 0x0040);
    write_chip_word(&mut bus, cop1 + 6, 0x1234);
    write_chip_word(&mut bus, cop1 + 8, 0x0182);
    write_chip_word(&mut bus, cop1 + 10, 0x0ABC);
    write_chip_word(&mut bus, cop1 + 12, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 14, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x02E, 2, 0x0002));
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    bus.advance_chipset(8);

    assert_eq!(bus.agnus.copcon, 0);
    assert_eq!(bus.blitter.bltcon0, 0);
    assert_eq!(bus.denise.palette[1], 0);
}

#[test]
fn copper_halts_on_forbidden_register_move_until_next_strobe() {
    let mut bus = empty_bus();
    let illegal = 0x0100usize;
    let legal = 0x0200usize;
    write_chip_word(&mut bus, illegal, 0x0000);
    write_chip_word(&mut bus, illegal + 2, 0x0000);
    write_chip_word(&mut bus, illegal + 4, 0x0180);
    write_chip_word(&mut bus, illegal + 6, 0x0555);
    write_chip_word(&mut bus, legal, 0x0180);
    write_chip_word(&mut bus, legal + 2, 0x0666);
    write_chip_word(&mut bus, legal + 4, 0xFFFF);
    write_chip_word(&mut bus, legal + 6, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, illegal as u64));
    assert!(!bus.custom_write(0x084, 2, 0x0000));
    assert!(!bus.custom_write(0x086, 2, legal as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));

    bus.advance_chipset(8);
    assert_eq!(bus.denise.palette[0], 0);
    assert!(!bus.copper.is_running());

    assert!(!bus.custom_write(0x08A, 2, 0xFFFF));
    bus.advance_chipset(4);
    assert_eq!(bus.denise.palette[0], 0x0666);
}

#[test]
fn forbidden_copper_move_recovers_at_start_of_frame_from_cop1lc() {
    let mut bus = empty_bus();
    let illegal = 0x0100usize;
    let legal = 0x0200usize;
    write_chip_word(&mut bus, illegal, 0x0000);
    write_chip_word(&mut bus, illegal + 2, 0x0000);
    write_chip_word(&mut bus, illegal + 4, 0x0180);
    write_chip_word(&mut bus, illegal + 6, 0x0555);
    write_chip_word(&mut bus, legal, 0x0180);
    write_chip_word(&mut bus, legal + 2, 0x0666);
    write_chip_word(&mut bus, legal + 4, 0xFFFF);
    write_chip_word(&mut bus, legal + 6, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, legal as u64));
    assert!(!bus.custom_write(0x084, 2, 0x0000));
    assert!(!bus.custom_write(0x086, 2, illegal as u64));
    assert!(!bus.custom_write(0x08A, 2, 0xFFFF));
    bus.advance_chipset(8);

    assert_eq!(bus.denise.palette[0], 0);
    assert!(!bus.copper.is_running());

    bus.agnus.vpos = crate::chipset::agnus::PAL_LINES - 1;
    bus.agnus.hpos = COLORCLOCKS_PER_LINE - 1;
    bus.advance_chipset(7);
    // Restart is immediate at the top of the frame, recovering from the
    // forbidden MOVE via the live COP1LC.
    assert_eq!(bus.pending_copper_frame_start, None);
    // From hpos 0 the restarted Copper spends its initial idle half, then its
    // MOVE fetches on the next two even (access-parity) color clocks: write
    // at hpos 0x0C.
    bus.advance_chipset(7);

    assert_eq!(bus.denise.palette[0], 0x0666);
}

#[test]
fn copper_move_updates_cop1lc_location_register() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, 0x0080);
    write_chip_word(&mut bus, cop1 + 2, 0x0003);
    write_chip_word(&mut bus, cop1 + 4, 0x0082);
    write_chip_word(&mut bus, cop1 + 6, 0x0200);
    write_chip_word(&mut bus, cop1 + 8, 0x0180);
    write_chip_word(&mut bus, cop1 + 10, 0x0555);
    write_chip_word(&mut bus, cop1 + 12, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 14, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    bus.advance_chipset(12);

    assert_eq!(bus.agnus.cop1lc, 0x030200);
    assert_eq!(bus.denise.palette[0], 0x0555);
}

#[test]
fn copper_cannot_set_copcon_itself() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, 0x002E);
    write_chip_word(&mut bus, cop1 + 2, 0x0002);
    write_chip_word(&mut bus, cop1 + 4, 0x0040);
    write_chip_word(&mut bus, cop1 + 6, 0x4321);
    write_chip_word(&mut bus, cop1 + 8, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 10, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    bus.advance_chipset(8);

    assert_eq!(bus.agnus.copcon, 0);
    assert_eq!(bus.blitter.bltcon0, 0);
}

#[test]
fn cpu_copjmp1_strobe_waits_for_target_instruction_dma_slots() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    write_chip_word(&mut bus, cop1, 0x0180);
    write_chip_word(&mut bus, cop1 + 2, 0x0123);
    write_chip_word(&mut bus, cop1 + 4, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 6, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));

    bus.advance_chipset(1);
    assert_eq!(bus.denise.palette[0], 0);
    assert!(bus.current_render_events().is_empty());

    // First-word fetch at 0x20, idle half at 0x21, second-word fetch+write
    // at 0x22 (recorded at its Denise-effective position, 0x26).
    bus.advance_chipset(2);
    assert_eq!(bus.denise.palette[0], 0x0123);
    let event = &bus.current_render_events()[0];
    assert_eq!(event.hpos, 0x26);
    assert_eq!(event.source, super::BeamWriteSource::Copper);
}

#[test]
fn copper_move_to_copjmp2_loads_second_list() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    let cop2 = 0x0200usize;
    write_chip_word(&mut bus, cop1, 0x008A);
    write_chip_word(&mut bus, cop1 + 2, 0x0000);
    write_chip_word(&mut bus, cop1 + 4, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 6, 0xFFFE);
    write_chip_word(&mut bus, cop2, 0x0180);
    write_chip_word(&mut bus, cop2 + 2, 0x0456);
    write_chip_word(&mut bus, cop2 + 4, 0xFFFF);
    write_chip_word(&mut bus, cop2 + 6, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x084, 2, 0x0000));
    assert!(!bus.custom_write(0x086, 2, cop2 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    // COPJMP2 MOVE fetch (4 cck), the strobe's two tail cycles
    // (COP_JMP1/COP_JMP2, reloading the program counter), then the cop2
    // MOVE fetch: write at hpos 0x2A.
    bus.advance_chipset(11);

    assert_eq!(bus.denise.palette[0], 0x0456);
    assert_eq!(
        bus.frame_render_events()[0].source,
        super::BeamWriteSource::Copper
    );
}

#[test]
fn copper_copjmp2_strobe_waits_for_target_instruction_dma_slots() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    let cop2 = 0x0200usize;
    write_chip_word(&mut bus, cop1, 0x008A);
    write_chip_word(&mut bus, cop1 + 2, 0x0000);
    write_chip_word(&mut bus, cop1 + 4, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 6, 0xFFFE);
    write_chip_word(&mut bus, cop2, 0x0180);
    write_chip_word(&mut bus, cop2 + 2, 0x0456);
    write_chip_word(&mut bus, cop2 + 4, 0xFFFF);
    write_chip_word(&mut bus, cop2 + 6, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x084, 2, 0x0000));
    assert!(!bus.custom_write(0x086, 2, cop2 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));

    bus.advance_chipset(2);
    assert_eq!(bus.denise.palette[0], 0);
    assert!(bus.current_render_events().is_empty());

    bus.advance_chipset(1);
    assert_eq!(bus.denise.palette[0], 0);
    assert!(bus.current_render_events().is_empty());

    // COPJMP2 strobe completes at 0x22, spends its two tail cycles
    // (COP_JMP1 0x24, COP_JMP2 0x26 reloading the program counter), then
    // the cop2 MOVE fetch writes at hpos 0x2A (recorded at its
    // Denise-effective position, 0x2E).
    bus.advance_chipset(8);
    assert_eq!(bus.denise.palette[0], 0x0456);
    let event = &bus.current_render_events()[0];
    assert_eq!(event.hpos, 0x2E);
    assert_eq!(event.source, super::BeamWriteSource::Copper);
}

#[test]
fn copper_can_program_cop2lc_before_copjmp2_loop_branch() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    let stale_cop2 = 0x0200usize;
    let programmed_cop2 = 0x0300usize;
    write_chip_word(&mut bus, cop1, 0x0084);
    write_chip_word(&mut bus, cop1 + 2, 0x0000);
    write_chip_word(&mut bus, cop1 + 4, 0x0086);
    write_chip_word(&mut bus, cop1 + 6, programmed_cop2 as u16);
    write_chip_word(&mut bus, cop1 + 8, 0x008A);
    write_chip_word(&mut bus, cop1 + 10, 0x0000);
    write_chip_word(&mut bus, cop1 + 12, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 14, 0xFFFE);
    write_chip_word(&mut bus, stale_cop2, 0x0180);
    write_chip_word(&mut bus, stale_cop2 + 2, 0x0111);
    write_chip_word(&mut bus, programmed_cop2, 0x0180);
    write_chip_word(&mut bus, programmed_cop2 + 2, 0x0789);
    write_chip_word(&mut bus, programmed_cop2 + 4, 0xFFFF);
    write_chip_word(&mut bus, programmed_cop2 + 6, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x084, 2, 0x0000));
    assert!(!bus.custom_write(0x086, 2, stale_cop2 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    // Three programming MOVEs, the strobe's two tail cycles
    // (COP_JMP1/COP_JMP2), then the jumped-to MOVE: the final write lands
    // at hpos 0x32.
    bus.advance_chipset(19);

    assert_eq!(bus.agnus.cop2lc, programmed_cop2 as u32);
    assert_eq!(bus.denise.palette[0], 0x0789);
}

#[test]
fn copper_can_program_cop1lc_before_copjmp1_loop_branch() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    let programmed_cop1 = 0x0300usize;
    write_chip_word(&mut bus, cop1, 0x0080);
    write_chip_word(&mut bus, cop1 + 2, 0x0000);
    write_chip_word(&mut bus, cop1 + 4, 0x0082);
    write_chip_word(&mut bus, cop1 + 6, programmed_cop1 as u16);
    write_chip_word(&mut bus, cop1 + 8, 0x0088);
    write_chip_word(&mut bus, cop1 + 10, 0x0000);
    write_chip_word(&mut bus, cop1 + 12, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 14, 0xFFFE);
    write_chip_word(&mut bus, programmed_cop1, 0x0180);
    write_chip_word(&mut bus, programmed_cop1 + 2, 0x0789);
    write_chip_word(&mut bus, programmed_cop1 + 4, 0xFFFF);
    write_chip_word(&mut bus, programmed_cop1 + 6, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    // Two programming MOVEs and the COPJMP1 MOVE, the strobe's two tail
    // cycles (COP_JMP1/COP_JMP2), then the jumped-to MOVE: the final write
    // lands at hpos 0x32.
    bus.advance_chipset(19);

    assert_eq!(bus.agnus.cop1lc, programmed_cop1 as u32);
    assert_eq!(bus.denise.palette[0], 0x0789);
}

#[test]
fn copper_programmed_cop1lc_sets_automatic_frame_restart() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    let programmed_cop1 = 0x0300usize;
    write_chip_word(&mut bus, cop1, 0x0080);
    write_chip_word(&mut bus, cop1 + 2, 0x0000);
    write_chip_word(&mut bus, cop1 + 4, 0x0082);
    write_chip_word(&mut bus, cop1 + 6, programmed_cop1 as u16);
    write_chip_word(&mut bus, cop1 + 8, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 10, 0xFFFE);
    write_chip_word(&mut bus, programmed_cop1, 0x0180);
    write_chip_word(&mut bus, programmed_cop1 + 2, 0x0789);
    write_chip_word(&mut bus, programmed_cop1 + 4, 0xFFFF);
    write_chip_word(&mut bus, programmed_cop1 + 6, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    bus.advance_chipset(8);

    assert_eq!(bus.agnus.cop1lc, programmed_cop1 as u32);

    bus.agnus.vpos = crate::chipset::agnus::PAL_LINES - 1;
    bus.agnus.hpos = COLORCLOCKS_PER_LINE - 1;
    bus.advance_chipset(7);

    // Restart is immediate at the top of the frame, picking up the
    // copper-programmed COP1LC straight away.
    assert_eq!(bus.pending_copper_frame_start, None);
    bus.advance_chipset(10);
    assert_eq!(bus.denise.palette[0], 0x0789);
}

#[test]
fn automatic_vblank_reload_restarts_cop1_after_copjmp2_branch() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    let cop2 = 0x0200usize;
    write_chip_word(&mut bus, cop1, 0x0180);
    write_chip_word(&mut bus, cop1 + 2, 0x0111);
    write_chip_word(&mut bus, cop1 + 4, 0x008A);
    write_chip_word(&mut bus, cop1 + 6, 0x0000);
    write_chip_word(&mut bus, cop1 + 8, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 10, 0xFFFE);
    write_chip_word(&mut bus, cop2, 0x0180);
    write_chip_word(&mut bus, cop2 + 2, 0x0222);
    write_chip_word(&mut bus, cop2 + 4, 0xFFFF);
    write_chip_word(&mut bus, cop2 + 6, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    assert!(!bus.custom_write(0x080, 2, 0x0000));
    assert!(!bus.custom_write(0x082, 2, cop1 as u64));
    assert!(!bus.custom_write(0x084, 2, 0x0000));
    assert!(!bus.custom_write(0x086, 2, cop2 as u64));
    assert!(!bus.custom_write(0x088, 2, 0xFFFF));
    // MOVE 0x0111 writes at 0x22; the COPJMP2 MOVE, its two strobe tail
    // cycles (COP_JMP1/COP_JMP2), then the cop2 MOVE follow, writing 0x0222
    // at hpos 0x2E. Recorded render events sit at the writes'
    // Denise-effective positions (+4).
    bus.advance_chipset(15);

    assert_eq!(bus.denise.palette[0], 0x0222);
    assert_eq!(bus.current_render_events()[0].hpos, 0x26);
    assert_eq!(bus.current_render_events()[1].hpos, 0x32);

    bus.agnus.vpos = crate::chipset::agnus::PAL_LINES - 1;
    bus.agnus.hpos = COLORCLOCKS_PER_LINE - 1;
    bus.advance_chipset(5);

    // Restart is immediate at the top of the frame (vpos 0), not delayed to
    // the end of vblank: the live COP1LC reload happens as the beam wraps.
    assert_eq!(bus.pending_copper_frame_start, None);
    assert_eq!(bus.denise.palette[0], 0x0222);

    let event_count = bus.current_render_events().len();

    // The vertical-blank strobe wakes the Copper at COPPER_FRAME_START_HPOS
    // (vAmiga-trace calibrated), so the restarted MOVE fetches on the first
    // free access-parity color clocks from there and the write lands at
    // hpos 0x04 (recorded at its Denise-effective position, 0x08).
    bus.advance_chipset(6);
    assert_eq!(bus.denise.palette[0], 0x0111);
    let event = &bus.current_render_events()[event_count];
    assert_eq!(event.vpos, 0);
    assert_eq!(event.hpos, 0x08);
    assert_eq!(event.source, super::BeamWriteSource::Copper);
}

#[test]
fn cpu_palette_writes_snapshot_top_and_status_palettes_by_interrupt_source() {
    let mut bus = empty_bus();

    assert!(!bus.custom_write(0x180, 2, 0x0123));
    assert_eq!(bus.beam_top_palette[0], 0x0123);
    assert!(!bus.beam_bottom_palette_valid);

    bus.agnus.vpos = 0xD4;
    bus.delivered_irq_pending = INT_COPER;
    assert!(!bus.custom_write(0x09C, 2, INT_COPER as u64));
    assert!(!bus.custom_write(0x182, 2, 0x0456));
    assert_eq!(bus.beam_top_palette[1], 0x0456);
    assert_eq!(bus.beam_bottom_palette[1], 0x0456);
    assert!(bus.beam_bottom_palette_valid);
    assert_eq!(
        bus.frame_render_events().last().map(|event| event.source),
        Some(super::BeamWriteSource::CpuCopperIrq)
    );

    bus.agnus.vpos = 0x10;
    assert!(!bus.custom_write(0x09C, 2, INT_VERTB as u64));
    assert!(!bus.custom_write(0x184, 2, 0x0789));
    assert_eq!(bus.beam_top_palette[2], 0x0789);
    assert_eq!(
        bus.frame_render_events().last().map(|event| event.source),
        Some(super::BeamWriteSource::Cpu)
    );
}

#[test]
fn cpu_palette_snapshot_routes_aga_banked_writes_by_bplcon3() {
    // Lisa decodes every COLORxx write against the BPLCON3 latch standing at
    // the write: BANK (bits 15-13) selects the 32-entry block and LOCT
    // (bit 9) the nibble half. The CPU-write shadow palette must decode the
    // same way, or an 8-bank CPU palette upload (Bubble and Squeak's level
    // fade) collapses onto entries 0..31: bank 0 ends up holding the
    // last-written bank and a frame later seeded from the shadow shows every
    // other bank black.
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);

    // Bank 0 high nibbles, then low nibbles via LOCT.
    assert!(!bus.custom_write(0x106, 2, 0x0000));
    assert!(!bus.custom_write(0x182, 2, 0x0123));
    assert!(!bus.custom_write(0x106, 2, 0x0200));
    assert!(!bus.custom_write(0x182, 2, 0x0456));
    // Bank 1 (BPLCON3 BANK=001): must land on entry 33, not entry 1.
    assert!(!bus.custom_write(0x106, 2, 0x2000));
    assert!(!bus.custom_write(0x182, 2, 0x0789));

    assert_eq!(bus.beam_top_palette[1], 0x0123, "bank 0 high word");
    assert_eq!(
        bus.beam_top_palette.rgb24(1),
        0x14_25_36,
        "bank 0 24-bit colour from high+low nibble writes"
    );
    assert_eq!(bus.beam_top_palette[33], 0x0789, "bank 1 entry");

    // A banked write inside a copper-interrupt window is not the OCS
    // split-palette pattern: it must not latch the bottom palette or record
    // replay events.
    bus.agnus.vpos = 0xD4;
    bus.delivered_irq_pending = INT_COPER;
    assert!(!bus.custom_write(0x09C, 2, INT_COPER as u64));
    assert!(!bus.custom_write(0x184, 2, 0x0AAA));
    assert_eq!(bus.beam_top_palette[34], 0x0AAA);
    assert!(!bus.beam_bottom_palette_valid);
    assert!(bus.frame_bottom_palette_events().is_empty());

    // The same write with bank 0 / LOCT clear selected is the classic
    // pattern and still latches.
    assert!(!bus.custom_write(0x106, 2, 0x0000));
    assert!(!bus.custom_write(0x186, 2, 0x0BBB));
    assert_eq!(bus.beam_bottom_palette[3], 0x0BBB);
    assert!(bus.beam_bottom_palette_valid);
}

#[test]
fn intreq_palette_target_uses_delivered_interrupt_when_coper_and_vertb_clear_together() {
    let mut bus = empty_bus();

    bus.delivered_irq_pending = INT_VERTB;
    assert!(!bus.custom_write(0x09C, 2, (INT_COPER | INT_VERTB) as u64));
    assert!(!bus.custom_write(0x180, 2, 0x0111));
    assert_eq!(bus.beam_top_palette[0], 0x0111);

    bus.agnus.vpos = 0xD4;
    bus.delivered_irq_pending = INT_COPER | INT_VERTB;
    assert!(!bus.custom_write(0x09C, 2, (INT_COPER | INT_VERTB) as u64));
    assert!(!bus.custom_write(0x182, 2, 0x0222));
    assert_eq!(bus.beam_top_palette[1], 0x0222);
    assert_eq!(bus.beam_bottom_palette[1], 0x0222);
    assert!(bus.beam_bottom_palette_valid);
}

#[test]
fn cpu_copper_irq_palette_events_persist_with_bottom_palette_for_render_replay() {
    let mut bus = empty_bus();

    bus.agnus.vpos = 0xFA;
    bus.delivered_irq_pending = INT_COPER;
    bus.delivered_copper_irq_beam = Some((0xD4, 0x16));
    assert!(!bus.custom_write(0x09C, 2, INT_COPER as u64));
    for idx in 0..16 {
        let value = if idx == 0 {
            0x0222
        } else {
            (0x0200 + idx) & 0x0FFF
        };
        assert!(!bus.custom_write(0x180 + idx * 2, 2, value));
    }
    assert_eq!(bus.frame_bottom_palette_events().len(), 16);
    assert_eq!(bus.frame_bottom_palette_events()[0].vpos, 0xD4);
    assert_eq!(bus.frame_bottom_palette_events()[0].hpos, 0x16);
    assert_eq!(bus.beam_top_palette[0], 0x0222);
    assert_eq!(bus.beam_top_palette[15], 0x020F);
    assert_eq!(bus.beam_bottom_palette[0], 0x0222);
    assert_eq!(bus.beam_bottom_palette[15], 0x020F);

    bus.agnus.vpos = 0xFA;
    bus.delivered_irq_pending = INT_COPER;
    bus.delivered_copper_irq_beam = Some((0x86, 0x10));
    assert!(!bus.custom_write(0x09C, 2, INT_COPER as u64));
    for idx in 0..16 {
        assert!(!bus.custom_write(0x180 + idx * 2, 2, (0x0400 + idx) & 0x0FFF));
    }
    assert_eq!(bus.beam_bottom_palette[0], 0x0222);
    assert_eq!(bus.frame_bottom_palette_events().len(), 16);
    assert_eq!(bus.frame_bottom_palette_events()[0].vpos, 0xD4);

    bus.begin_new_beam_frame();
    assert_eq!(bus.frame_bottom_palette_events().len(), 16);
    assert_eq!(bus.frame_bottom_palette_events()[0].vpos, 0xD4);
    for _ in 0..256 {
        bus.begin_new_beam_frame();
    }
    assert_eq!(bus.frame_bottom_palette_events().len(), 16);
    assert_eq!(bus.frame_bottom_palette_events()[0].vpos, 0xD4);
}

#[test]
fn cpu_copper_irq_high_palette_events_commit_bottom_replay_at_colour31() {
    let mut bus = empty_bus();

    bus.agnus.vpos = 0xFA;
    bus.delivered_irq_pending = INT_COPER;
    bus.delivered_copper_irq_beam = Some((0xD4, 0x16));
    assert!(!bus.custom_write(0x09C, 2, INT_COPER as u64));
    for idx in 17..32 {
        assert!(!bus.custom_write(0x180 + idx * 2, 2, (0x0300 + idx) & 0x0FFF));
    }

    assert_eq!(bus.frame_bottom_palette_events().len(), 15);
    assert_eq!(bus.frame_bottom_palette_events()[0].vpos, 0xD4);
    assert_eq!(bus.frame_bottom_palette_events()[0].hpos, 0x16);
    assert_eq!(bus.frame_bottom_palette_events()[0].offset, 0x1A2);
    assert_eq!(bus.frame_bottom_palette_events()[14].offset, 0x1BE);
    assert_eq!(bus.beam_bottom_palette[31], 0x031F);
}

#[test]
fn cpu_copper_irq_render_event_uses_actual_write_beam() {
    let mut bus = empty_bus();

    bus.delivered_irq_pending = INT_COPER;
    bus.delivered_copper_irq_beam = Some((0xD4, 0x16));
    assert!(!bus.custom_write(0x09C, 2, INT_COPER as u64));
    bus.agnus.vpos = 0xFA;
    bus.agnus.hpos = 0x40;
    assert!(!bus.custom_write(0x180, 2, 0x0222));

    // The render event records at the write's chip-bus slot plus the
    // Denise write-effect delay (source-independent, see
    // record_render_write); the beam-retargeted bottom-palette twin keeps
    // the delivered-IRQ beam verbatim.
    let render_event = bus.frame_render_events().last().unwrap();
    assert_eq!(render_event.vpos, 0xFA);
    assert_eq!(render_event.hpos, 0x44);
    assert_eq!(bus.pending_beam_bottom_palette_events[0].vpos, 0xD4);
    assert_eq!(bus.pending_beam_bottom_palette_events[0].hpos, 0x16);
}

#[test]
fn frame_palette_split_uses_completed_frame_snapshot() {
    let mut bus = empty_bus();

    bus.beam_top_palette.write_ocs(0, 0x0111);
    bus.begin_new_beam_frame();
    bus.beam_top_palette.write_ocs(0, 0x0333);
    bus.capture_current_frame_display_start();
    bus.beam_top_palette.write_ocs(0, 0x0555);
    bus.beam_bottom_palette.write_ocs(0, 0x0222);
    bus.beam_bottom_palette_valid = true;
    bus.begin_new_beam_frame();

    bus.beam_top_palette.write_ocs(0, 0x0777);
    bus.beam_bottom_palette.write_ocs(0, 0x0444);
    let (top, bottom, valid) = bus.frame_palette_split();

    assert_eq!(top[0], 0x0333);
    assert_eq!(bottom[0], 0x0222);
    assert!(valid);
}

#[test]
fn frame_chip_ram_uses_completed_frame_display_start_snapshot() {
    let mut bus = empty_bus();

    bus.mem.chip_ram[0] = 0x12;
    bus.begin_new_beam_frame();
    bus.mem.chip_ram[0] = 0x34;
    bus.capture_current_frame_display_start();
    bus.mem.chip_ram[0] = 0x56;
    bus.begin_new_beam_frame();
    bus.mem.chip_ram[0] = 0x78;

    assert_eq!(bus.frame_chip_ram()[0], 0x34);
}

#[test]
fn bitplane_dma_capture_samples_words_at_fetch_time() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3E;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x1111);
    write_chip_word(&mut bus, 0x0102, 0x2222);

    bus.advance_chipset(2);
    write_chip_word(&mut bus, 0x0100, 0xAAAA);
    write_chip_word(&mut bus, 0x0102, 0xBBBB);
    bus.advance_chipset(8);

    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    assert_eq!(row.planes[0], vec![0x1111, 0xBBBB]);
    assert_eq!(bus.display_dma_bplpt[0], 0x0104);
}

#[test]
fn bitplane_dma_capture_clips_ddfstart_to_hard_fetch_window() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x16;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0010;
    bus.denise.ddfstop = 0x0018;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x1111);
    write_chip_word(&mut bus, 0x0102, 0x2222);

    bus.advance_chipset(10);

    // Flop model: the DDFSTRT comparator at $10 fires while the hardware
    // start window (SHW, $18) is still down, so OCS never starts a run;
    // the old value-window model clamped the start to $18 and fetched.
    assert!(bus.frame_captured_bitplane_rows()[0].is_none());
    assert_eq!(bus.display_dma_bplpt[0], 0x0100);
}

#[test]
fn ocs_bitplane_dma_capture_extends_equal_ddf_window_to_hard_stop() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3E;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    for word in 0..21 {
        write_chip_word(&mut bus, 0x0100 + word * 2, 0x8000 | word as u16);
    }

    bus.advance_chipset(0x00E0 - 0x003E);

    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    assert_eq!(row.words_per_row, 21);
    assert_eq!(row.planes[0].len(), 21);
    assert_eq!(row.planes[0][0], 0x8000);
    assert_eq!(row.planes[0][20], 0x8014);
    assert_eq!(bus.display_dma_bplpt[0], 0x012A);
}

#[test]
fn bplxptl_half_write_merges_into_dma_advanced_live_pointer() {
    // BPLxPT is one live counter in Agnus: fetches and the end-of-line
    // modulo advance it with carry across the 16-bit register boundary,
    // and a BPLxPTL write replaces only the low half of that advanced
    // value. Software leans on this to flip 8-bitplane double buffers
    // with half the Copper writes: the modulo is sized so the carry
    // lands the high half on the next buffer, then only PTL is rewritten.
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3E;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x1000;
    assert!(!bus.custom_write(0x0E0, 2, 0x0001)); // BPL1PTH
    assert!(!bus.custom_write(0x0E2, 2, 0xFFF8)); // BPL1PTL
    assert!(!bus.custom_write(0x108, 2, 0x0010)); // BPL1MOD

    // Two lores words fetched (0x1FFF8 -> 0x1FFFC), then the line-end
    // modulo add carries into the high word: 0x1FFFC + 0x10 = 0x2000C.
    bus.advance_chipset(10);
    assert_eq!(bus.display_dma_bplpt[0], 0x0002_000C);

    // A PTL-only rewrite keeps the DMA-advanced high half, not the high
    // half of the last register write.
    assert!(!bus.custom_write(0x0E2, 2, 0x0100));
    assert_eq!(bus.display_dma_bplpt[0], 0x0002_0100);
    assert_eq!(bus.denise.bplpt[0], 0x0002_0100);
}

#[test]
fn bitplane_pointers_carry_across_vertical_blank_without_reload() {
    // Real Agnus never reloads BPLxPT at the top of a field: the counter
    // keeps its end-of-frame value until software rewrites it. Snapping
    // back to the last-written latch would break PTL-only buffer flips.
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3E;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x1000;
    assert!(!bus.custom_write(0x0E0, 2, 0x0000));
    assert!(!bus.custom_write(0x0E2, 2, 0x0100));

    bus.advance_chipset(10);
    assert_eq!(bus.display_dma_bplpt[0], 0x0104);

    // Stop bitplane DMA and run past the next vertical blank; the live
    // pointer must survive the frame boundary untouched.
    bus.agnus.dmacon = DMACON_DMAEN;
    bus.advance_chipset(80_000);
    assert_eq!(bus.display_dma_bplpt[0], 0x0104);
}

#[test]
fn aga_lores_eight_plane_dma_capture_fetches_all_eight_streams() {
    // FMODE=0 lo-res with BPLCON0 BPU3 set: Alice schedules all eight
    // plane streams inside the eight-colour-clock fetch unit (plane 8 in
    // the slot OCS/ECS leaves free at unit offset 0, plane 7 at offset 4),
    // so every plane's words reach Denise. A screen whose colours come
    // from the upper half of the 256-entry palette needs plane 8's bit.
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x30;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x0210; // BPU3 | COLOR: eight lo-res bitplanes
    for plane in 0..8 {
        let ptr = 0x0200 + plane as u32 * 0x0100;
        bus.denise.bplpt[plane] = ptr;
        bus.display_dma_bplpt[plane] = ptr;
        write_chip_word(&mut bus, ptr as usize, 0x1000 | plane as u16);
        write_chip_word(&mut bus, ptr as usize + 2, 0x2000 | plane as u16);
    }

    bus.advance_chipset(0x0050 - 0x0030);

    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    assert_eq!(row.nplanes, 8);
    assert_eq!(row.words_per_row, 2);
    for plane in 0..8 {
        assert_eq!(
            row.planes[plane],
            vec![0x1000 | plane as u16, 0x2000 | plane as u16],
            "plane {} fetched both words",
            plane + 1
        );
        assert_eq!(
            bus.display_dma_bplpt[plane],
            0x0200 + plane as u32 * 0x0100 + 4,
            "plane {} pointer advanced twice",
            plane + 1
        );
    }
}

#[test]
fn wide_fmode_dma_capture_packs_lores_slots_in_fetch_units() {
    // Lores FMODE=3 (32-cck fetch units) with DDFSTRT $30 / DDFSTOP
    // $D0: Agnus runs six 32-cck units from the DDFSTRT comparator and
    // packs the lores plane slots into the first eight CCK of each unit.
    // The final unit still completes before the PAL line edge, so the row
    // modulo advances after all 24 fetched words.
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    bus.agnus.write_fmode(0x0003); // BPL32 | BPAGEM = 64-bit fetches
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x16;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0030;
    bus.denise.ddfstop = 0x00D0;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bpl1mod = 0x0010;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    for word in 0..24 {
        write_chip_word(&mut bus, 0x0100 + word * 2, 0x9000 | word as u16);
    }

    // Advance through the whole line: every fetch unit lies inside it.
    bus.advance_chipset(0x00E3 - 0x16);

    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    assert_eq!(row.words_per_row, 24);
    assert_eq!(row.planes[0][0], 0x9000);
    assert_eq!(row.planes[0][23], 0x9017);
    // The line completed, so the modulo advanced the pointer past the
    // 48 fetched bytes.
    assert_eq!(bus.display_dma_bplpt[0], 0x0100 + 48 + 0x10);
}

#[test]
fn ecs_bitplane_dma_capture_extends_equal_ddf_window_to_hard_stop() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3E;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0xCAFE);
    write_chip_word(&mut bus, 0x0102, 0xBEEF);

    bus.advance_chipset(0x00E0 - 0x003E);

    // Flop model: the merged equal DDFSTRT/DDFSTOP strobe starts the run
    // with no stop pending on ECS too (the stop flop only latches while a
    // run is up), so the fetch extends to the hardware-stop drain:
    // 21 units at $38..$DF.
    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    assert_eq!(row.words_per_row, 21);
    assert_eq!(row.planes[0][0], 0xCAFE);
    assert_eq!(row.planes[0][1], 0xBEEF);
    assert_eq!(bus.display_dma_bplpt[0], 0x0100 + 21 * 2);
}

#[test]
fn bitplane_dmacon_enable_reaches_fetcher_after_two_cck() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3E;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x1111);
    write_chip_word(&mut bus, 0x0102, 0x2222);

    assert!(!bus.write_custom_word_from(0x096, 0x8000 | DMACON_BPLEN, BeamWriteSource::Cpu));
    bus.advance_chipset(0x0048 - 0x003E);

    // The enable reaches the sequencer two colour clocks after the write,
    // at $40 - the same strobe as the DDFSTOP match, which clears the
    // latched BPHSTART before the BMAPEN logic evaluates: no run starts
    // (flop model; the earlier $3F slot stayed idle either way).
    assert!(bus.frame_captured_bitplane_rows()[0].is_none());
    assert_eq!(bus.display_dma_bplpt[0], 0x0100);
}

#[test]
fn bitplane_dmacon_clear_reaches_fetcher_after_two_cck() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3E;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x1111);
    write_chip_word(&mut bus, 0x0102, 0x2222);

    assert!(!bus.write_custom_word_from(0x096, DMACON_BPLEN, BeamWriteSource::Cpu));
    bus.advance_chipset(0x0048 - 0x003E);

    // The clear reaches the sequencer two colour clocks after the write
    // ($40) and drops BPRUN immediately: the DDFSTOP drain unit does not
    // run, so only the $3F fetch of the first unit happened.
    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    assert_eq!(row.words_per_row, 1);
    assert_eq!(row.planes[0], vec![0x1111]);
    assert_eq!(bus.display_dma_bplpt[0], 0x0102);
}

#[test]
fn bitplane_bplcon0_enable_reaches_fetcher_after_three_cck() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3D;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x0000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x1111);
    write_chip_word(&mut bus, 0x0102, 0x2222);

    assert!(!bus.write_custom_word_from(0x100, 0x1000, BeamWriteSource::Cpu));
    bus.advance_chipset(0x0048 - 0x003D);

    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    assert_eq!(row.words_per_row, 2);
    assert_eq!(row.planes[0], vec![0x0000, 0x1111]);
    assert_eq!(bus.display_dma_bplpt[0], 0x0102);
}

#[test]
fn bitplane_bplcon0_clear_reaches_fetcher_after_three_cck() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3D;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x1111);
    write_chip_word(&mut bus, 0x0102, 0x2222);

    assert!(!bus.write_custom_word_from(0x100, 0x0000, BeamWriteSource::Cpu));
    bus.advance_chipset(0x0048 - 0x003D);

    // The BPLCON0 clear reaches the sequencer three colour clocks after the
    // write ($40): the DDFSTOP drain unit still runs, but with zero planes
    // it carries no fetch slots, so only the $3F fetch happened.
    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    assert_eq!(row.words_per_row, 1);
    assert_eq!(row.planes[0], vec![0x1111]);
    assert_eq!(bus.display_dma_bplpt[0], 0x0102);
}

#[test]
fn bitplane_dma_latches_plane_count_per_fetch_block() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x30;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0030;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x5200;
    bus.denise.bpl2mod = 4;
    for plane in 0..6 {
        let ptr = 0x0100 + plane * 0x0100;
        bus.denise.bplpt[plane] = ptr as u32;
        bus.display_dma_bplpt[plane] = ptr as u32;
        write_chip_word(&mut bus, ptr, 0x1000 | plane as u16);
        write_chip_word(&mut bus, ptr + 2, 0x2000 | plane as u16);
    }

    bus.advance_chipset(2);
    assert_eq!(bus.agnus.hpos, 0x32);
    assert!(!bus.write_custom_word_from(0x100, 0x6200, BeamWriteSource::Cpu));
    bus.advance_chipset(0x0040 - 0x0032);

    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    assert_eq!(row.nplanes, 6);
    assert_eq!(row.words_per_row, 2);
    assert_eq!(row.planes[5], vec![0x0000, 0x1005]);
    assert_eq!(bus.display_dma_bplpt[0], 0x0104);
    assert_eq!(bus.display_dma_bplpt[4], 0x0504);
    assert_eq!(bus.display_dma_bplpt[5], 0x0606);
}

#[test]
fn bitplane_ddfstrt_write_at_match_does_not_start_current_line() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x38;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0050;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x1111);
    write_chip_word(&mut bus, 0x0102, 0x2222);

    assert!(!bus.write_custom_word_from(0x092, 0x0038, BeamWriteSource::Cpu));
    bus.advance_chipset(0x0048 - 0x0038);

    assert!(bus.frame_captured_bitplane_rows()[0].is_none());
    assert_eq!(bus.display_dma_bplpt[0], 0x0100);
}

#[test]
fn bitplane_ddfstrt_write_before_match_starts_current_line() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x37;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0050;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x1111);
    write_chip_word(&mut bus, 0x0102, 0x2222);

    // A DDFSTRT write commits to the comparator four colour clocks after
    // the write slot (vAmiga's DMA_CYCLES(4) model): written at $37 it is
    // live from $3B, so a match position of $44 still fires this line. The
    // run then misses the already-passed $40 stop and extends to the
    // hardware-stop drain.
    assert!(!bus.write_custom_word_from(0x092, 0x0044, BeamWriteSource::Cpu));
    bus.advance_chipset(0x004C - 0x0037);

    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    assert_eq!(row.words_per_row, 19);
    assert_eq!(row.planes[0][0], 0x1111);
    assert_eq!(bus.display_dma_bplpt[0], 0x0102);
}

#[test]
fn bitplane_dma_capture_scans_fetch_window_independent_of_owner_hint() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3F;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0xCAFE);

    // Render capture is derived from the beam interval and DMA registers;
    // a coarse bus-owner hint must not suppress a due fetch.
    let (cck, tick) = bus.advance_one_chip_bus_quantum_limited(Some(ChipBusOwner::Idle), 2);

    assert_eq!(cck, 1);
    assert_eq!(tick.new_lines, 0);
    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    // Equal DDFSTRT/DDFSTOP plans to the hardware-stop drain (21 units);
    // the $3F fetch executed despite the Idle owner hint.
    assert_eq!(row.words_per_row, 21);
    assert_eq!(row.planes[0][0], 0xCAFE);
    assert_eq!(bus.display_dma_bplpt[0], 0x0102);
}

#[test]
fn bitplane_dma_capture_maps_early_vertical_overscan_to_first_framebuffer_row() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.current_frame_visible_start_vpos = RENDER_MIN_OVERSCAN_START_VPOS;
    bus.refresh_frame_geometry_visible_start();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = RENDER_MIN_OVERSCAN_START_VPOS;
    bus.agnus.hpos = 0x3E;
    bus.denise.diwstrt = 0x1C81;
    bus.denise.diwstop = 0x1DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0xCAFE);

    bus.capture_current_frame_display_start();
    bus.advance_chipset(2);

    assert_eq!(
        bus.current_frame_visible_start_vpos,
        RENDER_MIN_OVERSCAN_START_VPOS
    );
    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    // Equal DDFSTRT/DDFSTOP plans to the hardware-stop drain; one unit has
    // fetched so far.
    assert_eq!(row.words_per_row, 21);
    assert_eq!(row.planes[0][0], 0xCAFE);
    assert_eq!(bus.display_dma_bplpt[0], 0x0102);
}

#[test]
fn ecs_diwstrt_current_line_write_starts_live_bitplane_dma() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS;
    bus.agnus.hpos = 0x30;
    bus.current_frame_visible_start_vpos = RENDER_MIN_OVERSCAN_START_VPOS;
    bus.refresh_frame_geometry_visible_start();
    bus.denise.diwstrt = ((RENDER_VISIBLE_START_VPOS + 1) as u16) << 8 | 0x0083;
    bus.denise.diwstop = ((RENDER_VISIBLE_START_VPOS + 2) as u16) << 8 | 0x00C1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x1111);
    write_chip_word(&mut bus, 0x0102, 0x2222);

    assert!(!bus.write_custom_word_from(
        0x08E,
        (RENDER_VISIBLE_START_VPOS as u16) << 8 | 0x0083,
        BeamWriteSource::Cpu
    ));
    bus.advance_chipset(0x20);

    assert!(bus.current_frame_display_snapshot_taken);
    assert_eq!(
        bus.current_frame_visible_start_vpos,
        RENDER_MIN_OVERSCAN_START_VPOS
    );
    let fb_y = (RENDER_VISIBLE_START_VPOS - RENDER_MIN_OVERSCAN_START_VPOS) as usize;
    let row = bus.frame_captured_bitplane_rows()[fb_y].as_ref().unwrap();
    assert_eq!(row.words_per_row, 2);
    assert_eq!(row.planes[0], vec![0x1111, 0x2222]);
}

#[test]
fn ocs_diwstrt_current_line_write_does_not_start_live_bitplane_dma() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS;
    bus.agnus.hpos = 0x30;
    bus.current_frame_visible_start_vpos = RENDER_MIN_OVERSCAN_START_VPOS;
    bus.refresh_frame_geometry_visible_start();
    bus.denise.diwstrt = ((RENDER_VISIBLE_START_VPOS + 1) as u16) << 8 | 0x0083;
    bus.denise.diwstop = ((RENDER_VISIBLE_START_VPOS + 2) as u16) << 8 | 0x00C1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x1111);
    write_chip_word(&mut bus, 0x0102, 0x2222);

    assert!(!bus.write_custom_word_from(
        0x08E,
        (RENDER_VISIBLE_START_VPOS as u16) << 8 | 0x0083,
        BeamWriteSource::Cpu
    ));
    bus.advance_chipset(0x20);

    assert!(!bus.current_frame_display_snapshot_taken);
    let fb_y = (RENDER_VISIBLE_START_VPOS - RENDER_MIN_OVERSCAN_START_VPOS) as usize;
    assert!(bus.frame_captured_bitplane_rows()[fb_y].is_none());
    assert_eq!(bus.display_dma_bplpt[0], 0x0100);
}

#[test]
fn ecs_diwstrt_diwstop_write_reverts_to_implicit_diwhigh() {
    // ECS DIWHIGH only supplies the window MSBs when written after
    // DIWSTRT/DIWSTOP. A later DIWSTRT or DIWSTOP write must revert to
    // implicit (OCS-complement) decoding so that a program which sets the
    // window without DIWHIGH after an ECS display wrote DIWHIGH is not
    // clipped by a stale DIWHIGH.
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);

    assert!(!bus.write_custom_word_from(0x1E4, 0x0100, BeamWriteSource::Cpu));
    assert!(bus.denise.diwhigh_written);

    // A later DIWSTOP write clears the DIWHIGH-active state.
    assert!(!bus.write_custom_word_from(0x090, 0x34D1, BeamWriteSource::Cpu));
    assert!(!bus.denise.diwhigh_written);

    // Re-arm DIWHIGH, then a DIWSTRT write clears it too.
    assert!(!bus.write_custom_word_from(0x1E4, 0x0100, BeamWriteSource::Cpu));
    assert!(bus.denise.diwhigh_written);
    assert!(!bus.write_custom_word_from(0x08E, 0x2C81, BeamWriteSource::Cpu));
    assert!(!bus.denise.diwhigh_written);
}

#[test]
fn ocs_ignores_diwhigh_write_when_capturing_bitplane_dma() {
    let mut bus = empty_bus();
    assert!(!bus.write_custom_word_from(0x1E4, 0x0100, BeamWriteSource::Cpu));
    assert_eq!(bus.denise.diwhigh, 0);
    assert!(!bus.denise.diwhigh_written);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS;
    bus.agnus.hpos = 0x36;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2CC1;
    bus.denise.ddfstrt = 0x0030;
    bus.denise.ddfstop = 0x0030;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0xCAFE);

    bus.capture_current_frame_display_start();
    bus.advance_chipset(2);

    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    assert_eq!(row.planes[0][0], 0xCAFE);
}

#[test]
fn ocs_display_window_synthesizes_restricted_msb_ranges() {
    let implicit = DiwHigh::ocs_implicit();

    assert_eq!(diw_v_start(0xFC00, implicit), 0x00FC);
    assert_eq!(diw_h_start(0x00FC, implicit), 0x00FC);
    assert_eq!(diw_v_stop(0x7F00, implicit), 0x017F);
    assert_eq!(diw_v_stop(0x8000, implicit), 0x0080);
    assert_eq!(diw_h_stop(0x0001, implicit), 0x0101);
}

#[test]
fn ocs_display_window_zero_start_opens_from_beam_zero() {
    let implicit = DiwHigh::ocs_implicit();

    assert_eq!(
        visible_start_vpos_for_diw(0x0000, 0x2CC1, implicit),
        RENDER_MIN_OVERSCAN_START_VPOS
    );
    assert_eq!(
        clipped_display_rows_before_visible(
            0x0000,
            0x2CC1,
            implicit,
            RENDER_MIN_OVERSCAN_START_VPOS
        ),
        RENDER_MIN_OVERSCAN_START_VPOS as usize
    );
    assert!(display_window_contains_vpos(
        0x0000,
        0x2CC1,
        implicit,
        RENDER_MIN_OVERSCAN_START_VPOS,
    ));
    assert_eq!(
        live_display_window_x(0x0000, 0x2CC1, implicit),
        (0, (0x01C1 - RENDER_DIW_HSTART_FB0) * 2)
    );
    assert_eq!(
        visible_start_vpos_for_diw(0x0000, 0x0000, implicit),
        RENDER_VISIBLE_START_VPOS
    );
}

#[test]
fn ecs_diwhigh_write_zero_selects_direct_display_window_msbs() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);

    let before_diwhigh_write = bus.effective_diwhigh();
    assert_eq!(diw_v_stop(0x7F00, before_diwhigh_write), 0x017F);
    assert_eq!(diw_h_stop(0x0001, before_diwhigh_write), 0x0101);

    assert!(!bus.write_custom_word_from(0x1E4, 0x0000, BeamWriteSource::Cpu));
    let explicit_zero = bus.effective_diwhigh();
    assert_eq!(explicit_zero, DiwHigh::ecs_explicit(0));
    assert_eq!(diw_v_start(0xFC00, explicit_zero), 0x00FC);
    assert_eq!(diw_h_start(0x00FC, explicit_zero), 0x00FC);
    assert_eq!(diw_v_stop(0x7F00, explicit_zero), 0x007F);
    assert_eq!(diw_h_stop(0x0001, explicit_zero), 0x0001);
}

#[test]
fn manual_bpl1dat_write_sets_sprite_display_enable_at_denise_hpos() {
    let mut bus = empty_bus();
    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS;
    bus.agnus.hpos = RENDER_COPPER_WAIT_HPOS_FB0 + DENISE_HPOS_LAG_CCK + 2;

    assert!(!bus.write_custom_word_from(0x110, 0x8000, BeamWriteSource::Cpu));

    assert_eq!(bus.frame_sprite_display_enable_x_by_y()[0], Some(8));
}

#[test]
fn bitplane_dma_sets_sprite_display_enable_at_display_window_start() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS;
    bus.agnus.hpos = 0x38;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x4000);

    bus.advance_chipset(8);

    assert_eq!(
        bus.frame_sprite_display_enable_x_by_y()[0],
        Some(((0x81 - RENDER_DIW_HSTART_FB0) * 2) as usize)
    );
}

#[test]
fn late_bitplane_fetch_does_not_delay_sprite_display_inside_diw() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS;
    bus.agnus.hpos = 0x50;
    bus.denise.diwstrt = 0x2C91;
    bus.denise.diwstop = 0x2CC1;
    bus.denise.ddfstrt = 0x0050;
    bus.denise.ddfstop = 0x0050;
    bus.denise.bplcon0 = 0x3200;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x4000);

    bus.advance_chipset(8);

    assert_eq!(
        bus.frame_sprite_display_enable_x_by_y()[0],
        Some(((0x91 - RENDER_DIW_HSTART_FB0) * 2) as usize)
    );
}

#[test]
fn bitplane_dma_capture_keeps_pal_overscan_bottom_rows() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.current_frame_visible_start_vpos = RENDER_MIN_OVERSCAN_START_VPOS;
    bus.refresh_frame_geometry_visible_start();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x1C81;
    bus.denise.diwstop = 0x3EC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0xFACE);

    bus.capture_current_frame_display_start();
    let last_overscan_line = RENDER_VISIBLE_LINES - 1;
    bus.agnus.vpos = RENDER_MIN_OVERSCAN_START_VPOS + last_overscan_line as u32;
    bus.agnus.hpos = 0x3E;
    bus.advance_chipset(2);

    let row = bus.frame_captured_bitplane_rows()[last_overscan_line]
        .as_ref()
        .unwrap();
    // Equal DDFSTRT/DDFSTOP: the merged strobe starts the run without a
    // pending stop, so the plan extends to the hardware-stop drain (21
    // units); only the first unit's fetch has executed at this point.
    assert_eq!(row.words_per_row, 21);
    assert_eq!(row.planes[0][0], 0xFACE);
    assert_eq!(bus.display_dma_bplpt[0], 0x0102);
}

#[test]
fn bitplane_dma_capture_preserves_words_when_ddfstop_extends_same_line() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3E;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x1111);
    write_chip_word(&mut bus, 0x0102, 0x2222);

    bus.advance_chipset(2);
    bus.write_custom_word_from(0x094, 0x0040, BeamWriteSource::Cpu);
    bus.advance_chipset(8);

    // The equal start/stop strobe already started a run with no stop
    // pending, so the plan runs to the hardware-stop drain; the new $40
    // stop commits at $44, after $40 has passed, and never matches.
    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    assert_eq!(row.words_per_row, 21);
    assert_eq!(row.planes[0][0], 0x1111);
    assert_eq!(row.planes[0][1], 0x2222);
    assert_eq!(bus.display_dma_bplpt[0], 0x0104);
}

#[test]
fn bitplane_ddfstop_shrink_write_commits_too_late_to_cancel_the_match() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3E;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x1111);
    write_chip_word(&mut bus, 0x0102, 0x2222);

    bus.advance_chipset(2);
    // Written at $40, the new stop commits at $44 - after the old $40
    // value already matched, so the stop request stands and the drain
    // unit fetches the second word regardless of the rewrite.
    bus.write_custom_word_from(0x094, 0x0038, BeamWriteSource::Cpu);
    bus.advance_chipset(8);

    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    assert_eq!(row.words_per_row, 2);
    assert_eq!(row.planes[0], vec![0x1111, 0x2222]);
    assert_eq!(bus.display_dma_bplpt[0], 0x0104);
}

#[test]
fn captured_bitplane_rows_render_after_later_dmacon_clears_bplen() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x4000);
    bus.current_frame_render_base = bus.capture_render_snapshot();

    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3E;
    bus.advance_chipset(2);
    assert!(bus.frame_captured_bitplane_rows()[0].is_some());

    bus.write_custom_word_from(0x096, DMACON_BPLEN, BeamWriteSource::Cpu);
    assert_eq!(bus.agnus.dmacon & DMACON_BPLEN, 0);

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    // Content columns sit 2 fb px right of the hardware window edge
    // (bitmap positions are beam-anchored; STANDARD_VISIBLE_X0 moved to 62).
    assert_eq!(fb[STANDARD_VISIBLE_X0 + 2], rgb12_to_rgba8(0x0F00));
}

#[test]
fn beam_timed_display_window_changes_clip_later_bitplane_rows() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2EC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0048;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.denise.bplpt[0] = 0x0100;
    bus.current_frame_render_base = bus.capture_render_snapshot();
    for y in 0..2 {
        bus.current_frame_bitplane_rows[y] = Some(CapturedBitplaneRow {
            nplanes: 1,
            words_per_row: 3,
            fetch_origin_cck: None,
            planes: [
                vec![0x4000, 0, 0],
                vec![0; 3],
                vec![0; 3],
                vec![0; 3],
                vec![0; 3],
                vec![0; 3],
                Vec::new(),
                Vec::new(),
            ],
        });
    }
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS + 1,
        hpos: 0x38,
        offset: 0x08E,
        value: 0x2CA3,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    // Content columns sit 2 fb px right of the hardware window edge
    // (bitmap positions are beam-anchored; STANDARD_VISIBLE_X0 moved to 62).
    assert_eq!(fb[STANDARD_VISIBLE_X0 + 2], rgb12_to_rgba8(0x0F00));
    assert_eq!(
        fb[FB_WIDTH + STANDARD_VISIBLE_X0 + 2],
        rgb12_to_rgba8(0x0000)
    );
}

#[test]
fn hblank_tail_color_write_paints_following_row_from_left_edge() {
    // A copper COLOR00 write in the horizontal-blank tail (hpos < 0x12)
    // belongs to the previous output row's invisible tail; the following
    // row must show the new colour from its first framebuffer column.
    let mut bus = empty_bus();
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.bplcon0 = 0x0200;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS + 20,
        hpos: 0x06,
        offset: 0x180,
        value: 0x0F00,
        source: BeamWriteSource::Copper,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS + 20,
        hpos: 0xDE,
        offset: 0x180,
        value: 0x0333,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    let row = 20 * FB_WIDTH;
    assert_eq!(fb[row], rgb12_to_rgba8(0x0F00), "x=0");
    assert_eq!(fb[row + 8], rgb12_to_rgba8(0x0F00), "x=8");
    assert_eq!(fb[row + 20], rgb12_to_rgba8(0x0F00), "x=20");
}

#[test]
fn beam_timed_diwstrt_rewrite_after_window_open_does_not_reclip_line() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0048;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 3,
        fetch_origin_cck: None,
        planes: [
            vec![0xFFFF, 0xFFFF, 0xFFFF],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            Vec::new(),
            Vec::new(),
        ],
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x08E,
        value: 0x2C97,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    // The window flip-flop opened at the original HSTART before the
    // rewrite reached the comparators; a later HSTART only re-matches an
    // already-open window, so the line shows bitplanes continuously.
    assert_eq!(fb[68], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[106], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[108], rgb12_to_rgba8(0x0F00));
}

#[test]
fn beam_timed_diwstrt_clips_hidden_bitplane_pixels_without_rebasing_fetch_origin() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0048;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 3,
        fetch_origin_cck: None,
        planes: [
            vec![0x0400, 0, 0],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            Vec::new(),
            Vec::new(),
        ],
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x39,
        offset: 0x08E,
        value: 0x2C84,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    // Content columns sit 2 fb px right of the hardware window edge
    // (bitmap positions are beam-anchored; STANDARD_VISIBLE_X0 moved to 62).
    assert_eq!(fb[STANDARD_VISIBLE_X0 + 6], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[STANDARD_VISIBLE_X0 + 8], rgb12_to_rgba8(0x0F00));
}

#[test]
fn beam_timed_diwstrt_extends_later_bitplane_pixels_left_on_same_line() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2CA1;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0048;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 3,
        fetch_origin_cck: None,
        planes: [
            vec![0xFFFF, 0xFFFF, 0xFFFF],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            Vec::new(),
            Vec::new(),
        ],
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x08E,
        value: 0x2C83,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    // The rewrite reaches the comparators before the new HSTART's match
    // position, so the window opens there (not at the write position) and
    // shows bitplanes up to the fetched row's end.
    assert_eq!(fb[64], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[68], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[94], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[132], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[160], rgb12_to_rgba8(0x0000));
}

#[test]
fn beam_timed_diwstrt_can_enable_current_bitplane_line() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2D93;
    bus.denise.diwstop = 0x2EC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0048;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 3,
        fetch_origin_cck: None,
        planes: [
            vec![0xFFFF, 0xFFFF, 0xFFFF],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            Vec::new(),
            Vec::new(),
        ],
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x3C,
        offset: 0x08E,
        value: 0x2C93,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[STANDARD_VISIBLE_X0 + 34], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[STANDARD_VISIBLE_X0 + 36], rgb12_to_rgba8(0x0F00));
}

#[test]
fn beam_timed_diwstop_can_enable_current_bitplane_line() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C93;
    bus.denise.diwstop = 0x2C93;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0048;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 3,
        fetch_origin_cck: None,
        planes: [
            vec![0xFFFF, 0xFFFF, 0xFFFF],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            Vec::new(),
            Vec::new(),
        ],
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x3C,
        offset: 0x090,
        value: 0x2DC1,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[STANDARD_VISIBLE_X0 + 34], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[STANDARD_VISIBLE_X0 + 36], rgb12_to_rgba8(0x0F00));
}

#[test]
fn beam_timed_diwstop_extends_later_bitplane_pixels_on_same_line() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2D93;
    bus.denise.diwhigh = 0x0100;
    bus.denise.diwhigh_written = true;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0048;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 3,
        fetch_origin_cck: None,
        planes: [
            vec![0xFFFF, 0xFFFF, 0xFFFF],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            vec![0; 3],
            Vec::new(),
            Vec::new(),
        ],
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x3C,
        offset: 0x090,
        value: 0x2DC1,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[STANDARD_VISIBLE_X0 + 30], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[STANDARD_VISIBLE_X0 + 64], rgb12_to_rgba8(0x0F00));
}

#[test]
fn beam_timed_bitplane_pointer_changes_later_fallback_fetch_rows() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2EC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x4000);
    write_chip_word(&mut bus, 0x0102, 0x4000);
    write_chip_word(&mut bus, 0x0200, 0x0000);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS + 1,
        hpos: 0x38,
        offset: 0x0E2,
        value: 0x0200,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    // Content columns sit 2 fb px right of the hardware window edge
    // (bitmap positions are beam-anchored; STANDARD_VISIBLE_X0 moved to 62).
    assert_eq!(fb[STANDARD_VISIBLE_X0 + 2], rgb12_to_rgba8(0x0F00));
    assert_eq!(
        fb[FB_WIDTH + STANDARD_VISIBLE_X0 + 2],
        rgb12_to_rgba8(0x0000)
    );
}

#[test]
fn beam_timed_bitplane_pointer_changes_later_fallback_fetch_words() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2CA1;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0050;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x0000);
    write_chip_word(&mut bus, 0x0102, 0x0000);
    write_chip_word(&mut bus, 0x0104, 0x4000);
    write_chip_word(&mut bus, 0x0106, 0x4000);
    write_chip_word(&mut bus, 0x0200, 0x0000);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x50,
        offset: 0x0E2,
        value: 0x0200,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    let x_start = STANDARD_VISIBLE_X0 + 66;
    // Content columns sit 2 fb px right of the hardware window edge
    // (bitmap positions are beam-anchored; STANDARD_VISIBLE_X0 moved to 62).
    assert_eq!(fb[x_start], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[x_start + 32], rgb12_to_rgba8(0x0000));
}

#[test]
fn bplmod_write_before_last_lowres_fetch_slot_advances_next_line_pointer() {
    let pointer_after_mod_write = |write_hpos| {
        let mut bus = empty_bus();
        bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
        bus.agnus.vpos = RENDER_VISIBLE_START_VPOS;
        bus.agnus.hpos = 0x3E;
        bus.denise.diwstrt = 0x2C83;
        bus.denise.diwstop = 0x2DC1;
        bus.denise.ddfstrt = 0x0038;
        bus.denise.ddfstop = 0x0040;
        bus.denise.bplcon0 = 0x1000;
        bus.denise.bplpt[0] = 0x0100;
        bus.display_dma_bplpt[0] = 0x0100;

        bus.advance_chipset(write_hpos - bus.agnus.hpos);
        bus.write_custom_word_from(0x108, 0x0004, BeamWriteSource::Copper);
        bus.advance_chipset(0x48 - bus.agnus.hpos);
        bus.display_dma_bplpt[0]
    };

    assert_eq!(pointer_after_mod_write(0x46), 0x0108);
    assert_eq!(pointer_after_mod_write(0x48), 0x0104);
}

#[test]
fn manual_bitplane_data_respects_display_window_clip() {
    let mut bus = empty_bus();
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DD1;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    // First write: its serialiser grid slot (-2..30) sits entirely left of
    // the DIW edge at x 62, so the whole batch is border-clipped.
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x30,
        offset: 0x0110,
        value: 0xFFFF,
        source: BeamWriteSource::Copper,
    });
    // Second write: slot 62..94, opening exactly at the window edge.
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x0110,
        value: 0x8000,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[0], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[31], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[61], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[62], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[63], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[64], rgb12_to_rgba8(0x0000));
}

/// The serialiser parallel-loads a manually written word on its free-running
/// word cadence, not at the write position: writes 4 ccks apart can land in
/// the same 32-pixel lores slot, and a per-line raced stream clips to a
/// straight DIW edge (the Hamazing Hexagon field's left edge; grid calibrated
/// against vAmiga by timing-test/bplprobe-dat.asm).
#[test]
fn manual_bpl1dat_batches_snap_to_serializer_word_grid() {
    let mut bus = empty_bus();
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DD1;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    for (line, hpos) in [(0, 0x3C), (1, 0x40), (2, 0x44)] {
        bus.current_frame_render_events.push(BeamRegisterWrite {
            vpos: RENDER_VISIBLE_START_VPOS + line,
            hpos,
            offset: 0x0110,
            value: 0xFFFF,
            source: BeamWriteSource::Copper,
        });
    }

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    // Lines 0 and 1: writes 4 ccks apart, same slot 62..94, flush with the
    // window edge.
    for line in [0usize, 1] {
        assert_eq!(fb[line * FB_WIDTH + 61], rgb12_to_rgba8(0x0000));
        assert_eq!(fb[line * FB_WIDTH + 62], rgb12_to_rgba8(0x0F00));
        assert_eq!(fb[line * FB_WIDTH + 93], rgb12_to_rgba8(0x0F00));
        assert_eq!(fb[line * FB_WIDTH + 94], rgb12_to_rgba8(0x0000));
    }
    // Line 2: 4 ccks later again, next slot 94..126.
    assert_eq!(fb[2 * FB_WIDTH + 93], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[2 * FB_WIDTH + 94], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[2 * FB_WIDTH + 125], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[2 * FB_WIDTH + 126], rgb12_to_rgba8(0x0000));
}

/// Re-arming BPL1DAT before the next serialiser load strobe replaces the held
/// word: back-to-back writes in one slot display the second value once, with
/// no second batch in the following slot (bplprobe-dat's double-write band).
#[test]
fn manual_bpl1dat_rearm_before_load_replaces_held_word() {
    let mut bus = empty_bus();
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DD1;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x3C,
        offset: 0x0110,
        value: 0xFFFF,
        source: BeamWriteSource::Copper,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x0110,
        value: 0xF0F0,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    // Slot 62..94 shows the second word's comb, not the first's solid bar.
    assert_eq!(fb[62], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[69], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[70], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[78], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[86], rgb12_to_rgba8(0x0000));
    // And no second batch in the next slot.
    assert_eq!(fb[94], rgb12_to_rgba8(0x0000));
}

/// Hires manual batches load on the 16-pixel hires word cadence (probe bands
/// @51/@53: a 2-cck WAIT step moves the bar one hires slot).
#[test]
fn manual_bpl1dat_hires_batches_snap_to_hires_word_grid() {
    let mut bus = empty_bus();
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DD1;
    bus.denise.bplcon0 = 0x9200;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    for (line, hpos) in [(0, 0x40), (1, 0x42), (2, 0x44)] {
        bus.current_frame_render_events.push(BeamRegisterWrite {
            vpos: RENDER_VISIBLE_START_VPOS + line,
            hpos,
            offset: 0x0110,
            value: 0xFFFF,
            source: BeamWriteSource::Copper,
        });
    }

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    // Lines 0 and 1: slot 62..78; line 2: one hires slot later, 78..94.
    for line in [0usize, 1] {
        assert_eq!(fb[line * FB_WIDTH + 61], rgb12_to_rgba8(0x0000));
        assert_eq!(fb[line * FB_WIDTH + 62], rgb12_to_rgba8(0x0F00));
        assert_eq!(fb[line * FB_WIDTH + 77], rgb12_to_rgba8(0x0F00));
        assert_eq!(fb[line * FB_WIDTH + 78], rgb12_to_rgba8(0x0000));
    }
    assert_eq!(fb[2 * FB_WIDTH + 77], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[2 * FB_WIDTH + 78], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[2 * FB_WIDTH + 93], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[2 * FB_WIDTH + 94], rgb12_to_rgba8(0x0000));
}

#[test]
fn bpl1dat_write_triggers_output_while_bitplane_dma_enabled() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 2,
        fetch_origin_cck: None,
        planes: [
            vec![0, 0],
            vec![0, 0],
            vec![0, 0],
            vec![0, 0],
            vec![0, 0],
            vec![0, 0],
            Vec::new(),
            Vec::new(),
        ],
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x0110,
        value: 0x8000,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    // The write's batch snaps to serialiser slot 62..94; its set bit is the
    // first lores pixel of the slot.
    assert_eq!(fb[61], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[62], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[63], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[64], rgb12_to_rgba8(0x0000));
}

#[test]
fn previsible_bpl1dat_write_does_not_draw_first_visible_line() {
    let mut bus = empty_bus();
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS - 1,
        hpos: 0x40,
        offset: 0x0110,
        value: 0x8000,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[32], rgb12_to_rgba8(0x0000));
}

#[test]
fn same_line_diwstrt_extension_clips_later_manual_bitplane_pixels() {
    let mut bus = empty_bus();
    bus.denise.diwstrt = 0x2C93;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    // The DIWSTRT rewrite lands at beam x 80, mid-way through the batch in
    // serialiser slot 62..94: pixels left of it stay under the original
    // narrow window (start x 98, invisible), pixels from 80 see the extended
    // window (start x 66) and show.
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x3C,
        offset: 0x008E,
        value: 0x2C83,
        source: BeamWriteSource::Copper,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x0110,
        value: 0xFFFF,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[68], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[79], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[80], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[93], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[94], rgb12_to_rgba8(0x0000));
}

#[test]
fn same_line_palette_write_colors_later_manual_bitplane_pixels() {
    let mut bus = empty_bus();
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x0110,
        value: 0xFFFF,
        source: BeamWriteSource::Copper,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x4A,
        offset: 0x0182,
        value: 0x00F0,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    let event_x = render_color_write_x(0x4A);
    assert_eq!(fb[event_x - 1], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[event_x], rgb12_to_rgba8(0x00F0));
}

#[test]
fn same_line_bplcon0_plane_count_clips_later_manual_bitplane_pixels() {
    let mut bus = empty_bus();
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    // A CPU BPLCON0 write just before the copper arm: its output-stage
    // effect lands at beam x 88, mid-way through the batch in serialiser
    // slot 62..94. (An ordered copper stream cannot cut its own batch: the
    // earliest post-arm MOVE's beam effect always lands past the slot end.)
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x3E,
        offset: 0x0100,
        value: 0x0000,
        source: BeamWriteSource::Cpu,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x0110,
        value: 0xFFFF,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[87], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[88], rgb12_to_rgba8(0x0000));
}

#[test]
fn same_line_bplcon0_hires_changes_later_manual_bitplane_pixel_repeat() {
    let mut bus = empty_bus();
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    // CPU hires switch effective at beam x 88, mid-way through the batch in
    // slot 62..94: word bit 10 shifts out lores-wide before it (x 72..74),
    // bit 2 shifts out one hires pixel wide after it (x 88..89).
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x3E,
        offset: 0x0100,
        value: 0x9000,
        source: BeamWriteSource::Cpu,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x0110,
        value: 0x0404,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[71], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[72], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[73], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[74], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[88], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[89], rgb12_to_rgba8(0x0000));
}

#[test]
fn manual_ham_bitplane_words_carry_previous_pixel_color() {
    let mut bus = empty_bus();
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.bplcon0 = 0x6800;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0123);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    // Two batches in adjacent serialiser slots (62..94 and 94..126): the
    // first sets the base colour, the second's HAM modify starts from the
    // first batch's last pixel.
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x0110,
        value: 0xFFFF,
        source: BeamWriteSource::Copper,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x44,
        offset: 0x0114,
        value: 0xFFFF,
        source: BeamWriteSource::Copper,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x44,
        offset: 0x011A,
        value: 0xFFFF,
        source: BeamWriteSource::Copper,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x48,
        offset: 0x0110,
        value: 0xFFFF,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[79], rgb12_to_rgba8(0x0123));
    assert_eq!(fb[80], rgb12_to_rgba8(0x0123));
    assert_eq!(fb[93], rgb12_to_rgba8(0x0123));
    // The second batch's first lores pixel carries the held colour; the
    // modify shows from the next lores pixel on.
    assert_eq!(fb[94], rgb12_to_rgba8(0x0123));
    assert_eq!(fb[96], rgb12_to_rgba8(0x0523));
}

#[test]
fn same_line_ham_enable_does_not_retime_earlier_playfield_color() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0123);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 6,
        words_per_row: 1,
        fetch_origin_cck: None,
        planes: [
            vec![0xC000],
            vec![0x0000],
            vec![0x2000],
            vec![0x0000],
            vec![0x2000],
            vec![0x0000],
            Vec::new(),
            Vec::new(),
        ],
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x39,
        offset: 0x0100,
        value: 0x6800,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[2], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[4], rgb12_to_rgba8(0x0000));
}

#[test]
fn same_line_ham_enable_modifies_previous_manual_bitplane_color() {
    let mut bus = empty_bus();
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0123);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    // CPU HAM enable effective at beam x 88, inside the first batch (slot
    // 62..94, index 1: HAM set = the same palette colour on both sides of
    // the switch); the second batch (slot 94..126, index 20) then modifies
    // blue from the first batch's last pixel.
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x3E,
        offset: 0x0100,
        value: 0x6800,
        source: BeamWriteSource::Cpu,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x0110,
        value: 0xFFFF,
        source: BeamWriteSource::Copper,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x44,
        offset: 0x0114,
        value: 0xFFFF,
        source: BeamWriteSource::Copper,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x44,
        offset: 0x0118,
        value: 0xFFFF,
        source: BeamWriteSource::Copper,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x48,
        offset: 0x0110,
        value: 0x0000,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[80], rgb12_to_rgba8(0x0123));
    assert_eq!(fb[93], rgb12_to_rgba8(0x0123));
    // The second batch's first lores pixel carries the held colour; the
    // modify shows from the next lores pixel on.
    assert_eq!(fb[94], rgb12_to_rgba8(0x0123));
    assert_eq!(fb[96], rgb12_to_rgba8(0x0124));
}

#[test]
fn beam_timed_ddfstop_shrink_blanks_later_fallback_fetch_words_on_same_line() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0040;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.denise.bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x0000);
    write_chip_word(&mut bus, 0x0102, 0x8000);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x094,
        value: 0x0038,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[0], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[32], rgb12_to_rgba8(0x0000));
}

#[test]
fn beam_timed_bplcon1_scroll_changes_later_pixels_on_same_line() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplcon1 = 0;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(captured_row(1, 21, &[&[(0, 0x5000)]]));
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x39,
        offset: 0x0102,
        value: 0x0004,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    // The mid-line BPLCON1 write (horizontal scroll = 4) shifts this
    // word's lit pixels right by four lores pixels (eight framebuffer
    // columns): 0xA000's set pixels land at columns 40 and 44 instead
    // of the unscrolled origin at 32 and 36.
    let red = rgb12_to_rgba8(0x0F00);
    let black = rgb12_to_rgba8(0x0000);
    assert_eq!(fb[72], red);
    assert_eq!(fb[76], red);
    assert_eq!(fb[64], black);
    assert_eq!(fb[68], black);
}

#[test]
fn beam_timed_bplcon1_scroll_decrease_reveals_later_pixels_on_same_line() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplcon1 = 4;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(captured_row(1, 21, &[&[(0, 0x1000)]]));
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x39,
        offset: 0x0102,
        value: 0x0000,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    // The mid-line BPLCON1 write drops the scroll from 4 back to 0,
    // so the lit lores pixel 2 is revealed at the unscrolled origin
    // (columns 36/37) instead of the scrolled-by-4 position (44/45).
    let red = rgb12_to_rgba8(0x0F00);
    let black = rgb12_to_rgba8(0x0000);
    assert_eq!(fb[68], red);
    assert_eq!(fb[69], red);
    assert_eq!(fb[76], black);
    assert_eq!(fb[64], black);
}

#[test]
fn same_line_bplcon2_killehb_changes_later_extra_half_brite_pixels() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x6000;
    bus.denise.bplcon2 = 0;
    bus.denise.palette.write_ocs(1, 0x0E00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 6,
        words_per_row: 1,
        fetch_origin_cck: None,
        planes: [
            vec![0xFFFF],
            vec![0],
            vec![0],
            vec![0],
            vec![0],
            vec![0xFFFF],
            Vec::new(),
            Vec::new(),
        ],
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x3C,
        offset: 0x0104,
        value: 0x0200,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[68], rgb12_to_rgba8(0x0700));
    assert_eq!(fb[80], rgb12_to_rgba8(0x0E00));
}

#[test]
fn beam_timed_bplcon0_hires_narrows_later_bitplane_pixels() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x003C;
    bus.denise.ddfstop = 0x0044;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 4,
        fetch_origin_cck: None,
        planes: [
            vec![0x0000, 0x0000, 0x2000, 0x0000],
            vec![0; 4],
            vec![0; 4],
            vec![0; 4],
            vec![0; 4],
            vec![0; 4],
            Vec::new(),
            Vec::new(),
        ],
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x0100,
        value: 0x9000,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    // Content columns sit flush against the hardware window edge
    // (STANDARD_VISIBLE_X0 = 62): standard hi-res places its first fetched
    // sample at the window edge, so the set bit lands two fb px left of the
    // old beam-anchored placement.
    assert_eq!(fb[STANDARD_VISIBLE_X0 + 32], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[STANDARD_VISIBLE_X0 + 34], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[STANDARD_VISIBLE_X0 + 35], rgb12_to_rgba8(0x0000));
}

#[test]
fn beam_timed_bplcon0_lowres_widens_later_bitplane_pixels() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x003C;
    bus.denise.ddfstop = 0x003C;
    bus.denise.bplcon0 = 0x9000;
    bus.denise.palette.write_ocs(0, 0x0000);
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 2,
        fetch_origin_cck: None,
        planes: [
            vec![0x0000, 0x4000],
            vec![0; 2],
            vec![0; 2],
            vec![0; 2],
            vec![0; 2],
            vec![0; 2],
            Vec::new(),
            Vec::new(),
        ],
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x0100,
        value: 0x1000,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    // The lo-res reinterpretation places the picture on the lo-res shifter
    // reload grid: DDFSTRT $3C rounds UP to the $40 slot (hardware-verified
    // on the arosddf1 ECS photo), so the widened word-1 bit sits one 8-cck
    // unit right of a floor-aligned placement.
    assert_eq!(fb[STANDARD_VISIBLE_X0 + 64], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[STANDARD_VISIBLE_X0 + 66], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[STANDARD_VISIBLE_X0 + 67], rgb12_to_rgba8(0x0F00));
}

#[test]
fn same_line_bplcon2_priority_change_reveals_later_sprite_pixels() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN | DMACON_SPREN;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplcon2 = 0;
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.denise.palette.write_ocs(17, 0x00F0);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 1,
        fetch_origin_cck: None,
        planes: [
            vec![0xFFFF],
            vec![0],
            vec![0],
            vec![0],
            vec![0],
            vec![0],
            Vec::new(),
            Vec::new(),
        ],
    });
    bus.current_frame_sprite_dma_observed = true;
    bus.current_frame_sprite_display_enable_x_by_y[0] = Some(0);
    bus.current_frame_sprite_lines.push(CapturedSpriteLine {
        sprite: 0,
        hstart: RENDER_DIW_HSTART_FB0 + 34 - SPRITE_OUTPUT_DELAY_LORES,
        hsub_70ns: false,
        beam_y: RENDER_VISIBLE_START_VPOS as i32,
        data: 0xFFFF,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x3C,
        offset: 0x0104,
        value: 0x0008,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[68], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[80], rgb12_to_rgba8(0x00F0));
}

#[test]
fn same_line_bplcon3_spres_narrows_later_sprite_pixels() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.bplcon0 = 0;
    bus.denise.bplcon3 = 0;
    bus.denise.palette.write_ocs(17, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_sprite_dma_observed = true;
    bus.current_frame_sprite_lines.push(CapturedSpriteLine {
        sprite: 0,
        hstart: RENDER_DIW_HSTART_FB0 + 34 - SPRITE_OUTPUT_DELAY_LORES,
        hsub_70ns: false,
        beam_y: RENDER_VISIBLE_START_VPOS as i32,
        data: 0x0100,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    });
    bus.current_frame_sprite_display_enable_x_by_y[0] = Some(0);
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x3C,
        offset: 0x0106,
        value: BPLCON3_SPRES_HIRES,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[81], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[82], rgb12_to_rgba8(0x0000));
}

#[test]
fn same_line_bplcon3_pf2of_changes_later_dual_playfield_pixels() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x2400;
    bus.denise.bplcon3 = 0;
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.denise.palette.write_ocs(17, 0x00F0);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 2,
        words_per_row: 1,
        fetch_origin_cck: None,
        planes: [
            vec![0],
            vec![0xFFFF],
            vec![0],
            vec![0],
            vec![0],
            vec![0],
            Vec::new(),
            Vec::new(),
        ],
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x3C,
        offset: 0x0106,
        value: 0x1000,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[68], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[80], rgb12_to_rgba8(0x00F0));
}

#[test]
fn lowres_fallback_fetches_ignore_chip_ram_writes_after_plane_slot() {
    let mut bus = empty_bus();
    let mut snapshot = RenderRegisterSnapshot {
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x2000,
        diwstrt: 0x2C83,
        diwstop: 0x2DC1,
        ddfstrt: 0x0038,
        ddfstop: 0x0038,
        ..RenderRegisterSnapshot::default()
    };
    snapshot.palette.write_ocs(0, 0x0000);
    snapshot.palette.write_ocs(1, 0x0F00);
    snapshot.bplpt[0] = 0x0100;
    snapshot.bplpt[1] = 0x0200;
    bus.last_frame_render_base = Some(snapshot);
    bus.last_frame_chip_ram = std::sync::Arc::new(vec![0; bus.mem.chip_ram.len()]);
    bus.last_frame_chip_ram_writes
        .push(BeamChipRamWrite::from_bytes(
            RENDER_VISIBLE_START_VPOS,
            0x40,
            0x0100,
            &[0x80, 0x00],
        ));

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[0], rgb12_to_rgba8(0x0000));
}

#[test]
fn lowres_fallback_fetches_ignore_pointer_writes_after_plane_slot() {
    let mut bus = empty_bus();
    let mut snapshot = RenderRegisterSnapshot {
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x2000,
        diwstrt: 0x2C83,
        diwstop: 0x2DC1,
        ddfstrt: 0x0038,
        ddfstop: 0x0038,
        ..RenderRegisterSnapshot::default()
    };
    snapshot.palette.write_ocs(0, 0x0000);
    snapshot.palette.write_ocs(1, 0x0F00);
    snapshot.bplpt[0] = 0x0100;
    snapshot.bplpt[1] = 0x0200;
    bus.last_frame_render_base = Some(snapshot);
    bus.last_frame_chip_ram = std::sync::Arc::new(vec![0; bus.mem.chip_ram.len()]);
    std::sync::Arc::make_mut(&mut bus.last_frame_chip_ram)[0x0300] = 0x80;
    bus.last_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: 0x40,
        offset: 0x0E2,
        value: 0x0300,
        source: BeamWriteSource::Copper,
    });

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(fb[0], rgb12_to_rgba8(0x0000));
}

#[test]
fn sprite_dma_capture_samples_line_words_at_beam_time() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0100usize;
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0083);
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x1111);
    write_chip_word(&mut bus, sprite_ptr + 6, 0x2222);
    write_chip_word(&mut bus, sprite_ptr + 8, 0);
    write_chip_word(&mut bus, sprite_ptr + 10, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(1);
    write_chip_word(&mut bus, sprite_ptr + 4, 0xAAAA);
    write_chip_word(&mut bus, sprite_ptr + 6, 0xBBBB);
    // Crosses the first slot ($15): DATA is sampled here.
    bus.advance_chipset(1);
    write_chip_word(&mut bus, sprite_ptr + 4, 0xCCCC);
    write_chip_word(&mut bus, sprite_ptr + 6, 0xDDDD);
    // Crosses the second slot ($17): DATB is sampled here, after the
    // rewrite, so the two words of one line see different memory states.
    bus.advance_chipset(2);

    let lines = bus.frame_captured_sprite_lines();
    assert!(bus.frame_sprite_dma_observed());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].sprite, 0);
    assert_eq!(lines[0].beam_y, 0x2C);
    assert_eq!(lines[0].hstart, 0x0083);
    assert_eq!(lines[0].data, 0xAAAA);
    assert_eq!(lines[0].datb, 0xDDDD);
}

#[test]
fn inactive_sprite_pointer_write_before_pair_slot_seeds_next_descriptor_fetch() {
    let mut bus = empty_bus();
    let old_ptr = 0x0100usize;
    let new_ptr = 0x0200usize;
    let (old_pos, old_ctl) = sprite_control_words(0x2C, 0x30, 0x0083);
    let (new_pos, new_ctl) = sprite_control_words(0x2C, 0x30, 0x00A1);
    write_chip_word(&mut bus, old_ptr, old_pos);
    write_chip_word(&mut bus, old_ptr + 2, old_ctl);
    write_chip_word(&mut bus, old_ptr + 4, 0x1111);
    write_chip_word(&mut bus, old_ptr + 6, 0x2222);
    write_chip_word(&mut bus, new_ptr, new_pos);
    write_chip_word(&mut bus, new_ptr + 2, new_ctl);
    write_chip_word(&mut bus, new_ptr + 4, 0xAAAA);
    write_chip_word(&mut bus, new_ptr + 6, 0xBBBB);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.denise.sprpt[0] = old_ptr as u32;
    bus.display_dma_sprpt[0] = old_ptr as u32;
    bus.sprite_dma_frame_start_ptr[0] = old_ptr as u32;
    bus.current_frame_render_base.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.current_frame_render_base.sprpt[0] = old_ptr as u32;

    // The reload must land before the vertical-blank reset line: that
    // line's slots consume SPRxPT for the field's control-word fetch.
    bus.agnus.vpos = 0x10;
    bus.agnus.hpos = 0;
    let _ = bus.write_custom_word_from(0x120, (new_ptr >> 16) as u16, BeamWriteSource::Copper);
    let _ = bus.write_custom_word_from(0x122, new_ptr as u16, BeamWriteSource::Copper);

    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0;
    bus.capture_current_frame_display_start();
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert!(bus.frame_sprite_dma_observed());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].sprite, 0);
    assert_eq!(lines[0].beam_y, 0x2C);
    assert_eq!(lines[0].hstart, 0x00A1);
    assert_eq!(lines[0].data, 0xAAAA);
    assert_eq!(lines[0].datb, 0xBBBB);
}

#[test]
fn vertical_blank_sprite_pointer_write_reloads_descriptor_in_offscreen_replay() {
    let mut bus = empty_bus();
    let old_ptr = 0x0100usize;
    let new_ptr = 0x0200usize;
    let (old_pos, old_ctl) = sprite_control_words(0x2C, 0x30, 0x0083);
    let (new_pos, new_ctl) = sprite_control_words(0x2C, 0x30, 0x00A1);
    write_chip_word(&mut bus, old_ptr, old_pos);
    write_chip_word(&mut bus, old_ptr + 2, old_ctl);
    write_chip_word(&mut bus, old_ptr + 4, 0x1111);
    write_chip_word(&mut bus, old_ptr + 6, 0x2222);
    write_chip_word(&mut bus, new_ptr, new_pos);
    write_chip_word(&mut bus, new_ptr + 2, new_ctl);
    write_chip_word(&mut bus, new_ptr + 4, 0xAAAA);
    write_chip_word(&mut bus, new_ptr + 6, 0xBBBB);

    bus.current_frame_render_base.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.sprite_dma_frame_start_ptr[0] = old_ptr as u32;
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: PAL_SPRITE_DMA_FIRST_ACTIVE_VPOS - 1,
        hpos: 0,
        offset: 0x120,
        value: (new_ptr >> 16) as u16,
        source: BeamWriteSource::Copper,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: PAL_SPRITE_DMA_FIRST_ACTIVE_VPOS - 1,
        hpos: 0x0A,
        offset: 0x122,
        value: new_ptr as u16,
        source: BeamWriteSource::Copper,
    });

    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS;
    bus.capture_current_frame_display_start();
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].hstart, 0x00A1);
    assert_eq!(lines[0].data, 0xAAAA);
    assert_eq!(lines[0].datb, 0xBBBB);
}

#[test]
fn post_vertical_blank_sprite_pointer_write_retargets_pending_descriptor() {
    let mut bus = empty_bus();
    let descriptor_ptr = 0x0100usize;
    let data_ptr = 0x0200usize;
    let (pos, ctl) = sprite_control_words(0x2C, 0x30, 0x0083);
    write_chip_word(&mut bus, descriptor_ptr, pos);
    write_chip_word(&mut bus, descriptor_ptr + 2, ctl);
    write_chip_word(&mut bus, descriptor_ptr + 4, 0x1111);
    write_chip_word(&mut bus, descriptor_ptr + 6, 0x2222);
    write_chip_word(&mut bus, data_ptr, 0xAAAA);
    write_chip_word(&mut bus, data_ptr + 2, 0xBBBB);

    bus.current_frame_render_base.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.sprite_dma_frame_start_ptr[0] = descriptor_ptr as u32;
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: PAL_SPRITE_DMA_FIRST_ACTIVE_VPOS + 11,
        hpos: SPRITE_DMA_SLOT1_HPOS[0],
        offset: 0x120,
        value: (data_ptr >> 16) as u16,
        source: BeamWriteSource::Copper,
    });
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: PAL_SPRITE_DMA_FIRST_ACTIVE_VPOS + 11,
        hpos: SPRITE_DMA_SLOT1_HPOS[0] + 2,
        offset: 0x122,
        value: data_ptr as u16,
        source: BeamWriteSource::Copper,
    });

    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS;
    bus.capture_current_frame_display_start();
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].hstart, 0x0083);
    assert_eq!(lines[0].data, 0xAAAA);
    assert_eq!(lines[0].datb, 0xBBBB);
}

#[test]
fn manual_sprite_control_write_fetches_data_from_sprpt() {
    let mut bus = empty_bus();
    let data_ptr = 0x0200usize;
    let (pos, ctl) = sprite_control_words(0x2C, 0x2E, 0x0083);
    write_chip_word(&mut bus, data_ptr, 0xAAAA);
    write_chip_word(&mut bus, data_ptr + 2, 0xBBBB);
    write_chip_word(&mut bus, data_ptr + 4, 0xCCCC);
    write_chip_word(&mut bus, data_ptr + 6, 0xDDDD);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.denise.sprpt[0] = data_ptr as u32;
    bus.display_dma_sprpt[0] = data_ptr as u32;

    bus.agnus.vpos = 0x28;
    bus.agnus.hpos = 0;
    assert!(!bus.write_custom_word_from(0x140, pos, BeamWriteSource::Copper));
    assert!(!bus.write_custom_word_from(0x142, ctl, BeamWriteSource::Copper));

    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);
    bus.agnus.vpos = 0x2D;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert!(bus.frame_sprite_dma_observed());
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].sprite, 0);
    assert_eq!(lines[0].beam_y, 0x2C);
    assert_eq!(lines[0].hstart, 0x0083);
    assert_eq!(lines[0].data, 0xAAAA);
    assert_eq!(lines[0].datb, 0xBBBB);
    assert_eq!(lines[1].beam_y, 0x2D);
    assert_eq!(lines[1].data, 0xCCCC);
    assert_eq!(lines[1].datb, 0xDDDD);
}

#[test]
fn pending_register_control_sprite_pointer_write_retargets_data_stream() {
    let mut bus = empty_bus();
    let old_data_ptr = 0x0200usize;
    let new_data_ptr = 0x0300usize;
    let (pos, ctl) = sprite_control_words(0x40, 0x42, 0x0101);
    write_chip_word(&mut bus, old_data_ptr, 0x1111);
    write_chip_word(&mut bus, old_data_ptr + 2, 0x2222);
    write_chip_word(&mut bus, new_data_ptr, 0xAAAA);
    write_chip_word(&mut bus, new_data_ptr + 2, 0xBBBB);
    write_chip_word(&mut bus, new_data_ptr + 4, 0xCCCC);
    write_chip_word(&mut bus, new_data_ptr + 6, 0xDDDD);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.display_dma_sprpt[0] = old_data_ptr as u32;
    bus.denise.sprpt[0] = old_data_ptr as u32;

    bus.agnus.vpos = 0x20;
    bus.agnus.hpos = 0;
    assert!(!bus.write_custom_word_from(0x140, pos, BeamWriteSource::Copper));
    assert!(!bus.write_custom_word_from(0x142, ctl, BeamWriteSource::Copper));

    bus.agnus.vpos = 0x24;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0];
    let _ = bus.write_custom_word_from(0x120, (new_data_ptr >> 16) as u16, BeamWriteSource::Copper);
    let _ = bus.write_custom_word_from(0x122, new_data_ptr as u16, BeamWriteSource::Copper);

    bus.agnus.vpos = 0x40;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);
    bus.agnus.vpos = 0x41;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].hstart, 0x0101);
    assert_eq!(lines[0].data, 0xAAAA);
    assert_eq!(lines[0].datb, 0xBBBB);
    assert_eq!(lines[1].hstart, 0x0101);
    assert_eq!(lines[1].data, 0xCCCC);
    assert_eq!(lines[1].datb, 0xDDDD);
}

#[test]
fn pending_descriptor_sprite_pointer_write_retargets_data_stream() {
    let mut bus = empty_bus();
    let old_ptr = 0x0200usize;
    let new_data_ptr = 0x0300usize;
    let (pos, ctl) = sprite_control_words(0x60, 0x62, 0x0101);
    write_chip_word(&mut bus, old_ptr, pos);
    write_chip_word(&mut bus, old_ptr + 2, ctl);
    write_chip_word(&mut bus, old_ptr + 4, 0x1111);
    write_chip_word(&mut bus, old_ptr + 6, 0x2222);
    write_chip_word(&mut bus, new_data_ptr, 0xAAAA);
    write_chip_word(&mut bus, new_data_ptr + 2, 0xBBBB);
    write_chip_word(&mut bus, new_data_ptr + 4, 0xCCCC);
    write_chip_word(&mut bus, new_data_ptr + 6, 0xDDDD);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.denise.sprpt[0] = old_ptr as u32;
    bus.display_dma_sprpt[0] = old_ptr as u32;

    sprite_fetch_control_words_at_reset_line(&mut bus);
    let pending = bus.display_dma_sprite_state[0];
    assert_eq!(pending.vstrt, 0x60);
    assert!(!pending.dma_enabled);

    bus.agnus.vpos = 0x30;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0];
    let _ = bus.write_custom_word_from(0x120, (new_data_ptr >> 16) as u16, BeamWriteSource::Copper);
    let _ = bus.write_custom_word_from(0x122, new_data_ptr as u16, BeamWriteSource::Copper);

    bus.agnus.vpos = 0x60;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);
    bus.agnus.vpos = 0x61;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].hstart, 0x0101);
    assert_eq!(lines[0].data, 0xAAAA);
    assert_eq!(lines[0].datb, 0xBBBB);
    assert_eq!(lines[1].data, 0xCCCC);
    assert_eq!(lines[1].datb, 0xDDDD);
}

#[test]
fn armed_pointer_reload_before_vstart_fetches_descriptor_words() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0200usize;
    let (pos, ctl) = sprite_control_words(0x40, 0x42, 0x0101);
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0xAAAA);
    write_chip_word(&mut bus, sprite_ptr + 6, 0xBBBB);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.denise.diwstrt = (0x2C << 8) | 0x0081;
    bus.denise.diwstop = (0x80 << 8) | 0x00C1;

    // Reload SPRxPT during the vertical blank; the reset-line control-word
    // fetch consumes it.
    bus.agnus.vpos = 0x10;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0];
    let _ = bus.write_custom_word_from(0x120, (sprite_ptr >> 16) as u16, BeamWriteSource::Copper);
    let _ = bus.write_custom_word_from(0x122, sprite_ptr as u16, BeamWriteSource::Copper);

    sprite_fetch_control_words_at_reset_line(&mut bus);

    bus.agnus.vpos = 0x40;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].hstart, 0x0101);
    assert_eq!(lines[0].data, 0xAAAA);
    assert_eq!(lines[0].datb, 0xBBBB);
}

#[test]
fn after_slot_armed_sprite_pointer_write_seeds_dma_data_stream() {
    let mut bus = empty_bus();
    let data_ptr = 0x0300usize;
    let (pos, ctl) = sprite_control_words(0x40, 0x44, 0x0101);
    write_chip_word(&mut bus, data_ptr, 0xAAAA);
    write_chip_word(&mut bus, data_ptr + 2, 0xBBBB);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = 0x40;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0];
    let _ = bus.write_custom_word_from(0x140, pos, BeamWriteSource::Cpu);
    let _ = bus.write_custom_word_from(0x142, ctl, BeamWriteSource::Cpu);
    bus.denise.spr_armed[0] = true;

    let _ = bus.write_custom_word_from(0x120, (data_ptr >> 16) as u16, BeamWriteSource::Copper);
    let _ = bus.write_custom_word_from(0x122, data_ptr as u16, BeamWriteSource::Copper);
    bus.agnus.vpos = 0x41;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].hstart, 0x0101);
    assert_eq!(lines[0].beam_y, 0x41);
    assert_eq!(lines[0].data, 0xAAAA);
    assert_eq!(lines[0].datb, 0xBBBB);
}

#[test]
fn active_sprite_control_rewrite_preserves_descriptor_data_origin() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0100usize;
    let (pos, ctl) = sprite_control_words(0x2C, 0x30, 0x0083);
    let (moved_pos, moved_ctl) = sprite_control_words(0x2D, 0x30, 0x0091);
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x1111);
    write_chip_word(&mut bus, sprite_ptr + 6, 0x2222);
    write_chip_word(&mut bus, sprite_ptr + 8, 0x3333);
    write_chip_word(&mut bus, sprite_ptr + 10, 0x4444);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;

    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    bus.agnus.vpos = 0x2D;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 8;
    assert!(!bus.write_custom_word_from(0x140, moved_pos, BeamWriteSource::Copper));
    assert!(!bus.write_custom_word_from(0x142, moved_ctl, BeamWriteSource::Copper));

    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert!(bus.frame_sprite_dma_observed());
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].data, 0x1111);
    assert_eq!(lines[0].datb, 0x2222);
    assert_eq!(lines[1].beam_y, 0x2D);
    assert_eq!(lines[1].hstart, 0x0091);
    assert_eq!(lines[1].data, 0x3333);
    assert_eq!(lines[1].datb, 0x4444);
}

#[test]
fn sprite_pointer_write_after_pair_slot_seeds_next_descriptor_fetch() {
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    bus.agnus.write_fmode(0x000C); // SPR32 | SPAGEM: 64-bit sprite fetches.

    let old_ptr = 0x0100usize;
    let new_ptr = 0x0200usize;
    let (old_pos, old_ctl) = sprite_control_words(0x2C, 0x30, 0x0083);
    let (new_pos, new_ctl) = sprite_control_words(0x2C, 0x30, 0x00C1);
    write_chip_word(&mut bus, old_ptr, old_pos);
    write_chip_word(&mut bus, old_ptr + 8, old_ctl);
    write_chip_word(&mut bus, old_ptr + 16, 0x1111);
    write_chip_word(&mut bus, old_ptr + 24, 0x2222);
    write_chip_word(&mut bus, new_ptr, new_pos);
    write_chip_word(&mut bus, new_ptr + 8, new_ctl);
    for w in 0..4 {
        write_chip_word(&mut bus, new_ptr + 16 + w * 2, 0xA000 + w as u16);
        write_chip_word(&mut bus, new_ptr + 24 + w * 2, 0xB000 + w as u16);
    }

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.denise.sprpt[2] = old_ptr as u32;
    bus.display_dma_sprpt[2] = old_ptr as u32;

    let slot = SPRITE_DMA_SLOT1_HPOS[2];
    bus.agnus.vpos = 0;
    bus.agnus.hpos = slot - 1;
    bus.advance_chipset(4);
    let _ = bus.write_custom_word_from(0x128, (new_ptr >> 16) as u16, BeamWriteSource::Copper);
    let _ = bus.write_custom_word_from(0x12A, new_ptr as u16, BeamWriteSource::Copper);

    // The reset-line control fetch consumes the rewritten pointer.
    sprite_fetch_control_words_at_reset_line(&mut bus);

    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = slot - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert!(bus.frame_sprite_dma_observed());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].sprite, 2);
    assert_eq!(lines[0].beam_y, 0x2C);
    assert_eq!(lines[0].hstart, 0x00C1);
    assert_eq!(lines[0].data, 0xA000);
    assert_eq!(lines[0].data_ext, [0xA001, 0xA002, 0xA003]);
    assert_eq!(lines[0].datb, 0xB000);
    assert_eq!(lines[0].datb_ext, [0xB001, 0xB002, 0xB003]);
}

/// An inverted vertical pair (vstop < vstart) does not disable a sprite:
/// Agnus arms it at vstart and, since the vstop comparator already passed,
/// keeps fetching data to the bottom of the frame instead of terminating.
/// Previously vstop<vstart killed sprites that deliberately reuse the same
/// fetched strip across the remaining field.
#[test]
fn sprite_dma_inverted_vstop_refetches_control_words_on_vstop_line() {
    // An inverted vertical pair (vstop < vstart) does not disable or clamp
    // the sprite: the vstop comparator simply fires first, and that line's
    // DMA slots consume the following words as the next POS/CTL control
    // pair (vAmiga semantics). Software that shows full-height strips this
    // way keeps rewriting SPRxPOS/SPRxCTL per line, which the register
    // pokes model directly.
    let mut bus = empty_bus();
    let sprite_ptr = 0x0100usize;
    let (pos, ctl) = sprite_control_words(0x2C, 0x20, 0x0083); // vstop < vstart
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0xAAAA);
    write_chip_word(&mut bus, sprite_ptr + 6, 0xBBBB);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    let loaded = bus.display_dma_sprite_state[0];
    assert_eq!(loaded.vstrt, 0x2C);
    assert_eq!(loaded.vstop, 0x20);

    // The vstop line's slots (an off-screen line here, so driven the way
    // the pre-display replay does) fetch the data words as the next
    // POS/CTL.
    let emitted = bus.captured_sprite_line_at(0, 0x20);
    assert!(emitted.is_none(), "the control-fetch line displays nothing");

    let state = bus.display_dma_sprite_state[0];
    assert_eq!(state.pos, 0xAAAA);
    assert_eq!(state.ctl, 0xBBBB);
    assert_eq!(
        bus.display_dma_sprpt[0],
        sprite_ptr as u32 + 8,
        "the control refetch advances SPRxPT past the consumed words"
    );
    assert!(bus.frame_captured_sprite_lines().is_empty());
}

/// Plan 3.4: FMODE SPR32/SPAGEM widen the sprite fetch. The descriptor
/// strides scale with the quantum (POS in the first word of the first
/// wide fetch, CTL in the first word of the second) and each line
/// carries 2/4 words per channel.
#[test]
fn fmode_wide_sprite_dma_captures_extension_words() {
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    bus.agnus.write_fmode(0x000C); // SPR32 | SPAGEM = 64-bit sprites

    let sprite_ptr = 0x0100usize;
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0083);
    // Control fetch pair: POS at +0, CTL at +8 (first word of the
    // second 64-bit fetch); line data starts at +16.
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 8, ctl);
    for w in 0..4 {
        write_chip_word(&mut bus, sprite_ptr + 16 + w * 2, 0xA000 + w as u16);
        write_chip_word(&mut bus, sprite_ptr + 24 + w * 2, 0xB000 + w as u16);
    }

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].width_words, 4);
    assert_eq!(lines[0].data, 0xA000);
    assert_eq!(lines[0].data_ext, [0xA001, 0xA002, 0xA003]);
    assert_eq!(lines[0].datb, 0xB000);
    assert_eq!(lines[0].datb_ext, [0xB001, 0xB002, 0xB003]);
}

/// FMODE SSCAN2 doubles each fetched sprite data line across two display
/// lines, and a chained descriptor starts after the halved data block.
#[test]
fn fmode_sscan2_sprite_dma_doubles_each_data_line() {
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    bus.agnus.write_fmode(0x8000); // SSCAN2

    let sprite_ptr = 0x0100usize;
    // First descriptor: 4 display lines backed by 2 data lines.
    let (pos, ctl) = sprite_control_words(0x2C, 0x30, 0x0083);
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x1111);
    write_chip_word(&mut bus, sprite_ptr + 6, 0x2222);
    write_chip_word(&mut bus, sprite_ptr + 8, 0x3333);
    write_chip_word(&mut bus, sprite_ptr + 10, 0x4444);
    // Chained descriptor immediately after the two data lines.
    let (pos2, ctl2) = sprite_control_words(0x32, 0x34, 0x0091);
    write_chip_word(&mut bus, sprite_ptr + 12, pos2);
    write_chip_word(&mut bus, sprite_ptr + 14, ctl2);
    write_chip_word(&mut bus, sprite_ptr + 16, 0x5555);
    write_chip_word(&mut bus, sprite_ptr + 18, 0x6666);
    write_chip_word(&mut bus, sprite_ptr + 20, 0);
    write_chip_word(&mut bus, sprite_ptr + 22, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    for vpos in 0x2C..=0x32u32 {
        bus.agnus.vpos = vpos;
        bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
        bus.advance_chipset(4);
    }

    let lines = bus.frame_captured_sprite_lines();
    let words: Vec<(i32, u16, u16)> = lines
        .iter()
        .map(|line| (line.beam_y, line.data, line.datb))
        .collect();
    assert_eq!(
        words,
        vec![
            (0x2C, 0x1111, 0x2222),
            (0x2D, 0x1111, 0x2222),
            (0x2E, 0x3333, 0x4444),
            (0x2F, 0x3333, 0x4444),
            (0x32, 0x5555, 0x6666),
        ]
    );
}

#[test]
fn fmode_sscan2_masks_sprite_horizontal_comparator_high_bit() {
    assert_eq!(sprite_hstart_for_fmode(0x165, 0), 0x165);
    assert_eq!(sprite_hstart_for_fmode(0x165, 0x8000), 0x065);
    assert_eq!(sprite_hstart_for_fmode(0x080, 0x8000), 0x080);

    let line = CapturedSpriteLine {
        sprite: 0,
        hstart: 0x165,
        hsub_70ns: false,
        beam_y: 42,
        data: 0x8000,
        datb: 0,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
        attached: false,
    };
    let sources = super::live_sprite_collision_sources_with_beam_gated_odd(&[line], 42, 0x8000);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].hstart, 0x065);
}

/// Frame geometry latches at the frame wrap: a standard frame reports
/// the fixed canvas (FB_HEIGHT rows, 227-cck lines); a VARBEAMEN frame
/// reports the programmable scan derived from HTOTAL/VTOTAL and the
/// programmable vertical blank. The renderer-facing accessor describes
/// the completed frame, so the programmable values appear one wrap
/// after the registers are programmed.
#[test]
fn frame_geometry_latches_programmable_scan_at_frame_wrap() {
    use crate::chipset::agnus::{BEAMCON0_PAL, BEAMCON0_VARBEAMEN, BEAMCON0_VARVBEN};
    use crate::video::FB_HEIGHT;

    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::Ecs8372Rev4, DeniseRevision::Ecs8373);

    let standard = bus.frame_geometry();
    assert!(!standard.programmable);
    assert_eq!(standard.visible_lines, FB_HEIGHT);
    assert_eq!(standard.line_cck, 227);
    assert_eq!(standard.frame_lines, PAL_LINES);
    assert_eq!(standard.visible_start_vpos, RENDER_VISIBLE_START_VPOS);

    // Programmable 31 kHz scan.
    bus.agnus.write_htotal(113);
    bus.agnus.write_vtotal(625);
    bus.agnus
        .write_beamcon0(BEAMCON0_PAL | BEAMCON0_VARBEAMEN | BEAMCON0_VARVBEN);
    bus.agnus.write_vbstrt(613);
    bus.agnus.write_vbstop(44);

    // The frame in progress when the registers were written still
    // reports standard geometry...
    bus.begin_new_beam_frame();
    assert!(!bus.frame_geometry().programmable);

    // ...and the first frame that starts under the programmable beam
    // reports the programmable scan once it completes.
    bus.begin_new_beam_frame();
    let geometry = bus.frame_geometry();
    assert!(geometry.programmable);
    assert_eq!(geometry.visible_start_vpos, 44);
    assert_eq!(geometry.visible_lines, 569);
    assert_eq!(geometry.line_cck, 114);
    assert_eq!(geometry.frame_lines, 626);
}

/// The renderer draws the frame that just completed. If the next frame
/// programs a shorter VARBEAMEN scan before the host presents that
/// completed standard frame, the standard frame's bottom border still
/// belongs to the completed 313-line PAL field and must not be blanked
/// using the live shorter beam length.
#[test]
fn render_uses_completed_frame_line_count_for_bottom_border() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.current_frame_visible_start_vpos = 28;
    bus.refresh_frame_geometry_visible_start();
    bus.denise.palette.write_ocs(0, 0x0123);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.begin_new_beam_frame();

    bus.agnus.write_vtotal(298);
    bus.agnus.write_beamcon0(BEAMCON0_PAL | BEAMCON0_VARBEAMEN);
    assert_eq!(bus.agnus.current_frame_lines(), 299);
    assert_eq!(bus.frame_lines(), PAL_LINES);

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(
        fb[(FB_HEIGHT - 1) * FB_WIDTH + (FB_WIDTH - 1)],
        rgb12_to_rgba8(0x0123)
    );
}

/// Save-state restore rebuilds transient render metadata from serialized
/// Agnus/Denise state. A restored standard PAL frame must keep the fixed
/// PAL frame end even if live Agnus has already been programmed for a
/// shorter VARBEAMEN frame.
#[test]
fn state_load_rebuilds_standard_frame_lines_from_fixed_video_standard() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.current_frame_visible_start_vpos = 28;
    bus.refresh_frame_geometry_visible_start();
    bus.current_frame_geometry.frame_lines = 0;
    bus.denise.palette.write_ocs(0, 0x0123);
    bus.current_frame_render_base = bus.capture_render_snapshot();

    bus.agnus.write_vtotal(298);
    bus.agnus.write_beamcon0(BEAMCON0_PAL | BEAMCON0_VARBEAMEN);
    assert_eq!(bus.agnus.current_frame_lines(), 299);

    bus.reset_transient_video_after_state_load();
    assert_eq!(bus.frame_lines(), PAL_LINES);

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);

    assert_eq!(
        fb[(FB_HEIGHT - 1) * FB_WIDTH + (FB_WIDTH - 1)],
        rgb12_to_rgba8(0x0123)
    );
}

/// FMODE BSCAN2 makes both plane groups share one end-of-line modulo,
/// selected by line parity relative to DIWSTRT's vertical start.
#[test]
fn fmode_bscan2_selects_shared_modulo_by_line_parity() {
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    bus.denise.bpl1mod = -40;
    bus.denise.bpl2mod = 8;
    bus.denise.diwstrt = 0x2C81;

    bus.agnus.write_fmode(0x4000); // BSCAN2
    assert_eq!(bus.display_dma_modulo_for_plane(0, 0x2C), -40);
    assert_eq!(bus.display_dma_modulo_for_plane(1, 0x2C), -40);
    assert_eq!(bus.display_dma_modulo_for_plane(0, 0x2D), 8);
    assert_eq!(bus.display_dma_modulo_for_plane(1, 0x2D), 8);

    bus.agnus.write_fmode(0);
    assert_eq!(bus.display_dma_modulo_for_plane(0, 0x2D), -40);
    assert_eq!(bus.display_dma_modulo_for_plane(1, 0x2D), 8);
}

#[test]
fn sprite_dma_capture_preserves_sprite_started_before_visible_area() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0100usize;
    let vstart = RENDER_VISIBLE_START_VPOS as u16 - 2;
    let vstop = RENDER_VISIBLE_START_VPOS as u16 + 1;
    let (pos, ctl) = sprite_control_words(vstart, vstop, 0x0083);
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x1111);
    write_chip_word(&mut bus, sprite_ptr + 6, 0x2222);
    write_chip_word(&mut bus, sprite_ptr + 8, 0x3333);
    write_chip_word(&mut bus, sprite_ptr + 10, 0x4444);
    write_chip_word(&mut bus, sprite_ptr + 12, 0x5555);
    write_chip_word(&mut bus, sprite_ptr + 14, 0x6666);
    write_chip_word(&mut bus, sprite_ptr + 16, 0);
    write_chip_word(&mut bus, sprite_ptr + 18, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    // The offscreen sprite-DMA replay seeds from the frame-start DMACON and
    // the carried SPRxPT frontier (and replays $096/$120..$13F writes across
    // the span); mirror what begin_new_beam_frame records so SPREN and the
    // sprite pointer are live for the offscreen lines this sprite starts on.
    bus.current_frame_render_base.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.current_frame_render_base.sprpt[0] = sprite_ptr as u32;
    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS;
    bus.agnus.hpos = 0;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;
    bus.sprite_dma_frame_start_ptr[0] = sprite_ptr as u32;

    bus.capture_current_frame_display_start();
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].sprite, 0);
    assert_eq!(lines[0].beam_y, RENDER_VISIBLE_START_VPOS as i32);
    assert_eq!(lines[0].hstart, 0x0083);
    assert_eq!(lines[0].data, 0x5555);
    assert_eq!(lines[0].datb, 0x6666);
}

#[test]
fn pending_sprite_control_rewrite_preserves_descriptor_data_origin() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0100usize;
    let vstart = RENDER_VISIBLE_START_VPOS as u16 + 10;
    let vstop = vstart + 1;
    let (pos, ctl) = sprite_control_words(vstart, vstop, 0x0083);
    let (moved_pos, moved_ctl) = sprite_control_words(vstart, vstop, 0x00A1);
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x1111);
    write_chip_word(&mut bus, sprite_ptr + 6, 0x2222);
    write_chip_word(&mut bus, sprite_ptr + 8, 0);
    write_chip_word(&mut bus, sprite_ptr + 10, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.current_frame_render_base.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.current_frame_render_base.sprpt[0] = sprite_ptr as u32;
    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS;
    bus.agnus.hpos = 0;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;
    bus.sprite_dma_frame_start_ptr[0] = sprite_ptr as u32;

    bus.capture_current_frame_display_start();

    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS + 2;
    bus.agnus.hpos = 0;
    assert!(!bus.write_custom_word_from(0x140, moved_pos, BeamWriteSource::Copper));
    assert!(!bus.write_custom_word_from(0x142, moved_ctl, BeamWriteSource::Copper));

    bus.agnus.vpos = vstart as u32;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].sprite, 0);
    assert_eq!(lines[0].beam_y, vstart as i32);
    assert_eq!(lines[0].hstart, 0x00A1);
    assert_eq!(lines[0].data, 0x1111);
    assert_eq!(lines[0].datb, 0x2222);
}

#[test]
fn active_sprite_pos_write_retimes_hstart_without_clearing_dma_stream() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0100usize;
    let (pos, ctl) = sprite_control_words(0x2C, 0x30, 0x0083);
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x1111);
    write_chip_word(&mut bus, sprite_ptr + 6, 0x2222);
    write_chip_word(&mut bus, sprite_ptr + 8, 0x3333);
    write_chip_word(&mut bus, sprite_ptr + 10, 0x4444);
    write_chip_word(&mut bus, sprite_ptr + 12, 0x5555);
    write_chip_word(&mut bus, sprite_ptr + 14, 0x6666);
    write_chip_word(&mut bus, sprite_ptr + 16, 0);
    write_chip_word(&mut bus, sprite_ptr + 18, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    // Rewriting SPRxPOS while DMA is already enabled changes the horizontal
    // comparator. It must not recompute the active descriptor from the
    // software-visible SPRxCTL value, which may not be the DMA-fetched CTL.
    let moved_pos = ((0x00A1 >> 1) & 0x00FF) as u16;
    bus.agnus.vpos = 0x2D;
    bus.agnus.hpos = 0;
    assert!(!bus.write_custom_word_from(0x140, moved_pos, BeamWriteSource::Copper));
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    bus.agnus.vpos = 0x2E;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let words: Vec<(i32, i32, u16, u16)> = bus
        .frame_captured_sprite_lines()
        .iter()
        .map(|line| (line.beam_y, line.hstart, line.data, line.datb))
        .collect();
    assert_eq!(
        words,
        vec![
            (0x2C, 0x0083, 0x1111, 0x2222),
            (0x2D, 0x00A1, 0x3333, 0x4444),
            (0x2E, 0x00A1, 0x5555, 0x6666),
        ]
    );
}

#[test]
fn finished_sprite_channel_carries_dma_frontier_across_frame_boundary() {
    // Real Agnus advances SPRxPT through sprite DMA. A channel that has read
    // its terminating descriptor leaves the pointer parked at the DMA
    // frontier past the consumed list; it does NOT snap back to the last
    // value the Copper/CPU wrote into SPRxPT. begin_new_beam_frame must seed
    // the next frame's sprite-DMA replay from that frontier for a finished
    // channel (so a reused descriptor buffer that software overwrites every
    // field is not re-armed from its stale address before the Copper reloads
    // SPRxPT), and from the written pointer for a channel still mid-field.
    let mut bus = empty_bus();

    // Channel 0 finished the field: SPRxPT advanced through its fetches and
    // sits at the DMA frontier past the consumed list. denise.sprpt still
    // holds the now-overwritten descriptor address the Copper wrote earlier.
    let frontier0 = 0x0005_F3E4u32;
    bus.denise.sprpt[0] = 0x0005_F0BC;
    bus.display_dma_sprpt[0] = frontier0;

    // Channel 1 is mid-stream at frame end: its live pointer sits at the
    // next data word.
    let mid_stream1 = 0x0006_0104u32;
    bus.denise.sprpt[1] = 0x0006_0000;
    bus.display_dma_sprpt[1] = mid_stream1;

    // Channel 2 never fetched this field: the live pointer still equals the
    // written value.
    bus.denise.sprpt[2] = 0x0007_0000;
    bus.display_dma_sprpt[2] = 0x0007_0000;

    bus.begin_new_beam_frame();

    assert_eq!(
        bus.sprite_dma_frame_start_ptr[0], frontier0,
        "finished channel must carry the DMA frontier, not the stale written pointer"
    );
    assert_eq!(
        bus.sprite_dma_frame_start_ptr[1], mid_stream1,
        "mid-stream channel carries its live pointer"
    );
    assert_eq!(
        bus.sprite_dma_frame_start_ptr[2], 0x0007_0000,
        "channel that never fetched keeps the written SPRxPT"
    );
}

#[test]
fn sprite_dma_capture_treats_zero_pointer_as_chip_address() {
    let mut bus = empty_bus();
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0083);
    write_chip_word(&mut bus, 0, pos);
    write_chip_word(&mut bus, 2, ctl);
    write_chip_word(&mut bus, 4, 0x1111);
    write_chip_word(&mut bus, 6, 0x2222);
    write_chip_word(&mut bus, 8, 0);
    write_chip_word(&mut bus, 10, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.sprpt[0] = 0;
    bus.denise.sprpt[1] = 0x20;
    bus.display_dma_sprpt[0] = 0;
    bus.display_dma_sprpt[1] = 0x20;

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].sprite, 0);
    assert_eq!(lines[0].hstart, 0x0083);
    assert_eq!(lines[0].data, 0x1111);
    assert_eq!(lines[0].datb, 0x2222);
}

#[test]
fn state_load_resets_transient_video_latches() {
    let mut bus = empty_bus();
    bus.last_frame_render_base = Some(RenderRegisterSnapshot::default());
    bus.last_frame_render_events.push(BeamRegisterWrite {
        vpos: 0x2C,
        hpos: 0x40,
        offset: 0x180,
        value: 0x0FFF,
        source: BeamWriteSource::Copper,
    });
    bus.last_frame_chip_ram = std::sync::Arc::new(vec![0xA5; bus.mem.chip_ram.len()]);
    bus.last_frame_chip_ram_writes
        .push(BeamChipRamWrite::from_bytes(0x2C, 0x40, 0x0100, &[0x12]));
    bus.current_frame_chip_ram.clear();
    bus.current_frame_chip_ram_writes
        .push(BeamChipRamWrite::from_bytes(0x2C, 0x40, 0x0100, &[0x34]));
    std::sync::Arc::make_mut(&mut bus.last_frame_bitplane_rows)[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 1,
        fetch_origin_cck: None,
        planes: std::array::from_fn(|_| vec![0xFFFF]),
    });
    bus.current_frame_sprite_lines.push(CapturedSpriteLine {
        sprite: 0,
        hstart: 0x80,
        hsub_70ns: false,
        beam_y: 0x2C,
        data: 0x1111,
        datb: 0x2222,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
        attached: false,
    });
    bus.display_dma_sprite_state[0] = DisplaySpriteDmaState {
        vstrt: 0x20,
        vstop: 0x40,
        dma_enabled: true,
        last_line: Some(DisplaySpriteLineData {
            hstart: 0x80,
            hsub_70ns: false,
            data: 0x1111,
            datb: 0x2222,
            data_ext: [0; 3],
            datb_ext: [0; 3],
            width_words: 1,
            attached: false,
        }),
        ..DisplaySpriteDmaState::default()
    };

    bus.reset_transient_video_after_state_load();

    assert!(bus.last_frame_render_base.is_none());
    assert!(!bus.current_frame_render_blocked);
    assert!(bus.last_frame_render_events.is_empty());
    assert_eq!(bus.current_frame_chip_ram, bus.mem.chip_ram);
    assert!(bus.last_frame_chip_ram.is_empty());
    assert!(bus.current_frame_chip_ram_writes.is_empty());
    assert!(bus.last_frame_chip_ram_writes.is_empty());
    assert!(bus.current_frame_bitplane_rows.iter().all(Option::is_none));
    assert!(bus.last_frame_bitplane_rows.iter().all(Option::is_none));
    assert!(bus.current_frame_sprite_lines.is_empty());
    assert!(bus.last_frame_sprite_lines.is_empty());
    // The sprite DMA state is chip state (register copies, DMA flip-flops,
    // display latches): a state load restores it rather than clearing it.
    let restored = bus.display_dma_sprite_state[0];
    assert_eq!(restored.vstrt, 0x20);
    assert_eq!(restored.vstop, 0x40);
    assert!(restored.dma_enabled);
    assert!(restored.last_line.is_some());
}

#[test]
fn state_load_after_display_start_suppresses_partial_render_frame() {
    let mut bus = empty_bus();
    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS + 20;
    bus.current_frame_render_base = RenderRegisterSnapshot {
        bplcon0: 0x1200,
        ..RenderRegisterSnapshot::default()
    };
    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS + 20,
        hpos: 0x40,
        offset: 0x180,
        value: 0x0FFF,
        source: BeamWriteSource::Copper,
    });
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 1,
        fetch_origin_cck: None,
        planes: std::array::from_fn(|_| vec![0xFFFF]),
    });
    bus.current_frame_sprite_lines.push(CapturedSpriteLine {
        sprite: 0,
        hstart: 0x80,
        hsub_70ns: false,
        beam_y: RENDER_VISIBLE_START_VPOS as i32 + 20,
        data: 0x1111,
        datb: 0x2222,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
        attached: false,
    });

    bus.reset_transient_video_after_state_load();
    assert!(bus.current_frame_render_blocked);

    bus.current_frame_render_events.push(BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS + 21,
        hpos: 0x40,
        offset: 0x180,
        value: 0x00F0,
        source: BeamWriteSource::Copper,
    });
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 1,
        fetch_origin_cck: None,
        planes: std::array::from_fn(|_| vec![0xAAAA]),
    });
    bus.current_frame_sprite_lines.push(CapturedSpriteLine {
        sprite: 1,
        hstart: 0x90,
        hsub_70ns: false,
        beam_y: RENDER_VISIBLE_START_VPOS as i32 + 21,
        data: 0x3333,
        datb: 0x4444,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
        attached: false,
    });

    bus.begin_new_beam_frame();

    assert!(!bus.frame_render_available());
    assert!(!bus.current_frame_render_blocked);
    assert!(bus.last_frame_render_base.is_none());
    assert!(bus.last_frame_render_events.is_empty());
    assert!(bus.last_frame_bitplane_rows.iter().all(Option::is_none));
    assert!(bus.last_frame_sprite_lines.is_empty());
}

#[test]
fn sprite_dma_zero_height_descriptor_terminates_stream() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0100usize;
    let (zero_pos, zero_ctl) = sprite_control_words(0x2C, 0x2C, 0x0083);
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0091);
    write_chip_word(&mut bus, sprite_ptr, zero_pos);
    write_chip_word(&mut bus, sprite_ptr + 2, zero_ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, pos);
    write_chip_word(&mut bus, sprite_ptr + 6, ctl);
    write_chip_word(&mut bus, sprite_ptr + 8, 0x1111);
    write_chip_word(&mut bus, sprite_ptr + 10, 0x2222);
    write_chip_word(&mut bus, sprite_ptr + 12, 0);
    write_chip_word(&mut bus, sprite_ptr + 14, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;

    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert!(lines.is_empty());
}

#[test]
fn sprite_dma_fetches_land_in_hardware_latch_view() {
    // A DMA DATA fetch arms the hardware-true display latch and overwrites
    // its data words; a DMA CTL fetch (the vstop control words, here the
    // 0/0 terminator) disarms it and leaves the fetched control words in
    // the registers. The CPU/Copper write shadow never sees DMA, so the
    // calibrated manual replay keeps its inputs. Software relies on the
    // hardware view across a scene switch: Hamazing re-arms its sprites
    // with SPRxDATA=$0000 after a DMA sprite scene and expects the
    // DMA-written zeros (an invisible sprite), not a redisplay of the last
    // manually written pattern as full-height stale bars.
    let mut bus = empty_bus();

    // A manual arm long before the DMA scene: both views hold the bar.
    assert!(!bus.write_custom_word_from(0x140, 0x1080, BeamWriteSource::Cpu));
    assert!(!bus.write_custom_word_from(0x146, 0xAAAA, BeamWriteSource::Cpu));
    assert!(!bus.write_custom_word_from(0x144, 0xFFFF, BeamWriteSource::Cpu));
    assert!(bus.denise.spr_armed[0]);
    assert!(bus.denise.spr_hw_armed[0]);

    // One DMA data line, then the 0/0 terminator.
    let sprite_ptr = 0x0100usize;
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0091);
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x1234);
    write_chip_word(&mut bus, sprite_ptr + 6, 0x0000);
    write_chip_word(&mut bus, sprite_ptr + 8, 0);
    write_chip_word(&mut bus, sprite_ptr + 10, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;

    sprite_fetch_control_words_at_reset_line(&mut bus);
    let line = bus.captured_sprite_line_at(0, 0x2C);
    assert!(line.is_some());
    assert!(
        bus.denise.spr_hw_armed[0],
        "the DATA fetch arms the hardware latch"
    );
    assert_eq!(bus.denise.spr_hw_data[0], 0x1234);
    assert_eq!(bus.denise.spr_hw_datb[0], 0x0000);

    // The vstop line consumes the terminator: POS/CTL land in the hardware
    // view and the CTL write disarms it.
    let line = bus.captured_sprite_line_at(0, 0x2D);
    assert!(line.is_none());
    assert!(
        !bus.denise.spr_hw_armed[0],
        "the terminator CTL fetch disarms the hardware latch"
    );
    assert_eq!(bus.denise.spr_hw_pos[0], 0);
    assert_eq!(bus.denise.spr_hw_ctl[0], 0);

    // The write shadow never sees DMA: still the manual bar.
    assert!(bus.denise.spr_armed[0]);
    assert_eq!(bus.denise.sprdata[0], 0xFFFF);
    assert_eq!(bus.denise.sprdatb[0], 0xAAAA);

    // The scene-switch arm-with-zero: the hardware view arms with A=0 over
    // the DMA-written B=0 -- an invisible sprite, not the stale manual bar.
    assert!(!bus.write_custom_word_from(0x144, 0x0000, BeamWriteSource::Cpu));
    assert!(bus.denise.spr_hw_armed[0]);
    assert_eq!(bus.denise.spr_hw_data[0], 0x0000);
    assert_eq!(bus.denise.spr_hw_datb[0], 0x0000);
}

#[test]
fn sprite_dma_capture_wraps_control_words_at_chip_ram_end() {
    let mut bus = empty_bus();
    let sprite_ptr = bus.mem.chip_ram.len() - 2;
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0091);
    write_chip_word_wrapping(&mut bus, sprite_ptr, pos);
    write_chip_word_wrapping(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word_wrapping(&mut bus, sprite_ptr + 4, 0x1111);
    write_chip_word_wrapping(&mut bus, sprite_ptr + 6, 0x2222);
    write_chip_word_wrapping(&mut bus, sprite_ptr + 8, 0);
    write_chip_word_wrapping(&mut bus, sprite_ptr + 10, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].hstart, 0x0091);
    assert_eq!(lines[0].data, 0x1111);
    assert_eq!(lines[0].datb, 0x2222);
}

#[test]
fn sprite_dma_zero_height_descriptor_terminates_after_chip_address_wrap() {
    let mut bus = empty_bus();
    let sprite_ptr = bus.mem.chip_ram.len() - 2;
    let (zero_pos, zero_ctl) = sprite_control_words(0x2C, 0x2C, 0x0083);
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0091);
    write_chip_word_wrapping(&mut bus, sprite_ptr, zero_pos);
    write_chip_word_wrapping(&mut bus, sprite_ptr + 2, zero_ctl);
    let active_ptr = sprite_ptr + 4;
    write_chip_word_wrapping(&mut bus, active_ptr, pos);
    write_chip_word_wrapping(&mut bus, active_ptr + 2, ctl);
    write_chip_word_wrapping(&mut bus, active_ptr + 4, 0x1111);
    write_chip_word_wrapping(&mut bus, active_ptr + 6, 0x2222);
    write_chip_word_wrapping(&mut bus, active_ptr + 8, 0);
    write_chip_word_wrapping(&mut bus, active_ptr + 10, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;
    bus.denise.sprpt[1] = 0x0200;
    bus.display_dma_sprpt[1] = 0x0200;

    bus.advance_chipset(2);

    let lines = bus.frame_captured_sprite_lines();
    assert!(lines.is_empty());
}

#[test]
fn sprite_dma_capture_latches_control_words_until_stop() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0100usize;
    let (pos, ctl) = sprite_control_words(0x2C, 0x2E, 0x0083);
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x1111);
    write_chip_word(&mut bus, sprite_ptr + 6, 0x2222);
    write_chip_word(&mut bus, sprite_ptr + 8, 0x3333);
    write_chip_word(&mut bus, sprite_ptr + 10, 0x4444);
    write_chip_word(&mut bus, sprite_ptr + 12, 0);
    write_chip_word(&mut bus, sprite_ptr + 14, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let (rewritten_pos, rewritten_ctl) = sprite_control_words(0x2D, 0x2E, 0x0091);
    write_chip_word(&mut bus, sprite_ptr, rewritten_pos);
    write_chip_word(&mut bus, sprite_ptr + 2, rewritten_ctl);
    bus.agnus.vpos = 0x2D;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].hstart, 0x0083);
    assert_eq!(lines[0].data, 0x1111);
    assert_eq!(lines[1].beam_y, 0x2D);
    assert_eq!(lines[1].hstart, 0x0083);
    assert_eq!(lines[1].data, 0x3333);
    assert_eq!(lines[1].datb, 0x4444);
}

#[test]
fn sprite_dma_capture_samples_later_pairs_at_their_fetch_slot() {
    let mut bus = empty_bus();
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0083);
    let sprite0_ptr = 0x0100usize;
    let sprite6_ptr = 0x0200usize;
    for ptr in [sprite0_ptr, sprite6_ptr] {
        write_chip_word(&mut bus, ptr, pos);
        write_chip_word(&mut bus, ptr + 2, ctl);
        write_chip_word(&mut bus, ptr + 8, 0);
        write_chip_word(&mut bus, ptr + 10, 0);
    }
    write_chip_word(&mut bus, sprite0_ptr + 4, 0x1111);
    write_chip_word(&mut bus, sprite0_ptr + 6, 0x2222);
    write_chip_word(&mut bus, sprite6_ptr + 4, 0x6666);
    write_chip_word(&mut bus, sprite6_ptr + 6, 0x7777);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.sprpt[0] = sprite0_ptr as u32;
    bus.denise.sprpt[6] = sprite6_ptr as u32;
    bus.display_dma_sprpt[0] = sprite0_ptr as u32;
    bus.display_dma_sprpt[6] = sprite6_ptr as u32;

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(2);
    write_chip_word(&mut bus, sprite0_ptr + 4, 0xAAAA);
    write_chip_word(&mut bus, sprite6_ptr + 4, 0xBBBB);
    let remaining = SPRITE_DMA_SLOT1_HPOS[6] + 3 - bus.agnus.hpos;
    bus.advance_chipset(remaining);

    let lines = bus.frame_captured_sprite_lines();
    let sprite0 = lines.iter().find(|line| line.sprite == 0).unwrap();
    let sprite6 = lines.iter().find(|line| line.sprite == 6).unwrap();
    assert_eq!(sprite0.data, 0x1111);
    assert_eq!(sprite6.data, 0xBBBB);
}

#[test]
fn sprite_pointer_write_at_pair_slot_seeds_next_line_descriptor_fetch() {
    let mut bus = empty_bus();
    let old_ptr = 0x0100usize;
    let new_ptr = 0x0200usize;
    let (old_pos, old_ctl) = sprite_control_words(0x40, 0x44, 0x0083);
    let (new_pos, new_ctl) = sprite_control_words(0x40, 0x44, 0x00C1);

    write_chip_word(&mut bus, old_ptr, old_pos);
    write_chip_word(&mut bus, old_ptr + 2, old_ctl);
    write_chip_word(&mut bus, old_ptr + 4, 0x1111);
    write_chip_word(&mut bus, old_ptr + 6, 0x2222);
    write_chip_word(&mut bus, new_ptr, new_pos);
    write_chip_word(&mut bus, new_ptr + 2, new_ctl);
    write_chip_word(&mut bus, new_ptr + 4, 0xAAAA);
    write_chip_word(&mut bus, new_ptr + 6, 0xBBBB);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.denise.sprpt[0] = old_ptr as u32;
    bus.display_dma_sprpt[0] = old_ptr as u32;

    bus.agnus.vpos = 0x10;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(1);
    let _ = bus.write_custom_word_from(0x120, (new_ptr >> 16) as u16, BeamWriteSource::Copper);
    let _ = bus.write_custom_word_from(0x122, new_ptr as u16, BeamWriteSource::Copper);
    assert_eq!(
        bus.display_dma_sprpt[0], new_ptr as u32,
        "the pointer write must seed the next control-word fetch"
    );

    // The vertical-blank reset line fetches POS/CTL from the new pointer.
    sprite_fetch_control_words_at_reset_line(&mut bus);

    bus.agnus.vpos = 0x40;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].hstart, 0x00C1);
    assert_eq!(lines[0].data, 0xAAAA);
    assert_eq!(lines[0].datb, 0xBBBB);
}

#[test]
fn empty_sprite_dma_slot_does_not_mark_frame_dma_observed() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;

    bus.advance_chipset(4);

    assert!(bus.frame_captured_sprite_lines().is_empty());
    assert!(
        !bus.frame_sprite_dma_observed(),
        "enabled sprite slots without fetched sprite data must not suppress register/manual sprites"
    );
}

#[test]
fn sprite_dma_capture_blocks_sprite_seven_when_ddfstrt_uses_early_fetch_slot() {
    let mut bus = empty_bus();
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0083);
    let sprite6_ptr = 0x0200usize;
    let sprite7_ptr = 0x0300usize;
    for ptr in [sprite6_ptr, sprite7_ptr] {
        write_chip_word(&mut bus, ptr, pos);
        write_chip_word(&mut bus, ptr + 2, ctl);
        write_chip_word(&mut bus, ptr + 8, 0);
        write_chip_word(&mut bus, ptr + 10, 0);
    }
    write_chip_word(&mut bus, sprite6_ptr + 4, 0x6666);
    write_chip_word(&mut bus, sprite6_ptr + 6, 0x7777);
    write_chip_word(&mut bus, sprite7_ptr + 4, 0x8888);
    write_chip_word(&mut bus, sprite7_ptr + 6, 0x9999);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[6] - 1;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.ddfstrt = 0x0028;
    bus.denise.ddfstop = 0x0038;
    bus.denise.sprpt[6] = sprite6_ptr as u32;
    bus.denise.sprpt[7] = sprite7_ptr as u32;
    bus.display_dma_sprpt[6] = sprite6_ptr as u32;
    bus.display_dma_sprpt[7] = sprite7_ptr as u32;

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[6] - 1;
    bus.advance_chipset(SPRITE_DMA_SLOT1_HPOS[7] + 1 - bus.agnus.hpos);

    let lines = bus.frame_captured_sprite_lines();
    assert!(lines.iter().any(|line| line.sprite == 6));
    assert!(!lines.iter().any(|line| line.sprite == 7));
}

#[test]
fn sprite_dma_capture_keeps_sprite_seven_when_ddfstrt_matches_sprite_slot() {
    let mut bus = empty_bus();
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0083);
    let sprite6_ptr = 0x0200usize;
    let sprite7_ptr = 0x0300usize;
    for ptr in [sprite6_ptr, sprite7_ptr] {
        write_chip_word(&mut bus, ptr, pos);
        write_chip_word(&mut bus, ptr + 2, ctl);
        write_chip_word(&mut bus, ptr + 8, 0);
        write_chip_word(&mut bus, ptr + 10, 0);
    }
    write_chip_word(&mut bus, sprite6_ptr + 4, 0x6666);
    write_chip_word(&mut bus, sprite6_ptr + 6, 0x7777);
    write_chip_word(&mut bus, sprite7_ptr + 4, 0x8888);
    write_chip_word(&mut bus, sprite7_ptr + 6, 0x9999);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[6] - 1;
    bus.denise.bplcon0 = 0x3000;
    bus.denise.ddfstrt = 0x0030;
    bus.denise.ddfstop = 0x0038;
    bus.denise.sprpt[6] = sprite6_ptr as u32;
    bus.denise.sprpt[7] = sprite7_ptr as u32;
    bus.display_dma_sprpt[6] = sprite6_ptr as u32;
    bus.display_dma_sprpt[7] = sprite7_ptr as u32;

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[6] - 1;
    bus.advance_chipset(SPRITE_DMA_SLOT1_HPOS[7] + 3 - bus.agnus.hpos);

    let lines = bus.frame_captured_sprite_lines();
    assert!(lines.iter().any(|line| line.sprite == 6));
    assert!(lines.iter().any(|line| line.sprite == 7));
}

#[test]
fn sprite_dma_capture_repeats_last_fetched_line_after_dma_disable_until_vstop() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0100usize;
    let (pos, ctl) = sprite_control_words(0x2C, 0x2E, 0x0083);
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x1111);
    write_chip_word(&mut bus, sprite_ptr + 6, 0x2222);
    write_chip_word(&mut bus, sprite_ptr + 8, 0x3333);
    write_chip_word(&mut bus, sprite_ptr + 10, 0x4444);
    write_chip_word(&mut bus, sprite_ptr + 12, 0);
    write_chip_word(&mut bus, sprite_ptr + 14, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);
    bus.agnus.dmacon = DMACON_DMAEN;
    bus.agnus.vpos = 0x2D;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);
    bus.agnus.vpos = 0x2E;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    let first = lines
        .iter()
        .find(|line| line.sprite == 0 && line.beam_y == 0x2C)
        .unwrap();
    let repeated = lines
        .iter()
        .find(|line| line.sprite == 0 && line.beam_y == 0x2D)
        .unwrap();
    assert_eq!((first.data, first.datb), (0x1111, 0x2222));
    assert_eq!((repeated.data, repeated.datb), (0x1111, 0x2222));
    assert!(!lines
        .iter()
        .any(|line| line.sprite == 0 && line.beam_y == 0x2E));
}

#[test]
fn sprite_dma_capture_does_not_start_descriptor_at_or_before_current_vpos() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0100usize;
    let (pos, ctl) = sprite_control_words(0x2C, 0x2E, 0x0083);
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x1111);
    write_chip_word(&mut bus, sprite_ptr + 6, 0x2222);
    write_chip_word(&mut bus, sprite_ptr + 8, 0x3333);
    write_chip_word(&mut bus, sprite_ptr + 10, 0x4444);
    write_chip_word(&mut bus, sprite_ptr + 12, 0);
    write_chip_word(&mut bus, sprite_ptr + 14, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;

    bus.advance_chipset(4);
    bus.agnus.vpos = 0x2D;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    assert!(bus
        .frame_captured_sprite_lines()
        .iter()
        .all(|line| line.sprite != 0));
}

#[test]
fn sprite_dma_reuse_skips_descriptor_with_vstart_before_current_vpos() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0100usize;
    let (first_pos, first_ctl) = sprite_control_words(0x2C, 0x2D, 0x0083);
    let (past_pos, past_ctl) = sprite_control_words(0x2C, 0x2F, 0x0091);
    write_chip_word(&mut bus, sprite_ptr, first_pos);
    write_chip_word(&mut bus, sprite_ptr + 2, first_ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x1111);
    write_chip_word(&mut bus, sprite_ptr + 6, 0x2222);
    write_chip_word(&mut bus, sprite_ptr + 8, past_pos);
    write_chip_word(&mut bus, sprite_ptr + 10, past_ctl);
    write_chip_word(&mut bus, sprite_ptr + 12, 0x3333);
    write_chip_word(&mut bus, sprite_ptr + 14, 0x4444);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2B;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2B;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);
    bus.agnus.vpos = 0x2D;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(4);

    let lines = bus.frame_captured_sprite_lines();
    assert!(lines
        .iter()
        .any(|line| line.sprite == 0 && line.beam_y == 0x2C));
    assert!(!lines
        .iter()
        .any(|line| line.sprite == 0 && line.beam_y == 0x2D));
}

#[test]
fn sprite_dma_chained_descriptor_with_same_vstart_arms_after_control_fetch_line() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0100usize;
    let (first_pos, first_ctl) = sprite_control_words(0x2C, 0x2D, 0x0083);
    let (same_line_pos, same_line_ctl) = sprite_control_words(0x2D, 0x30, 0x0091);
    let (later_pos, later_ctl) = sprite_control_words(0x31, 0x32, 0x00A1);
    write_chip_word(&mut bus, sprite_ptr, first_pos);
    write_chip_word(&mut bus, sprite_ptr + 2, first_ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x1111);
    write_chip_word(&mut bus, sprite_ptr + 6, 0x2222);
    write_chip_word(&mut bus, sprite_ptr + 8, same_line_pos);
    write_chip_word(&mut bus, sprite_ptr + 10, same_line_ctl);
    write_chip_word(&mut bus, sprite_ptr + 12, 0x3333);
    write_chip_word(&mut bus, sprite_ptr + 14, 0x4444);
    write_chip_word(&mut bus, sprite_ptr + 16, 0x5555);
    write_chip_word(&mut bus, sprite_ptr + 18, 0x6666);
    write_chip_word(&mut bus, sprite_ptr + 20, later_pos);
    write_chip_word(&mut bus, sprite_ptr + 22, later_ctl);
    write_chip_word(&mut bus, sprite_ptr + 24, 0x7777);
    write_chip_word(&mut bus, sprite_ptr + 26, 0x8888);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    for vpos in 0x2C..=0x31u32 {
        bus.agnus.vpos = vpos;
        bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
        bus.advance_chipset(4);
    }

    let words: Vec<(i32, i32, u16, u16)> = bus
        .frame_captured_sprite_lines()
        .iter()
        .filter(|line| line.sprite == 0)
        .map(|line| (line.beam_y, line.hstart, line.data, line.datb))
        .collect();
    assert_eq!(
        words,
        vec![
            (0x2C, 0x0083, 0x1111, 0x2222),
            (0x2E, 0x0091, 0x3333, 0x4444),
            (0x2F, 0x0091, 0x5555, 0x6666),
            (0x31, 0x00A1, 0x7777, 0x8888),
        ]
    );
}

#[test]
fn visible_sprite_pixels_accumulate_live_sprite_sprite_clxdat() {
    let mut bus = empty_bus();
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0083);
    let sprite0_ptr = 0x0100usize;
    let sprite2_ptr = 0x0200usize;
    for ptr in [sprite0_ptr, sprite2_ptr] {
        write_chip_word(&mut bus, ptr, pos);
        write_chip_word(&mut bus, ptr + 2, ctl);
        write_chip_word(&mut bus, ptr + 4, 0x8000);
        write_chip_word(&mut bus, ptr + 6, 0);
        write_chip_word(&mut bus, ptr + 8, 0);
        write_chip_word(&mut bus, ptr + 10, 0);
    }

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.sprpt[0] = sprite0_ptr as u32;
    bus.denise.sprpt[2] = sprite2_ptr as u32;
    bus.display_dma_sprpt[0] = sprite0_ptr as u32;
    bus.display_dma_sprpt[2] = sprite2_ptr as u32;
    bus.current_frame_sprite_display_enable_x_by_y[0] = Some(0);

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(1);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);

    let remaining = SPRITE_DMA_SLOT1_HPOS[1] + 1 - bus.agnus.hpos;
    bus.advance_chipset(remaining);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);

    let remaining = 0x3A - bus.agnus.hpos;
    bus.advance_chipset(remaining);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8200);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
}

#[test]
fn palette_register_writes_do_not_feed_live_collision_replay() {
    let mut bus = empty_bus();

    bus.write_custom_word_from(0x180, 0x0ABC, BeamWriteSource::Copper);
    bus.write_custom_word_from(0x098, 1 << 12, BeamWriteSource::Copper);

    assert_eq!(bus.current_frame_render_events.len(), 2);
    assert_eq!(bus.current_frame_collision_events.len(), 1);
    assert_eq!(bus.current_frame_collision_events[0].offset, 0x098);
    assert_eq!(bus.current_frame_collision_control_events.len(), 1);
    assert_eq!(bus.current_frame_collision_control_events[0].offset, 0x098);
    assert!(bus.current_frame_collision_bpldat_events.is_empty());
    assert!(bus.current_frame_collision_sprite_events.is_empty());
}

#[test]
fn sprite_data_writes_do_not_feed_live_collision_control_replay() {
    let mut bus = empty_bus();

    bus.write_custom_word_from(0x144, 0x8000, BeamWriteSource::Cpu);

    assert_eq!(bus.current_frame_collision_events.len(), 1);
    assert_eq!(bus.current_frame_collision_events[0].offset, 0x144);
    assert_eq!(bus.current_frame_collision_sprite_events.len(), 1);
    assert_eq!(bus.current_frame_collision_sprite_events[0].offset, 0x144);
    assert!(bus.current_frame_collision_control_events.is_empty());
    assert!(bus.current_frame_collision_bpldat_events.is_empty());
}

#[test]
fn beam_timed_collision_indexes_are_reused_until_relevant_register_writes() {
    let mut bus = empty_bus();

    bus.write_custom_word_from(0x098, 1 << 12, BeamWriteSource::Copper);
    bus.ensure_current_collision_control_index();
    assert!(bus.current_frame_collision_control_index.is_some());

    bus.write_custom_word_from(0x180, 0x0ABC, BeamWriteSource::Copper);
    assert!(bus.current_frame_collision_control_index.is_some());

    bus.ensure_current_collision_sprite_index();
    assert!(bus.current_frame_collision_sprite_index.is_some());
    bus.write_custom_word_from(0x144, 0x8000, BeamWriteSource::Cpu);
    assert!(bus.current_frame_collision_control_index.is_some());
    assert!(bus.current_frame_collision_sprite_index.is_none());

    bus.ensure_current_collision_bpldat_index();
    assert!(bus.current_frame_collision_bpldat_index.is_some());
    bus.write_custom_word_from(0x110, 0xFFFF, BeamWriteSource::Copper);
    assert!(bus.current_frame_collision_control_index.is_none());
    assert!(bus.current_frame_collision_bpldat_index.is_none());
}

#[test]
fn manual_sprite_data_writes_accumulate_live_sprite_sprite_clxdat() {
    let mut bus = empty_bus();
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0083);
    bus.agnus.vpos = 0x2C;
    // The staged beam position is the write's chip-bus slot; the collision
    // event records at slot + DENISE_WRITE_EFFECT_DELAY_CCK ($38), and the
    // advance below runs the beam past it.
    bus.agnus.hpos = 0x34;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.sprpos[0] = pos;
    bus.denise.sprctl[0] = ctl;
    bus.denise.sprpos[2] = pos;
    bus.denise.sprctl[2] = ctl;
    bus.current_frame_sprite_display_enable_x_by_y[0] = Some(0);
    bus.current_frame_render_base = bus.capture_render_snapshot();

    bus.write_custom_word_from(0x144, 0x8000, BeamWriteSource::Cpu);
    bus.write_custom_word_from(0x154, 0x8000, BeamWriteSource::Cpu);
    bus.advance_chipset(6);

    assert_eq!(bus.custom_read(0x00E, 2), 0x8200);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
}

#[test]
fn frame_end_completes_unread_live_sprite_sprite_clxdat() {
    let mut bus = empty_bus();
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0083);
    bus.agnus.vpos = 0x2C;
    // Slot $34: the write events record at $38 (slot + write-effect delay).
    bus.agnus.hpos = 0x34;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.sprpos[0] = pos;
    bus.denise.sprctl[0] = ctl;
    bus.denise.sprpos[2] = pos;
    bus.denise.sprctl[2] = ctl;
    bus.current_frame_sprite_display_enable_x_by_y[0] = Some(0);
    bus.current_frame_render_base = bus.capture_render_snapshot();

    bus.write_custom_word_from(0x144, 0x8000, BeamWriteSource::Cpu);
    bus.write_custom_word_from(0x154, 0x8000, BeamWriteSource::Cpu);
    bus.accumulate_live_collisions_to_frame_end();

    assert_eq!(bus.custom_read(0x00E, 2), 0x8200);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
}

#[test]
fn attached_manual_sprite_data_writes_accumulate_live_sprite_sprite_clxdat() {
    let mut bus = empty_bus();
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0083);
    bus.agnus.vpos = 0x2C;
    // Slot $34: the write events record at $38 (slot + write-effect delay).
    bus.agnus.hpos = 0x34;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.sprpos[0] = pos;
    bus.denise.sprctl[0] = ctl;
    bus.denise.sprpos[1] = pos;
    bus.denise.sprctl[1] = ctl | 0x0080;
    bus.denise.sprpos[2] = pos;
    bus.denise.sprctl[2] = ctl;
    bus.denise.clxcon = 1 << 12;
    bus.current_frame_sprite_display_enable_x_by_y[0] = Some(0);
    bus.current_frame_render_base = bus.capture_render_snapshot();

    bus.write_custom_word_from(0x144, 0x0000, BeamWriteSource::Cpu);
    bus.write_custom_word_from(0x14C, 0x8000, BeamWriteSource::Cpu);
    bus.write_custom_word_from(0x154, 0x8000, BeamWriteSource::Cpu);
    bus.advance_chipset(6);

    assert_eq!(bus.custom_read(0x00E, 2), 0x8200);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
}

#[test]
fn attached_manual_sprite_odd_data_writes_accumulate_later_live_sprite_sprite_clxdat() {
    let mut bus = empty_bus();
    // The sprites sit at hstart $8C so the overlap pixels land exactly at
    // the second odd-DATA write's effective position (slot $3A plus the
    // write-effect delay = event $3E, write-domain x 88): pixels before it
    // replay the first odd word, pixels from it replay the rewrite.
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x008C);
    bus.agnus.vpos = 0x2C;
    // Slot $34: the write events record at $38 (slot + write-effect delay).
    bus.agnus.hpos = 0x34;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.sprpos[0] = pos;
    bus.denise.sprctl[0] = ctl;
    bus.denise.sprpos[1] = pos;
    bus.denise.sprctl[1] = ctl | 0x0080;
    bus.denise.sprpos[2] = pos;
    bus.denise.sprctl[2] = ctl;
    bus.denise.clxcon = 1 << 12;
    bus.current_frame_sprite_display_enable_x_by_y[0] = Some(0);
    bus.current_frame_render_base = bus.capture_render_snapshot();

    bus.write_custom_word_from(0x144, 0x0000, BeamWriteSource::Cpu);
    bus.write_custom_word_from(0x14C, 0x8000, BeamWriteSource::Cpu);
    bus.write_custom_word_from(0x154, 0x2000, BeamWriteSource::Cpu);
    bus.advance_chipset(6);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);

    bus.write_custom_word_from(0x14C, 0x2000, BeamWriteSource::Cpu);
    bus.advance_chipset(9);

    assert_eq!(bus.custom_read(0x00E, 2), 0x8200);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
}

#[test]
fn attached_manual_sprite_sources_preserve_even_intervals_outside_odd_attachment() {
    let mut bus = empty_bus();
    let (pos, ctl) = sprite_control_words(0x2C, 0x2D, 0x0083);
    bus.denise.sprpos[0] = pos;
    bus.denise.sprctl[0] = ctl;
    bus.denise.sprdata[0] = 0xA000;
    bus.denise.spr_armed[0] = true;
    bus.denise.sprpos[1] = pos;
    bus.denise.sprctl[1] = ctl | 0x0080;
    let frame_base = bus.capture_render_snapshot();
    let events = [BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: RENDER_COPPER_WAIT_HPOS_FB0 + 1,
        offset: 0x014C,
        value: 0x2000,
        source: BeamWriteSource::Cpu,
    }];

    let event_index = BeamEventIndex::from_register_writes(&events);
    let sources = live_manual_sprite_collision_sources(frame_base, &event_index, 0x2C, 0, 8);

    assert!(sources.iter().any(|source| {
        source.sprite == 0
            && source.x_start == 0
            && source.x_stop == 8
            && source.source.words == [0xA000, 0, 0, 0]
            && !source.source.requires_odd_enable
    }));
    assert!(sources.iter().any(|source| {
        source.sprite == 1
            && source.x_start == 4
            && source.x_stop == 8
            && source.source.words == [0x2000, 0, 0, 0]
            && source.source.requires_odd_enable
    }));
}

#[test]
fn manual_sprite_position_write_uses_sprite_compare_domain_for_live_sources() {
    let event_hpos = 96;
    let event = BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: event_hpos,
        offset: 0x0150,
        value: 0,
        source: BeamWriteSource::Cpu,
    };
    let sprite_compare_hpos = event_hpos.saturating_sub(DENISE_HPOS_LAG_CCK);
    let sprite_compare_x = (sprite_compare_hpos as i32 * 2 - RENDER_DIW_HSTART_FB0) * 2;
    let colour_output_x = (event_hpos as i32 - RENDER_COPPER_WAIT_HPOS_FB0 as i32) * 4;

    assert_eq!(super::live_manual_sprite_event_x(event), sprite_compare_x);
    assert_ne!(super::live_manual_sprite_event_x(event), colour_output_x);
}

#[test]
fn manual_sprite_position_write_on_compare_boundary_preserves_live_source() {
    let mut bus = empty_bus();
    let first_hstart = 0x007E;
    let second_hstart = 0x008E;
    let (first_pos, ctl) = sprite_control_words(0x2C, 0x2D, first_hstart);
    let (second_pos, _) = sprite_control_words(0x2C, 0x2D, second_hstart);
    bus.denise.sprpos[0] = first_pos;
    bus.denise.sprctl[0] = ctl;
    bus.denise.sprdata[0] = 0xFFFF;
    bus.denise.spr_armed[0] = true;
    let frame_base = bus.capture_render_snapshot();
    let boundary_hpos = u32::from(first_hstart / 2) + DENISE_HPOS_LAG_CCK;
    let event = BeamRegisterWrite {
        vpos: RENDER_VISIBLE_START_VPOS,
        hpos: boundary_hpos,
        offset: 0x0140,
        value: second_pos,
        source: BeamWriteSource::Cpu,
    };
    let event_index = BeamEventIndex::from_register_writes(&[event]);
    let first_base_x = (i32::from(first_hstart) - RENDER_DIW_HSTART_FB0) * 2;

    let sources = live_manual_sprite_collision_sources(
        frame_base,
        &event_index,
        0x2C,
        first_base_x,
        first_base_x + 2,
    );

    assert!(sources.iter().any(|source| {
        source.sprite == 0
            && source.x_start == first_base_x
            && source.x_stop == first_base_x + 2
            && source.source.words == [0xFFFF, 0, 0, 0]
    }));
}

#[test]
fn manual_sprite_data_writes_accumulate_live_sprite_playfield_clxdat() {
    let mut bus = empty_bus();
    // hstart +1 vs the pre-fix value: sprite comparator positions share
    // Denise's counter and moved with the corrected window-edge anchor
    // (2H-196); the beam-anchored playfield sample did not.
    let (pos, ctl) = sprite_control_words_for_output(0x2C, 0x2D, 0x0082);
    bus.agnus.vpos = 0x2C;
    // Slot $34: the write event records at $38 (slot + write-effect delay).
    bus.agnus.hpos = 0x34;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    // One-bitplane playfield: match only bitplane 1 so the absent planes 2-6
    // still match (CLXCON_RESET's all-six-planes=1 never matches one plane ->
    // no collision on hardware).
    bus.denise.clxcon = 0x0FC1;
    bus.denise.sprpos[0] = pos;
    bus.denise.sprctl[0] = ctl;
    bus.current_frame_sprite_display_enable_x_by_y[0] = Some(0);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 1,
        fetch_origin_cck: None,
        planes: [
            vec![0x4000],
            vec![0],
            vec![0],
            vec![0],
            vec![0],
            vec![0],
            Vec::new(),
            Vec::new(),
        ],
    });

    bus.write_custom_word_from(0x144, 0x8000, BeamWriteSource::Cpu);
    bus.advance_chipset(6);

    assert_eq!(bus.custom_read(0x00E, 2), 0x8022);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
}

#[test]
fn attached_manual_sprite_data_writes_accumulate_live_sprite_playfield_clxdat() {
    let mut bus = empty_bus();
    let (pos, ctl) = sprite_control_words_for_output(0x2C, 0x2D, 0x0083);
    bus.agnus.vpos = 0x2C;
    // Slot $34: the write events record at $38 (slot + write-effect delay).
    bus.agnus.hpos = 0x34;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.sprpos[0] = pos;
    bus.denise.sprctl[0] = ctl;
    bus.denise.sprpos[1] = pos;
    bus.denise.sprctl[1] = ctl | 0x0080;
    bus.denise.clxcon = 1 << 12;
    bus.current_frame_sprite_display_enable_x_by_y[0] = Some(0);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 1,
        fetch_origin_cck: None,
        planes: [
            vec![0x8000],
            vec![0],
            vec![0],
            vec![0],
            vec![0],
            vec![0],
            Vec::new(),
            Vec::new(),
        ],
    });

    bus.write_custom_word_from(0x144, 0x0000, BeamWriteSource::Cpu);
    bus.write_custom_word_from(0x14C, 0x8000, BeamWriteSource::Cpu);
    bus.advance_chipset(6);

    assert_eq!(bus.custom_read(0x00E, 2), 0x8022);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
}

#[test]
fn bplcon3_spres_hires_narrows_live_sprite_sprite_clxdat() {
    let clxdat_after_visible_sprite_pixels = |bplcon3| {
        let mut bus = empty_bus();
        let (pos0, ctl0) = sprite_control_words_for_output(0x2C, 0x2D, 0x0083);
        let (pos2, ctl2) = sprite_control_words_for_output(0x2C, 0x2D, 0x0084);
        let sprite0_ptr = 0x0100usize;
        let sprite2_ptr = 0x0200usize;

        write_chip_word(&mut bus, sprite0_ptr, pos0);
        write_chip_word(&mut bus, sprite0_ptr + 2, ctl0);
        write_chip_word(&mut bus, sprite0_ptr + 4, 0x4000);
        write_chip_word(&mut bus, sprite0_ptr + 6, 0);
        write_chip_word(&mut bus, sprite0_ptr + 8, 0);
        write_chip_word(&mut bus, sprite0_ptr + 10, 0);
        write_chip_word(&mut bus, sprite2_ptr, pos2);
        write_chip_word(&mut bus, sprite2_ptr + 2, ctl2);
        write_chip_word(&mut bus, sprite2_ptr + 4, 0x8000);
        write_chip_word(&mut bus, sprite2_ptr + 6, 0);
        write_chip_word(&mut bus, sprite2_ptr + 8, 0);
        write_chip_word(&mut bus, sprite2_ptr + 10, 0);

        bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
        bus.agnus.vpos = 0x2C;
        bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
        bus.denise.bplcon0 = 0x8000;
        bus.denise.bplcon3 = bplcon3;
        bus.denise.sprpt[0] = sprite0_ptr as u32;
        bus.denise.sprpt[2] = sprite2_ptr as u32;
        bus.display_dma_sprpt[0] = sprite0_ptr as u32;
        bus.display_dma_sprpt[2] = sprite2_ptr as u32;
        bus.current_frame_sprite_display_enable_x_by_y[0] = Some(0);

        let remaining = 0x3A - bus.agnus.hpos;
        // Control words load at the vertical-blank reset line.
        sprite_fetch_control_words_at_reset_line(&mut bus);
        bus.agnus.vpos = 0x2C;
        bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
        bus.advance_chipset(remaining);

        bus.custom_read(0x00E, 2)
    };

    assert_eq!(clxdat_after_visible_sprite_pixels(0), 0x8200);
    assert_eq!(
        clxdat_after_visible_sprite_pixels(BPLCON3_SPRES_HIRES),
        0x8000
    );
}

#[test]
fn same_line_clxcon_odd_sprite_enable_does_not_retime_earlier_live_sprite_sprite_clxdat() {
    let clxdat_after_visible_sprite_pixels = |initial_clxcon, enable_hpos: Option<u32>| {
        let mut bus = empty_bus();
        let (pos, ctl) = sprite_control_words_for_output(0x2C, 0x2D, 0x0083);
        let sprite1_ptr = 0x0100usize;
        let sprite2_ptr = 0x0200usize;
        for ptr in [sprite1_ptr, sprite2_ptr] {
            write_chip_word(&mut bus, ptr, pos);
            write_chip_word(&mut bus, ptr + 2, ctl);
            write_chip_word(&mut bus, ptr + 4, 0x8000);
            write_chip_word(&mut bus, ptr + 6, 0);
            write_chip_word(&mut bus, ptr + 8, 0);
            write_chip_word(&mut bus, ptr + 10, 0);
        }

        bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
        bus.agnus.vpos = 0x2C;
        bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
        bus.denise.clxcon = initial_clxcon;
        bus.denise.sprpt[1] = sprite1_ptr as u32;
        bus.denise.sprpt[2] = sprite2_ptr as u32;
        bus.display_dma_sprpt[1] = sprite1_ptr as u32;
        bus.display_dma_sprpt[2] = sprite2_ptr as u32;
        bus.current_frame_sprite_display_enable_x_by_y[0] = Some(0);
        bus.current_frame_render_base = bus.capture_render_snapshot();

        let after_pair_capture = SPRITE_DMA_SLOT1_HPOS[1] + 1 - bus.agnus.hpos;
        // Control words load at the vertical-blank reset line.
        sprite_fetch_control_words_at_reset_line(&mut bus);
        bus.agnus.vpos = 0x2C;
        bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
        bus.advance_chipset(after_pair_capture);

        if let Some(enable_hpos) = enable_hpos {
            let before_enable = enable_hpos - bus.agnus.hpos;
            bus.advance_chipset(before_enable);
            bus.write_custom_word_from(0x098, 1 << 12, BeamWriteSource::Cpu);
        }
        let remaining = 0x3A - bus.agnus.hpos;
        bus.advance_chipset(remaining);

        bus.custom_read(0x00E, 2)
    };

    // The CLXCON events record at the write slot + the write-effect delay:
    // slot $34 -> event $38 (before the sprite pixels), slot $36 -> event
    // $3A (after them) -- the same recorded boundary the pre-delay test
    // staged directly at $38/$3A.
    assert_eq!(clxdat_after_visible_sprite_pixels(1 << 12, None), 0x8200);
    assert_eq!(clxdat_after_visible_sprite_pixels(0, Some(0x34)), 0x8200);
    assert_eq!(clxdat_after_visible_sprite_pixels(0, Some(0x36)), 0x8000);
}

#[test]
fn ocs_lowres_bitplane_dma_fetches_plane_two_before_plane_one() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3A;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x2000;
    bus.denise.bplpt[0] = 0x0100;
    bus.denise.bplpt[1] = 0x0200;
    bus.display_dma_bplpt[0] = 0x0100;
    bus.display_dma_bplpt[1] = 0x0200;
    write_chip_word(&mut bus, 0x0100, 0x1111);
    write_chip_word(&mut bus, 0x0200, 0x2222);

    bus.advance_chipset(2);
    write_chip_word(&mut bus, 0x0100, 0xAAAA);
    write_chip_word(&mut bus, 0x0200, 0xBBBB);
    bus.advance_chipset(4);

    let row = bus.frame_captured_bitplane_rows()[0].as_ref().unwrap();
    assert_eq!(row.planes[0][0], 0xAAAA);
    assert_eq!(row.planes[1][0], 0x2222);
    assert_eq!(bus.display_dma_bplpt[0], 0x0102);
    assert_eq!(bus.display_dma_bplpt[1], 0x0202);
}

#[test]
fn bitplane_dma_fetch_loads_bpldat_latch() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3A;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    write_chip_word(&mut bus, 0x0100, 0x8001);

    bus.advance_chipset(6);

    assert_eq!(bus.denise.bpldat[0], 0x8001);
}

#[test]
fn bitplane_dma_capture_accumulates_live_dual_playfield_clxdat() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3A;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x2400;
    bus.denise.bplpt[0] = 0x0100;
    bus.denise.bplpt[1] = 0x0200;
    bus.display_dma_bplpt[0] = 0x0100;
    bus.display_dma_bplpt[1] = 0x0200;
    write_chip_word(&mut bus, 0x0100, 0x4000);
    write_chip_word(&mut bus, 0x0200, 0x4000);

    bus.advance_chipset(6);

    assert_eq!(bus.custom_read(0x00E, 2), 0x8001);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
}

#[test]
fn latched_playfield_playfield_clxdat_bit_skips_completed_row_scan() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3A;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x2400;
    bus.denise.clxdat = 1;
    bus.denise.bplpt[0] = 0x0100;
    bus.denise.bplpt[1] = 0x0200;
    bus.display_dma_bplpt[0] = 0x0100;
    bus.display_dma_bplpt[1] = 0x0200;
    write_chip_word(&mut bus, 0x0100, 0x4000);
    write_chip_word(&mut bus, 0x0200, 0x8000);

    bus.advance_chipset(6);

    assert_eq!(bus.video_pipeline_stats.collision_calls, 0);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8001);
}

#[test]
fn horizontal_diw_clips_live_playfield_clxdat() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = 0x3A;
    bus.denise.diwstrt = 0x2C84;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x2400;
    bus.denise.bplpt[0] = 0x0100;
    bus.denise.bplpt[1] = 0x0200;
    bus.display_dma_bplpt[0] = 0x0100;
    bus.display_dma_bplpt[1] = 0x0200;
    write_chip_word(&mut bus, 0x0100, 0x4000);
    write_chip_word(&mut bus, 0x0200, 0x8000);

    bus.advance_chipset(6);

    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
}

#[test]
fn shifted_horizontal_diw_offsets_live_playfield_clxdat_fetch_origin() {
    let row = CapturedBitplaneRow {
        nplanes: 2,
        words_per_row: 2,
        fetch_origin_cck: None,
        planes: [
            vec![0, 0x1000],
            vec![0, 0x1000],
            vec![0; 2],
            vec![0; 2],
            vec![0; 2],
            vec![0; 2],
            Vec::new(),
            Vec::new(),
        ],
    };
    let control = LiveCollisionControl::from_current(
        AgnusRevision::Ocs,
        0x2400,
        0,
        0,
        0,
        0x2C93,
        0x2DC1,
        DiwHigh::ocs_implicit(),
        0x0038,
        [0; 8],
    );

    assert_eq!(
        live_bitplane_collision_bits(
            &row,
            &LiveCollisionLineReplay::from_index(
                control,
                RenderRegisterSnapshot::default(),
                &BeamEventIndex::from_register_writes(&[]),
                RENDER_VISIBLE_START_VPOS as i32,
            ),
            RENDER_VISIBLE_START_VPOS as i32,
        ),
        1
    );
}

#[test]
fn denise_horizontal_delay_aligns_sprite_playfield_collision_domain() {
    let display_x = live_display_window_x(0x2C81, 0x2DC1, DiwHigh::ocs_implicit()).0;
    // The hardware window edge (62) is off the 4-px copper/register grid;
    // the nearest register-domain position maps one lo-res pixel later.
    let copper_hpos = RENDER_COPPER_WAIT_HPOS_FB0 + ((display_x as u32 + 2) / 4);
    assert_eq!(display_x, STANDARD_VISIBLE_X0 as i32);
    assert_eq!(
        framebuffer_x_for_live_collision_hpos(copper_hpos),
        display_x + 2
    );

    let row = CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 1,
        fetch_origin_cck: None,
        planes: [
            vec![0x8000],
            vec![0],
            vec![0],
            vec![0],
            vec![0],
            vec![0],
            Vec::new(),
            Vec::new(),
        ],
    };
    let control = LiveCollisionControl::from_current(
        AgnusRevision::Ocs,
        0x1000,
        0,
        0,
        0,
        0x2C81,
        0x2DC1,
        DiwHigh::ocs_implicit(),
        0x0038,
        [0; 8],
    );
    let replay = LiveCollisionLineReplay {
        line_start: control,
        segments: Vec::new(),
    };
    let source = LiveSpriteCollisionSource {
        group: 0,
        hstart: 0x81 - SPRITE_OUTPUT_DELAY_LORES,
        hsub_70ns: false,
        words: [0x8000, 0, 0, 0],
        requires_odd_enable: false,
    };

    assert_eq!(
        live_sprite_playfield_collision_bits_in_range(
            &row,
            &[source],
            &replay,
            &replay,
            RENDER_VISIBLE_START_VPOS as i32,
            display_x - 2,
            display_x,
            Some(0),
            0,
        ),
        0
    );
    assert_ne!(
        live_sprite_playfield_collision_bits_in_range(
            &row,
            &[source],
            &replay,
            &replay,
            RENDER_VISIBLE_START_VPOS as i32,
            display_x,
            display_x + 2,
            Some(0),
            0,
        ) & (1 << 5),
        0
    );
}

#[test]
fn sprite_sprite_clxdat_waits_for_bpl1dat_display_enable() {
    let control = LiveCollisionControl::from_current(
        AgnusRevision::Ocs,
        0x1000,
        0,
        0,
        0,
        ((RENDER_VISIBLE_START_VPOS as u16) << 8) | RENDER_DIW_HSTART_FB0 as u16,
        ((RENDER_VISIBLE_START_VPOS as u16 + 1) << 8) | 0x00C1,
        DiwHigh::ocs_implicit(),
        0x0038,
        [0; 8],
    );
    let replay = LiveCollisionLineReplay {
        line_start: control,
        segments: Vec::new(),
    };
    let sources = [
        LiveSpriteCollisionSource {
            group: 0,
            hstart: RENDER_DIW_HSTART_FB0 - SPRITE_OUTPUT_DELAY_LORES,
            hsub_70ns: false,
            words: [0x8000, 0, 0, 0],
            requires_odd_enable: false,
        },
        LiveSpriteCollisionSource {
            group: 1,
            hstart: RENDER_DIW_HSTART_FB0 - SPRITE_OUTPUT_DELAY_LORES,
            hsub_70ns: false,
            words: [0x8000, 0, 0, 0],
            requires_odd_enable: false,
        },
    ];

    assert_eq!(
        live_sprite_sprite_collision_bits(
            &sources,
            &replay,
            RENDER_VISIBLE_START_VPOS as i32,
            0,
            2,
            None,
            0,
        ),
        0
    );
    assert_eq!(
        live_sprite_sprite_collision_bits(
            &sources,
            &replay,
            RENDER_VISIBLE_START_VPOS as i32,
            0,
            2,
            Some(2),
            0,
        ),
        0
    );
    assert_eq!(
        live_sprite_sprite_collision_bits(
            &sources,
            &replay,
            RENDER_VISIBLE_START_VPOS as i32,
            0,
            2,
            Some(0),
            0,
        ) & (1 << 9),
        1 << 9
    );
}

#[test]
fn live_sprite_sprite_clxdat_skips_already_latched_bits() {
    let control = LiveCollisionControl::from_current(
        AgnusRevision::Ocs,
        0x1000,
        0,
        0,
        0,
        ((RENDER_VISIBLE_START_VPOS as u16) << 8) | RENDER_DIW_HSTART_FB0 as u16,
        ((RENDER_VISIBLE_START_VPOS as u16 + 1) << 8) | 0x00C1,
        DiwHigh::ocs_implicit(),
        0x0038,
        [0; 8],
    );
    let replay = LiveCollisionLineReplay {
        line_start: control,
        segments: Vec::new(),
    };
    let sources = [
        LiveSpriteCollisionSource {
            group: 0,
            hstart: RENDER_DIW_HSTART_FB0 - SPRITE_OUTPUT_DELAY_LORES,
            hsub_70ns: false,
            words: [0x8000, 0, 0, 0],
            requires_odd_enable: false,
        },
        LiveSpriteCollisionSource {
            group: 1,
            hstart: RENDER_DIW_HSTART_FB0 - SPRITE_OUTPUT_DELAY_LORES,
            hsub_70ns: false,
            words: [0x8000, 0, 0, 0],
            requires_odd_enable: false,
        },
        LiveSpriteCollisionSource {
            group: 2,
            hstart: RENDER_DIW_HSTART_FB0 - SPRITE_OUTPUT_DELAY_LORES,
            hsub_70ns: false,
            words: [0x8000, 0, 0, 0],
            requires_odd_enable: false,
        },
    ];

    let clxdat = live_sprite_sprite_collision_bits(
        &sources,
        &replay,
        RENDER_VISIBLE_START_VPOS as i32,
        0,
        2,
        Some(0),
        1 << 9,
    );

    assert_eq!(clxdat & (1 << 9), 0);
    assert_ne!(clxdat & (1 << 10), 0);
    assert_ne!(clxdat & (1 << 12), 0);
}

#[test]
fn live_sprite_playfield_clxdat_skips_already_latched_bits() {
    let display_x = live_display_window_x(0x2C81, 0x2DC1, DiwHigh::ocs_implicit()).0;
    let row = CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 1,
        fetch_origin_cck: None,
        planes: [
            vec![0x8000],
            vec![0],
            vec![0],
            vec![0],
            vec![0],
            vec![0],
            Vec::new(),
            Vec::new(),
        ],
    };
    let control = LiveCollisionControl::from_current(
        AgnusRevision::Ocs,
        0x1000,
        0,
        0,
        0,
        0x2C81,
        0x2DC1,
        DiwHigh::ocs_implicit(),
        0x0038,
        [0; 8],
    );
    let replay = LiveCollisionLineReplay {
        line_start: control,
        segments: Vec::new(),
    };
    let source = LiveSpriteCollisionSource {
        group: 0,
        hstart: 0x81 - SPRITE_OUTPUT_DELAY_LORES,
        hsub_70ns: false,
        words: [0x8000, 0, 0, 0],
        requires_odd_enable: false,
    };

    let clxdat = live_sprite_playfield_collision_bits_in_range(
        &row,
        &[source],
        &replay,
        &replay,
        RENDER_VISIBLE_START_VPOS as i32,
        display_x,
        display_x + 2,
        Some(0),
        1 << 5,
    );
    assert_ne!(clxdat & (1 << 1), 0);
    assert_eq!(clxdat & (1 << 5), 0);
    assert_eq!(
        live_sprite_playfield_collision_bits_in_range(
            &row,
            &[source],
            &replay,
            &replay,
            RENDER_VISIBLE_START_VPOS as i32,
            display_x,
            display_x + 2,
            Some(0),
            (1 << 1) | (1 << 5),
        ),
        0
    );
    assert_ne!(
        live_sprite_playfield_collision_bits_in_range(
            &row,
            &[source],
            &replay,
            &replay,
            RENDER_VISIBLE_START_VPOS as i32,
            display_x,
            display_x + 2,
            Some(0),
            1 << 1,
        ) & (1 << 5),
        0
    );
}

#[test]
fn brdsprt_bypasses_bpl1dat_display_enable_for_live_sprite_clxdat() {
    let control = LiveCollisionControl::from_current(
        AgnusRevision::Ocs,
        BPLCON0_ECSENA | 0x1000,
        0,
        BPLCON3_BRDSPRT,
        0,
        ((RENDER_VISIBLE_START_VPOS as u16) << 8) | RENDER_DIW_HSTART_FB0 as u16,
        ((RENDER_VISIBLE_START_VPOS as u16 + 1) << 8) | 0x00C1,
        DiwHigh::ocs_implicit(),
        0x0038,
        [0; 8],
    );
    let replay = LiveCollisionLineReplay {
        line_start: control,
        segments: Vec::new(),
    };
    let sources = [
        LiveSpriteCollisionSource {
            group: 0,
            hstart: RENDER_DIW_HSTART_FB0 - SPRITE_OUTPUT_DELAY_LORES,
            hsub_70ns: false,
            words: [0x8000, 0, 0, 0],
            requires_odd_enable: false,
        },
        LiveSpriteCollisionSource {
            group: 1,
            hstart: RENDER_DIW_HSTART_FB0 - SPRITE_OUTPUT_DELAY_LORES,
            hsub_70ns: false,
            words: [0x8000, 0, 0, 0],
            requires_odd_enable: false,
        },
    ];

    assert_eq!(
        live_sprite_sprite_collision_bits(
            &sources,
            &replay,
            RENDER_VISIBLE_START_VPOS as i32,
            0,
            2,
            None,
            0,
        ) & (1 << 9),
        1 << 9
    );
}

#[test]
fn brdrblnk_suppresses_brdsprt_live_sprite_clxdat_bypass() {
    let control = LiveCollisionControl::from_current(
        AgnusRevision::Ocs,
        BPLCON0_ECSENA | 0x1000,
        0,
        BPLCON3_BRDSPRT | BPLCON3_BRDRBLNK,
        0,
        ((RENDER_VISIBLE_START_VPOS as u16) << 8) | RENDER_DIW_HSTART_FB0 as u16,
        ((RENDER_VISIBLE_START_VPOS as u16 + 1) << 8) | 0x00C1,
        DiwHigh::ocs_implicit(),
        0x0038,
        [0; 8],
    );
    let replay = LiveCollisionLineReplay {
        line_start: control,
        segments: Vec::new(),
    };
    let sources = [
        LiveSpriteCollisionSource {
            group: 0,
            hstart: RENDER_DIW_HSTART_FB0 - SPRITE_OUTPUT_DELAY_LORES,
            hsub_70ns: false,
            words: [0x8000, 0, 0, 0],
            requires_odd_enable: false,
        },
        LiveSpriteCollisionSource {
            group: 1,
            hstart: RENDER_DIW_HSTART_FB0 - SPRITE_OUTPUT_DELAY_LORES,
            hsub_70ns: false,
            words: [0x8000, 0, 0, 0],
            requires_odd_enable: false,
        },
    ];

    assert_eq!(
        live_sprite_sprite_collision_bits(
            &sources,
            &replay,
            RENDER_VISIBLE_START_VPOS as i32,
            0,
            2,
            None,
            0,
        ) & (1 << 9),
        0
    );
}

#[test]
fn manual_bpl1dat_display_enable_allows_live_sprite_clxdat_on_vertically_closed_diw_line() {
    let control = LiveCollisionControl::from_current(
        AgnusRevision::Ocs,
        0x1000,
        0,
        0,
        0,
        ((RENDER_VISIBLE_START_VPOS as u16 + 10) << 8) | RENDER_DIW_HSTART_FB0 as u16,
        ((RENDER_VISIBLE_START_VPOS as u16 + 20) << 8) | 0x00C1,
        DiwHigh::ocs_implicit(),
        0x0038,
        [0; 8],
    );
    let replay = LiveCollisionLineReplay {
        line_start: control,
        segments: Vec::new(),
    };
    let sources = [
        LiveSpriteCollisionSource {
            group: 0,
            hstart: RENDER_DIW_HSTART_FB0 - SPRITE_OUTPUT_DELAY_LORES,
            hsub_70ns: false,
            words: [0x8000, 0, 0, 0],
            requires_odd_enable: false,
        },
        LiveSpriteCollisionSource {
            group: 1,
            hstart: RENDER_DIW_HSTART_FB0 - SPRITE_OUTPUT_DELAY_LORES,
            hsub_70ns: false,
            words: [0x8000, 0, 0, 0],
            requires_odd_enable: false,
        },
    ];

    assert_eq!(
        live_sprite_sprite_collision_bits(
            &sources,
            &replay,
            RENDER_VISIBLE_START_VPOS as i32,
            0,
            2,
            None,
            0,
        ) & (1 << 9),
        0
    );
    assert_eq!(
        live_sprite_sprite_collision_bits(
            &sources,
            &replay,
            RENDER_VISIBLE_START_VPOS as i32,
            0,
            2,
            Some(0),
            0,
        ) & (1 << 9),
        1 << 9
    );
}

#[test]
fn bpldat_writes_update_latched_planes_for_live_playfield_clxdat() {
    let clxdat_after_row_capture = |bpldat_hpos: Option<u32>| {
        let mut bus = empty_bus();
        bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
        bus.agnus.vpos = 0x2C;
        bus.agnus.hpos = 0x3A;
        bus.denise.diwstrt = 0x2C83;
        bus.denise.diwstop = 0x2DC1;
        bus.denise.ddfstrt = 0x0038;
        bus.denise.ddfstop = 0x0040;
        bus.denise.bplcon0 = 0x7400;
        for plane in 0..4 {
            let ptr = 0x0100 + plane * 0x40;
            bus.denise.bplpt[plane] = ptr as u32;
            bus.display_dma_bplpt[plane] = ptr as u32;
        }
        bus.current_frame_render_base = bus.capture_render_snapshot();
        write_chip_word(&mut bus, 0x0100, 0);
        write_chip_word(&mut bus, 0x0102, 0x8000);

        if let Some(bpldat_hpos) = bpldat_hpos {
            let before_bpldat = bpldat_hpos - bus.agnus.hpos;
            bus.advance_chipset(before_bpldat);
            bus.write_custom_word_from(0x11A, 0x8000, BeamWriteSource::Cpu);
        }
        let remaining = 0x48 - bus.agnus.hpos;
        bus.advance_chipset(remaining);

        bus.custom_read(0x00E, 2)
    };

    // Slot $3A: the BPLxDAT event records at $3E (slot + write-effect
    // delay), the boundary the pre-delay test staged directly.
    assert_eq!(clxdat_after_row_capture(None), 0x8000);
    assert_eq!(clxdat_after_row_capture(Some(0x3A)), 0x8001);
}

#[test]
fn same_line_bplcon0_dual_playfield_enable_does_not_retime_earlier_live_clxdat() {
    let clxdat_after_row_capture = |initial_bplcon0, enable_hpos: Option<u32>| {
        let mut bus = empty_bus();
        bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
        bus.agnus.vpos = 0x2C;
        bus.agnus.hpos = 0x3A;
        bus.denise.diwstrt = 0x2C81;
        bus.denise.diwstop = 0x2DC1;
        bus.denise.ddfstrt = 0x0038;
        bus.denise.ddfstop = 0x0040;
        bus.denise.bplcon0 = initial_bplcon0;
        bus.denise.bplpt[0] = 0x0100;
        bus.denise.bplpt[1] = 0x0200;
        bus.display_dma_bplpt[0] = 0x0100;
        bus.display_dma_bplpt[1] = 0x0200;
        bus.current_frame_render_base = bus.capture_render_snapshot();
        write_chip_word(&mut bus, 0x0100, 0x4000);
        write_chip_word(&mut bus, 0x0102, 0);
        write_chip_word(&mut bus, 0x0200, 0x4000);
        write_chip_word(&mut bus, 0x0202, 0);

        if let Some(enable_hpos) = enable_hpos {
            let before_enable = enable_hpos - bus.agnus.hpos;
            bus.advance_chipset(before_enable);
            bus.write_custom_word_from(0x100, 0x2400, BeamWriteSource::Cpu);
        }
        let remaining = 0x48 - bus.agnus.hpos;
        bus.advance_chipset(remaining);

        bus.custom_read(0x00E, 2)
    };

    assert_eq!(clxdat_after_row_capture(0x2400, None), 0x8001);
    assert_eq!(clxdat_after_row_capture(0x2000, Some(0x40)), 0x8000);
}

#[test]
fn captured_sprite_and_bitplane_rows_accumulate_live_sprite_playfield_clxdat() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0300usize;
    // hstart +1 vs the pre-fix value: sprite comparator positions share
    // Denise's counter and moved with the corrected window-edge anchor
    // (2H-196); the beam-anchored playfield sample did not.
    let (pos, ctl) = sprite_control_words_for_output(0x2C, 0x2D, 0x0082);
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x8000);
    write_chip_word(&mut bus, sprite_ptr + 6, 0);
    write_chip_word(&mut bus, sprite_ptr + 8, 0);
    write_chip_word(&mut bus, sprite_ptr + 10, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    // One-bitplane playfield: match only bitplane 1 so the absent (zero)
    // planes 2-6 still match. The default CLXCON_RESET wants all six planes
    // = 1, which one plane cannot satisfy -> no collision on hardware.
    bus.denise.clxcon = 0x0FC1;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    bus.current_frame_sprite_display_enable_x_by_y[0] = Some(0);
    write_chip_word(&mut bus, 0x0100, 0x4000);

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(1);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
    let remaining = 0x40 - bus.agnus.hpos;
    bus.advance_chipset(remaining);

    assert_eq!(bus.custom_read(0x00E, 2), 0x8022);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
}

#[test]
fn explicit_bpl1dat_output_accumulates_live_sprite_playfield_clxdat() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0300usize;
    let (pos, ctl) = sprite_control_words_for_output(0x2C, 0x2D, 0x0083);
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x8000);
    write_chip_word(&mut bus, sprite_ptr + 6, 0);
    write_chip_word(&mut bus, sprite_ptr + 8, 0);
    write_chip_word(&mut bus, sprite_ptr + 10, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.bplcon0 = 0x1000;
    // One-bitplane playfield: enable all plane-collision inputs but only match
    // bitplane 1 (MVBP1), so the absent planes 2-6 (which read 0) still match.
    // The default CLXCON_RESET (0x0FFF) demands all six planes = 1, which a
    // one-plane playfield can never satisfy -> no collision on real hardware.
    bus.denise.clxcon = 0x0FC1;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;
    bus.current_frame_render_base = bus.capture_render_snapshot();

    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(2);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);

    // Slot $34: the BPL1DAT event records at $38 (slot + write-effect
    // delay), the same boundary the pre-delay test staged directly.
    let before_bpl1dat = 0x34 - bus.agnus.hpos;
    bus.advance_chipset(before_bpl1dat);
    bus.write_custom_word_from(0x110, 0x8000, BeamWriteSource::Cpu);
    bus.advance_chipset(6);

    assert_eq!(bus.custom_read(0x00E, 2), 0x8020);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
}

#[test]
fn manual_sprite_and_bpl1dat_writes_accumulate_live_sprite_playfield_clxdat() {
    let mut bus = empty_bus();
    let (pos, ctl) = sprite_control_words_for_output(0x2C, 0x2D, 0x0083);
    bus.agnus.vpos = 0x2C;
    // Slot $34: the write events record at $38 (slot + write-effect delay).
    bus.agnus.hpos = 0x34;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.bplcon0 = 0x1000;
    // One-bitplane playfield: match only bitplane 1 (absent planes 2-6 read 0
    // and still match); CLXCON_RESET's all-six-planes=1 never matches.
    bus.denise.clxcon = 0x0FC1;
    bus.denise.sprpos[0] = pos;
    bus.denise.sprctl[0] = ctl;
    bus.current_frame_render_base = bus.capture_render_snapshot();

    bus.write_custom_word_from(0x144, 0x8000, BeamWriteSource::Cpu);
    bus.write_custom_word_from(0x110, 0x8000, BeamWriteSource::Cpu);
    bus.advance_chipset(6);

    assert_eq!(bus.custom_read(0x00E, 2), 0x8020);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
}

#[test]
fn same_line_bplcon1_scroll_increase_latches_later_live_sprite_playfield_clxdat() {
    let mut bus = empty_bus();
    let sprite_ptr = 0x0300usize;
    // hstart +1 vs the pre-fix value: sprite comparator positions share
    // Denise's counter and moved with the corrected window-edge anchor
    // (2H-196); the beam-anchored playfield sample did not.
    let (pos, ctl) = sprite_control_words_for_output(0x2C, 0x2D, 0x0094);
    write_chip_word(&mut bus, sprite_ptr, pos);
    write_chip_word(&mut bus, sprite_ptr + 2, ctl);
    write_chip_word(&mut bus, sprite_ptr + 4, 0x8000);
    write_chip_word(&mut bus, sprite_ptr + 6, 0);
    write_chip_word(&mut bus, sprite_ptr + 8, 0);
    write_chip_word(&mut bus, sprite_ptr + 10, 0);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN | DMACON_BPLEN;
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.denise.diwstrt = 0x2C83;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.bplcon1 = 0;
    // One-bitplane playfield: match only bitplane 1 (absent planes 2-6 read 0
    // and still match); CLXCON_RESET's all-six-planes=1 never matches.
    bus.denise.clxcon = 0x0FC1;
    bus.denise.sprpt[0] = sprite_ptr as u32;
    bus.display_dma_sprpt[0] = sprite_ptr as u32;
    bus.denise.bplpt[0] = 0x0100;
    bus.display_dma_bplpt[0] = 0x0100;
    bus.current_frame_render_base = bus.capture_render_snapshot();
    write_chip_word(&mut bus, 0x0100, 0x0001);

    // Slot $3C: the BPLCON1 event records at $40 (slot + write-effect
    // delay), the same boundary the pre-delay test staged directly.
    let before_scroll_write = 0x3C - bus.agnus.hpos;
    // Control words load at the vertical-blank reset line.
    sprite_fetch_control_words_at_reset_line(&mut bus);
    bus.agnus.vpos = 0x2C;
    bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
    bus.advance_chipset(before_scroll_write);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);

    bus.write_custom_word_from(0x102, 0x0004, BeamWriteSource::Cpu);
    let remaining = 0x48 - bus.agnus.hpos;
    bus.advance_chipset(remaining);

    assert_eq!(bus.custom_read(0x00E, 2), 0x8022);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
}

#[test]
fn same_line_clxcon_odd_sprite_enable_does_not_retime_earlier_live_sprite_playfield_clxdat() {
    let clxdat_after_row_capture = |initial_clxcon, enable_hpos: Option<u32>| {
        let mut bus = empty_bus();
        let sprite_ptr = 0x0300usize;
        let (pos, ctl) = sprite_control_words_for_output(0x2C, 0x2D, 0x0083);
        write_chip_word(&mut bus, sprite_ptr, pos);
        write_chip_word(&mut bus, sprite_ptr + 2, ctl);
        write_chip_word(&mut bus, sprite_ptr + 4, 0x8000);
        write_chip_word(&mut bus, sprite_ptr + 6, 0);
        write_chip_word(&mut bus, sprite_ptr + 8, 0);
        write_chip_word(&mut bus, sprite_ptr + 10, 0);

        bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN | DMACON_BPLEN;
        bus.agnus.vpos = 0x2C;
        bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
        bus.denise.diwstrt = 0x2C83;
        bus.denise.diwstop = 0x2DC1;
        bus.denise.ddfstrt = 0x0038;
        bus.denise.ddfstop = 0x0040;
        bus.denise.bplcon0 = 0x1000;
        bus.denise.clxcon = initial_clxcon;
        bus.denise.sprpt[1] = sprite_ptr as u32;
        bus.display_dma_sprpt[1] = sprite_ptr as u32;
        bus.denise.bplpt[0] = 0x0100;
        bus.display_dma_bplpt[0] = 0x0100;
        bus.current_frame_sprite_display_enable_x_by_y[0] = Some(0);
        bus.current_frame_render_base = bus.capture_render_snapshot();
        write_chip_word(&mut bus, 0x0100, 0x8000);
        write_chip_word(&mut bus, 0x0102, 0);

        // Control words load at the vertical-blank reset line.
        sprite_fetch_control_words_at_reset_line(&mut bus);
        bus.agnus.vpos = 0x2C;
        bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
        bus.advance_chipset(1);
        assert_eq!(bus.custom_read(0x00E, 2), 0x8000);

        if let Some(enable_hpos) = enable_hpos {
            let before_enable = enable_hpos - bus.agnus.hpos;
            bus.advance_chipset(before_enable);
            bus.write_custom_word_from(0x098, 1 << 12, BeamWriteSource::Cpu);
        }
        let remaining = 0x48 - bus.agnus.hpos;
        bus.advance_chipset(remaining);

        bus.custom_read(0x00E, 2)
    };

    assert_eq!(clxdat_after_row_capture(1 << 12, None), 0x8022);
    assert_eq!(clxdat_after_row_capture(0, Some(0x40)), 0x8000);
}

#[test]
fn bplcon3_spres_hires_narrows_live_sprite_playfield_clxdat() {
    let clxdat_after_bitplane_row_capture = |bplcon3| {
        let mut bus = empty_bus();
        let sprite_ptr = 0x0300usize;
        // hstart +1 vs the pre-fix value: see the sibling clxdat tests.
        let (pos, ctl) = sprite_control_words_for_output(0x2C, 0x2D, 0x0082);
        write_chip_word(&mut bus, sprite_ptr, pos);
        write_chip_word(&mut bus, sprite_ptr + 2, ctl);
        write_chip_word(&mut bus, sprite_ptr + 4, 0x8000);
        write_chip_word(&mut bus, sprite_ptr + 6, 0);
        write_chip_word(&mut bus, sprite_ptr + 8, 0);
        write_chip_word(&mut bus, sprite_ptr + 10, 0);

        bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
        bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN | DMACON_BPLEN;
        bus.agnus.vpos = 0x2C;
        bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
        bus.denise.diwstrt = 0x2C81;
        bus.denise.diwstop = 0x2DC1;
        bus.denise.ddfstrt = 0x0038;
        bus.denise.ddfstop = 0x0038;
        // ECSENA/ENBPLCN3 set so the live SPRES write below latches.
        bus.denise.bplcon0 = 0x9000 | BPLCON0_ECSENA;
        bus.denise.bplcon3 = bplcon3;
        // One-bitplane playfield: match only bitplane 1 so the absent planes
        // 2-6 still match (CLXCON_RESET's all-six=1 never matches one plane).
        bus.denise.clxcon = 0x0FC1;
        bus.denise.sprpt[0] = sprite_ptr as u32;
        bus.display_dma_sprpt[0] = sprite_ptr as u32;
        bus.denise.bplpt[0] = 0x0100;
        bus.display_dma_bplpt[0] = 0x0100;
        write_chip_word(&mut bus, 0x0100, 0x0004);
        write_chip_word(&mut bus, 0x0102, 0);

        // Control words load at the vertical-blank reset line.
        sprite_fetch_control_words_at_reset_line(&mut bus);
        bus.agnus.vpos = 0x2C;
        bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
        bus.advance_chipset(1);
        let remaining = 0x40 - bus.agnus.hpos;
        bus.advance_chipset(remaining);

        bus.custom_read(0x00E, 2)
    };

    assert_eq!(clxdat_after_bitplane_row_capture(0), 0x8022);
    assert_eq!(
        clxdat_after_bitplane_row_capture(BPLCON3_SPRES_HIRES),
        0x8020
    );
}

#[test]
fn same_line_bplcon3_spres_write_does_not_retime_earlier_live_sprite_playfield_clxdat() {
    let clxdat_after_bitplane_row_capture = |spres_hpos: Option<u32>| {
        let mut bus = empty_bus();
        let sprite_ptr = 0x0300usize;
        // hstart +1 vs the pre-fix value: see the sibling clxdat tests.
        let (pos, ctl) = sprite_control_words_for_output(0x2C, 0x2D, 0x0082);
        write_chip_word(&mut bus, sprite_ptr, pos);
        write_chip_word(&mut bus, sprite_ptr + 2, ctl);
        write_chip_word(&mut bus, sprite_ptr + 4, 0x8000);
        write_chip_word(&mut bus, sprite_ptr + 6, 0);
        write_chip_word(&mut bus, sprite_ptr + 8, 0);
        write_chip_word(&mut bus, sprite_ptr + 10, 0);

        bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
        bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN | DMACON_BPLEN;
        bus.agnus.vpos = 0x2C;
        bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
        bus.denise.diwstrt = 0x2C81;
        bus.denise.diwstop = 0x2DC1;
        bus.denise.ddfstrt = 0x0038;
        bus.denise.ddfstop = 0x0038;
        // ECSENA/ENBPLCN3 set so the live SPRES write below latches.
        bus.denise.bplcon0 = 0x9000 | BPLCON0_ECSENA;
        bus.denise.bplcon3 = 0;
        // One-bitplane playfield: match only bitplane 1 so the absent planes
        // 2-6 still match (CLXCON_RESET's all-six=1 never matches one plane).
        bus.denise.clxcon = 0x0FC1;
        bus.denise.sprpt[0] = sprite_ptr as u32;
        bus.display_dma_sprpt[0] = sprite_ptr as u32;
        bus.denise.bplpt[0] = 0x0100;
        bus.display_dma_bplpt[0] = 0x0100;
        bus.current_frame_render_base = bus.capture_render_snapshot();
        write_chip_word(&mut bus, 0x0100, 0x0004);
        write_chip_word(&mut bus, 0x0102, 0);

        // Control words load at the vertical-blank reset line.
        sprite_fetch_control_words_at_reset_line(&mut bus);
        bus.agnus.vpos = 0x2C;
        bus.agnus.hpos = SPRITE_DMA_SLOT1_HPOS[0] - 1;
        bus.advance_chipset(1);
        if let Some(spres_hpos) = spres_hpos {
            let before_spres = spres_hpos - bus.agnus.hpos;
            bus.advance_chipset(before_spres);
            bus.write_custom_word_from(0x106, BPLCON3_SPRES_HIRES, BeamWriteSource::Cpu);
        }
        let remaining = 0x40 - bus.agnus.hpos;
        bus.advance_chipset(remaining);

        bus.custom_read(0x00E, 2)
    };

    // The BPLCON3 events record at the write slot + the write-effect delay:
    // slot $34 -> event $38 (retimes the sprite before its pixels), slot
    // $36 -> event $3A (too late) -- the same recorded boundary the
    // pre-delay test staged directly at $38/$3A.
    assert_eq!(clxdat_after_bitplane_row_capture(None), 0x8022);
    assert_eq!(clxdat_after_bitplane_row_capture(Some(0x34)), 0x8020);
    assert_eq!(clxdat_after_bitplane_row_capture(Some(0x36)), 0x8022);
}

#[test]
fn serial_receive_completion_preempts_the_cpu_slice() {
    // A word finishing in Paula's receiver raises RBF through the same
    // slice-preempting path as every other interrupt source. Regression:
    // the serial tick's bits were OR'd into INTREQ silently, so a running
    // CPU stayed blind to RBF until its instruction slice ran out --
    // longer than a character time at BBS baud rates, overrunning the
    // one-word receive buffer before the guest handler ran.
    let mut bus = empty_bus();
    let (sink, handle) = crate::serial::ChannelSerialSink::pair();
    bus.paula.serial = Box::new(sink);
    bus.paula.serper = 0; // 1 cck per bit: a word completes in 10 ccks
    handle.push_input(b"a");
    bus.begin_cpu_slice();

    bus.advance_devices(20);

    assert!(bus.slice_preempted);
    assert_ne!(bus.paula.intreq & crate::chipset::paula::INT_RBF, 0);
}

#[test]
fn bltsize_starts_dma_and_preempts_cpu_slice_without_irq_preempt() {
    let mut bus = empty_bus();
    bus.paula.intena = INT_BLIT;
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BLTEN;
    bus.agnus.hpos = 0x20;
    bus.blitter.bltcon0 = 0x0100;
    bus.begin_cpu_slice();

    let preempt = bus.custom_write(0x058, 2, (1 << 6) | 1);

    assert!(!preempt);
    assert!(bus.slice_preempted);
    assert_eq!(bus.paula.intreq & INT_BLIT, 0);
    assert!(bus.blitter.busy);
    assert_eq!(bus.next_blitter_completion_cck(), Some(8));
    bus.advance_chipset(1);
    assert_eq!(bus.paula.intreq & INT_BLIT, 0);
    assert!(bus.blitter.busy);
    assert_eq!(bus.next_blitter_completion_cck(), Some(7));
    bus.advance_chipset(8);
    assert_ne!(bus.paula.intreq & INT_BLIT, 0);
    assert!(!bus.blitter.busy);
    assert_eq!(bus.next_blitter_completion_cck(), None);
}

#[test]
fn lowmem_blit_diagnostic_uses_mode_specific_write_gate() {
    let mut bus = empty_bus();
    bus.blitter.bltdpt = 0x0FFE;

    bus.blitter.bltcon0 = BLTCON0_USE_D;
    bus.blitter.bltcon1 = 0;
    assert!(
        bus.blitter_start_may_write_lowmem(),
        "normal mode writes through channel D"
    );

    bus.blitter.bltcon1 = BLTCON1_DOFF;
    assert!(
        !bus.blitter_start_may_write_lowmem(),
        "normal mode DOFF suppresses the D-channel store"
    );

    bus.blitter.bltcon0 = BLTCON0_USE_C;
    bus.blitter.bltcon1 = BLTCON1_LINE;
    assert!(
        bus.blitter_start_may_write_lowmem(),
        "line mode stores are gated by channel C"
    );

    bus.blitter.bltcon0 = BLTCON0_USE_D;
    assert!(
        !bus.blitter_start_may_write_lowmem(),
        "line mode does not use channel D enable as the store gate"
    );

    bus.blitter.bltcon0 = BLTCON0_USE_C;
    bus.blitter.bltdpt = 0x1000;
    assert!(
        !bus.blitter_start_may_write_lowmem(),
        "the diagnostic only trips for the exception-vector area"
    );

    bus.diag_lowmem_blit = true;
    bus.reset_transient_diagnostics_after_state_load();
    assert!(
        !bus.diag_lowmem_blit,
        "a restored save state must not replay stale diagnostic alarms"
    );
}

#[test]
fn bltsize_stale_blit_clear_rearms_interrupt_recognition_for_next_completion() {
    let mut bus = empty_bus();
    bus.irq_latency_setting = 65;
    bus.paula.intena = INT_MASTER | INT_BLIT;
    bus.paula.intreq = INT_BLIT;
    bus.arm_irq_recognition_latency();
    assert_eq!(bus.irq_latency_last_pending & INT_BLIT, INT_BLIT);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BLTEN;
    bus.agnus.hpos = 0x20;
    bus.blitter.bltcon0 = 0x0100;
    bus.begin_cpu_slice();

    assert!(!bus.custom_write(0x058, 2, (1 << 6) | 1));
    assert_eq!(bus.paula.intreq & INT_BLIT, 0);
    assert_eq!(bus.irq_latency_last_pending & INT_BLIT, 0);
    assert_eq!(bus.irq_latency_mask & INT_BLIT, 0);

    let completion_cck = bus.next_blitter_completion_cck().unwrap();
    bus.advance_chipset(completion_cck);
    assert_ne!(bus.paula.intreq & INT_BLIT, 0);
    assert_eq!(bus.irq_latency_mask & INT_BLIT, INT_BLIT);
    assert_eq!(bus.cpu_visible_intreq() & INT_BLIT, 0);

    bus.advance_chipset(65);
    assert_ne!(bus.cpu_visible_intreq() & INT_BLIT, 0);
}

#[test]
fn busy_blitter_register_writes_finish_current_blit_before_latching_next_state() {
    assert_busy_blitter_register_write_drains_current_blit(0x044, 0x0FF0, |bus| {
        assert_eq!(bus.blitter.bltafwm, 0x0FF0);
    });
    assert_busy_blitter_register_write_drains_current_blit(0x046, 0xF0F0, |bus| {
        assert_eq!(bus.blitter.bltalwm, 0xF0F0);
    });
    assert_busy_blitter_register_write_drains_current_blit(0x04A, 0x0060, |bus| {
        assert_eq!(bus.blitter.bltcpt, 0x0060);
    });
    assert_busy_blitter_register_write_drains_current_blit(0x04E, 0x0070, |bus| {
        assert_eq!(bus.blitter.bltbpt, 0x0070);
    });
    assert_busy_blitter_register_write_drains_current_blit(0x052, 0x0080, |bus| {
        assert_eq!(bus.blitter.bltapt, 0x0080);
    });
    assert_busy_blitter_register_write_drains_current_blit(0x056, 0x0090, |bus| {
        assert_eq!(bus.blitter.bltdpt, 0x0090);
    });
    assert_busy_blitter_register_write_drains_current_blit(0x060, 0x0012, |bus| {
        assert_eq!(bus.blitter.bltcmod, 0x0012);
    });
    assert_busy_blitter_register_write_drains_current_blit(0x062, 0x0022, |bus| {
        assert_eq!(bus.blitter.bltbmod, 0x0022);
    });
    assert_busy_blitter_register_write_drains_current_blit(0x064, 0x0032, |bus| {
        assert_eq!(bus.blitter.bltamod, 0x0032);
    });
    assert_busy_blitter_register_write_drains_current_blit(0x066, 0x0042, |bus| {
        assert_eq!(bus.blitter.bltdmod, 0x0042);
    });
}

#[test]
fn busy_bltcon0_write_disables_remaining_d_output_without_draining_blit() {
    let mut bus = bus_with_pending_two_word_a_to_d_blit();
    bus.advance_chipset(4);
    assert!(bus.blitter.busy);
    assert_eq!(&bus.mem.chip_ram[0x20..0x24], &[0, 0, 0, 0]);
    assert_eq!(bus.paula.intreq & INT_BLIT, 0);

    assert!(!bus.custom_write(0x040, 2, 0x0000));

    assert!(bus.blitter.busy);
    assert_eq!(bus.blitter.bltcon0, 0x0000);
    assert_eq!(&bus.mem.chip_ram[0x20..0x24], &[0, 0, 0, 0]);
    assert_eq!(bus.paula.intreq & INT_BLIT, 0);

    bus.advance_chipset(8);
    assert!(!bus.blitter.busy);
    assert_eq!(&bus.mem.chip_ram[0x20..0x24], &[0, 0, 0, 0]);
    assert_ne!(bus.paula.intreq & INT_BLIT, 0);
}

#[test]
fn busy_bltcon1_line_bit_write_updates_register_without_reinterpreting_pipeline_snapshot() {
    let mut bus = bus_with_pending_two_word_a_to_d_blit();

    assert!(!bus.custom_write(0x042, 2, 0x0001));

    assert!(bus.blitter.busy);
    assert_eq!(bus.blitter.bltcon1, 0x0001);
    assert_eq!(bus.paula.intreq & INT_BLIT, 0);

    bus.advance_chipset(11);
    assert!(!bus.blitter.busy);
    assert_eq!(&bus.mem.chip_ram[0x20..0x24], &[0x11, 0x11, 0x22, 0x22]);
    assert_ne!(bus.paula.intreq & INT_BLIT, 0);
}

#[test]
fn ecs_busy_bltcon0l_write_finishes_current_blit_before_latching_minterm() {
    let mut bus = bus_with_pending_two_word_a_to_d_blit();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);

    assert!(!bus.custom_write(0x05A, 2, 0x00A5));

    assert!(!bus.blitter.busy);
    assert_eq!(&bus.mem.chip_ram[0x20..0x24], &[0x11, 0x11, 0x22, 0x22]);
    assert_ne!(bus.paula.intreq & INT_BLIT, 0);
    assert_eq!(bus.blitter.bltcon0, 0x09A5);
}

#[test]
fn busy_bltsize_write_finishes_current_blit_then_starts_replacement() {
    let mut bus = bus_with_pending_two_word_a_to_d_blit();

    assert!(!bus.custom_write(0x058, 2, ((1 << 6) | 1) as u64));

    assert!(bus.blitter.busy);
    assert_eq!(&bus.mem.chip_ram[0x20..0x24], &[0x11, 0x11, 0x22, 0x22]);
    // Starting the replacement blit consumes the finished blit's pending
    // interrupt request: INTREQ.BLIT means "the last started blit has
    // finished", and the replacement has not finished yet.
    assert_eq!(bus.paula.intreq & INT_BLIT, 0);
    assert_eq!(bus.next_blitter_completion_cck(), Some(8));

    bus.advance_chipset(8);
    assert!(!bus.blitter.busy);
    assert_eq!(&bus.mem.chip_ram[0x24..0x26], &[0x33, 0x33]);
    // The replacement blit's completion raises the request.
    assert_ne!(bus.paula.intreq & INT_BLIT, 0);
}

#[test]
fn busy_blitter_dmacon_clear_gates_dma_without_finishing_pending_blit() {
    let mut bus = bus_with_pending_two_word_a_to_d_blit();

    assert!(!bus.custom_write(0x096, 2, DMACON_BLTEN as u64));

    assert!(bus.blitter.busy);
    assert_eq!(bus.agnus.dmacon & DMACON_BLTEN, 0);
    assert_eq!(bus.paula.intreq & INT_BLIT, 0);
    assert_eq!(bus.next_blitter_completion_cck(), None);
    assert_ne!(bus.custom_read(0x002, 2) as u16 & (1 << 14), 0);

    bus.advance_chipset(8);
    assert!(bus.blitter.busy);
    assert_eq!(&bus.mem.chip_ram[0x20..0x24], &[0, 0, 0, 0]);

    assert!(!bus.custom_write(0x096, 2, (0x8000 | DMACON_BLTEN) as u64));
    assert_eq!(bus.next_blitter_completion_cck(), Some(10));
    bus.advance_chipset(11);

    assert!(!bus.blitter.busy);
    assert_eq!(&bus.mem.chip_ram[0x20..0x24], &[0x11, 0x11, 0x22, 0x22]);
    assert_ne!(bus.paula.intreq & INT_BLIT, 0);
}

#[test]
fn blithog_clear_busy_blitter_yields_to_cpu_only_after_starvation() {
    // With BLTPRI=0 the blitter is "nice" but still holds the chip bus while
    // it has work: it does not hand the CPU a regular alternate slot. A
    // BLITWAIT-ing CPU is made to wait BLITTER_SLOWDOWN_CPU_MISS_LIMIT cycles
    // (the blitter advancing its scheduled slots) before the blitter yields
    // and the CPU gets its access. This matches real OCS giving a busy
    // blitter roughly 2:1 over the CPU.
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BLTEN;
    bus.agnus.hpos = 0x20;
    // ABC source-only blit: after the lead-in every pipeline phase needs
    // the bus, so this directly exercises the nice-blitter starvation
    // yield rather than the D-output pipeline bubble.
    bus.blitter.bltcon0 = 0x0E00;
    bus.blitter.bltcon1 = 0;
    bus.blitter.bltafwm = 0xFFFF;
    bus.blitter.bltalwm = 0xFFFF;
    bus.blitter.bltapt = 0x10;
    bus.blitter.bltbpt = 0x20;
    bus.blitter.bltcpt = 0x30;
    write_chip_word(&mut bus, 0x10, 0x1111);
    write_chip_word(&mut bus, 0x12, 0x2222);
    write_chip_word(&mut bus, 0x14, 0x3333);
    write_chip_word(&mut bus, 0x16, 0x4444);
    write_chip_word(&mut bus, 0x20, 0xAAAA);
    write_chip_word(&mut bus, 0x22, 0xBBBB);
    write_chip_word(&mut bus, 0x24, 0xCCCC);
    write_chip_word(&mut bus, 0x26, 0xDDDD);
    write_chip_word(&mut bus, 0x30, 0x5555);
    write_chip_word(&mut bus, 0x32, 0x6666);
    write_chip_word(&mut bus, 0x34, 0x7777);
    write_chip_word(&mut bus, 0x36, 0x8888);
    bus.blitter.start_scheduled((1 << 6) | 4, &bus.mem.chip_ram);
    // Walk the blit past its four internal lead-in cycles (those are
    // CPU-available) so its pending slot is an A-channel bus access.
    bus.advance_chipset(4);
    let initial_slots = bus.blitter.scheduled_slots_remaining();
    bus.set_cpu_bus_arbitration_enabled(true);
    bus.begin_cpu_slice();

    let dmaconr = bus.custom_read(0x002, 2) as u16;
    let (poll_cck, poll_tick) = bus.take_slice_bus_advance();
    // The CPU was starved for BLITTER_SLOWDOWN_CPU_MISS_LIMIT cycles, then
    // granted its access (one slot) plus the bus-free tail cck -- one access
    // takes limit + 2 color clocks.
    assert_eq!(poll_cck, u32::from(BLITTER_SLOWDOWN_CPU_MISS_LIMIT) + 2);
    assert_eq!(poll_tick.new_lines, 0);
    assert_ne!(dmaconr & (1 << 14), 0);
    // The blitter kept running through the CPU's wait (it did not yield its
    // regular slots), so it advanced its scheduled work.
    assert!(bus.blitter.scheduled_slots_remaining() < initial_slots);
    assert_eq!(bus.paula.intreq & INT_BLIT, 0);
    assert_eq!(
        bus.agnus.hpos,
        0x24 + u32::from(BLITTER_SLOWDOWN_CPU_MISS_LIMIT) + 2
    );
    // The granted slot was the CPU's; the trailing bus-free tail cck is
    // reclaimed by the still-busy blitter, so it is the last chip-bus owner.
    assert_eq!(bus.last_chip_bus_owner(), ChipBusOwner::Blitter);
}

#[test]
fn blithog_clear_bls_count_yields_blitter_priority_slot_to_cpu() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BLTEN;
    bus.agnus.hpos = 0x22;
    bus.blitter.bltcon0 = 0x09F0;
    bus.blitter.start_scheduled((1 << 6) | 1, &bus.mem.chip_ram);
    // Walk the blit past its four internal lead-in cycles (those are
    // CPU-available) so its pending slot is an A-channel bus access.
    bus.advance_chipset(4);

    assert_eq!(bus.scheduled_dma_owner(true), ChipBusOwner::Blitter);

    bus.blitter_slowdown_cpu_misses = BLITTER_SLOWDOWN_CPU_MISS_LIMIT - 1;
    assert_eq!(bus.scheduled_dma_owner(true), ChipBusOwner::Blitter);

    bus.blitter_slowdown_cpu_misses = BLITTER_SLOWDOWN_CPU_MISS_LIMIT;
    assert_eq!(bus.scheduled_dma_owner(true), ChipBusOwner::Idle);
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Blitter);
}

#[test]
fn normal_blitter_idle_slots_do_not_feed_bls_back_pressure() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BLTEN;
    bus.blitter.bltcon0 = 0x0100; // D-only clear.
    bus.blitter.bltdpt = 0x20;
    bus.blitter.start_scheduled((1 << 6) | 2, &bus.mem.chip_ram);

    // Walk to the first normal-mode A phase. It is a disabled-channel idle
    // slot, so a CPU miss caused by refresh/fixed DMA must not make the nice
    // blitter yield earlier.
    for _ in 0..4 {
        assert!(!bus.blitter.tick_scheduled_slot(&mut bus.mem.chip_ram));
    }
    bus.note_cpu_missed_chip_bus_cycle();
    assert_eq!(bus.blitter_slowdown_cpu_misses, 0);

    // The first D phase is also the empty destination pipeline bubble.
    assert!(!bus.blitter.tick_scheduled_slot(&mut bus.mem.chip_ram)); // A0.
    bus.note_cpu_missed_chip_bus_cycle();
    assert_eq!(bus.blitter_slowdown_cpu_misses, 0);

    // Once a real destination write is pending, misses do feed BLS pressure.
    assert!(!bus.blitter.tick_scheduled_slot(&mut bus.mem.chip_ram)); // D0 bubble.
    assert!(!bus.blitter.tick_scheduled_slot(&mut bus.mem.chip_ram)); // A1.
    bus.note_cpu_missed_chip_bus_cycle();
    assert_eq!(bus.blitter_slowdown_cpu_misses, 1);
}

#[test]
fn fill_idle_slots_feed_bls_back_pressure() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BLTEN;
    bus.blitter.bltcon0 = 0x09F0; // A -> D.
    bus.blitter.bltcon1 = 0x0012; // DESC + exclusive fill.
    bus.blitter.bltafwm = 0xFFFF;
    bus.blitter.bltalwm = 0xFFFF;
    bus.blitter.bltapt = 0x10;
    bus.blitter.bltdpt = 0x20;
    bus.blitter.start_scheduled((1 << 6) | 1, &bus.mem.chip_ram);

    for _ in 0..6 {
        assert!(!bus.blitter.tick_scheduled_slot(&mut bus.mem.chip_ram));
    }
    bus.blitter_slowdown_cpu_misses = 0;
    bus.note_cpu_missed_chip_bus_cycle();
    assert_eq!(bus.blitter_slowdown_cpu_misses, 1);
}

#[test]
fn bltcon0l_updates_only_minterm_byte() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.blitter.bltcon0 = 0xABCD;

    assert!(!bus.custom_write(0x05A, 2, 0x0012));

    assert_eq!(bus.blitter.bltcon0, 0xAB12);
}

#[test]
fn ocs_ignores_ecs_blitter_extension_registers() {
    let mut bus = empty_bus();
    bus.blitter.bltcon0 = 0xABCD;

    assert!(!bus.custom_write(0x05A, 2, 0x0012));
    assert!(!bus.custom_write(0x05C, 2, 0x1234));
    assert!(!bus.custom_write(0x05E, 2, 0x0001));

    assert_eq!(bus.blitter.bltcon0, 0xABCD);
    assert_eq!(bus.blitter.bltsizv, 0);
    assert!(!bus.blitter.busy);
    assert_eq!(bus.custom_read(0x05A, 2), 0);
    assert_eq!(bus.custom_read(0x05C, 2), 0);
}

#[test]
fn ecs_bltcon1_doff_suppresses_destination_writes_but_advances_pointer() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BLTEN;
    bus.agnus.hpos = 0x20;
    bus.blitter.bltcon0 = 0x09F0;
    bus.blitter.bltafwm = 0xFFFF;
    bus.blitter.bltalwm = 0xFFFF;
    bus.blitter.bltapt = 0x10;
    bus.blitter.bltdpt = 0x20;
    write_chip_word(&mut bus, 0x10, 0x1234);
    write_chip_word(&mut bus, 0x20, 0xAAAA);

    assert!(!bus.custom_write(0x042, 2, BLTCON1_DOFF as u64));
    assert!(!bus.custom_write(0x058, 2, ((1 << 6) | 1) as u64));

    bus.advance_chipset(5);
    assert!(bus.blitter.busy);
    assert_eq!(&bus.mem.chip_ram[0x20..0x22], &[0xAA, 0xAA]);
    assert_eq!(bus.blitter.bltdpt, 0x20);

    bus.advance_chipset(2);
    assert!(bus.blitter.busy);
    assert!(!bus.blitter.bzero);
    assert_eq!(&bus.mem.chip_ram[0x20..0x22], &[0xAA, 0xAA]);
    assert_eq!(bus.blitter.bltdpt, 0x20);

    bus.advance_chipset(2);

    assert!(!bus.blitter.busy);
    assert_eq!(&bus.mem.chip_ram[0x20..0x22], &[0xAA, 0xAA]);
    assert_eq!(bus.blitter.bltdpt, 0x22);
    assert!(!bus.blitter.bzero);
}

#[test]
fn ocs_masks_ecs_bltcon1_doff_bit() {
    let mut bus = empty_bus();

    assert!(!bus.custom_write(0x042, 2, BLTCON1_DOFF as u64));
    assert_eq!(bus.blitter.bltcon1 & BLTCON1_DOFF, 0);

    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    assert!(!bus.custom_write(0x042, 2, BLTCON1_DOFF as u64));
    assert_eq!(bus.blitter.bltcon1 & BLTCON1_DOFF, BLTCON1_DOFF);
}

#[test]
fn ecs_bltsizv_bltsizh_start_extended_blit() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    bus.paula.intena = INT_BLIT;
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BLTEN;
    bus.agnus.hpos = 0x20;
    bus.blitter.bltcon0 = 0x09F0;
    bus.blitter.bltafwm = 0xFFFF;
    bus.blitter.bltalwm = 0xFFFF;
    bus.blitter.bltapt = 0x10;
    bus.blitter.bltdpt = 0x20;
    write_chip_word(&mut bus, 0x10, 0x1234);
    write_chip_word(&mut bus, 0x12, 0x5678);

    assert!(!bus.custom_write(0x05C, 2, 0x0002));
    assert!(!bus.blitter.busy);
    assert!(!bus.custom_write(0x05E, 2, 0x0001));

    assert!(bus.blitter.busy);
    bus.advance_chipset(16);
    assert!(!bus.blitter.busy);
    assert_ne!(bus.paula.intreq & INT_BLIT, 0);
    assert_eq!(&bus.mem.chip_ram[0x20..0x24], &[0x12, 0x34, 0x56, 0x78]);
}

#[test]
fn cpu_chip_access_waits_through_refresh_slots() {
    let mut bus = empty_bus();
    bus.set_cpu_bus_arbitration_enabled(true);
    // Start on a refresh slot (hardware model: refresh occupies 0x001,
    // 0x003, 0x005, and one line-end slot). The CPU misses that slot and
    // is granted the following free color clock.
    bus.agnus.hpos = 0x003;

    bus.grant_cpu_bus_access(2, CpuBusAccessKind::Read);

    // The CPU waits one cck through the refresh slot (0x003), is granted the
    // following free color clock (0x004 = CPU slot), then spends one bus-free
    // "tail" cck (0x005): wait + slot + tail = three color clocks.
    let (cck, tick) = bus.take_slice_bus_advance();
    assert_eq!(cck, 3);
    assert_eq!(tick.new_lines, 0);
    assert_eq!(bus.agnus.hpos, 0x006);
    assert_eq!(bus.last_chip_bus_owner(), ChipBusOwner::Refresh);
}

#[test]
fn cpu_chip_access_uses_two_color_clocks_slot_plus_bus_free_tail() {
    let mut bus = empty_bus();
    bus.agnus.hpos = 0x21;
    bus.set_cpu_bus_arbitration_enabled(true);

    bus.grant_cpu_bus_access(2, CpuBusAccessKind::Read);

    // A single-word CPU chip access now costs two color clocks: one granted
    // chip-bus slot (the CPU owns hpos 0x21) plus one bus-free "tail" cck
    // (hpos 0x22), modelling the 68000's 4-clock bus cycle. The tail cck is
    // not a CPU bus slot, so the bus is free for whatever the chipset gives
    // (here Idle, since no DMA channel is active).
    let (cck, tick) = bus.take_slice_bus_advance();
    assert_eq!(cck, 2);
    assert_eq!(tick.new_lines, 0);
    assert_eq!(bus.agnus.hpos, 0x23);
    assert_eq!(bus.last_chip_bus_owner(), ChipBusOwner::Idle);
}

#[test]
fn aga_68020_chip_reads_wait_for_data_but_writes_are_posted() {
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    bus.set_cpu_clocks_per_cck(4);
    bus.set_cpu_short_bus_cycle(true);
    bus.set_cpu_bus_arbitration_enabled(true);

    bus.agnus.hpos = 0x20;
    bus.grant_cpu_bus_access_at(Some(0x0002_0000), 2, CpuBusAccessKind::Read);
    let (chip_read_cck, _) = bus.take_slice_bus_advance();
    assert_eq!(chip_read_cck, 2);
    assert_eq!(bus.last_chip_bus_owner(), ChipBusOwner::Idle);

    // The write posts: no beam advance at issue, and the transfer's 3 clocks
    // are credited back against the instruction's CPU charge.
    bus.grant_cpu_bus_access_at(Some(0x0002_0000), 2, CpuBusAccessKind::Write);
    let (chip_write_cck, _) = bus.take_slice_bus_advance();
    assert_eq!(chip_write_cck, 0);
    assert_eq!(bus.take_cpu_bus_overlap_clocks(), 3);

    // The next access first retires the posted write into a free slot, then
    // waits out the port's 2-cck turnaround before its own grant: drain +
    // turnaround wait + grant + fetch data return.
    bus.grant_cpu_bus_access_at(Some(0x0002_0004), 2, CpuBusAccessKind::Fetch);
    let (chip_fetch_cck, _) = bus.take_slice_bus_advance();
    assert_eq!(chip_fetch_cck, 4);
    assert_eq!(bus.last_chip_bus_owner(), ChipBusOwner::Idle);

    // A custom-register read crosses to Agnus/Denise/Paula and back over the
    // 16-bit chipset bus, one colour clock beyond the chip-RAM data return.
    let _ = bus.custom_read(0x002, 2);
    let (custom_read_cck, _) = bus.take_slice_bus_advance();
    assert_eq!(custom_read_cck, 3);
    assert_eq!(bus.last_chip_bus_owner(), ChipBusOwner::Idle);
}

/// An AGA 68020 bus at the A1200's 4 CPU clocks per colour clock, with the
/// per-instruction charging that keeps the shared chip clock phase current.
fn phase_synced_020_bus() -> Bus {
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    bus.set_cpu_clocks_per_cck(4);
    bus.set_cpu_short_bus_cycle(true);
    bus.set_cpu_bus_arbitration_enabled(true);
    bus.set_cpu_access_phase_sync(true);
    bus.agnus.hpos = 0x20;
    let _ = bus.take_slice_bus_advance();
    bus
}

#[test]
fn aga_68020_read_return_sync_clock_accumulates_to_a_slot() {
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    bus.set_cpu_clocks_per_cck(4);
    bus.set_cpu_short_bus_cycle(true);
    bus.set_cpu_bus_arbitration_enabled(true);

    // Without the shared chip clock phase (the JIT slice, and any caller that
    // has not opted in) each read bills grant + data return (2 cck) plus one
    // CPU clock of domain-crossing synchronizer, so at 4 clocks per cck the
    // fourth read's accumulated clock crosses a slot boundary.
    bus.agnus.hpos = 0x20;
    let mut total = 0;
    for _ in 0..4 {
        bus.grant_cpu_bus_access_at(Some(0x0002_0000), 2, CpuBusAccessKind::Read);
        let (cck, _) = bus.take_slice_bus_advance();
        total += cck;
    }
    assert_eq!(total, 9);
}

/// A 68020 chip read cannot begin part way through a colour clock: it stalls
/// out the remainder first. Real-hardware basis in `timing-test/rdprobe.asm`.
#[test]
fn aga_68020_chip_read_synchronises_to_the_colour_clock() {
    let mut bus = phase_synced_020_bus();

    // On a boundary the read costs grant + data return and leaves the CPU two
    // clocks into the following colour clock.
    bus.grant_cpu_bus_access_at(Some(0x0002_0000), 2, CpuBusAccessKind::Read);
    assert_eq!(bus.take_slice_bus_advance().0, 2);
    assert_eq!(bus.cpu_chip_clock_phase(), 2);

    // Mid-colour-clock the next read stalls out the remainder first.
    bus.grant_cpu_bus_access_at(Some(0x0002_0000), 2, CpuBusAccessKind::Read);
    assert_eq!(bus.take_slice_bus_advance().0, 3);
    assert_eq!(bus.cpu_chip_clock_phase(), 2);
}

/// A chip-read loop runs a whole number of colour clocks per iteration, and
/// the same loop costs the same whatever phase it was entered with. This is
/// the property the two probe disks disagreed on before the CPU and the chip
/// bus shared one timeline (`rdprobe` row 0 against `timing-test` row 2).
#[test]
fn aga_68020_chip_read_loop_period_is_phase_independent() {
    // `move.w (a0),d0` = 6 clocks, taken `dbra` = 6 at pc%4==2 (rdprobe row 0)
    // and 7 at pc%4==0 (row 1).
    for (branch_clocks, want_cck) in [(6u32, 4u32), (7, 5)] {
        let mut periods = Vec::new();
        for entry_phase in 0..4u32 {
            let mut bus = phase_synced_020_bus();
            // Enter the loop at each possible sub-colour-clock phase.
            bus.charge_cpu_clocks_to_cck(entry_phase);
            let _ = bus.take_slice_bus_advance();
            // Two warm-up iterations, then measure the next four.
            let mut measured = 0;
            for iteration in 0..6 {
                bus.grant_cpu_bus_access_at(Some(0x0002_0000), 2, CpuBusAccessKind::Read);
                let overlap = bus.take_cpu_bus_overlap_clocks();
                bus.charge_cpu_clocks_to_cck(6u32.saturating_sub(overlap));
                bus.charge_cpu_clocks_to_cck(branch_clocks);
                let (cck, _) = bus.take_slice_bus_advance();
                if iteration >= 2 {
                    measured += cck;
                }
            }
            periods.push(measured);
        }
        assert!(
            periods.iter().all(|p| *p == want_cck * 4),
            "branch {branch_clocks} clocks: want {} cck per iteration from every entry phase, got {periods:?}",
            want_cck
        );
    }
}

/// Execution that never touches the chip bus is not synchronised, so its loop
/// period stays fractional (`rdprobe` rows 4 and 5).
#[test]
fn aga_68020_execution_only_loop_is_not_quantised() {
    let mut bus = phase_synced_020_bus();
    // 2-clock register move plus a 7-clock branch = 9 clocks = 2.25 cck.
    for _ in 0..4 {
        bus.charge_cpu_clocks_to_cck(2);
        bus.charge_cpu_clocks_to_cck(7);
    }
    assert_eq!(bus.take_slice_bus_advance().0, 9);
}

/// The phase is machine state, not slice state: it must survive the
/// arbitration toggle that brackets every CPU slice.
#[test]
fn cpu_chip_clock_phase_survives_a_slice_boundary() {
    let mut bus = phase_synced_020_bus();
    bus.charge_cpu_clocks_to_cck(2);
    assert_eq!(bus.cpu_chip_clock_phase(), 2);
    bus.set_cpu_bus_arbitration_enabled(false);
    bus.set_cpu_bus_arbitration_enabled(true);
    assert_eq!(bus.cpu_chip_clock_phase(), 2);
}

#[test]
fn aga_68020_posted_write_drains_during_internal_advance() {
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    bus.set_cpu_clocks_per_cck(4);
    bus.set_cpu_short_bus_cycle(true);
    bus.set_cpu_bus_arbitration_enabled(true);

    bus.agnus.hpos = 0x20;
    bus.grant_cpu_bus_access_at(Some(0x0002_0000), 2, CpuBusAccessKind::Write);
    let (issue_cck, _) = bus.take_slice_bus_advance();
    assert_eq!(issue_cck, 0);

    // Execution time passing on the CPU retires the write into a free slot
    // behind the execution unit's back...
    bus.advance_cpu_internal_cycles(2);
    let _ = bus.take_slice_bus_advance();

    // ...so a following write posts again without stalling.
    bus.grant_cpu_bus_access_at(Some(0x0002_0000), 2, CpuBusAccessKind::Write);
    let (second_issue_cck, _) = bus.take_slice_bus_advance();
    assert_eq!(second_issue_cck, 0);
}

#[test]
fn aga_68020_back_to_back_chip_writes_pace_to_port_cadence() {
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    bus.set_cpu_clocks_per_cck(4);
    bus.set_cpu_short_bus_cycle(true);
    bus.set_cpu_bus_arbitration_enabled(true);

    // With no execution time between them, each write after the first stalls
    // until the previous one retires, settling to the port's one-slot-every-
    // other-cck cadence (timing-test rows 3/10: ~8 CPU clocks per write on a
    // real A1200): 0 + 1 + 2 + 2.
    bus.agnus.hpos = 0x20;
    let mut total = 0;
    for _ in 0..4 {
        bus.grant_cpu_bus_access_at(Some(0x0002_0000), 2, CpuBusAccessKind::Write);
        let (cck, _) = bus.take_slice_bus_advance();
        total += cck;
    }
    assert_eq!(total, 5);
}

#[test]
fn ecs_68020_chip_and_custom_reads_wait_for_16_bit_data_return() {
    let mut bus = empty_bus();
    bus.set_chipset_revisions(AgnusRevision::Ecs8375, DeniseRevision::Ecs8373);
    bus.set_cpu_clocks_per_cck(4);
    bus.set_cpu_short_bus_cycle(true);
    bus.set_cpu_bus_arbitration_enabled(true);

    bus.agnus.hpos = 0x20;
    bus.grant_cpu_bus_access_at(Some(0x0002_0000), 2, CpuBusAccessKind::Read);
    let (chip_read_cck, _) = bus.take_slice_bus_advance();
    assert_eq!(chip_read_cck, 2);
    assert_eq!(bus.last_chip_bus_owner(), ChipBusOwner::Idle);

    // Writes post on the 16-bit chip bus too: the ECS 020's bus unit is the
    // same silicon, only the data path is narrower.
    bus.grant_cpu_bus_access_at(Some(0x0002_0000), 2, CpuBusAccessKind::Write);
    let (chip_write_cck, _) = bus.take_slice_bus_advance();
    assert_eq!(chip_write_cck, 0);
    assert_eq!(bus.take_cpu_bus_overlap_clocks(), 3);

    // The custom read first retires the posted write (drain + the port's
    // 2-cck turnaround), then pays grant + data return + register crossing.
    let _ = bus.custom_read(0x002, 2);
    let (custom_read_cck, _) = bus.take_slice_bus_advance();
    assert_eq!(custom_read_cck, 5);
    assert_eq!(bus.last_chip_bus_owner(), ChipBusOwner::Idle);
}

#[test]
fn running_copper_yields_alternate_chip_bus_slot_to_waiting_cpu() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    // A list of back-to-back MOVEs keeps the Copper in the running state
    // so it competes for every contended slot.
    write_chip_word(&mut bus, 0x100, 0x0180); // MOVE COLOR00, $0000
    write_chip_word(&mut bus, 0x102, 0x0000);
    write_chip_word(&mut bus, 0x104, 0x0180);
    write_chip_word(&mut bus, 0x106, 0x0000);
    bus.copper.jump(0x100);
    assert!(bus.copper.is_running());
    bus.set_cpu_bus_arbitration_enabled(true);

    bus.grant_cpu_bus_access(2, CpuBusAccessKind::Read);

    // The Copper takes one slot, then yields the next color clock to the
    // waiting CPU instead of monopolising the bus. The single-word access
    // then costs a third color clock for its bus-free "tail": Copper +
    // CPU slot + tail = three color clocks, not a full Copper run. This
    // models the OCS Copper's 4-color-clock MOVE cadence, which leaves the
    // alternate cycles free for the CPU. The bus-free tail cck is not a CPU
    // slot, so the still-running Copper reclaims the bus on it.
    let (cck, _) = bus.take_slice_bus_advance();
    assert_eq!(cck, 3);
    assert_eq!(bus.last_chip_bus_owner(), ChipBusOwner::Copper);
}

// Audit: drive the CPU across one display line under 6-plane lores bitplane
// DMA and print the per-access contention, to localise the fractional
// over-charge the timing-test ROM measured (Copperline ~0.25 cck/line/plane high
// vs FS-UAE/vAmiga). Run with: cargo test audit_six_plane_cpu_contention -- --nocapture
#[test]
fn audit_six_plane_cpu_contention() {
    let mut bus = empty_bus();
    bus.set_cpu_bus_arbitration_enabled(true);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.agnus.vpos = RENDER_VISIBLE_START_VPOS + 20;
    bus.agnus.hpos = 0;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2CC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x00D0;
    bus.denise.bplcon0 = 0x6000; // 6 bitplanes, lores
    for i in 0..6 {
        bus.denise.bplpt[i] = 0x0002_0000;
        bus.display_dma_bplpt[i] = 0x0002_0000;
    }
    let line_cck = bus.agnus.current_line_cck();
    let mut total = 0u32;
    let mut accesses = 0u32;
    eprintln!("=== 6-plane lores: CPU access cost by start hpos (DDF $38..$D0) ===");
    while bus.agnus.hpos + 4 < line_cck {
        let h0 = bus.agnus.hpos;
        bus.grant_cpu_bus_access(2, CpuBusAccessKind::Write);
        let owner = bus.last_chip_bus_owner();
        let (cck, _) = bus.take_slice_bus_advance();
        total += cck;
        accesses += 1;
        let in_ddf = (0x38..=0xD7).contains(&h0);
        if in_ddf {
            eprintln!("  h={h0:#04X} cost={cck} last_owner={owner:?}");
        }
    }
    eprintln!("total cck for {accesses} accesses across line = {total}");
}

#[test]
fn copper_move_spends_four_color_clocks_leaving_alternate_cycles_free() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x30;
    write_chip_word(&mut bus, 0x100, 0x0180); // MOVE COLOR00, $0ABC
    write_chip_word(&mut bus, 0x102, 0x0ABC);
    write_chip_word(&mut bus, 0x104, 0xFFFF);
    write_chip_word(&mut bus, 0x106, 0xFFFE);
    bus.copper.jump(0x100);

    // A MOVE fetches its two words on alternate color clocks (Copper, free,
    // Copper, free), spanning four color clocks, with the register write on
    // the second fetch and the idle halves left for the blitter/CPU.
    bus.advance_chipset(1);
    assert_eq!(bus.last_chip_bus_owner(), ChipBusOwner::Copper);
    assert_eq!(bus.denise.palette[0], 0);

    bus.advance_chipset(1);
    assert_eq!(bus.last_chip_bus_owner(), ChipBusOwner::Idle);
    assert_eq!(bus.denise.palette[0], 0);

    bus.advance_chipset(1);
    assert_eq!(bus.last_chip_bus_owner(), ChipBusOwner::Copper);
    assert_eq!(bus.denise.palette[0], 0x0ABC);

    bus.advance_chipset(1);
    assert_eq!(bus.last_chip_bus_owner(), ChipBusOwner::Idle);
}

#[test]
fn blitter_completion_prediction_matches_actual_with_running_copper() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    // A long run of back-to-back MOVEs keeps the Copper contending for the
    // bus the whole time the blitter is running.
    for i in 0..8usize {
        write_chip_word(&mut bus, cop1 + i * 4, 0x0180);
        write_chip_word(&mut bus, cop1 + i * 4 + 2, 0x0000);
    }
    write_chip_word(&mut bus, cop1 + 32, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 34, 0xFFFE);

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_COPEN | DMACON_BLTEN;
    bus.agnus.hpos = 0x20;
    bus.copper.jump(cop1 as u32);
    bus.blitter.bltcon0 = 0;
    bus.blitter.start_scheduled((1 << 6) | 3, &bus.mem.chip_ram);

    // The predicted completion (which simulates the Copper's cadence on a
    // clone via the shared step primitive) must match when the blitter
    // actually finishes once executed, or wake-up scheduling would drift.
    let predicted = bus.next_blitter_completion_cck().expect("blitter deadline");
    assert!(predicted > 1);
    bus.advance_chipset(predicted - 1);
    assert!(bus.blitter.busy);
    bus.advance_chipset(1);
    assert!(!bus.blitter.busy);
}

#[test]
fn fixed_agnus_dma_slot_bands_drive_owner_selection() {
    let mut bus = empty_bus();

    for hpos in [0x001, 0x003, 0x005, 0x0E2, 0x0E3] {
        assert!(
            Bus::refresh_slot_active_at(hpos),
            "hpos {hpos:#05X} should be a refresh slot"
        );
    }
    for hpos in [0x000, 0x007, 0x0E1, 0x0E4] {
        assert!(
            !Bus::refresh_slot_active_at(hpos),
            "hpos {hpos:#05X} should not be a refresh slot"
        );
    }
    assert_eq!(Bus::audio_dma_channel_at(0x00D), Some(0));
    assert_eq!(Bus::audio_dma_channel_at(0x00F), Some(1));
    assert_eq!(Bus::audio_dma_channel_at(0x011), Some(2));
    assert_eq!(Bus::audio_dma_channel_at(0x013), Some(3));
    assert_eq!(Bus::audio_dma_channel_at(0x015), None);

    bus.agnus.dmacon = DMACON_DMAEN;
    bus.agnus.hpos = 0x005;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Refresh);
    bus.agnus.hpos = 0x007;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);

    bus.agnus.dmacon = DMACON_DMAEN | 0x0001;
    bus.paula.apply_audio_dmacon_edges(0, bus.agnus.dmacon);
    bus.paula.transfer_audio_dma_requests();
    bus.agnus.hpos = 0x00D;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Audio);
    bus.agnus.hpos = 0x016;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    // Drain the channel's start-up request chain so the latched-request
    // slot reservation (independent of DMACON) does not leak into the
    // sprite checks below: service the two start-up fetches, then let the
    // machine idle with the channel bit off.
    let dmacon = bus.agnus.dmacon;
    let _ = bus.paula.grant_audio_dma(0, 0, dmacon);
    bus.agnus.dmacon = DMACON_DMAEN;
    bus.paula.apply_audio_dmacon_edges(dmacon, bus.agnus.dmacon);
    bus.paula.transfer_audio_dma_requests();
    let _ = bus.paula.grant_audio_dma(0, 0, bus.agnus.dmacon);
    bus.paula.transfer_audio_dma_requests();

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_SPREN;
    // A sprite's slot is reserved only while that sprite is actually
    // fetching (its DMA flip-flop is set, or the line is its vstop line
    // fetching POS/CTL); a parked sprite frees it. Sprite N owns the odd
    // color clocks $15+4N and $17+4N (hardware slot chart).
    bus.agnus.vpos = 0x2C;
    bus.display_dma_sprite_state[0].vstop = 0x60;
    bus.agnus.hpos = 0x015;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    bus.display_dma_sprite_state[0].dma_enabled = true;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Sprite);
    bus.agnus.hpos = 0x017;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Sprite);
    // The even color clock inside the band stays free for the Copper/CPU,
    // and another sprite's slot stays free while that sprite is parked.
    bus.agnus.hpos = 0x016;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    bus.agnus.hpos = 0x019;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    bus.agnus.hpos = 0x035;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    bus.display_dma_sprite_state[0].dma_enabled = false;
    bus.agnus.vpos = 0;

    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x00D0;
    bus.agnus.vpos = 0x40; // inside the default vertical display window
    bus.agnus.hpos = 0x036;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    bus.agnus.hpos = 0x038;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    bus.agnus.hpos = 0x03A;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    bus.agnus.hpos = 0x03E;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    bus.agnus.hpos = 0x03F;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Bitplane);
    bus.agnus.hpos = 0x040;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    bus.agnus.hpos = 0x0E4;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
}

#[test]
fn bitplane_dma_ownership_gated_to_vertical_display_window() {
    // Bitplane DMA only runs inside the vertical display window. The same
    // fetch hpos that the arbiter hands to the bitplane on a display line
    // must be left free (Idle, available to a busy blitter) on a
    // top-border / vertical-blank line. Guards against over-reserving
    // display DMA outside the vertical window. See docs/internals/timing.md.
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x00D0;
    // Explicit vertical display window: lines 0x2C..0xF4.
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0xF4C1;
    bus.agnus.hpos = 0x03F;

    bus.agnus.vpos = 0x40; // inside the window -> bitplane owns the slot
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Bitplane);

    bus.agnus.vpos = 0x10; // top border, before vstart -> slot is free
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);

    bus.agnus.vpos = 0xF8; // past vstop -> slot is free
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
}

#[test]
fn bitplane_dma_ownership_clips_ddfstart_to_hard_fetch_window() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.ddfstrt = 0x0010;
    bus.denise.ddfstop = 0x0018;
    bus.agnus.vpos = 0x40; // inside the default vertical display window

    // Flop model: the DDFSTRT comparator at $10 fires while the hardware
    // start window (SHW, $18) is still down; OCS starts no run at all.
    bus.agnus.hpos = 0x017;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    bus.agnus.hpos = 0x01F;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    bus.agnus.hpos = 0x020;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
}

#[test]
fn bitplane_dma_ownership_clips_ddfstop_to_hard_fetch_window() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.ddfstrt = 0x00D8;
    bus.denise.ddfstop = 0x00E0;
    bus.agnus.vpos = 0x40; // inside the default vertical display window

    bus.agnus.hpos = 0x0DF;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Bitplane);
    bus.agnus.hpos = 0x0E0;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
}

#[test]
fn bitplane_dma_ownership_matches_revision_for_equal_ddf_window() {
    let mut ocs = empty_bus();
    ocs.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    ocs.denise.bplcon0 = 0x1000;
    ocs.denise.ddfstrt = 0x0038;
    ocs.denise.ddfstop = 0x0038;
    ocs.agnus.vpos = 0x40; // inside the default vertical display window

    ocs.agnus.hpos = 0x047;
    assert_eq!(ocs.scheduled_dma_owner(false), ChipBusOwner::Bitplane);
    ocs.agnus.hpos = 0x0DF;
    assert_eq!(ocs.scheduled_dma_owner(false), ChipBusOwner::Bitplane);
    ocs.agnus.hpos = 0x0E0;
    assert_eq!(ocs.scheduled_dma_owner(false), ChipBusOwner::Idle);
    // Flop model: ECS behaves like OCS here - the merged equal-value
    // strobe starts a run with no stop pending, which only the
    // hardware-stop drain ends.

    let mut ecs = empty_bus();
    ecs.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    ecs.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    ecs.denise.bplcon0 = 0x1000;
    ecs.denise.ddfstrt = 0x0038;
    ecs.denise.ddfstop = 0x0038;
    ecs.agnus.vpos = 0x40; // inside the default vertical display window

    ecs.agnus.hpos = 0x047;
    assert_eq!(ecs.scheduled_dma_owner(false), ChipBusOwner::Bitplane);
    ecs.agnus.hpos = 0x0DF;
    assert_eq!(ecs.scheduled_dma_owner(false), ChipBusOwner::Bitplane);
    ecs.agnus.hpos = 0x0E0;
    assert_eq!(ecs.scheduled_dma_owner(false), ChipBusOwner::Idle);
}

#[test]
fn hires_bitplane_dma_ownership_uses_four_cck_fetch_cadence() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.bplcon0 = 0x9000;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0040;
    bus.agnus.vpos = 0x40; // inside the default vertical display window

    // Flop model (vAmiga fetch tables): a hires unit carries H4 H2 H3 H1
    // twice over its 8 colour clocks, so a single-plane display only
    // reserves the H1 slots at unit offsets 3 and 7; the other clocks are
    // free for the copper/CPU.
    bus.agnus.hpos = 0x038;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    bus.agnus.hpos = 0x03B;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Bitplane);
    bus.agnus.hpos = 0x03C;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Idle);
    bus.agnus.hpos = 0x03F;
    assert_eq!(bus.scheduled_dma_owner(false), ChipBusOwner::Bitplane);
}

#[test]
fn bltpri_stalls_cpu_chip_access_through_blitter_access_cycles() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BLTEN | DMACON_BLTPRI;
    bus.agnus.hpos = 0x20;
    bus.blitter.bltcon0 = 0x09F0;
    bus.blitter.bltafwm = 0xFFFF;
    bus.blitter.bltalwm = 0xFFFF;
    bus.blitter.bltapt = 0x10;
    bus.blitter.bltdpt = 0x20;
    bus.mem.chip_ram[0x10] = 0x12;
    bus.mem.chip_ram[0x11] = 0x34;
    bus.blitter.start_scheduled((1 << 6) | 1, &bus.mem.chip_ram);
    // Walk the blit past its four internal lead-in cycles so its pending
    // slot is the A-channel access.
    bus.advance_chipset(4);
    bus.set_cpu_bus_arbitration_enabled(true);

    bus.grant_cpu_bus_access(2, CpuBusAccessKind::Read);

    // With BLTPRI set, BLS fences the CPU through the D pipeline's warm-up:
    // the idle first-D bubble after the A fetch is bus-free, but the
    // sequencer's bus request is still asserted (its first fetches are
    // queued back-to-back), so the CPU is denied it. The first cycle the
    // CPU wins is the internal E flush, where BBUSY has already dropped;
    // its granted slot and bus-free tail ride E/F while the blitter's
    // terminal F cycle writes the queued word: 4 color clocks in all.
    // Software depends on the closed bubble: MFM-decode trackloaders
    // (e.g. Jim Power's) restore the word below a decode blit's destination
    // right after BLTSIZE, relying on the startup and first-D holes staying
    // shut so the restore's prefetches queue behind the blit instead.
    let (cck, _) = bus.take_slice_bus_advance();
    assert_eq!(cck, 4);
    assert!(!bus.blitter.busy);
    assert_eq!(&bus.mem.chip_ram[0x20..0x22], &[0x12, 0x34]);
    assert_ne!(bus.paula.intreq & INT_BLIT, 0);
}

#[test]
fn blithog_set_blocks_cpu_slowdown_back_pressure_until_blitter_finishes() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BLTEN | DMACON_BLTPRI;
    bus.agnus.hpos = 0x20;
    // Use an ABC source-only blit so there are no idle destination bubbles
    // before completion; BLTPRI should block the nice-blitter starvation
    // yield for the whole remaining source cadence.
    bus.blitter.bltcon0 = 0x0E00;
    bus.blitter.bltafwm = 0xFFFF;
    bus.blitter.bltalwm = 0xFFFF;
    bus.blitter.bltapt = 0x10;
    bus.blitter.bltbpt = 0x20;
    bus.blitter.bltcpt = 0x30;
    write_chip_word(&mut bus, 0x10, 0x1111);
    write_chip_word(&mut bus, 0x12, 0x2222);
    write_chip_word(&mut bus, 0x14, 0x3333);
    write_chip_word(&mut bus, 0x16, 0x4444);
    write_chip_word(&mut bus, 0x20, 0xAAAA);
    write_chip_word(&mut bus, 0x22, 0xBBBB);
    write_chip_word(&mut bus, 0x24, 0xCCCC);
    write_chip_word(&mut bus, 0x26, 0xDDDD);
    write_chip_word(&mut bus, 0x30, 0x5555);
    write_chip_word(&mut bus, 0x32, 0x6666);
    write_chip_word(&mut bus, 0x34, 0x7777);
    write_chip_word(&mut bus, 0x36, 0x8888);
    bus.blitter.start_scheduled((1 << 6) | 4, &bus.mem.chip_ram);
    // Walk the blit past its four internal lead-in cycles so its pending
    // slot is the first A-channel access.
    bus.advance_chipset(4);
    bus.set_cpu_bus_arbitration_enabled(true);

    bus.grant_cpu_bus_access(2, CpuBusAccessKind::Read);

    // With BLTPRI set the CPU gets no starvation yield: it waits through
    // all twelve A/B/C accesses of the four words (a source-only blit has
    // no bus-free micro-cycles to release the request line), then spends
    // its granted slot plus the bus-free tail cck, costing 14 color
    // clocks. The terminal (internal) E/F cycles ride the CPU's
    // granted/tail clocks -- BBUSY has already dropped -- so the engine
    // finishes within the same span and the interrupt is asserted off the
    // terminal cycle.
    let (cck, _) = bus.take_slice_bus_advance();
    assert_eq!(cck, 14);
    assert!(!bus.blitter.busy);
    assert_ne!(bus.paula.intreq & INT_BLIT, 0);
}

#[test]
fn bltpri_line_blit_bresenham_cycles_stay_cpu_available_after_warmup() {
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BLTEN | DMACON_BLTPRI;
    bus.agnus.hpos = 0x20;
    // Canonical 4-pixel line draw: USEA|USEC|USED, octant 0. The per-pixel
    // cadence is [L1 -, L2 C-fetch, L3 -, L4 D-write]: two bus accesses and
    // two bus-free Bresenham cycles.
    bus.blitter.bltcon0 = BLTCON0_USE_A | BLTCON0_USE_C | BLTCON0_USE_D | 0x00CA;
    bus.blitter.bltcon1 = BLTCON1_LINE;
    bus.blitter.bltafwm = 0xFFFF;
    bus.blitter.bltalwm = 0xFFFF;
    bus.blitter.bltapt = 0;
    bus.blitter.bltcpt = 0x1000;
    bus.blitter.bltdpt = 0x1000;
    bus.blitter.start_scheduled((4 << 6) | 2, &bus.mem.chip_ram);

    // The startup ladder holds the BLS fence: with BLTPRI set the CPU is
    // denied the register-commit/BLT_STRT/Init cycles even though they do
    // not access the bus.
    let mut warmup_cycles = 0;
    while bus.blitter.bltpri_warmup_fences_cpu() {
        assert_eq!(
            bus.scheduled_dma_owner(true),
            ChipBusOwner::Blitter,
            "warm-up cycle must fence the CPU under BLTPRI"
        );
        bus.advance_chipset(1);
        warmup_cycles += 1;
        assert!(warmup_cycles < 8, "line warm-up should end with the ladder");
    }

    // Body: line mode's internal Bresenham cycles (L1/L3) release the bus
    // request, so the CPU may use them even under BLTPRI -- line-heavy demo
    // main loops (Rampage's vector parts) rely on that CPU time. Bus cycles
    // (C fetch, D write) still deny the CPU.
    let mut cpu_available = 0;
    let mut guard = 0;
    while bus.blitter.busy {
        let owner = bus.scheduled_dma_owner(true);
        if bus.blitter.current_slot_needs_bus() {
            assert_eq!(owner, ChipBusOwner::Blitter);
        } else if owner == ChipBusOwner::Idle {
            cpu_available += 1;
        }
        bus.advance_chipset(1);
        guard += 1;
        assert!(guard < 64, "4-pixel line must finish within 64 cck");
    }
    assert!(
        cpu_available >= 8,
        "expected at least 2 CPU-available cycles per pixel, got {cpu_available}"
    );
}

#[test]
fn front_panel_power_led_follows_cia_a_led_bit() {
    let mut bus = empty_bus();
    // /LED engaged at power-on: the LED starts at full brightness.
    assert!(bus.front_panel_status().power_led_bright);

    // Drive PA1 (/LED) as an output, as the OS does, so the written level
    // reaches the pin the LED and the filter follow.
    let ddra = (REG_DDRA as u64) << 8;
    let _ = bus.cia_a_write(ddra, 1, 0x02);

    let pra = (REG_PRA as u64) << 8;
    // Bit 1 high = /LED released = LED dimmed, filter bypassed.
    let _ = bus.cia_a_write(pra, 1, 0x02);
    assert!(!bus.front_panel_status().power_led_bright);
    // Bit 1 low = /LED asserted = LED bright, filter engaged.
    let _ = bus.cia_a_write(pra, 1, 0x00);
    assert!(bus.front_panel_status().power_led_bright);
}

#[test]
fn front_panel_power_led_ignores_the_host_filter_override() {
    use crate::config::AudioFilterMode;
    let mut bus = empty_bus();
    let ddra = (REG_DDRA as u64) << 8;
    let pra = (REG_PRA as u64) << 8;
    let _ = bus.cia_a_write(ddra, 1, 0x02);
    let _ = bus.cia_a_write(pra, 1, 0x02); // /LED released: LED dimmed

    // Forcing the filter on is a host mix preference; the LED stays on
    // the pin, dimmed, while the effective filter engages.
    bus.paula.set_led_filter_mode(AudioFilterMode::On);
    assert!(bus.paula.led_filter_enabled());
    assert!(!bus.front_panel_status().power_led_bright);

    // And forcing it off never dims a bright LED.
    let _ = bus.cia_a_write(pra, 1, 0x00); // /LED engaged
    bus.paula.set_led_filter_mode(AudioFilterMode::Off);
    assert!(!bus.paula.led_filter_enabled());
    assert!(bus.front_panel_status().power_led_bright);
}

#[test]
fn a1000_led_line_does_not_switch_the_audio_filter() {
    use crate::chipset::cia::REG_DDRA;
    let ddra = (REG_DDRA as u64) << 8;
    let pra = (REG_PRA as u64) << 8;

    // A post-A1000 machine: CIA-A PRA bit 1 (/LED) switches the analogue
    // audio low-pass filter. Drive bits 0 (/OVL, kept high) and 1 as
    // outputs, then toggle /LED. The filter starts engaged.
    let mut normal = empty_bus();
    assert!(normal.paula.led_filter_enabled());
    let _ = normal.cia_a_write(ddra, 1, 0x03);
    let _ = normal.cia_a_write(pra, 1, 0x03); // /LED high -> filter bypassed
    assert!(!normal.paula.led_filter_enabled());
    let _ = normal.cia_a_write(pra, 1, 0x01); // /LED low -> filter engaged
    assert!(normal.paula.led_filter_enabled());

    // The A1000 (identified by its WCS) lacks that circuit: its fixed
    // filter stays engaged no matter what the LED line does, and its
    // power LED -- fed straight from the supply -- stays bright.
    let mut a1000 = empty_bus();
    a1000.mem.wcs = vec![0u8; crate::memory::WCS_SIZE];
    let _ = a1000.cia_a_write(ddra, 1, 0x03);
    let _ = a1000.cia_a_write(pra, 1, 0x03); // /LED high: no effect on the filter
    assert!(a1000.paula.led_filter_enabled());
    assert!(a1000.front_panel_status().power_led_bright);
}

#[test]
fn cd32_pad_serial_protocol_shifts_button_bits() {
    use crate::chipset::cia::REG_DDRA;
    let mut bus = empty_bus();
    bus.input.set_port_device(1, PortDevice::Cd32Pad);
    // Pressed: Red (fire line), Green, Play. Released: Blue, Yellow,
    // FFW, RWD.
    bus.input.ports[1].fire = true;
    bus.input.ports[1].cd32_green = true;
    bus.input.ports[1].cd32_play = true;

    // lowlevel.library's read: drive /FIR1 as output high, put the
    // pad into serial mode by driving POT1X low through POTGO, then
    // sample POT1Y and clock with /FIR1 falling/rising edges.
    let ddra = (REG_DDRA as u64) << 8;
    let pra = (REG_PRA as u64) << 8;
    let _ = bus.cia_a_write(ddra, 1, 0x80);
    let _ = bus.cia_a_write(pra, 1, 0x80);
    bus.custom_write(0x034, 2, 0x2000); // POTGO: OUTRX, DATRX=0

    // Shift order from 8: Blue, Red, Yellow, Green, FFW, RWD, Play,
    // then the pad-present bit, then zeros. Active low.
    let expected = [
        true,  // 8 Blue released
        false, // 7 Red pressed
        true,  // 6 Yellow released
        false, // 5 Green pressed
        true,  // 4 FFW released
        true,  // 3 RWD released
        false, // 2 Play pressed
        true,  // 1 pad-present
        false, // 0 zeros
        false, // stays zero
    ];
    for (step, want) in expected.iter().enumerate() {
        let potgor = bus.custom_read(0x016, 2) as u16;
        assert_eq!(potgor & (1 << 14) != 0, *want, "serial bit at step {step}");
        let _ = bus.cia_a_write(pra, 1, 0x00); // falling edge: shift
        let _ = bus.cia_a_write(pra, 1, 0x80); // rising edge
    }

    // Leaving serial mode reloads the shifter: Blue again first.
    bus.custom_write(0x034, 2, 0x3000); // POT1X driven high
    let _ = bus.custom_read(0x016, 2);
    bus.custom_write(0x034, 2, 0x2000);
    let potgor = bus.custom_read(0x016, 2) as u16;
    assert!(potgor & (1 << 14) != 0, "Blue (released) after reload");
    // And P5 reads low while in serial mode.
    assert_eq!(potgor & (1 << 12), 0);
}

#[test]
fn cd32_pad_held_blue_does_not_ground_the_serial_report() {
    use crate::chipset::cia::REG_DDRA;
    let mut bus = empty_bus();
    bus.input.set_port_device(1, PortDevice::Cd32Pad);
    // Blue arrives as the port's second button; Yellow gives the report a
    // later pressed bit of its own.
    bus.input
        .set_joystick(1, false, false, false, false, false, true);
    bus.input
        .set_cd32_buttons(1, false, false, false, false, true);

    let ddra = (REG_DDRA as u64) << 8;
    let pra = (REG_PRA as u64) << 8;
    let _ = bus.cia_a_write(ddra, 1, 0x80);
    let _ = bus.cia_a_write(pra, 1, 0x80);
    bus.custom_write(0x034, 2, 0x2000); // POTGO: OUTRX, DATRX=0

    // The pad's 4021 output follows Blue only in load mode: while the
    // register is clocking, the held Blue reads low as its own bit and
    // every later bit still reflects its own button line (active low).
    let expected = [
        false, // 8 Blue pressed
        true,  // 7 Red released
        false, // 6 Yellow pressed
        true,  // 5 Green released
        true,  // 4 FFW released
        true,  // 3 RWD released
        true,  // 2 Play released
        true,  // 1 pad-present
        false, // 0 zeros
    ];
    for (step, want) in expected.iter().enumerate() {
        let potgor = bus.custom_read(0x016, 2) as u16;
        assert_eq!(potgor & (1 << 14) != 0, *want, "serial bit at step {step}");
        let _ = bus.cia_a_write(pra, 1, 0x00); // falling edge: shift
        let _ = bus.cia_a_write(pra, 1, 0x80); // rising edge
    }
}

#[test]
fn cd32_pad_serial_protocol_on_port_1_uses_pot0x_fir0_and_pot0y() {
    use crate::chipset::cia::REG_DDRA;
    let mut bus = empty_bus();
    bus.input.set_port_device(0, PortDevice::Cd32Pad);
    // Pressed: Red (fire line), Green, Play. Released: Blue, Yellow,
    // FFW, RWD.
    bus.input.ports[0].fire = true;
    bus.input.ports[0].cd32_green = true;
    bus.input.ports[0].cd32_play = true;

    // The port-1 mirror of the pad read: drive /FIR0 (PA6) as output
    // high, select serial mode by driving POT0X low through POTGO
    // (OUTLX set, DATLX clear -> bits 9/8 = 10), then sample POT0Y
    // (bit 10) and clock with /FIR0 edges.
    let ddra = (REG_DDRA as u64) << 8;
    let pra = (REG_PRA as u64) << 8;
    let _ = bus.cia_a_write(ddra, 1, 0x40);
    let _ = bus.cia_a_write(pra, 1, 0x40);
    bus.custom_write(0x034, 2, 0x0200);

    let expected = [
        true,  // 8 Blue released
        false, // 7 Red pressed
        true,  // 6 Yellow released
        false, // 5 Green pressed
        true,  // 4 FFW released
        true,  // 3 RWD released
        false, // 2 Play pressed
        true,  // 1 pad-present
        false, // 0 zeros
        false, // stays zero
    ];
    for (step, want) in expected.iter().enumerate() {
        let potgor = bus.custom_read(0x016, 2) as u16;
        assert_eq!(potgor & (1 << 10) != 0, *want, "serial bit at step {step}");
        // The mode-select POT0X pin reads low throughout.
        assert_eq!(potgor & (1 << 8), 0, "POT0X driven low at step {step}");
        let _ = bus.cia_a_write(pra, 1, 0x00); // falling edge: shift
        let _ = bus.cia_a_write(pra, 1, 0x40); // rising edge
    }

    // Leaving serial mode reloads the shifter: Blue again first.
    bus.custom_write(0x034, 2, 0x0300); // POT0X driven high
    let _ = bus.custom_read(0x016, 2);
    bus.custom_write(0x034, 2, 0x0200);
    let potgor = bus.custom_read(0x016, 2) as u16;
    assert!(potgor & (1 << 10) != 0, "Blue (released) after reload");
}

#[test]
fn cd32_pads_shift_independently_per_port() {
    use crate::chipset::cia::REG_DDRA;
    let mut bus = empty_bus();
    bus.input.set_port_device(0, PortDevice::Cd32Pad);
    bus.input.set_port_device(1, PortDevice::Cd32Pad);
    // Red pressed on both pads: a pad that has shifted once reports Red
    // (0), one still at the reload position reports Blue released (1).
    bus.input.ports[0].fire = true;
    bus.input.ports[1].fire = true;

    let ddra = (REG_DDRA as u64) << 8;
    let pra = (REG_PRA as u64) << 8;
    // Only /FIR1 is CPU-driven; /FIR0 stays an input.
    let _ = bus.cia_a_write(ddra, 1, 0x80);
    let _ = bus.cia_a_write(pra, 1, 0x80);
    // Both X pins driven low: both pads in serial mode.
    bus.custom_write(0x034, 2, 0x2200);

    // Clock /FIR1 once: only the port-2 shifter advances to Red.
    let _ = bus.cia_a_write(pra, 1, 0x00);
    let _ = bus.cia_a_write(pra, 1, 0x80);
    let potgor = bus.custom_read(0x016, 2) as u16;
    assert_eq!(potgor & (1 << 14), 0, "port 2 shifted to Red (pressed)");
    assert_ne!(potgor & (1 << 10), 0, "port 1 still at Blue (released)");
}

#[test]
fn mouse_middle_button_grounds_potx_pin_per_port() {
    let mut bus = empty_bus();
    assert_eq!(bus.custom_read(0x016, 2) & 0x1100, 0x1100);
    bus.input.set_mouse_button(0, 2, true);
    assert_eq!(bus.custom_read(0x016, 2) & 0x1100, 0x1000);
    bus.input.set_mouse_button(1, 2, true);
    bus.input.set_mouse_button(0, 2, false);
    assert_eq!(bus.custom_read(0x016, 2) & 0x1100, 0x0100);
}

#[test]
fn analogue_stick_position_latches_as_potxdat_count_per_port() {
    let mut bus = empty_bus();
    bus.input.set_analogue(0, 100, 200);
    bus.input.set_analogue(1, 30, 60);

    // START the scan, then advance well past the 8 discharge lines plus
    // the 255-count ramp.
    assert!(!bus.custom_write(0x034, 2, 0x0001));
    bus.advance_devices(227 * 300);

    assert_eq!(bus.custom_read(0x012, 2), 0xC864); // POT0DAT: y=200, x=100
    assert_eq!(bus.custom_read(0x014, 2), 0x3C1E); // POT1DAT: y=60, x=30
}

#[test]
fn analogue_buttons_read_through_joydat_left_right_lines() {
    let mut bus = empty_bus();
    bus.input.set_analogue(0, 10, 10);
    bus.input.set_analogue(1, 10, 10);

    // Driving the joystick lines on an analogue port must not change the
    // device: the left/right switch lines ARE the paddle buttons.
    bus.input
        .set_joystick(0, false, false, true, false, false, false);
    bus.input
        .set_joystick(1, false, false, false, true, false, false);
    assert_eq!(bus.input.device(0), PortDevice::Analogue);
    assert_eq!(bus.input.device(1), PortDevice::Analogue);

    // Button 1 (left line) reads back as JOYxDAT bit 9, button 2 (right
    // line) as bit 1, on top of the idle counters.
    assert_eq!(bus.custom_read(0x00A, 2), 0x0200);
    assert_eq!(bus.custom_read(0x00C, 2), 0x0002);
    // The paddle buttons do not touch the /FIRx lines.
    assert_eq!(bus.cia_a_read((REG_PRA as u64) * 256, 1) & 0xC0, 0xC0);
}

#[test]
fn unplugging_a_device_releases_its_lines_but_keeps_the_counters() {
    let mut bus = empty_bus();
    assert!(!bus.custom_write(0x036, 2, 0x1234)); // JOYTEST loads counters
    bus.input
        .set_joystick(1, true, false, false, false, true, true);
    assert_eq!(
        bus.custom_read(0x00C, 2) as u16,
        super::digital_joydat(true, false, false, false)
    );

    // Swapping the joystick for a mouse releases fire and button 2, and
    // the chip-side quadrature counters still hold the JOYTEST value.
    bus.input.set_port_device(1, PortDevice::Mouse);
    assert_eq!(bus.custom_read(0x00C, 2), 0x1234);
    assert_ne!(bus.cia_a_read((REG_PRA as u64) * 256, 1) & 0x80, 0);
    assert_ne!(bus.custom_read(0x016, 2) & 0x4000, 0);
}

#[test]
fn machine_reset_keeps_plugged_devices_and_knob_positions() {
    let mut bus = empty_bus();
    bus.input.set_port_device(0, PortDevice::Cd32Pad);
    bus.input.set_analogue(1, 50, 200);
    bus.input.ports[0].fire = true;
    bus.input.ports[0].up = true;
    assert!(!bus.custom_write(0x036, 2, 0x1234)); // JOYTEST loads counters

    // A reset does not unplug anything: Ctrl-Amiga-Amiga, the CPU RESET
    // instruction, and a cold boot all leave the physical controllers
    // (and a paddle's knob position) where they are, while the chip-side
    // counters clear and the driven lines release.
    bus.reset_for_keyboard_reset();
    assert_eq!(bus.input.device(0), PortDevice::Cd32Pad);
    assert_eq!(bus.input.device(1), PortDevice::Analogue);
    assert_eq!(
        bus.input.ports[1].pot_x_ohms,
        Some(crate::chipset::paula::pot_position_resistance_ohms(50))
    );
    assert!(!bus.input.ports[0].fire, "reset released the fire line");
    assert!(!bus.input.ports[0].up);
    assert_eq!(bus.input.ports[0].counter_x, 0, "chip counters cleared");
    assert_eq!(bus.input.ports[0].cd32_shifter, 8, "pad shifter reloaded");
}

#[test]
fn empty_port_reads_like_an_idle_mouse() {
    let mut bus = empty_bus();
    bus.input.set_port_device(0, PortDevice::None);
    assert!(!bus.custom_write(0x036, 2, 0xABCD));

    // Nothing drives the pins: JOY0DAT reports the counters, the pot
    // pins float high, and /FIR0 stays released.
    assert_eq!(bus.custom_read(0x00A, 2), 0xABCD);
    assert_eq!(bus.custom_read(0x016, 2) & 0x0500, 0x0500);
    assert_ne!(bus.cia_a_read((REG_PRA as u64) * 256, 1) & 0x40, 0);
}

#[test]
fn front_panel_hdd_led_follows_gayle_activity_with_hold() {
    let mut bus = empty_bus();
    // No Gayle: no HDD LED at all.
    assert_eq!(bus.front_panel_status().hdd_led, None);

    bus.attach_gayle(crate::gayle::Gayle::new(0xD1));
    assert_eq!(bus.front_panel_status().hdd_led, Some(false));

    bus.note_hdd_activity();
    assert_eq!(bus.front_panel_status().hdd_led, Some(true));

    // The hold expires once emulated time passes the deadline.
    bus.emulated_cck += u64::from(PAULA_CLOCK_HZ);
    assert_eq!(bus.front_panel_status().hdd_led, Some(false));
}

#[test]
fn front_panel_hdd_led_present_for_filesys_mount() {
    let mut bus = empty_bus();
    // No storage controller and no filesys mount: no HDD LED at all.
    assert_eq!(bus.front_panel_status().hdd_led, None);

    // A host-folder filesystem board with a mount lights the HDD LED like
    // any other storage device, even without a real disk controller.
    let mount = crate::filesys::MountSpec {
        path: std::env::temp_dir(),
        volume: "Work".to_string(),
        boot_pri: 0,
        readonly: true,
    };
    bus.attach_devices(vec![crate::zorro_device::BoardDevice::Filesys(
        crate::filesys::FilesysBoard::new(vec![mount]),
    )]);
    assert_eq!(bus.front_panel_status().hdd_led, Some(false));

    bus.note_hdd_activity();
    assert_eq!(bus.front_panel_status().hdd_led, Some(true));
}

#[test]
fn front_panel_hdd_led_present_for_zorro_ide() {
    let mut bus = empty_bus();
    // No storage controller: no HDD LED at all.
    assert_eq!(bus.front_panel_status().hdd_led, None);

    // A lide-compatible Zorro IDE board is a hard-disk controller, so its
    // presence alone gives the machine an HDD LED -- even hardware-only
    // (no boot ROM) with no drives attached.
    let board =
        crate::ide_zorro::IdeZorro::new(crate::ide_zorro::LidePersonality::Ripple, Vec::new())
            .unwrap();
    bus.attach_devices(vec![crate::zorro_device::BoardDevice::IdeZorro(board)]);
    assert_eq!(bus.front_panel_status().hdd_led, Some(false));

    bus.note_hdd_activity();
    assert_eq!(bus.front_panel_status().hdd_led, Some(true));

    // The hold expires once emulated time passes the deadline.
    bus.emulated_cck += u64::from(PAULA_CLOCK_HZ);
    assert_eq!(bus.front_panel_status().hdd_led, Some(false));
}

#[test]
fn front_panel_reports_host_output_volume() {
    let mut bus = empty_bus();

    assert_eq!(bus.front_panel_status().output_volume_percent, 100);
    bus.set_output_volume_percent(35);
    assert_eq!(bus.front_panel_status().output_volume_percent, 35);
    bus.adjust_output_volume_percent(-50);
    assert_eq!(bus.front_panel_status().output_volume_percent, 0);
    bus.adjust_output_volume_percent(150);
    assert_eq!(bus.front_panel_status().output_volume_percent, 100);
}

#[test]
fn joy0dat_reports_wrapping_mouse_counters() {
    let mut bus = empty_bus();

    bus.input.add_mouse_delta(0, 5, -2);
    assert_eq!(bus.custom_read(0x00A, 2), 0xFE05);

    bus.input.add_mouse_delta(0, -6, 4);
    assert_eq!(bus.custom_read(0x00A, 2), 0x02FF);
}

#[test]
fn mouse_on_port_2_counts_through_real_deltas() {
    let mut bus = empty_bus();

    bus.input.add_mouse_delta(1, -1, 3);

    assert_eq!(bus.custom_read(0x00C, 2), 0x03FF);
    // Port 1's counters are independent.
    assert_eq!(bus.custom_read(0x00A, 2), 0x0000);
}

/// The decode an Amiga game (or AmigaTestKit) applies to a JOYxDAT word to
/// recover digital-joystick directions, per the HRM: right = bit1,
/// down = bit1 ^ bit0, left = bit9, up = bit9 ^ bit8. The encoding must
/// round-trip through it.
fn decode_digital_joydat(joy: u16) -> (bool, bool, bool, bool) {
    let right = joy & 0x0002 != 0;
    let down = (joy ^ (joy >> 1)) & 0x0001 != 0;
    let left = joy & 0x0200 != 0;
    let up = (joy ^ (joy >> 1)) & 0x0100 != 0;
    (up, down, left, right)
}

#[test]
fn digital_joystick_directions_round_trip_through_joydat_on_both_ports() {
    // Every direction combination read back through JOY0DAT/JOY1DAT must
    // decode to the same directions a game would read.
    for bits in 0u8..16 {
        let (up, down, left, right) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0);

        let mut bus = empty_bus();
        bus.input
            .set_joystick(0, up, down, left, right, false, false);
        bus.input
            .set_joystick(1, up, down, left, right, false, false);

        let p1 = bus.custom_read(0x00A, 2) as u16;
        let p2 = bus.custom_read(0x00C, 2) as u16;
        assert_eq!(
            decode_digital_joydat(p1),
            (up, down, left, right),
            "port1 {bits:#04b}"
        );
        assert_eq!(
            decode_digital_joydat(p2),
            (up, down, left, right),
            "port2 {bits:#04b}"
        );
    }
}

#[test]
fn joydat_reports_mouse_until_joystick_engaged() {
    let mut bus = empty_bus();
    bus.input.ports[1].counter_x = 0x12;
    bus.input.ports[1].counter_y = 0x34;
    // Direction lines set but the port is still a mouse: JOY1DAT must keep
    // reporting the quadrature counters.
    bus.input.ports[1].right = true;
    assert_eq!(bus.custom_read(0x00C, 2), 0x3412);

    // Engaging the joystick switches JOY1DAT to the direction encoding.
    bus.input
        .set_joystick(1, false, false, false, true, false, false);
    assert_eq!(
        bus.custom_read(0x00C, 2) as u16,
        super::digital_joydat(false, false, false, true)
    );
}

#[test]
fn fire_drives_cia_a_pra_fir0_and_fir1_per_port() {
    let mut bus = empty_bus();
    // Released: /FIR0 (PRA bit 6) and /FIR1 (PRA bit 7) read high.
    bus.input
        .set_joystick(0, false, false, false, false, false, false);
    bus.input
        .set_joystick(1, false, false, false, false, false, false);
    assert_eq!(bus.cia_a_read((REG_PRA as u64) * 256, 1) & 0xC0, 0xC0);
    // Port-1 fire pulls only /FIR0 low (active-low).
    bus.input
        .set_joystick(0, false, false, false, false, true, false);
    assert_eq!(bus.cia_a_read((REG_PRA as u64) * 256, 1) & 0xC0, 0x80);
    // Port-2 fire pulls only /FIR1 low.
    bus.input
        .set_joystick(0, false, false, false, false, false, false);
    bus.input
        .set_joystick(1, false, false, false, false, true, false);
    assert_eq!(bus.cia_a_read((REG_PRA as u64) * 256, 1) & 0xC0, 0x40);
}

#[test]
fn joystick_button2_drives_potxy_through_potgor_per_port() {
    let mut bus = empty_bus();
    // Released: POT0Y (bit 10) and POT1Y (bit 14) read high (input pins,
    // buttons up).
    bus.input
        .set_joystick(0, false, false, false, false, false, false);
    bus.input
        .set_joystick(1, false, false, false, false, false, false);
    assert_eq!(bus.custom_read(0x016, 2) & 0x4400, 0x4400);
    // Port-2 button 2 pulls only POT1Y low.
    bus.input
        .set_joystick(1, false, false, false, false, false, true);
    assert_eq!(bus.custom_read(0x016, 2) & 0x4400, 0x0400);
    // Port-1 button 2 pulls only POT0Y low.
    bus.input
        .set_joystick(0, false, false, false, false, false, true);
    bus.input
        .set_joystick(1, false, false, false, false, false, false);
    assert_eq!(bus.custom_read(0x016, 2) & 0x4400, 0x4000);
}

#[test]
fn fire_buttons_read_through_potgo_pullup_mode() {
    // Software reads fire 2/3 by enabling the pot pin pull-ups and then
    // sampling POTGOR; e.g. AmigaTestKit writes POTGO = 0x0f00 << (port*4),
    // which sets output-enable + data 1 on POT0X/Y (port 1) or POT1X/Y
    // (port 2). A pressed button still pulls its pull-up pin low, so the
    // button must remain visible despite output being enabled.
    let mut bus = empty_bus();

    // Port 2 pull-ups (POT1X bits 12/13, POT1Y bits 14/15).
    assert!(!bus.custom_write(0x034, 2, 0xF000));
    bus.input
        .set_joystick(1, false, false, false, false, false, false);
    assert_ne!(bus.custom_read(0x016, 2) & 0x4000, 0); // button 2 up -> high
    bus.input
        .set_joystick(1, false, false, false, false, false, true);
    assert_eq!(bus.custom_read(0x016, 2) & 0x4000, 0); // button 2 down -> low

    // Port 1 pull-ups (POT0X bits 8/9, POT0Y bits 10/11): the right mouse
    // button reads through POT0Y the same way.
    assert!(!bus.custom_write(0x034, 2, 0x0F00));
    bus.input.set_mouse_button(0, 1, false);
    assert_ne!(bus.custom_read(0x016, 2) & 0x0400, 0); // RMB up -> high
    bus.input.set_mouse_button(0, 1, true);
    assert_eq!(bus.custom_read(0x016, 2) & 0x0400, 0); // RMB down -> low
}

#[test]
fn vposw_and_vhposw_update_beam_register_reads() {
    let mut bus = empty_bus();

    assert!(!bus.custom_write(0x02A, 2, 0x8001));
    assert!(!bus.custom_write(0x02C, 2, 0x2034));

    assert_eq!(bus.custom_read(0x004, 2), 0x8001);
    // The readback reports a few colour clocks ahead of the written counter
    // (the calibrated VHPOSR lookahead).
    assert_eq!(bus.custom_read(0x006, 2), 0x2037);
}

#[test]
fn joytest_sets_both_mouse_counter_pairs() {
    let mut bus = empty_bus();

    assert!(!bus.custom_write(0x036, 2, 0x1234));

    assert_eq!(bus.custom_read(0x00A, 2), 0x1234);
    assert_eq!(bus.custom_read(0x00C, 2), 0x1234);
}

#[test]
fn potgo_starts_counters_and_potgor_reflects_button_pins() {
    let mut bus = empty_bus();
    bus.input.set_mouse_button(0, 1, true);

    assert!(!bus.custom_write(0x034, 2, 0x0001));
    // The pot counters clock at H-sync, so while the scan runs it caps the
    // advance at the next line boundary, and POT0DAT reads 0 during discharge.
    assert!(bus.next_pot_event_cck().is_some());
    assert_eq!(bus.custom_read(0x012, 2), 0);

    // Advance well past the 8-line discharge phase; both POT0DAT bytes (X and Y
    // pin counters) ramp together and stay below the 256 wrap.
    bus.advance_devices(227 * 40);

    let pot0 = bus.custom_read(0x012, 2);
    assert_ne!(pot0, 0);
    assert_eq!(pot0 & 0xFF, (pot0 >> 8) & 0xFF);
    assert!(bus.next_pot_event_cck().is_some());
    assert_eq!(bus.custom_read(0x016, 2) & (1 << 10), 0);
}

#[test]
fn clxdat_reads_and_clears_collision_latch() {
    let mut bus = empty_bus();

    bus.denise.or_clxdat(0x1234);

    assert_eq!(bus.custom_read(0x00E, 2), 0x9234);
    assert_eq!(bus.custom_read(0x00E, 2), 0x8000);
}

#[test]
fn denise_write_only_reads_use_zero_bus_approximation() {
    let mut bus = empty_bus();

    assert!(!bus.custom_write(0x098, 2, 0x0A5A));
    assert!(!bus.custom_write(0x104, 2, 0x1234));
    assert!(!bus.custom_write(0x180, 2, u64::from(COLOR_TRANSPARENCY_BIT | 0x0BCD)));
    assert!(!bus.custom_write(0x1BE, 2, 0x0FED));

    assert_eq!(bus.denise.clxcon, 0x0A5A);
    assert_eq!(bus.denise.bplcon2, 0x1234);
    assert_eq!(bus.denise.palette[0], COLOR_TRANSPARENCY_BIT | 0x0BCD);
    assert_eq!(bus.denise.palette[31], 0x0FED);
    assert_eq!(bus.custom_read(0x098, 2), 0);
    assert_eq!(bus.custom_read(0x104, 2), 0);
    assert_eq!(bus.custom_read(0x180, 2), 0);
    assert_eq!(bus.custom_read(0x1BE, 2), 0);
    assert_eq!(bus.custom_read(0x099, 1), 0);
    assert_eq!(bus.custom_read(0x181, 1), 0);
}

#[test]
fn custom_writes_latch_sprite_pointer_and_data_registers() {
    let mut bus = empty_bus();

    assert!(!bus.custom_write(0x120, 2, 0x0002));
    assert!(!bus.custom_write(0x122, 2, 0x3456));
    assert_eq!(bus.denise.sprpt[0], 0x0002_3456);

    assert!(!bus.custom_write(0x140, 2, 0x2C40));
    assert!(!bus.custom_write(0x142, 2, 0x3000));
    assert!(!bus.custom_write(0x144, 2, 0x8000));
    assert!(!bus.custom_write(0x146, 2, 0x0000));
    assert_eq!(bus.denise.sprpos[0], 0x2C40);
    assert_eq!(bus.denise.sprctl[0], 0x3000);
    assert_eq!(bus.denise.sprdata[0], 0x8000);
    assert_eq!(bus.denise.sprdatb[0], 0x0000);
}

#[test]
fn manual_sprite_data_arm_persists_across_frame_start() {
    let mut bus = empty_bus();

    assert!(!bus.custom_write(0x140, 2, 0x2C40));
    assert!(!bus.custom_write(0x142, 2, 0x2D00));
    assert!(!bus.custom_write(0x146, 2, 0x4000));
    assert!(!bus.custom_write(0x144, 2, 0x8000));

    bus.begin_new_beam_frame();

    assert!(bus.denise.spr_armed[0]);
    assert_eq!(bus.denise.sprdata[0], 0x8000);
    assert_eq!(bus.denise.sprdatb[0], 0x4000);
    assert!(bus.current_frame_render_base.spr_armed[0]);
    assert_eq!(bus.current_frame_render_base.sprdata[0], 0x8000);
    assert_eq!(bus.current_frame_render_base.sprdatb[0], 0x4000);
}

/// One emulated keyboard byte takes 8 bits x 3 phases x ~20 us.
const KEYBOARD_BYTE_CCK: u32 = 8 * 3 * 71;

fn unmask_cia_a_sp(bus: &mut Bus) {
    let icr = (crate::chipset::cia::REG_ICR as u64) << 8;
    let _ = bus.cia_a_write(icr, 1, 0x88); // set SP mask bit
}

fn read_cia_a_sdr_and_ack(bus: &mut Bus) -> u8 {
    let sdr = bus.cia_a.read(crate::chipset::cia::REG_SDR);
    bus.cia_a.read(crate::chipset::cia::REG_ICR);
    bus.paula.intreq &= !INT_PORTS;
    sdr
}

/// Walk the keyboard MCU through its post-power-on flow at the bus
/// level: self-test, lone-bit sync, then the $FD/$FE stream, each
/// handshaked the way keyboard.device does. Leaves the MCU idle.
fn complete_keyboard_power_up(bus: &mut Bus) {
    unmask_cia_a_sp(bus);
    // Self-test (50 ms) plus the first sync bit.
    bus.advance_devices(180_000);
    keyboard_handshake(bus);
    // $FD then $FE, each handshaked.
    for _ in 0..2 {
        bus.advance_devices(2 * KEYBOARD_BYTE_CCK);
        read_cia_a_sdr_and_ack(bus);
        keyboard_handshake(bus);
    }
    bus.advance_devices(2 * KEYBOARD_BYTE_CCK);
    assert_eq!(bus.next_keyboard_event_cck(), None, "MCU should be idle");
}

#[test]
fn keyboard_power_up_streams_fd_fe_over_the_bit_path() {
    let mut bus = empty_bus();
    unmask_cia_a_sp(&mut bus);
    // Self-test, then the lone sync bit; handshake it.
    bus.advance_devices(180_000);
    keyboard_handshake(&mut bus);
    // $FD ("initiate power-up key stream"), rotated on the wire.
    bus.advance_devices(2 * KEYBOARD_BYTE_CCK);
    assert_eq!(read_cia_a_sdr_and_ack(&mut bus), !0xFDu8.rotate_left(1));
    keyboard_handshake(&mut bus);
    // $FE ("terminate key stream").
    bus.advance_devices(2 * KEYBOARD_BYTE_CCK);
    assert_eq!(read_cia_a_sdr_and_ack(&mut bus), !0xFEu8.rotate_left(1));
}

#[test]
fn keyboard_chord_runs_reset_protocol_and_requests_machine_reset() {
    let mut bus = empty_bus();
    complete_keyboard_power_up(&mut bus);
    bus.enqueue_key_event(0x63, true);
    bus.enqueue_key_event(0x66, true);
    bus.enqueue_key_event(0x67, true);

    // Nobody handshakes the $78 warnings; the keyboard still pulls
    // KCLK low and, 500 ms later, requests the system reset.
    // ($78 byte + 143 ms window + 500 ms hold, with margin.)
    assert!(!bus.keyboard_system_reset_pending);
    bus.advance_devices(4_000_000);
    assert!(bus.keyboard_system_reset_pending);
}

#[test]
fn keyboard_bit_stream_delivers_encoded_transition_to_sdr() {
    let mut bus = empty_bus();
    complete_keyboard_power_up(&mut bus);
    bus.enqueue_key_event(0x01, true);

    // Half a byte: no SP interrupt yet.
    bus.advance_devices(KEYBOARD_BYTE_CCK / 2);
    assert_eq!(bus.paula.intreq & INT_PORTS, 0);
    // The full byte arrives bit by bit over KCLK/KDAT; the CIA pin edge
    // follows one E-cycle behind the final shift.
    bus.advance_devices(2 * KEYBOARD_BYTE_CCK);
    bus.advance_devices(8);
    assert_ne!(bus.paula.intreq & INT_PORTS, 0);
    assert_eq!(bus.cia_a.read(crate::chipset::cia::REG_SDR), 0xFD);
}

#[test]
fn keyboard_second_byte_waits_for_a_timed_handshake() {
    let mut bus = empty_bus();
    complete_keyboard_power_up(&mut bus);
    bus.enqueue_key_event(0x01, true);
    bus.enqueue_key_event(0x02, true);
    bus.advance_devices(2 * KEYBOARD_BYTE_CCK);
    assert_eq!(bus.cia_a.read(crate::chipset::cia::REG_SDR), 0xFD);

    // No handshake: the second byte must not transmit.
    bus.advance_devices(2 * KEYBOARD_BYTE_CCK);
    assert_eq!(bus.cia_a.read(crate::chipset::cia::REG_SDR), 0xFD);

    // A zero-width CRA double-write (SPMODE set then cleared with no
    // emulated time between) is not a timed pulse and is ignored.
    let cra = (REG_CRA as u64) << 8;
    let _ = bus.cia_a_write(cra, 1, 0x40);
    let _ = bus.cia_a_write(cra, 1, 0x00);
    bus.advance_devices(2 * KEYBOARD_BYTE_CCK);
    assert_eq!(bus.cia_a.read(crate::chipset::cia::REG_SDR), 0xFD);

    // A brief but real KDAT pulse releases the next byte.
    keyboard_handshake(&mut bus);
    bus.advance_devices(2 * KEYBOARD_BYTE_CCK);
    assert_eq!(bus.cia_a.read(crate::chipset::cia::REG_SDR), 0xFB);
}

#[test]
fn keyboard_burst_requires_a_handshake_between_each_byte() {
    let mut bus = empty_bus();
    complete_keyboard_power_up(&mut bus);
    bus.enqueue_key_event(0x01, true);
    bus.enqueue_key_event(0x02, true);
    bus.enqueue_key_event(0x03, true);

    for expected in [0xFDu8, 0xFB, 0xF9] {
        bus.advance_devices(2 * KEYBOARD_BYTE_CCK);
        assert_eq!(bus.cia_a.read(crate::chipset::cia::REG_SDR), expected);
        keyboard_handshake(&mut bus);
    }
}

#[test]
fn keyboard_event_deadline_caps_idle_fast_forward_while_active() {
    let mut bus = empty_bus();
    // During power-up the MCU always has a deadline.
    assert!(bus.next_keyboard_event_cck().is_some());
    complete_keyboard_power_up(&mut bus);
    assert_eq!(bus.next_keyboard_event_cck(), None);
    bus.enqueue_key_event(0x01, true);
    assert_eq!(bus.next_keyboard_event_cck(), Some(1));
    bus.advance_devices(400);
    // Mid-transmission: the next KCLK edge bounds the fast-forward.
    let deadline = bus.next_keyboard_event_cck().expect("edge deadline");
    assert!(deadline <= 71, "deadline {deadline} cck");
    // After the byte, the resync timeout still provides a deadline.
    bus.advance_devices(2 * KEYBOARD_BYTE_CCK);
    assert!(bus.next_keyboard_event_cck().is_some());
}

// ------------------------------------------------------------------
// Custom-register cross-check (HRM Appendix A/B drift guard).
//
// These tests are the machine-checked replacement for the
// hand-maintained custom-register audit prose: they enumerate the
// $DFFxxx map as a single source of truth and assert the bus's
// read/latch/write dispatch matches it. A register that is added,
// removed, or mis-classified fails here -- not silently in a demo.
// When a previously-unmodeled register is implemented, the matching
// table below must be updated, which is exactly the conscious audit
// step we want to force.
// ------------------------------------------------------------------

/// Every CPU-readable custom register (the `read_custom_word` arms),
/// HRM Appendix A read column. Everything else returns the undriven
/// custom-bus fallback (0). The 4-channel audio block ($0A0-$0DE) is
/// dispatched to Paula as well but is handled separately below because
/// HRM marks those write-only and Copperline returns the latch as an
/// approximation.
const CPU_READABLE_CUSTOM_REGS: &[(u16, &str)] = &[
    (0x002, "DMACONR"),
    (0x004, "VPOSR"),
    (0x006, "VHPOSR"),
    (0x008, "DSKDATR"),
    (0x00A, "JOY0DAT"),
    (0x00C, "JOY1DAT"),
    (0x00E, "CLXDAT"),
    (0x010, "ADKCONR"),
    (0x012, "POT0DAT"),
    (0x014, "POT1DAT"),
    (0x016, "POTGOR"),
    (0x018, "SERDATR"),
    (0x01A, "DSKBYTR"),
    (0x01C, "INTENAR"),
    (0x01E, "INTREQR"),
    // DENISEID: ECS drives 0xFFFC; OCS floats the undriven bus high to 0xFFFF.
    // Either way it reads a fixed non-zero value, so it is a readable register.
    (0x07C, "DENISEID"),
];

fn is_readable_custom_off(off: u16) -> bool {
    CPU_READABLE_CUSTOM_REGS.iter().any(|&(o, _)| o == off)
        // Paula audio block reads back its latches in this model.
        || (0x0A0..=0x0DE).contains(&off)
}

#[test]
fn custom_register_read_map_matches_dispatch() {
    // Drive distinctive state through the write side and read it back
    // through the read side, pinning both halves of the dispatch for a
    // representative readable register from each chip.
    let mut bus = empty_bus();
    bus.custom_write(0x096, 2, u64::from(0x8000 | DMACON_DMAEN)); // DMACON SET
    assert_ne!(
        bus.custom_read(0x002, 2) & u64::from(DMACON_DMAEN),
        0,
        "DMACONR must reflect a DMACON SET write"
    );
    bus.custom_write(0x09E, 2, 0x8000 | 0x0100); // ADKCON SET bit 8
    assert_ne!(bus.custom_read(0x010, 2) & 0x0100, 0, "ADKCONR readback");
    bus.custom_write(0x09A, 2, u64::from(0x8000 | INT_BLIT)); // INTENA SET
    assert_ne!(
        bus.custom_read(0x01C, 2) & u64::from(INT_BLIT),
        0,
        "INTENAR readback"
    );
    bus.custom_write(0x09C, 2, u64::from(0x8000 | INT_BLIT)); // INTREQ SET
    assert_ne!(
        bus.custom_read(0x01E, 2) & u64::from(INT_BLIT),
        0,
        "INTREQR readback"
    );

    // Every offset NOT in the readable map must return the undriven-bus
    // fallback (0). A fresh bus has no driven state, so a stray new read
    // arm (or a removed write-only fallback) is caught here.
    let mut fresh = empty_bus();
    let mut off = 0x000u16;
    while off <= 0x1FE {
        if !is_readable_custom_off(off) {
            assert_eq!(
                fresh.custom_read(u64::from(off), 2),
                0,
                "write-only/unmodeled custom register {off:#05X} must read as the bus fallback"
            );
        }
        off += 2;
    }
}

/// ECS-only registers whose debugger latch view (and dispatch) appears
/// only on ECS Agnus/Denise. The bus gates these on the configured
/// revision; this list is the single place that fact is asserted.
const ECS_ONLY_LATCHED_REGS: &[(u16, &str)] = &[
    (0x05A, "BLTCON0L"),
    (0x05C, "BLTSIZV"),
    (0x078, "SPRHDAT"),
    (0x1C0, "HTOTAL"),
    (0x1C2, "HSSTOP"),
    (0x1C4, "HBSTRT"),
    (0x1C6, "HBSTOP"),
    (0x1C8, "VTOTAL"),
    (0x1CA, "VSSTOP"),
    (0x1CC, "VBSTRT"),
    (0x1CE, "VBSTOP"),
    (0x1DC, "BEAMCON0"),
    (0x1DE, "HSSTRT"),
    (0x1E0, "VSSTRT"),
    (0x1E2, "HCENTER"),
    (0x1E4, "DIWHIGH"),
];

#[test]
fn custom_register_ecs_latches_follow_agnus_revision() {
    // OCS: the ECS-only latches must report "unmodeled" so the debugger
    // does not invent state for registers the chipset does not have.
    let ocs = empty_bus();
    for &(off, name) in ECS_ONLY_LATCHED_REGS {
        assert!(
            ocs.custom_reg_latch(off).is_none(),
            "{name} ({off:#05X}) must not latch on OCS"
        );
    }

    // ECS: the same registers gain a debugger latch view.
    let mut ecs = empty_bus();
    ecs.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    for &(off, name) in ECS_ONLY_LATCHED_REGS {
        assert!(
            ecs.custom_reg_latch(off).is_some(),
            "{name} ({off:#05X}) must latch on ECS"
        );
    }

    // Revision-independent anchors: a few always-latched registers and a
    // few never-latched offsets, so a wholesale latch-map change is
    // caught regardless of revision.
    for bus in [&ocs, &ecs] {
        // COPCON, BLTCON0, BPLCON0, BPL1PTH, BPL1DAT, SPR0PTH, SPR0POS,
        // COLOR00 -- a spread across the latched register groups.
        for off in [0x02E, 0x040, 0x100, 0x0E0, 0x110, 0x120, 0x140, 0x180] {
            assert!(
                bus.custom_reg_latch(off).is_some(),
                "always-latched register {off:#05X}"
            );
        }
        // DMACONR (read-only), COPJMP1 and DMACON (strobes): no latch.
        for off in [0x002, 0x088, 0x096] {
            assert!(
                bus.custom_reg_latch(off).is_none(),
                "read-only/strobe register {off:#05X} must not latch"
            );
        }
    }
}

#[test]
fn ecs_scan_registers_latch_only_on_ecs_agnus() {
    // ECS: each programmable sync/blank latch (and the UHRES SPRHDAT)
    // stores the written value, masked to its register width. OCS ignores
    // the write. No scan-rate geometry is derived from them yet.
    let mut ecs = empty_bus();
    ecs.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    ecs.custom_write(0x1DE, 2, 0x1234); // HSSTRT (9-bit)
    ecs.custom_write(0x1C2, 2, 0x1235); // HSSTOP
    ecs.custom_write(0x1C4, 2, 0x1236); // HBSTRT
    ecs.custom_write(0x1C6, 2, 0x1237); // HBSTOP
    ecs.custom_write(0x1E2, 2, 0x1238); // HCENTER
    ecs.custom_write(0x1C8, 2, 0x1244); // VTOTAL (11-bit)
    ecs.custom_write(0x1E0, 2, 0x1245); // VSSTRT
    ecs.custom_write(0x1CA, 2, 0x1246); // VSSTOP
    ecs.custom_write(0x1CC, 2, 0x1247); // VBSTRT
    ecs.custom_write(0x1CE, 2, 0x1248); // VBSTOP
    ecs.custom_write(0x078, 2, 0x1249); // SPRHDAT (raw u16)

    assert_eq!(ecs.agnus.hsstrt(), 0x1234 & 0x01FF);
    assert_eq!(ecs.agnus.hsstop(), 0x1235 & 0x01FF);
    assert_eq!(ecs.agnus.hbstrt(), 0x1236 & 0x01FF);
    assert_eq!(ecs.agnus.hbstop(), 0x1237 & 0x01FF);
    assert_eq!(ecs.agnus.hcenter(), 0x1238 & 0x01FF);
    assert_eq!(ecs.agnus.vtotal(), 0x1244 & 0x07FF);
    assert_eq!(ecs.agnus.vsstrt(), 0x1245 & 0x07FF);
    assert_eq!(ecs.agnus.vsstop(), 0x1246 & 0x07FF);
    assert_eq!(ecs.agnus.vbstrt(), 0x1247 & 0x07FF);
    assert_eq!(ecs.agnus.vbstop(), 0x1248 & 0x07FF);
    assert_eq!(ecs.agnus.sprhdat(), 0x1249);

    // OCS Agnus has none of these; writes are dropped and the fields stay
    // at their reset 0.
    let mut ocs = empty_bus();
    for off in [
        0x1DEu64, 0x1C2, 0x1C4, 0x1C6, 0x1E2, 0x1C8, 0x1E0, 0x1CA, 0x1CC, 0x1CE, 0x078,
    ] {
        ocs.custom_write(off, 2, 0x1FFF);
    }
    assert_eq!(ocs.agnus.hsstrt(), 0);
    assert_eq!(ocs.agnus.hcenter(), 0);
    assert_eq!(ocs.agnus.vtotal(), 0);
    assert_eq!(ocs.agnus.vbstop(), 0);
    assert_eq!(ocs.agnus.sprhdat(), 0);
}

#[test]
fn deniseid_reads_ecs_denise_id_on_ecs_only() {
    // ECS Denise (8373) drives DENISEID = 0xFFFC; the low byte 0xFC is how
    // software detects ECS. OCS Denise has no such register, so $07C reads the
    // undriven custom bus, which floats high to 0xFFFF (low byte 0xFF != 0xFC,
    // so software correctly detects OCS).
    let mut ecs = empty_bus();
    ecs.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    let id = ecs.custom_read(0x07C, 2) as u16;
    assert_eq!(id, 0xFFFC, "ECS Denise DENISEID");
    assert_eq!(id & 0x00FF, 0x00FC, "ECS detection low byte");

    let mut ocs = empty_bus();
    let ocs_id = ocs.custom_read(0x07C, 2) as u16;
    assert_eq!(ocs_id, 0xFFFF, "OCS Denise $07C floats high");
    assert_ne!(
        ocs_id & 0x00FF,
        0x00FC,
        "OCS low byte is not the ECS marker"
    );
}

/// Plan 1.3: the chip revisions are independent. Late A500s shipped an
/// ECS Agnus with an OCS Denise; software must see the split ids.
#[test]
fn chip_revisions_split_deniseid_from_vposr_id() {
    let mut mixed = empty_bus();
    mixed.set_chipset_revisions(AgnusRevision::Ecs8372Rev4, DeniseRevision::Ocs);
    assert_eq!(
        mixed.custom_read(0x07C, 2),
        0xFFFF,
        "OCS Denise $07C floats high"
    );
    assert_eq!(
        mixed.custom_read(0x004, 2) & 0x7F00,
        0x2000,
        "ECS Agnus VPOSR id"
    );

    let mut mixed = empty_bus();
    mixed.set_chipset_revisions(AgnusRevision::Ocs, DeniseRevision::Ecs8373);
    assert_eq!(mixed.custom_read(0x07C, 2), 0xFFFC, "ECS Denise id");
    assert_eq!(
        mixed.custom_read(0x004, 2) & 0x7F00,
        0x0000,
        "OCS Agnus VPOSR id"
    );
}

/// AGA identification and register latches, gated on the Alice/Lisa
/// revisions that `Chipset::Aga` selects.
#[test]
fn aga_ids_and_register_latches_gate_on_alice_lisa() {
    let mut aga = empty_bus();
    aga.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    assert_eq!(aga.custom_read(0x004, 2) & 0x7F00, 0x2300, "Alice PAL id");
    assert_eq!(aga.custom_read(0x07C, 2), 0x00F8, "Lisa DENISEID");

    assert!(!aga.custom_write(0x1FC, 2, 0xFFFF));
    assert_eq!(aga.agnus.fmode(), 0xC00F, "FMODE defined bits latch");
    assert!(!aga.custom_write(0x10C, 2, 0x1234));
    assert_eq!(aga.denise.bplcon4, 0x1234);
    assert!(!aga.custom_write(0x10E, 2, 0xFFFF));
    assert_eq!(aga.denise.clxcon2, 0x0FFF);

    // ECS machines ignore the AGA registers entirely.
    let mut ecs = empty_bus();
    ecs.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    assert!(!ecs.custom_write(0x1FC, 2, 0xFFFF));
    assert_eq!(ecs.agnus.fmode(), 0);
    assert!(!ecs.custom_write(0x10C, 2, 0x1234));
    assert_eq!(ecs.denise.bplcon4, 0x0011, "BPLCON4 keeps its reset value");
    assert!(!ecs.custom_write(0x10E, 2, 0x0FFF));
    assert_eq!(ecs.denise.clxcon2, 0);
}

/// Plan 3.2: Lisa routes COLORxx writes through BPLCON3 BANK/LOCT into
/// the 256-entry store; bank 0 with LOCT clear stays OCS-compatible.
#[test]
fn lisa_palette_writes_follow_bplcon3_bank_and_loct() {
    let mut aga = empty_bus();
    aga.set_chipset_revisions(AgnusRevision::AgaAlice, DeniseRevision::AgaLisa);
    // ENBPLCN3 so BPLCON3 writes latch.
    assert!(!aga.custom_write(0x100, 2, 0x0001));
    assert!(!aga.custom_write(0x106, 2, 1 << 13)); // BANK = 1
    assert!(!aga.custom_write(0x180, 2, 0x0123)); // COLOR00 -> entry 32
    assert_eq!(aga.denise.palette.rgb24(32), 0x0011_2233);
    assert_eq!(aga.denise.palette[0], 0, "bank 0 untouched");

    // LOCT set: low nibbles only.
    assert!(!aga.custom_write(0x106, 2, (1 << 13) | 0x0200));
    assert!(!aga.custom_write(0x180, 2, 0x0FFF));
    assert_eq!(aga.denise.palette.rgb24(32), 0x001F_2F3F);

    // Bank 0, LOCT clear: classic OCS write, visible in the render view.
    assert!(!aga.custom_write(0x106, 2, 0));
    assert!(!aga.custom_write(0x182, 2, 0x0ABC)); // COLOR01
    assert_eq!(aga.denise.palette[1], 0x0ABC);
}

#[test]
fn hhposr_reads_hhposw_latch_on_ecs_agnus_only() {
    let mut ocs = empty_bus();
    assert!(!ocs.custom_write(0x1D8, 2, 0x0155));
    assert_eq!(ocs.custom_read(0x1DA, 2), 0, "no HHPOSR on OCS");

    let mut ecs = empty_bus();
    ecs.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
    assert!(!ecs.custom_write(0x1D8, 2, 0x0155));
    assert_eq!(ecs.custom_read(0x1DA, 2), 0x0155);
}

#[test]
fn gayle_int2_level_keeps_setting_paula_ports_intreq() {
    use crate::gayle::{Gayle, IdeDrive};

    let image =
        std::env::temp_dir().join(format!("copperline-bus-gayle-{}.hdf", std::process::id()));
    std::fs::write(&image, vec![0u8; 512 * 16]).unwrap();

    let mut bus = empty_bus();
    let mut gayle = Gayle::new(0xD0);
    gayle.attach_drive(
        0,
        IdeDrive::open(&image, 0, None, 0, crate::diskimage::FileSystem::FFS).unwrap(),
    );
    bus.attach_gayle(gayle);

    // Enable the IDE interrupt at $DAA000 and issue a READ SECTORS via
    // the memory-mapped interface.
    let g = bus.gayle.as_mut().unwrap();
    g.write(0x00DA_A000, 1, 0x80); // INTENA.IDE
    g.write(0x00DA_2018, 1, 0x40); // LBA, drive 0
    g.write(0x00DA_200C, 1, 0); // LBA 0
    g.write(0x00DA_2010, 1, 0);
    g.write(0x00DA_2014, 1, 0);
    g.write(0x00DA_2008, 1, 1); // one sector
    g.write(0x00DA_201C, 1, 0x20); // READ SECTORS
    assert!(bus.gayle.as_ref().unwrap().int2_line());

    // The timed-device tick re-latches INTREQ.PORTS while the line holds.
    bus.advance_devices(4);
    assert_ne!(bus.paula.intreq & INT_PORTS, 0);
    bus.paula.intreq &= !INT_PORTS;
    bus.advance_devices(4);
    assert_ne!(
        bus.paula.intreq & INT_PORTS,
        0,
        "level interrupt re-latches after a clear"
    );

    // Acknowledging at Gayle (write-to-clear) drops the line.
    let g = bus.gayle.as_mut().unwrap();
    g.write(0x00DA_9000, 1, 0x7F);
    assert!(!bus.gayle.as_ref().unwrap().int2_line());
    bus.paula.intreq &= !INT_PORTS;
    bus.advance_devices(4);
    assert_eq!(bus.paula.intreq & INT_PORTS, 0);
    std::fs::remove_file(image).ok();
}

#[test]
fn bplcon0_lpen_enables_light_pen_latch_via_bus() {
    let mut bus = empty_bus();
    assert!(!bus.custom_write(0x100, 2, 0x0008));
    bus.advance_chipset(5 * 227 + 0x40);
    bus.light_pen_pulse();
    bus.advance_chipset(20);
    assert_eq!(bus.custom_read(0x006, 2), (5 << 8) | 0x40);
}

#[test]
fn custom_register_space_sweep_is_panic_free_and_unused_offsets_inert() {
    // Sweeping every register with byte/word/long reads and writes on
    // both revisions must never panic -- the dispatch has total coverage
    // (an explicit arm or the silent `_ => false` / `=> 0` fallbacks).
    for ecs in [false, true] {
        let mut bus = empty_bus();
        if ecs {
            bus.set_agnus_revision(AgnusRevision::Ecs8372Rev4);
        }
        let mut off = 0x000u16;
        while off <= 0x1FE {
            let addr = u64::from(off);
            bus.custom_write(addr, 2, 0xFFFF);
            bus.custom_write(addr, 1, 0xAA);
            bus.custom_write(addr | 1, 1, 0x55);
            bus.custom_write(addr, 4, 0x1234_5678);
            let _ = bus.custom_read(addr, 1);
            let _ = bus.custom_read(addr, 2);
            let _ = bus.custom_read(addr, 4);
            off += 2;
        }
    }

    // Offsets with no modelled register stay inert: a write leaves no
    // readable state behind. Catches a reserved offset accidentally
    // gaining a read arm or a stored latch.
    let mut bus = empty_bus();
    for off in [0x1F0u16, 0x1F2, 0x1F4, 0x1F6, 0x1F8, 0x1FA, 0x1FC, 0x1FE] {
        assert!(bus.custom_reg_latch(off).is_none());
        bus.custom_write(u64::from(off), 2, 0xFFFF);
        assert_eq!(
            bus.custom_read(u64::from(off), 2),
            0,
            "unused custom offset {off:#05X} must stay inert"
        );
    }
}

#[test]
fn render_input_refill_from_bus_matches_fresh_snapshot() {
    // Snapshot one frame state into a RenderInput, then mutate the machine
    // into a visibly different frame and refill the same RenderInput from
    // it. The recycled snapshot must render pixel-identically to a freshly
    // allocated one: refill_from_bus only reuses buffers, never state.
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0037);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    let mut recycled = bitplane::RenderInput::from_bus(&bus);

    // Second frame: a copper palette event plus a captured bitplane row so
    // the refill has to replace event lists, captured rows, and chip RAM.
    bus.denise.palette.write_ocs(1, 0x0F00);
    bus.current_frame_render_base = bus.capture_render_snapshot();
    run_copper_moves_at(
        &mut bus,
        0x0100,
        RENDER_VISIBLE_START_VPOS,
        RENDER_COPPER_WAIT_HPOS_FB0 + 30,
        &[(0x0182, 0x00F0)],
    );
    let words_per_row = bitplane_words_per_row(
        bus.agnus.revision(),
        bus.denise.bplcon0,
        bus.agnus.fmode(),
        bus.denise.ddfstrt,
        bus.denise.ddfstop,
        bus.harddis_active(),
    );
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row,
        fetch_origin_cck: None,
        planes: [
            vec![0xFFFF; words_per_row],
            vec![0; words_per_row],
            vec![0; words_per_row],
            vec![0; words_per_row],
            vec![0; words_per_row],
            vec![0; words_per_row],
            Vec::new(),
            Vec::new(),
        ],
    });

    let fresh = bitplane::RenderInput::from_bus(&bus);
    recycled.refill_from_bus(&bus);

    let mut fb_fresh = vec![0u32; FB_PIXELS];
    let mut fb_recycled = vec![0u32; FB_PIXELS];
    bitplane::render_from_input(&fresh, &mut fb_fresh);
    bitplane::render_from_input(&recycled, &mut fb_recycled);
    assert!(
        fb_fresh == fb_recycled,
        "recycled RenderInput must render identically to a fresh snapshot"
    );
    assert!(
        fb_fresh.iter().any(|&px| px != fb_fresh[0]),
        "frame must contain non-uniform pixels for the comparison to mean anything"
    );
}

#[test]
fn line_without_captured_fetch_paints_no_playfield() {
    // Agnus arms a line's fetch run at the DDFSTRT comparator match; a
    // BPLCON0 raised mid-line only after that point starts no run until the
    // next line. The DMA capture records which lines ran, so a line with no
    // captured fetch fetched nothing and the renderer must not synthesize a
    // picture for it from the register-derived window (regression: the
    // Rampage bottom-scroller band entry painted a phantom, word-skewed
    // copy of the first bitmap row one line above the real first fetch row).
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2FC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x00D0;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(1, 0x0FFF);
    bus.denise.bplpt[0] = 0x0100;
    let words_per_row = bitplane_words_per_row(
        bus.agnus.revision(),
        bus.denise.bplcon0,
        bus.agnus.fmode(),
        bus.denise.ddfstrt,
        bus.denise.ddfstop,
        bus.harddis_active(),
    );
    // Non-zero bitmap behind the pointer so a synthesized re-fetch of the
    // uncaptured line would visibly paint.
    for word in 0..words_per_row {
        write_chip_word(&mut bus, 0x0100 + word * 2, 0xFFFF);
    }
    bus.current_frame_render_base = bus.capture_render_snapshot();
    let captured_row = || {
        Some(CapturedBitplaneRow {
            nplanes: 1,
            words_per_row,
            fetch_origin_cck: None,
            planes: std::array::from_fn(|plane| {
                if plane == 0 {
                    vec![0xFFFF; words_per_row]
                } else {
                    vec![0; words_per_row]
                }
            }),
        })
    };

    let lit_rows = |fb: &[u32]| -> Vec<usize> {
        let background = fb[0];
        (0..FB_HEIGHT)
            .filter(|row| {
                fb[row * FB_WIDTH..(row + 1) * FB_WIDTH]
                    .iter()
                    .any(|&px| px != background)
            })
            .collect()
    };

    // Only line 1 recorded a fetch: line 0's sequencer never ran.
    bus.current_frame_bitplane_rows[1] = captured_row();
    let mut fb_skipped = vec![0u32; FB_PIXELS];
    bitplane::render_from_input(&bitplane::RenderInput::from_bus(&bus), &mut fb_skipped);

    // Both lines recorded fetches: shows where line 0's picture would sit.
    bus.current_frame_bitplane_rows[0] = captured_row();
    let mut fb_both = vec![0u32; FB_PIXELS];
    bitplane::render_from_input(&bitplane::RenderInput::from_bus(&bus), &mut fb_both);

    let skipped = lit_rows(&fb_skipped);
    let both = lit_rows(&fb_both);
    assert!(!skipped.is_empty(), "the captured line must paint");
    assert!(
        both.first() < skipped.first(),
        "the two-capture render must start a line above ({both:?} vs {skipped:?})"
    );
    assert!(
        skipped.iter().all(|row| both.contains(row)),
        "the uncaptured line must not add pixels of its own"
    );
}

#[test]
fn beam_trap_fires_at_exact_position_and_one_shot_disarms() {
    let mut bus = empty_bus();
    // From power-on the beam sits at (0, 0). A one-shot trap two lines
    // and a few clocks ahead fires only once the beam crosses it.
    bus.ui_arm_beam_trap_once(2, Some(10));
    assert!(bus.take_ui_beam_hit().is_none());
    bus.advance_devices(COLORCLOCKS_PER_LINE * 2 + 9); // beam just short of (2, 10)
    assert!(bus.take_ui_beam_hit().is_none());
    bus.advance_devices(1);
    assert_eq!(bus.take_ui_beam_hit(), Some((2, 10)));
    // One-shot: disarmed by the hit; a later frame does not re-fire it.
    assert!(bus.ui_beam_traps().is_empty());
    let frame_lines = bus.agnus.current_frame_lines();
    bus.advance_devices(COLORCLOCKS_PER_LINE * (frame_lines + 3));
    assert!(bus.take_ui_beam_hit().is_none());
}

#[test]
fn beam_trap_line_start_persistent_refires_each_frame() {
    let mut bus = empty_bus();
    // hpos None = the first colour clock of the line.
    assert!(bus.ui_toggle_beam_trap(1, None));
    bus.advance_devices(COLORCLOCKS_PER_LINE);
    assert_eq!(bus.take_ui_beam_hit(), Some((1, 0)));
    // Persistent: still armed, and it fires again a frame later.
    assert_eq!(bus.ui_beam_traps().len(), 1);
    let frame_lines = bus.agnus.current_frame_lines();
    bus.advance_devices(COLORCLOCKS_PER_LINE * (frame_lines + 1));
    assert_eq!(bus.take_ui_beam_hit(), Some((1, 0)));
    // Toggling the same position removes it.
    assert!(!bus.ui_toggle_beam_trap(1, None));
    assert!(bus.ui_beam_traps().is_empty());
}

#[test]
fn beam_trap_beyond_scan_does_not_fire_at_frame_wrap() {
    let mut bus = empty_bus();
    let frame_lines = bus.agnus.current_frame_lines();
    // A trap on a line the scan never reaches must not fire spuriously
    // when the beam order wraps at the end of the frame.
    assert!(bus.ui_toggle_beam_trap((frame_lines + 10).min(u32::from(u16::MAX)) as u16, None));
    bus.advance_devices(COLORCLOCKS_PER_LINE * (frame_lines + 5));
    assert!(bus.take_ui_beam_hit().is_none());
}

#[test]
fn beam_trap_hit_survives_beam_only_advance_without_cpu() {
    // The check lives on the beam advance itself, so a hit lands even
    // when no CPU instruction retires (the CPU sitting in STOP while
    // the chipset free-runs).
    let mut bus = empty_bus();
    bus.ui_arm_beam_trap_once(0, Some(100));
    bus.advance_devices(COLORCLOCKS_PER_LINE * 3);
    assert_eq!(bus.take_ui_beam_hit(), Some((0, 100)));
}

#[test]
fn copper_breakpoint_fires_when_the_list_reaches_the_address() {
    let mut bus = empty_bus();
    let cop1 = 0x0100usize;
    // Two MOVEs then end-of-list; break on the second MOVE's address.
    write_chip_word(&mut bus, cop1, 0x0180);
    write_chip_word(&mut bus, cop1 + 2, 0x0111);
    write_chip_word(&mut bus, cop1 + 4, 0x0182);
    write_chip_word(&mut bus, cop1 + 6, 0x0222);
    write_chip_word(&mut bus, cop1 + 8, 0xFFFF);
    write_chip_word(&mut bus, cop1 + 10, 0xFFFE);
    assert!(bus.ui_toggle_copper_break(cop1 as u32 + 4));

    bus.agnus.dmacon |= DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    bus.copper.jump(cop1 as u32);
    // Nothing fires before the Copper completes the first MOVE.
    assert!(bus.take_ui_copper_hit().is_none());
    bus.advance_chipset(12);
    let (pc, _vpos, _hpos) = bus.take_ui_copper_hit().expect("copper breakpoint hit");
    assert_eq!(pc, cop1 as u32 + 4);
    // Both MOVEs still executed (the breakpoint does not stall the
    // chipset itself; the CPU machine is what pauses).
    assert!(bus.take_ui_copper_hit().is_none());
}

#[test]
fn copper_breakpoint_does_not_refire_while_the_pc_rests_there() {
    let mut bus = empty_bus();
    let cop1 = 0x0200usize;
    // A WAIT that parks the Copper, then a MOVE at the breakpointed
    // address: the PC arrives once and rests during the wait.
    write_copper_wait_then_move(&mut bus, cop1, 0x8001, 0xFFFE, 0x0180, 0x0FFF);
    assert!(bus.ui_toggle_copper_break(cop1 as u32 + 4));

    bus.agnus.dmacon |= DMACON_DMAEN | DMACON_COPEN;
    bus.agnus.hpos = 0x20;
    bus.copper.jump(cop1 as u32);
    bus.advance_chipset(12);
    // The WAIT retired: the PC arrived at the MOVE's address and fired.
    assert!(bus.take_ui_copper_hit().is_some());
    // Many more colour clocks of waiting must not re-fire it.
    bus.advance_chipset(40);
    assert!(bus.take_ui_copper_hit().is_none());
}

#[test]
fn debug_plane_mask_hides_pixels_but_not_collisions() {
    // A one-plane display with a solid row: masking plane 1 must blank
    // the playfield pixels while leaving the collision result untouched
    // (layer isolation is an output-only filter).
    let mut bus = empty_bus();
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x2C81;
    bus.denise.diwstop = 0x2DC1;
    bus.denise.ddfstrt = 0x0038;
    bus.denise.ddfstop = 0x0038;
    bus.denise.bplcon0 = 0x1000;
    bus.denise.palette.write_ocs(0, 0x0007);
    bus.denise.palette.write_ocs(1, 0x0F00);
    // Odd-plane collisions include plane 1 so the clxdat comparison is
    // sensitive to the sample index staying unmasked.
    bus.denise.clxcon = 0x1040;
    let words_per_row = bitplane_words_per_row(
        bus.agnus.revision(),
        bus.denise.bplcon0,
        bus.agnus.fmode(),
        bus.denise.ddfstrt,
        bus.denise.ddfstop,
        bus.harddis_active(),
    );
    bus.current_frame_bitplane_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 1,
        words_per_row,
        fetch_origin_cck: None,
        planes: [
            vec![0xFFFF; words_per_row],
            vec![0; words_per_row],
            vec![0; words_per_row],
            vec![0; words_per_row],
            vec![0; words_per_row],
            vec![0; words_per_row],
            Vec::new(),
            Vec::new(),
        ],
    });
    bus.current_frame_render_base = bus.capture_render_snapshot();

    let full = bitplane::RenderInput::from_bus(&bus).with_debug_masks(0xFF, 0xFF);
    let masked = bitplane::RenderInput::from_bus(&bus).with_debug_masks(0xFE, 0xFF);

    let mut fb_full = vec![0u32; FB_PIXELS];
    let mut fb_masked = vec![0u32; FB_PIXELS];
    let result_full = bitplane::render_from_input(&full, &mut fb_full);
    let result_masked = bitplane::render_from_input(&masked, &mut fb_masked);

    assert!(
        fb_full.iter().any(|&px| px != fb_full[0]),
        "the unmasked frame must show the plane"
    );
    assert_ne!(
        fb_full, fb_masked,
        "masking the only plane must change the picture"
    );
    assert_eq!(
        result_full.clxdat, result_masked.clxdat,
        "layer isolation must never change the collision result"
    );
    // The plane's colour (COLOR01 = $F00, bright red in framebuffer RGBA)
    // is present unmasked and fully gone when the plane is hidden.
    let red = 0xFF00_00FF_u32;
    assert!(
        fb_full.contains(&red),
        "the unmasked frame must contain the plane colour"
    );
    assert!(
        !fb_masked.contains(&red),
        "a masked plane's colour must not reach the framebuffer"
    );
}

/// OCS/ECS BPLCON1 scroll nibbles count lo-res pixels, so a hi-res
/// playfield shifts two hi-res samples per scroll step (Denise reloads the
/// shifter when the low 3 nibble bits match the pixel counter; the vAmiga
/// event model encodes the same grid as scrollOdd cck phase plus a
/// 2-hires-pixel LSB offset). With the scroll halved, the whole picture
/// sat one colour clock left of hardware. Regression example: Kickstart
/// 2.05's insert-disk screen (hi-res, DDFSTRT $40, DIWSTRT hstart $95,
/// BPLCON1 $44, BPL1MOD -6) clipped the first text column at the window's
/// left edge and leaked the negative-modulo overlap words - the NEXT
/// row's first characters - into the last pixels before the right edge.
#[test]
fn ecs_hires_bplcon1_scroll_counts_lores_pixels_on_late_ddf_row() {
    let mut bus = empty_bus();
    bus.set_agnus_revision(AgnusRevision::Ecs8375);
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
    bus.denise.diwstrt = 0x6395;
    bus.denise.diwstop = 0xF4AD;
    bus.denise.ddfstrt = 0x0040;
    bus.denise.ddfstop = 0x00D0;
    bus.denise.bplcon0 = 0x9200;
    bus.denise.bplcon1 = 0x0044;
    // Re-fetch the identical 38-word row on every line, mirroring the boot
    // screen's negative-modulo overlap in the simplest form.
    bus.denise.bpl1mod = -76;
    bus.denise.palette.write_ocs(1, 0x0FFF);
    // Comb bitmap: a marker at the start of every fetched word, with the
    // second and third words width-coded so the placement is unambiguous.
    for w in 0..38 {
        let val = match w {
            1 => 0xC000,
            2 => 0xE000,
            _ => 0x8000,
        };
        write_chip_word(&mut bus, 0x1000 + w * 2, val);
    }
    bus.denise.bplpt[0] = 0x1000;
    bus.display_dma_bplpt[0] = 0x1000;
    bus.current_frame_render_base = bus.capture_render_snapshot();

    bus.agnus.vpos = 0x63;
    bus.agnus.hpos = 0;
    for _ in 0..2 * COLORCLOCKS_PER_LINE {
        bus.advance_chipset(1);
    }

    let mut fb = vec![0; FB_PIXELS];
    bitplane::render(&mut bus, &mut fb);
    let white = rgb12_to_rgba8(0x0FFF);
    // The window spans fb x [102, 662). DDFSTRT $40 places the row's first
    // sample 4 colour clocks (16 hi-res px) right of the standard $3C
    // picture, and the scroll delays it another 8 hi-res px (4 lo-res px),
    // so sample 0 sits at fb x 86 and the window edge shows fetch x 16.
    let row = (0x63 + 1 - 0x2C) * FB_WIDTH;
    // Word 1's two-pixel marker (fetch x 16, 17) is flush at the window
    // edge: the first character column is fully visible.
    assert_eq!(fb[row + 102], white, "fetch x 16 at the window edge");
    assert_eq!(fb[row + 103], white, "fetch x 17 beside it");
    assert_ne!(fb[row + 104], white, "marker is exactly two px wide");
    // Word 2's three-pixel marker confirms the origin (86 + 32 = 118).
    for x in 118..=120 {
        assert_eq!(fb[row + x], white, "word 2 marker at x={x}");
    }
    assert_ne!(fb[row + 121], white, "marker is exactly three px wide");
    // The last visible word marker is fetch x 560 at fb x 646. The three
    // overlap words (fetch x 576+) belong past the window's right edge and
    // must not leak inside it: with the hi-res scroll halved they painted
    // at fb x 658+, which showed the next row's first characters.
    assert_eq!(fb[row + 646], white, "fetch x 560 still inside the window");
    for x in 647..662 {
        assert_ne!(
            fb[row + x],
            white,
            "overlap word content leaked inside the window at x={x}"
        );
    }
}

// ---------------------------------------------------------------------------
// Waveform (VCD) capture taps
// ---------------------------------------------------------------------------

fn temp_vcd_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("copperline-wave-{}-{name}.vcd", std::process::id()))
}

fn wave_opts(path: std::path::PathBuf) -> crate::waveform::WaveOptions {
    crate::waveform::WaveOptions::new(path)
}

#[test]
fn waveform_capture_records_beam_and_owner_changes() {
    let mut bus = empty_bus();
    let path = temp_vcd_path("beam-owner");
    let _ = std::fs::remove_file(&path);
    let mut opts = wave_opts(path.clone());
    opts.duration = crate::waveform::WaveDuration::Cck(500);
    bus.wave_arm(opts).unwrap();
    assert_eq!(bus.wave_status().unwrap().state, "capturing");

    // Start a small A->D blit so the blitter's owner slots and pipeline
    // labels land in the trace alongside the idle bus's refresh slots.
    bus.agnus.dmacon = DMACON_DMAEN | DMACON_BLTEN;
    bus.blitter.bltcon0 = 0x09F0;
    bus.blitter.start_scheduled((4 << 6) | 2, &bus.mem.chip_ram);

    // Refresh slots, beam movement, and line rollovers are sampled even
    // with the CPU quiet. Run well past the 500 cck window so it expires.
    for _ in 0..2000 {
        bus.advance_one_chip_bus_quantum(None);
    }
    let status = bus.wave_status().unwrap();
    assert_eq!(status.state, "done");
    assert!(status.samples > 0, "{status:?}");
    assert!(status.captured_cck >= 490, "{status:?}");

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("$timescale 1 us $end"), "{text}");
    assert!(text.contains("$enddefinitions $end"));
    // All eight signal-group scopes are declared by default.
    for scope in [
        "beam", "bus", "cpu", "copper", "blitter", "regs", "irq", "audio",
    ] {
        assert!(
            text.contains(&format!("$scope module {scope} $end")),
            "missing scope {scope}"
        );
    }
    // The beam moved: many timestamps, and hpos changes between them.
    let stamps = text.lines().filter(|l| l.starts_with('#')).count();
    assert!(stamps > 50, "only {stamps} timestamps:\n{text}");
    // Owner names appear as string-var changes (refresh slots at least).
    assert!(text.contains("srefresh "), "{text}");
    // The blit shows up as the blitter owning bus slots and as its
    // pipeline labels (A fetches and D writes for an A->D copy).
    assert!(text.contains("sblitter "), "{text}");
    assert!(text.contains("sA "), "{text}");
    assert!(text.contains("sD "), "{text}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn waveform_reg_trigger_fires_on_custom_register_write() {
    let mut bus = empty_bus();
    let path = temp_vcd_path("reg-trigger");
    let _ = std::fs::remove_file(&path);
    let mut opts = wave_opts(path.clone());
    opts.trigger = crate::waveform::Trigger::RegWrite(0x180);
    opts.duration = crate::waveform::WaveDuration::Cck(100);
    bus.wave_arm(opts).unwrap();

    // Unrelated writes and bus activity leave the capture armed.
    bus.write_custom_word_from(0x096, 0x8000, BeamWriteSource::Cpu);
    for _ in 0..50 {
        bus.advance_one_chip_bus_quantum(None);
    }
    assert_eq!(bus.wave_status().unwrap().state, "armed");

    // The matching write (COLOR00) fires the trigger and is itself the
    // first recorded regs-group sample.
    bus.write_custom_word_from(0x180, 0x0F0F, BeamWriteSource::Cpu);
    assert_eq!(bus.wave_status().unwrap().state, "capturing");
    for _ in 0..500 {
        bus.advance_one_chip_bus_quantum(None);
    }
    let status = bus.wave_status().unwrap();
    assert_eq!(status.state, "done");

    let text = std::fs::read_to_string(&path).unwrap();
    // The regs group recorded offset $180 (9-bit vector 110000000) and
    // value $0F0F at the trigger point, attributed to the CPU.
    assert!(text.contains("b110000000 "), "{text}");
    assert!(text.contains("b0000111100001111 "), "{text}");
    assert!(text.contains("scpu "), "{text}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn waveform_stop_finishes_early_and_rearm_replaces() {
    let mut bus = empty_bus();
    let path = temp_vcd_path("stop-early");
    let _ = std::fs::remove_file(&path);
    let mut opts = wave_opts(path.clone());
    opts.duration = crate::waveform::WaveDuration::Secs(5.0);
    bus.wave_arm(opts).unwrap();
    for _ in 0..50 {
        bus.advance_one_chip_bus_quantum(None);
    }
    let status = bus.wave_stop().unwrap();
    assert_eq!(status.state, "done");
    assert!(bus.wave_status().is_none());
    // The finished file is flushed and parseable.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("$enddefinitions $end"));

    // Re-arming with a beam trigger works after a stop, and the beam
    // crossing fires it (line 5, any hpos).
    let path2 = temp_vcd_path("beam-trigger");
    let _ = std::fs::remove_file(&path2);
    let mut opts = wave_opts(path2.clone());
    opts.trigger = crate::waveform::Trigger::Beam {
        vpos: 5,
        hpos: None,
    };
    opts.duration = crate::waveform::WaveDuration::Cck(50);
    bus.wave_arm(opts).unwrap();
    assert_eq!(bus.wave_status().unwrap().state, "armed");
    // 5 lines of 227 cck arrive within ~2000 quanta on an idle bus.
    for _ in 0..4000 {
        bus.advance_one_chip_bus_quantum(None);
        if bus.wave_status().unwrap().state != "armed" {
            break;
        }
    }
    assert_ne!(bus.wave_status().unwrap().state, "armed");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&path2);
}

/// The debugger-facing helpers must reach the Ramsey motherboard RAM bank:
/// peek_word_any serves memory dumps at its bus address and
/// writable_ram_regions offers it to the memory hunt.
#[test]
fn motherboard_ram_reaches_the_debugger_helpers() {
    let mut bus = empty_bus();
    bus.mem.fit_mb_ram(1024 * 1024);
    let base = bus.mem.mb_ram_base() as u32;
    assert_eq!(base, 0x07F0_0000);
    let top = bus.mem.mb_ram.len();
    bus.mem.mb_ram[top - 2] = 0xBE;
    bus.mem.mb_ram[top - 1] = 0xEF;
    assert_eq!(bus.peek_word_any(0x07FF_FFFE), 0xBEEF);
    // Below the fitted bank nothing answers.
    assert_eq!(bus.peek_word_any(base - 2), 0);
    assert!(bus
        .writable_ram_regions()
        .contains(&(base, 1024 * 1024_u32)));
}

/// A debugger pattern search sweeps the decoded map, so it must offer the
/// RAM banks that sit past the 24-bit space -- the Ramsey motherboard
/// bank, the CPU-slot accelerator bank, and Zorro III boards -- alongside
/// the chip window and the ROMs. Chip RAM is offered as the whole
/// $000000-$1FFFFF select window so the Agnus image repeats are searched.
#[test]
fn searchable_regions_cover_the_32_bit_ram_banks() {
    let mut bus = empty_bus();
    bus.mem.fit_mb_ram(4 * 1024 * 1024);
    bus.mem.fit_accel_ram(8 * 1024 * 1024);
    bus.mem
        .zorro
        .add_board_configured_at(
            crate::zorro::BoardSpec::z3_ram(16 * 1024 * 1024),
            0x1000_0000,
        )
        .expect("Z3 RAM board");

    let regions = bus.searchable_regions();
    assert!(regions.contains(&(0, crate::memory::CHIP_WINDOW_SIZE as u32)));
    assert!(regions.contains(&(0x07C0_0000, 4 * 1024 * 1024)));
    assert!(regions.contains(&(crate::memory::ACCEL_RAM_BASE as u32, 8 * 1024 * 1024)));
    assert!(
        regions
            .iter()
            .any(|(base, len)| *base >= 0x1000_0000 && *len == 16 * 1024 * 1024),
        "Zorro III RAM board missing: {regions:08X?}"
    );
    assert!(regions.contains(&(crate::memory::ROM_BASE as u32, 512 * 1024)));
    assert!(
        regions.windows(2).all(|w| w[0].0 <= w[1].0),
        "regions must be in ascending address order: {regions:08X?}"
    );
}
