// A500 measurements: amiga-hardware-probes/references/playfield-modes-v1.
// ROM 3a614aba9686ad37d83a210a0016b6ec870e36d1fa07294ba0e4ab0c31d82765.
use super::*;

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
#[ignore = "Known A500 PF-PF mask/zero-boundary mismatch; strict ROM replay requires 512 matches"]
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
#[ignore = "Known A500 PF-PF readback mismatch; see hardware matrix and zero-burst references"]
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
