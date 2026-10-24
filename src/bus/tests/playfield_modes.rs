// A500 measurements: amiga-hardware-probes/references/playfield-modes-v1.
// ROM 3a614aba9686ad37d83a210a0016b6ec870e36d1fa07294ba0e4ab0c31d82765.
use super::*;

#[test]
fn a500_copper_gates_pin_match_delay_startup_and_first_last_bits() {
    let mut count = 0;
    for line in include_str!("data/a500_playfield_gates.csv")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.starts_with("kind,"))
    {
        let fields: Vec<u16> = line.split(',').map(|v| v.parse().unwrap()).collect();
        let [kind, target, variant, pattern, delay, expected]: [u16; 6] =
            fields.try_into().unwrap();
        let mut bus = empty_bus();
        bus.denise.bplcon0 = 0x2200 | ((variant & 1) * 0x0400);
        bus.denise.clxcon = 0x0820;
        bus.denise.diwstrt = if variant & 4 != 0 { 0x2c91 } else { 0x2c81 };
        bus.denise.diwstop = 0xf4c1;
        bus.denise.ddfstrt = if variant & 2 != 0 { 0x0040 } else { 0x0038 };
        bus.denise.ddfstop = 0x00d0;
        let snapshot = bus.capture_render_snapshot();
        let control = LiveCollisionControl::from_snapshot(snapshot);
        let varying = 0x30 + 2 * u32::from(delay);
        let (opening, closing) = if target & 16 != 0 {
            (0x20, varying)
        } else {
            (varying, 0x70)
        };
        let mask = if target & 15 == 4 { 0x0040 } else { 0x00c3 };
        for (wait, value) in [(opening, mask), (closing, 0x0820)] {
            bus.agnus.vpos = 0x44;
            // Finite list: MOVE data slot follows WAIT release by two CCK.
            bus.agnus.hpos = wait + 2;
            bus.record_render_write(0x098, value, BeamWriteSource::Copper);
            assert_eq!(
                bus.current_frame_collision_control_events
                    .last()
                    .unwrap()
                    .hpos,
                wait + 4,
            );
        }
        let index =
            BeamEventIndex::from_register_writes(&bus.current_frame_collision_control_events);
        let replay = LiveCollisionLineReplay::from_index(control, snapshot, &index, 0x44);
        let word = if kind == 104 {
            if pattern == 3 {
                0xffff
            } else {
                0
            }
        } else {
            [0, 0xffff, 0x8000, 1][pattern as usize]
        };
        let words = if variant & 2 != 0 { 19 } else { 20 };
        let row = CapturedBitplaneRow {
            nplanes: 2,
            words_per_row: words,
            fetch_origin_cck: Some(bus.denise.ddfstrt),
            planes: std::array::from_fn(|plane| vec![if plane < 2 { word } else { 0 }; words]),
        };
        assert_eq!(
            live_bitplane_collision_bits(&row, &replay, 0x44),
            expected,
            "kind={kind} target={target} variant={variant} pattern={pattern} delay={delay}",
        );
        count += 1;
    }
    assert_eq!(count, 896);
}

#[test]
fn clxcon_comparator_delay_wraps_independently_of_render_write_delay() {
    let mut bus = empty_bus();
    let line_cck = bus.agnus.current_line_cck();
    bus.agnus.vpos = 0x44;
    bus.agnus.hpos = line_cck - 3;
    bus.record_render_write(0x098, 0x00c3, BeamWriteSource::Copper);
    let collision = bus.current_frame_collision_control_events.last().unwrap();
    let render = bus.current_frame_render_events.last().unwrap();
    assert_eq!((collision.vpos, collision.hpos), (0x44, line_cck - 1));
    assert_eq!((render.vpos, render.hpos), (0x45, 1));
}

fn hardware_cases() -> impl Iterator<Item = [u16; 5]> {
    include_str!("data/a500_playfield_modes.csv")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.starts_with("target,"))
        .map(|line| {
            let values: Vec<u16> = line.split(',').map(|v| v.parse().unwrap()).collect();
            values.try_into().unwrap()
        })
}

#[test]
fn a500_paired_single_dual_sprite_playfield_truth_table() {
    let mut count = 0;
    for [target, pixel, sprite, dual, expected] in hardware_cases() {
        let clxcon = ((target >> 2) << 6) | (target & 3);
        let collision =
            super::super::live_playfield_collision_pixel(pixel as u8, clxcon, 0, dual != 0);
        let actual = if sprite != 0 {
            u16::from(collision.pf1_match) * 2 + u16::from(collision.pf2_match) * 32
        } else {
            0
        };
        assert_eq!(
            actual,
            expected & 0x22,
            "target={target} pixel={pixel} sprite={sprite} DPF={dual}"
        );
        count += 1;
    }
    assert_eq!(count, 256);
}

#[test]
fn a500_paired_single_dual_pf_pf_uses_match_masks_including_zero_pixels() {
    for [target, pixel, sprite, dual, expected] in hardware_cases() {
        // The probe clears BPLDAT before constant DMA data arrives. Capture
        // both the initial zero sample and the constant fetched sample.
        let row = CapturedBitplaneRow {
            nplanes: 2,
            words_per_row: 2,
            fetch_origin_cck: None,
            planes: std::array::from_fn(|plane| {
                if plane < 2 {
                    vec![0, if pixel & (1 << plane) != 0 { 0xffff } else { 0 }]
                } else {
                    vec![0; 2]
                }
            }),
        };
        let clxcon = ((target >> 2) << 6) | (target & 3);
        let control = LiveCollisionControl::from_current(
            AgnusRevision::Ocs,
            0x2200 | (dual * 0x0400),
            0,
            0,
            clxcon,
            0,
            0x2c81,
            0x2dc1,
            DiwHigh::ocs_implicit(),
            0x0038,
            [0; 8],
        );
        let replay = LiveCollisionLineReplay::from_index(
            control,
            RenderRegisterSnapshot::default(),
            &BeamEventIndex::from_register_writes(&[]),
            RENDER_VISIBLE_START_VPOS as i32,
        );
        assert_eq!(
            live_bitplane_collision_bits(&row, &replay, RENDER_VISIBLE_START_VPOS as i32),
            expected & 1,
            "target={target} pixel={pixel} sprite={sprite} DPF={dual}",
        );
    }
}

#[test]
fn a500_single_and_dual_live_readback_respects_zero_and_nonzero_match_values() {
    for dual in [false, true] {
        for (pixel, desired, expected) in [(3, 3, 1), (3, 1, 0), (0, 0, 1), (0, 3, 0)] {
            let mut bus = empty_bus();
            bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
            bus.agnus.vpos = 0x2c;
            bus.agnus.hpos = 0x3a;
            bus.denise.diwstrt = 0x2c81;
            bus.denise.diwstop = 0x2dc1;
            bus.denise.ddfstrt = 0x0038;
            bus.denise.ddfstop = 0x0038;
            bus.denise.bplcon0 = if dual { 0x2600 } else { 0x2200 };
            bus.denise.clxcon = 0x00c0 | desired;
            for (plane, address) in [(0, 0x100), (1, 0x200)] {
                bus.denise.bplpt[plane] = address as u32;
                bus.display_dma_bplpt[plane] = address as u32;
                write_chip_word(
                    &mut bus,
                    address,
                    if pixel & (1 << plane) != 0 { 0xffff } else { 0 },
                );
            }
            advance_collision_fixture_observation(&mut bus, 12);
            assert_eq!(
                bus.custom_read(0x00e, 2) & 1,
                expected,
                "DPF={dual} pixel={pixel} desired={desired}"
            );
        }
    }
}
