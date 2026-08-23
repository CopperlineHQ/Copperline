// SPDX-License-Identifier: GPL-3.0-or-later

//! Unit tests for the bitplane renderer: split out of `bitplane.rs`
//! for size, they are the same `bitplane::tests` module and keep
//! full access to the parent's private items via `super::`.

use super::*;
use crate::bus::{BeamRegisterWrite, BeamWriteSource};

/// Single-span window row matching the control's display_window_x, for
/// direct render_planned_playfield_line tests.
fn h_row_for(control: ControlState) -> HWindowRow {
    HWindowRow {
        open_runs: vec![control.display_window_x()],
        comparator_anchor: Some(control.display_window_x().0),
    }
}

fn repeated_frame_test_input() -> RenderInput {
    let mut row = CapturedBitplaneRow {
        nplanes: 1,
        words_per_row: 1,
        planes: std::array::from_fn(|_| Vec::new()),
        fetch_origin_cck: None,
    };
    row.planes[0].push(0xA55A);
    RenderInput {
        geometry: FrameGeometry::standard(PAL_VISIBLE_LINE0 as u32, 312, false),
        presentation_h_window: None,
        presentation_v_window: None,
        visible_start_vpos: PAL_VISIBLE_LINE0 as u32,
        palette_split: (Palette::default(), Palette::default(), false),
        render_base: RenderRegisterSnapshot::default(),
        frame_render_events: Vec::new(),
        current_render_base: RenderRegisterSnapshot::default(),
        current_render_events: Vec::new(),
        bottom_palette_events: Vec::new(),
        top_palette_end: Palette::default(),
        chip_ram: std::sync::Arc::new(vec![0; 1024]),
        chip_ram_writes: Vec::new(),
        captured_bitplane_rows: std::sync::Arc::new(vec![Some(row)]),
        captured_sprite_lines: Vec::new(),
        held_sprites: [None; 8],
        sprite_display_enable_x_by_y: Vec::new(),
        sprite_dma_observed: false,
        frame_lines: 312,
        programmable_vertical_blank: None,
        programmable_horizontal_blank: None,
        emulated_seconds: 1.0,
        emulated_frames: 50,
        debug_plane_mask: 0xFF,
        debug_sprite_mask: 0xFF,
    }
}

fn repeated_frame_test_result(chip_ram_reads: Option<Vec<ChipRamReadDependency>>) -> RenderResult {
    RenderResult {
        timing: VideoRenderFrameTiming::default(),
        clxdat: 0x1234,
        chip_ram_reads,
    }
}

#[test]
fn repeated_frame_detector_ignores_time_but_matches_captured_content_exactly() {
    let first = repeated_frame_test_input();
    let mut detector = RepeatedFrameDetector::default();
    let mut result = repeated_frame_test_result(Some(Vec::new()));
    detector.note_rendered(&first, &mut result);
    assert!(!detector.can_reuse(&first));
    let mut repeated_result = repeated_frame_test_result(Some(Vec::new()));
    detector.note_rendered(&first, &mut repeated_result);

    let mut same_pixels = repeated_frame_test_input();
    same_pixels.emulated_seconds = 99.0;
    same_pixels.emulated_frames = 4_950;
    // Unobserved RAM is intentionally outside the key: the prior render
    // proved that all visible words came from exact DMA/register capture.
    same_pixels.chip_ram = std::sync::Arc::new(vec![0xCC; 1024]);
    assert!(detector.can_reuse(&same_pixels));
    assert_eq!(detector.reused_clxdat(), 0x1234);

    std::sync::Arc::make_mut(&mut same_pixels.captured_bitplane_rows)[0]
        .as_mut()
        .unwrap()
        .planes[0][0] ^= 1;
    assert!(!detector.can_reuse(&same_pixels));
}

#[test]
fn repeated_frame_detector_matches_ram_dependencies_and_rejects_incomplete_or_interlaced_keys() {
    let progressive = repeated_frame_test_input();
    let mut detector = RepeatedFrameDetector::default();
    let mut result = repeated_frame_test_result(Some(vec![ChipRamReadDependency {
        addr: 0,
        vpos: 44,
        hpos: 0x38,
        value: 0,
    }]));
    detector.note_rendered(&progressive, &mut result);
    assert!(!detector.can_reuse(&progressive));
    let mut repeated_result = repeated_frame_test_result(Some(vec![ChipRamReadDependency {
        addr: 0,
        vpos: 44,
        hpos: 0x38,
        value: 0,
    }]));
    detector.note_rendered(&progressive, &mut repeated_result);
    assert!(detector.can_reuse(&progressive));

    let mut changed_dependency = repeated_frame_test_input();
    std::sync::Arc::make_mut(&mut changed_dependency.chip_ram)[0] = 1;
    assert!(!detector.can_reuse(&changed_dependency));

    let mut incomplete = repeated_frame_test_result(None);
    detector.note_rendered(&progressive, &mut incomplete);
    assert!(!detector.can_reuse(&progressive));

    let mut interlaced = repeated_frame_test_input();
    interlaced.render_base.bplcon0 |= 0x0004;
    let mut interlaced_result = repeated_frame_test_result(Some(Vec::new()));
    detector.note_rendered(&interlaced, &mut interlaced_result);
    assert!(!detector.can_reuse(&interlaced));
}

#[test]
fn static_h_window_transition_solver_matches_counter_replay() {
    let mut random = 0xC0_77_E2_11u32;
    let beam_lines = [0, 1, 8, 9, PAL_VISIBLE_LINE0, 255, 311];

    for case in 0..4096 {
        random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let hstart = ((random >> 3) & 0x1FF) as u16;
        random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let hstop = ((random >> 7) & 0x1FF) as u16;
        let control = ControlState {
            diwstrt: hstart & 0x00FF,
            diwstop: hstop & 0x00FF,
            diwhigh: DiwHigh::ecs_explicit(((hstart >> 8) << 5) | ((hstop >> 8) << 13)),
            ..ControlState::default()
        };
        let beam_line = beam_lines[case % beam_lines.len()];
        let is_ecs = case & 1 != 0;
        // A segment whose effect lies beyond the line selects the original
        // per-tick implementation without changing its comparator values.
        let inert_segment = [ControlSegment { x: 2000, control }];

        for initial_flop in [false, true] {
            let mut solved_flop = initial_flop;
            let mut solved = HWindowRow::default();
            scan_h_window_line(
                &mut solved_flop,
                beam_line,
                is_ecs,
                control,
                &[],
                Some(&mut solved),
            );

            let mut replayed_flop = initial_flop;
            let mut replayed = HWindowRow::default();
            scan_h_window_line(
                &mut replayed_flop,
                beam_line,
                is_ecs,
                control,
                &inert_segment,
                Some(&mut replayed),
            );

            assert_eq!(solved_flop, replayed_flop, "case {case}");
            assert_eq!(solved.open_runs, replayed.open_runs, "case {case}");
            assert_eq!(
                solved.comparator_anchor, replayed.comparator_anchor,
                "case {case}"
            );
        }
    }
}

#[test]
fn h_window_flop_carries_open_past_line_with_unreachable_hstart() {
    // Standard window rows, then a late-line DIWSTOP rewrite to $2C00
    // before the standard stop matched: the flip-flop stays open across
    // the line boundary. The following row (hstart $00 unreachable,
    // hstop $100) is open from the left framebuffer edge until $100.
    let standard = ControlState {
        diwstrt: 0x2C81,
        diwstop: 0x2CC1,
        agnus_revision: AgnusRevision::Ocs,
        ..ControlState::default()
    };
    let degenerate = ControlState {
        diwstrt: 0x2C00,
        diwstop: 0x2C00,
        ..standard
    };
    let base_controls = [standard, standard, degenerate];
    let mut control_segments = vec![Vec::new(); 3];
    // Late write on row 1 (copper-x 644 = cck $C9), before the standard
    // stop's comparator position.
    control_segments[1].push(ControlSegment {
        x: 644,
        control: degenerate,
    });
    let rows = compute_h_window_rows(&base_controls, &control_segments, PAL_VISIBLE_LINE0);
    // Row 0: standard window (edge at 2H-196, hardware-verified).
    assert_eq!(rows[0].open_runs(), &[(62, 702)]);
    // Row 1: opens at the standard hstart; the rewritten stop never
    // matches before the line ends, so the run reaches the edge.
    assert_eq!(rows[1].open_runs(), &[(62, FB_WIDTH)]);
    // Row 2: carried open, closes at hstop $100.
    assert_eq!(rows[2].open_runs(), &[(0, 316)]);
}

#[test]
fn programmable_blanking_blanks_vbstrt_vbstop_rows_under_varvben() {
    use crate::chipset::agnus::{
        Agnus, AgnusRevision, VideoStandard, BEAMCON0_PAL, BEAMCON0_VARVBEN,
    };

    let mut agnus =
        Agnus::with_video_standard_and_revision(VideoStandard::Pal, AgnusRevision::Ecs8372Rev4);
    agnus.write_beamcon0(BEAMCON0_PAL | BEAMCON0_VARVBEN);
    // Blank beam lines 0x50..0x58 (rows 0x24..0x2C of the canvas).
    agnus.write_vbstrt(0x50);
    agnus.write_vbstop(0x58);

    const FILL: u32 = 0xFFAA_BBCC;
    let mut fb = vec![FILL; FB_PIXELS];
    apply_programmable_blanking(
        agnus.programmable_vertical_blank(),
        agnus.programmable_horizontal_blank(),
        &mut fb,
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
    );

    let row = |fb: &[u32], y: usize| fb[y * FB_WIDTH];
    assert_eq!(row(&fb, 0x23), FILL, "line before VBSTRT untouched");
    assert_eq!(row(&fb, 0x24), 0xFF00_0000, "VBSTRT line blanked");
    assert_eq!(row(&fb, 0x2B), 0xFF00_0000, "last blanked line");
    assert_eq!(row(&fb, 0x2C), FILL, "VBSTOP line shows again");

    // Without VARVBEN the window is ignored.
    agnus.write_beamcon0(BEAMCON0_PAL);
    let mut fb = vec![FILL; FB_PIXELS];
    apply_programmable_blanking(
        agnus.programmable_vertical_blank(),
        agnus.programmable_horizontal_blank(),
        &mut fb,
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
    );
    assert_eq!(row(&fb, 0x24), FILL);
}

#[test]
fn programmable_blanking_blanks_hbstrt_hbstop_columns_under_blanken() {
    use crate::chipset::agnus::{
        Agnus, AgnusRevision, VideoStandard, BEAMCON0_BLANKEN, BEAMCON0_PAL,
    };

    let mut agnus =
        Agnus::with_video_standard_and_revision(VideoStandard::Pal, AgnusRevision::Ecs8372Rev4);
    agnus.write_beamcon0(BEAMCON0_PAL | BEAMCON0_BLANKEN);
    // Blank colour clocks 0x40..0x48. CCK 0x40 = DIW position 0x80 =
    // framebuffer x (0x80 - DIW_HSTART_FB0) * 2 = 0x3E.
    agnus.write_hbstrt(0x40);
    agnus.write_hbstop(0x48);

    const FILL: u32 = 0xFFAA_BBCC;
    let mut fb = vec![FILL; FB_PIXELS];
    apply_programmable_blanking(
        agnus.programmable_vertical_blank(),
        agnus.programmable_horizontal_blank(),
        &mut fb,
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
    );

    let x_first = ((0x80 - DIW_HSTART_FB0) * 2) as usize;
    let x_last = ((0x90 - DIW_HSTART_FB0) * 2) as usize - 1;
    assert_eq!(fb[x_first - 1], FILL, "pixel before HBSTRT untouched");
    assert_eq!(fb[x_first], 0xFF00_0000, "HBSTRT pixel blanked");
    assert_eq!(fb[x_last], 0xFF00_0000, "last blanked pixel");
    assert_eq!(fb[x_last + 1], FILL, "HBSTOP pixel shows again");
    // Applies to every row.
    assert_eq!(fb[100 * FB_WIDTH + x_first], 0xFF00_0000);
}

#[test]
fn programmable_blanking_requires_ecs_and_wraps() {
    use crate::chipset::agnus::{
        Agnus, AgnusRevision, VideoStandard, BEAMCON0_PAL, BEAMCON0_VARVBEN,
    };

    // OCS Agnus: BEAMCON0 writes are dropped, so nothing blanks.
    let mut ocs = Agnus::with_video_standard(VideoStandard::Pal);
    ocs.write_beamcon0(BEAMCON0_PAL | BEAMCON0_VARVBEN);
    ocs.write_vbstrt(0x50);
    ocs.write_vbstop(0x58);
    const FILL: u32 = 0xFFAA_BBCC;
    let mut fb = vec![FILL; FB_PIXELS];
    apply_programmable_blanking(
        ocs.programmable_vertical_blank(),
        ocs.programmable_horizontal_blank(),
        &mut fb,
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
    );
    assert!(fb.iter().all(|&px| px == FILL));

    // VBSTRT >= VBSTOP wraps through the frame top: everything from
    // VBSTRT down plus the top rows below VBSTOP is blanked.
    let mut ecs =
        Agnus::with_video_standard_and_revision(VideoStandard::Pal, AgnusRevision::Ecs8372Rev4);
    ecs.write_beamcon0(BEAMCON0_PAL | BEAMCON0_VARVBEN);
    ecs.write_vbstrt(0x120);
    ecs.write_vbstop(0x30);
    let mut fb = vec![FILL; FB_PIXELS];
    apply_programmable_blanking(
        ecs.programmable_vertical_blank(),
        ecs.programmable_horizontal_blank(),
        &mut fb,
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
    );
    let row = |fb: &[u32], y: usize| fb[y * FB_WIDTH];
    // Beam line 0x2C (row 0) is below VBSTOP 0x30: blanked.
    assert_eq!(row(&fb, 0), 0xFF00_0000);
    assert_eq!(row(&fb, 0x30 - 0x2C), FILL, "VBSTOP clears the blank");
    assert_eq!(row(&fb, 0x120 - 0x2C), 0xFF00_0000, "VBSTRT asserts again");
}

/// Plan 3.4: AGA sprite colours come from the BPLCON4 ESPRM/OSPRM
/// banks; pre-AGA stays on the classic 16..31 block.
#[test]
fn sprite_color_entry_follows_bplcon4_on_aga() {
    let ocs = ControlState::default();
    assert_eq!(sprite_color_entry(ocs, 0, 1, false), 17);
    assert_eq!(sprite_color_entry(ocs, 6, 3, false), 16 + 12 + 3);
    assert_eq!(sprite_color_entry(ocs, 0, 9, true), 25);

    // AGA with the reset default 0x0011: same 16..31 block.
    let aga_default = ControlState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon4: 0x0011,
        ..ControlState::default()
    };
    assert_eq!(sprite_color_entry(aga_default, 0, 1, false), 17);
    assert_eq!(sprite_color_entry(aga_default, 1, 1, false), 17);

    // Distinct even/odd banks: ESPRM=7 (even sprites and attached
    // pairs at 112..), OSPRM=2 (odd sprites at 32..).
    let aga = ControlState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon4: 0x0027,
        ..ControlState::default()
    };
    assert_eq!(sprite_color_entry(aga, 0, 1, false), 112 + 1);
    assert_eq!(sprite_color_entry(aga, 1, 1, false), 32 + 1);
    assert_eq!(sprite_color_entry(aga, 4, 2, false), 112 + 8 + 2);
    assert_eq!(sprite_color_entry(aga, 2, 9, true), 112 + 9);
}

#[test]
fn present_h_shift_centres_standard_window() {
    // Stock PAL DIW ($2C81/$2CC1 -> H 0x81/0x1C1). 62px left border vs
    // 14px right border; recentre by half the difference = 24px.
    assert_eq!(present_h_shift(0x81, 0x1C1), 24);
}

#[test]
fn present_h_shift_leaves_overscan_frames_untouched() {
    // Left overscan (DIWSTRT H left of standard).
    assert_eq!(present_h_shift(0x41, 0x1C1), 0);
    // Right overscan (DIWSTOP H past standard).
    assert_eq!(present_h_shift(0x81, 0x1D4), 0);
    // Both.
    assert_eq!(present_h_shift(0x71, 0x1E0), 0);
}

#[test]
fn content_window_h_matches_standard_diw_for_stock_ddf() {
    // A standard lo-res fetch ($38..$D0) covers exactly the standard DIW
    // window, so a stock display's presentation centring is unchanged.
    let lores = ControlState {
        ddfstrt: 0x0038,
        ddfstop: 0x00D0,
        ..ControlState::default()
    };
    assert_eq!(
        lores.bitplane_content_window_h(),
        Some((STANDARD_DIW_HSTART, STANDARD_DIW_HSTOP))
    );
    // The standard hi-res fetch ($3C..$D4) covers the same window.
    let hires = ControlState {
        bplcon0: 0x8000,
        ddfstrt: 0x003C,
        ddfstop: 0x00D4,
        ..ControlState::default()
    };
    assert_eq!(
        hires.bitplane_content_window_h(),
        Some((STANDARD_DIW_HSTART, STANDARD_DIW_HSTOP))
    );
}

#[test]
fn early_ddf_hires_origin_clips_prefetch_against_the_window_edge() {
    // XSysInfo's hardware-information panel: hi-res, FMODE=0, DDFSTRT=$38
    // (the lo-res "Normal" slot) - one 4-cck fetch word earlier than the
    // hi-res standard $3C - with DIWSTRT=$81 and BPL1MOD/BPL2MOD=-4. The
    // early pre-fetch word is clocked into the left border before the
    // window opens: native x-offset 16 (one 16-pixel word) skips it. With
    // the negative modulo that word equals the previous row's right-edge
    // word; showing it bled that edge into the left column one scanline
    // down and cropped it off the right. Confirmed against vAmiga (clean
    // left edge, the >OVERVIEW box keeps its right border).
    let xsysinfo = ControlState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon0: 0x8200,
        diwstrt: 0x2C81,
        diwstop: 0x2CC1,
        ddfstrt: 0x0038,
        ddfstop: 0x00D8,
        ..ControlState::default()
    };
    let repeat = xsysinfo.framebuffer_pixel_repeat();
    assert_eq!(xsysinfo.native_x_offset(xsysinfo.diw_h_start(), repeat), 16);

    // The hi-res standard slot pre-fetches nothing; origin stays clamped at
    // 0 (no snap).
    let standard = ControlState {
        ddfstrt: 0x003C,
        ..xsysinfo
    };
    assert_eq!(standard.native_x_offset(standard.diw_h_start(), repeat), 0);

    // Kickstart 2.05 insert-disk screen: hi-res but DDFSTRT=$40 (late), so
    // there is no pre-fetch word; the fetch origin sits inside the window and
    // the left border shows the pre-window samples. With the hi-res fetch
    // reference sharing the lo-res 0x81 anchor (both flush at the 2H-196 window
    // edge), the window's first visible sample is 24 hi-res samples into the
    // fetched row. The Agnus/DDF vAmigaTS references improve, none regress.
    let ks_boot = ControlState {
        diwstrt: 0x2C95,
        diwstop: 0x2CAD,
        ddfstrt: 0x0040,
        ddfstop: 0x00D0,
        ..xsysinfo
    };
    assert_eq!(ks_boot.native_x_offset(ks_boot.diw_h_start(), repeat), 24);

    // ECS extreme-overscan hi-res screen (KS 3.2 Overscan editor's edit
    // display, issue #186): DDFSTRT=$28 fetches 80 hi-res px ahead of the
    // standard slot, but DIWSTRT h=$5D opens the window 72 px early too, so
    // nearly all of that early content is INSIDE the window. Only the part
    // left of the framebuffer origin is skipped (10 px window clamp plus the
    // 8 px between the window edge and the reference): origin 18, not the
    // early-fetch width 80. Snapping to 80 shifted the whole picture left
    // and left a blank band at the right window edge.
    let overscan = ControlState {
        agnus_revision: AgnusRevision::Ecs8375,
        bplcon0: 0x9200,
        diwstrt: 0x1D5D,
        diwstop: 0x38C7,
        diwhigh: DiwHigh::ecs_explicit(0x2100),
        ddfstrt: 0x0028,
        ddfstop: 0x00D8,
        bplcon1: 0x0044,
        ..xsysinfo
    };
    assert_eq!(overscan.diw_h_start(), 0x5D);
    // (0x5D - 0x81)*2 display shift, -(0x3C - 0x28)*4 DDF shift, +(0x62 -
    // 0x5D)*2 framebuffer-origin clamp = 18.
    assert_eq!(overscan.native_x_offset(overscan.diw_h_start(), repeat), 18);
}

fn ocs_snapshot(diwstrt: u16, diwstop: u16, ddfstrt: u16, ddfstop: u16) -> RenderRegisterSnapshot {
    RenderRegisterSnapshot {
        // 5 lo-res planes; resolution/plane count is irrelevant to the H
        // window, but keep it a plausible playfield.
        bplcon0: 0x5200,
        diwstrt,
        diwstop,
        ddfstrt,
        ddfstop,
        ..RenderRegisterSnapshot::default()
    }
}

#[test]
fn horizontal_class_centres_wide_diw_around_standard_fetch() {
    // Virtual Dreams "Absolute Inebriation": DIW opened wide
    // (DIWSTRT $5702 -> H $02, DIWSTOP $FFFF -> H $1FF) around a standard
    // 320-px lo-res picture (DDF $38..$D0). The open window only reveals
    // COLOR0 border the TV crops, so the frame stays standard and the
    // picture must still recentre by the stock 24px instead of sitting
    // right-of-centre.
    assert_eq!(
        horizontal_content_class(&ocs_snapshot(0x5702, 0xFFFF, 0x0038, 0x00D0)),
        HorizontalContentClass::Standard { shift: 24 }
    );
    // A genuinely centred stock display is unchanged.
    assert_eq!(
        horizontal_content_class(&ocs_snapshot(0x2C81, 0x2CC1, 0x0038, 0x00D0)),
        HorizontalContentClass::Standard { shift: 24 }
    );
}

#[test]
fn horizontal_class_calls_true_overscan_fetch_overscan() {
    // Wide DIW *and* a fetch that reaches into the overscan border
    // (DDFSTRT $30 starts the picture left of the standard window): a real
    // overscan display. Full-overscan mode presents it without recentring;
    // the TV glass crops it like a real set.
    assert_eq!(
        horizontal_content_class(&ocs_snapshot(0x5702, 0xFFFF, 0x0030, 0x00D8)),
        HorizontalContentClass::Overscan
    );
    let aga = RenderRegisterSnapshot {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon0: 0x8214,
        diwstrt: 0x1D61,
        diwstop: 0x37C7,
        ddfstrt: 0x0028,
        ddfstop: 0x00D8,
        ..RenderRegisterSnapshot::default()
    };
    assert_eq!(
        horizontal_content_class(&aga),
        HorizontalContentClass::Overscan
    );
}

#[test]
fn horizontal_class_keeps_narrow_late_fetch_in_beam_position() {
    // A normal DIW can be used around a tiny one-word late-DDF object. The
    // display window stays within standard bounds and uses the standard
    // presentation centering shift.
    assert_eq!(
        horizontal_content_class(&ocs_snapshot(0x3481, 0x24D1, 0x0050, 0x0058)),
        HorizontalContentClass::Standard { shift: 24 }
    );
}

#[test]
fn horizontal_class_centres_grandslam_intro_window() {
    // Issue #465: Chambers of Shaolin Grandslam intro sets DIW $30C0/$F8D8
    // around standard DDF $38..$D0 fetch. The effective visible area stays
    // within standard bounds and must use the standard raster centering shift.
    assert_eq!(
        horizontal_content_class(&ocs_snapshot(0x30C0, 0xF8D8, 0x0038, 0x00D0)),
        HorizontalContentClass::Standard { shift: 24 }
    );
}

#[test]
fn horizontal_class_calls_border_only_frames_neutral() {
    // A frame with no valid fetch at all -- registers cleared, the state a
    // machine shows for several frames during boot and for a frame or two
    // at every screen change while the copper list is rebuilt -- carries no
    // evidence about the display's layout. It must classify neutral so
    // presentation keeps its previous geometry instead of snapping to the
    // full framebuffer (the Kickstart 2.05 boot picture-jump regression).
    assert_eq!(
        horizontal_content_class(&RenderRegisterSnapshot::default()),
        HorizontalContentClass::Neutral
    );
    // A window that opens only after the fetched content has ended (DIW
    // H $E1..$1FF into the right overscan, content $B1..$D1) shows border
    // only: neutral too.
    assert_eq!(
        horizontal_content_class(&ocs_snapshot(0x2CE1, 0xFFFF, 0x0050, 0x0058)),
        HorizontalContentClass::Neutral
    );
}

fn put_word(ram: &mut [u8], pc: usize, word: u16) {
    ram[pc..pc + 2].copy_from_slice(&word.to_be_bytes());
}

fn beam_event(vpos: u32, hpos: u32, offset: u16, value: u16) -> BeamRegisterWrite {
    BeamRegisterWrite {
        vpos,
        hpos,
        offset,
        value,
        source: BeamWriteSource::Copper,
    }
}

fn cpu_event(vpos: u32, hpos: u32, offset: u16, value: u16) -> BeamRegisterWrite {
    BeamRegisterWrite {
        vpos,
        hpos,
        offset,
        value,
        source: BeamWriteSource::Cpu,
    }
}

fn cpu_copper_irq_event(vpos: u32, hpos: u32, offset: u16, value: u16) -> BeamRegisterWrite {
    BeamRegisterWrite {
        vpos,
        hpos,
        offset,
        value,
        source: BeamWriteSource::CpuCopperIrq,
    }
}

fn visible_lowres_control(bplcon0: u16) -> ControlState {
    ControlState {
        bplcon0,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | DIW_HSTART_FETCH_REFERENCE_LORES as u16,
        diwstop: (((PAL_VISIBLE_LINE0 + 1) as u16) << 8)
            | (DIW_HSTART_FETCH_REFERENCE_LORES as u16 + 4),
        ddfstrt: 0x0038,
        ddfstop: 0x0038,
        ..ControlState::default()
    }
}

#[test]
fn bottom_palette_replay_uses_matching_cpu_copper_irq_events() {
    let frame_events = [cpu_copper_irq_event(0xFA, 0x40, 0x180, 0x0222)];
    let bottom_events = [cpu_copper_irq_event(0xD4, 0x16, 0x180, 0x0222)];

    assert!(should_replay_bottom_palette_events(
        &frame_events,
        &frame_events,
        &bottom_events,
        true
    ));
}

#[test]
fn bottom_palette_replay_not_reinjected_when_frame_has_matching_cpu_copper_irq_events() {
    // A copper interrupt raised on the scanline above the bottom of an
    // index band triggers a CPU palette MOVE. The cycle-stepped CPU records
    // that write as a raw CpuCopperIrq event at the beam position where the
    // MOVE actually executes (one line later, after interrupt latency). The
    // bottom-palette replay carries the same write stamped at the earlier
    // copper-interrupt trigger position. Both should not be applied: the raw
    // beam-accurate write is authoritative, so the replay must not be
    // injected (otherwise it recolors the band's final visible scanline).
    let frame_events = [cpu_copper_irq_event(0xD5, 0x2B, 0x19C, 0x0999)];
    let bottom_events = [cpu_copper_irq_event(0xD4, 0x0B, 0x19C, 0x0999)];

    assert!(should_replay_bottom_palette_events(
        &frame_events,
        &frame_events,
        &bottom_events,
        true
    ));
    assert!(!should_inject_bottom_palette_replay_events(
        &frame_events,
        &frame_events,
        &bottom_events,
        true
    ));
}

#[test]
fn bottom_palette_replay_injected_when_frame_carries_palette_forward() {
    // No raw CpuCopperIrq palette writes this frame: the palette was set by
    // a copper interrupt in an earlier frame and must be replayed at the
    // copper-interrupt beam position to reconstruct it for this frame.
    let frame_events = [beam_event(0x19, 0x2D, 0x092, 0x0038)];
    let bottom_events = [cpu_copper_irq_event(0xD4, 0x16, 0x180, 0x0222)];

    assert!(should_inject_bottom_palette_replay_events(
        &frame_events,
        &[],
        &bottom_events,
        true
    ));
}

#[test]
fn bottom_palette_replay_persists_when_frame_has_no_palette_timing() {
    let frame_events = [beam_event(0x19, 0x2D, 0x092, 0x0038)];
    let bottom_events = [cpu_copper_irq_event(0xD4, 0x16, 0x180, 0x0222)];

    assert!(should_replay_bottom_palette_events(
        &frame_events,
        &[],
        &bottom_events,
        true
    ));
}

#[test]
fn bottom_palette_replay_does_not_override_copper_palette_bands() {
    let frame_events = [beam_event(0x84, 0xDF, 0x180, 0x0111)];
    let bottom_events = [cpu_copper_irq_event(0xD4, 0x16, 0x180, 0x0222)];

    assert!(!should_replay_bottom_palette_events(
        &frame_events,
        &[],
        &bottom_events,
        true
    ));
}

#[test]
fn current_cpu_copper_irq_palette_replay_requires_same_primary_bitplane_buffer() {
    let completed_pointer_events = [
        beam_event(0x20, 0x20, 0x0E0, 0x0004),
        beam_event(0x20, 0x24, 0x0E2, 0x2000),
    ];

    assert!(primary_bitplane_buffer_carries_forward(
        0x001000,
        &completed_pointer_events,
        0x042000,
        &[],
    ));
    assert!(!primary_bitplane_buffer_carries_forward(
        0x001000,
        &completed_pointer_events,
        0x052000,
        &[],
    ));
    assert!(!primary_bitplane_buffer_carries_forward(0, &[], 0, &[],));
}

/// POS/CTL words for a sprite whose FIRST OUTPUT PIXEL sits at lo-res
/// position `hstart` (framebuffer x = (hstart - DIW_HSTART_FB0) * 2). The
/// register value is one lo-res position lower: Denise's serializer emits
/// its first pixel one lo-res pixel after the comparator match
/// (`crate::bus::SPRITE_OUTPUT_DELAY_LORES`), and these tests are written
/// against output positions.
fn sprite_control_words(vstart: u16, vstop: u16, hstart: u16) -> (u16, u16) {
    let hstart = hstart - crate::bus::SPRITE_OUTPUT_DELAY_LORES as u16;
    let pos = ((vstart & 0x00FF) << 8) | ((hstart >> 1) & 0x00FF);
    let ctl = ((vstop & 0x00FF) << 8)
        | ((vstart & 0x0100) >> 6)
        | ((vstop & 0x0100) >> 7)
        | (hstart & 0x0001);
    (pos, ctl)
}

/// Register-decoded hstart whose first OUTPUT pixel lands at framebuffer
/// x = 0 (see `sprite_control_words`: output = register + serializer delay).
const SPRITE_HSTART_FB0: i32 = DIW_HSTART_FB0 - crate::bus::SPRITE_OUTPUT_DELAY_LORES;

/// Register-decoded hstart produced by `sprite_control_words(.., h)`:
/// the helper takes OUTPUT positions, while `SpriteLine::hstart` carries
/// the register domain (one lo-res position lower).
fn reg_hstart(output_hstart: i32) -> i32 {
    output_hstart - crate::bus::SPRITE_OUTPUT_DELAY_LORES
}

#[test]
fn fmode_sscan2_aliases_sprite_hstart_high_bit_in_renderer() {
    let (red_pos, red_ctl) = sprite_control_words_from_parts(42, 74, 357, false, false);
    let (green_pos, green_ctl) = sprite_control_words_from_parts(42, 74, 128, false, false);

    let red_without_sscan2 = sprite_nominal_base_framebuffer_x(red_pos, red_ctl, 0, 0);
    let red_with_sscan2 = sprite_nominal_base_framebuffer_x(red_pos, red_ctl, 0, 0x8000);
    let green_with_sscan2 = sprite_nominal_base_framebuffer_x(green_pos, green_ctl, 0, 0x8000);

    assert_eq!(red_without_sscan2 - red_with_sscan2, 256 * 2);
    assert_eq!(green_with_sscan2 - red_with_sscan2, (128 - 101) * 2);
}

#[test]
fn sprite_horizontal_subposition_only_applies_in_shres() {
    let (base_pos, base_ctl) = sprite_control_words_from_parts(42, 74, 128, false, false);
    let (sub_pos, sub_ctl) = sprite_control_words_from_parts(42, 74, 128, true, false);
    let base_x = sprite_nominal_base_framebuffer_x(base_pos, base_ctl, 0, 0x8000);

    assert_eq!(
        sprite_nominal_base_framebuffer_x(sub_pos, sub_ctl, 0, 0x8000),
        base_x
    );
    assert_eq!(
        sprite_nominal_base_framebuffer_x(sub_pos, sub_ctl, BPLCON0_SHRES, 0x8000),
        base_x + 1
    );
}

#[test]
fn manual_sprite_compare_tracks_midline_shres_enable() {
    let mut state = blank_state();
    let beam_y = PAL_VISIBLE_LINE0;
    let (pos, ctl) = sprite_control_words_from_parts(beam_y, beam_y + 1, 160, true, false);
    state.spr_hw_pos[0] = pos;
    state.spr_hw_ctl[0] = ctl;
    state.spr_hw_data[0] = 0x8000;
    state.spr_hw_armed[0] = true;

    let mut regs = BeamSpriteState::from_render_state(&state, &[None; 8], true);
    let lowres_base_x = sprite_nominal_base_framebuffer_x(pos, ctl, state.bplcon0, state.fmode);
    let after_lowres_compare = (lowres_base_x + 1) as usize;

    assert!(regs
        .line_for_sprite(0, beam_y, after_lowres_compare, FB_WIDTH)
        .is_none());
    regs.apply_write(0x100, BPLCON0_SHRES);
    assert!(regs
        .line_for_sprite(0, beam_y, after_lowres_compare, FB_WIDTH)
        .is_some());
}

fn blank_state() -> RenderState {
    RenderState {
        agnus_revision: AgnusRevision::Ocs,
        harddis: false,
        dmacon: 0,
        bplcon0: 0,
        bplcon1: 0,
        bplcon2: 0,
        bplcon3: BPLCON3_PF2OF_DEFAULT,
        bplcon4: 0x0011,
        fmode: 0,
        clxcon2: 0,
        clxcon: 0,
        bplpt: [0; 8],
        bpldat: [0; 8],
        sprpt: [0; 8],
        sprpos: [0; 8],
        sprctl: [0; 8],
        sprdata: [0; 8],
        sprdatb: [0; 8],
        spr_armed: [false; 8],
        spr_hw_pos: [0; 8],
        spr_hw_ctl: [0; 8],
        spr_hw_data: [0; 8],
        spr_hw_datb: [0; 8],
        spr_hw_armed: [false; 8],
        bpl1mod: 0,
        bpl2mod: 0,
        palette: Palette::from_ocs([0x0103; 32]),
        diwstrt: 0,
        diwstop: 0,
        diwhigh: DiwHigh::ocs_implicit(),
        ddfstrt: 0,
        ddfstop: 0,
    }
}

#[test]
fn display_window_converts_pal_beam_bounds_to_framebuffer_bounds() {
    let state = RenderState {
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | (DIW_HSTART_FB0 as u16 + 8),
        diwstop: (((PAL_VISIBLE_LINE0 + 128) as u16) << 8) | ((DIW_HSTART_FB0 + 63) as u16),
        ..blank_state()
    };

    assert_eq!(state.display_window_y(), (0, 128));
    assert_eq!(state.display_window_x(), (16, 638));
}

#[test]
fn display_window_counts_rows_clipped_above_framebuffer() {
    let state = RenderState {
        diwstrt: (((PAL_VISIBLE_LINE0 - 16) as u16) << 8) | DIW_HSTART_FB0 as u16,
        diwstop: (((PAL_VISIBLE_LINE0 + 128) as u16) << 8) | DIW_HSTART_FB0 as u16,
        ..blank_state()
    };

    assert_eq!(state.display_window_y(), (0, 128));
    assert_eq!(state.clipped_display_rows_before_frame(), 16);
}

#[test]
fn display_window_counts_pixels_clipped_left_of_framebuffer() {
    let state = RenderState {
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | (DIW_HSTART_FB0 as u16 - 18),
        diwstop: (((PAL_VISIBLE_LINE0 + 128) as u16) << 8) | 0x00D1,
        ..blank_state()
    };

    assert_eq!(state.display_window_x().0, 0);
    assert_eq!(state.clipped_display_pixels_before_frame(), 36);
}

#[test]
fn display_window_zero_start_uses_denise_comparator() {
    let state = RenderState {
        diwstrt: 0x0000,
        diwstop: 0x2CC1,
        ..blank_state()
    };

    assert_eq!(state.diw_v_start(), 0);
    assert_eq!(state.diw_h_start(), 0);
    assert_eq!(state.display_window_y(), (0, 256));
    assert_eq!(
        state.display_window_x(),
        (0, ((0x01C1 - DIW_HSTART_FB0) * 2) as usize)
    );
    assert_eq!(
        state.clipped_display_rows_before_frame(),
        PAL_VISIBLE_LINE0 as usize
    );
    assert_eq!(
        state.clipped_display_pixels_before_frame(),
        (DIW_HSTART_FB0 as usize) * 2
    );
}

#[test]
fn display_window_maps_pal_horizontal_overscan_to_full_framebuffer_width() {
    let state = RenderState {
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | DIW_HSTART_FB0 as u16,
        diwstop: (((PAL_VISIBLE_LINE0 + 128) as u16) << 8) | 0x00D1,
        ..blank_state()
    };

    assert_eq!(state.display_window_x(), (0, FB_WIDTH));
}

#[test]
fn display_window_uses_stop_as_exclusive_right_edge() {
    let state = RenderState {
        diwstrt: 0x6395,
        diwstop: 0xF4AD,
        diwhigh: DiwHigh::ecs_explicit(0x2000),
        ..blank_state()
    };

    assert_eq!(state.display_window_x(), (102, 662));
}

#[test]
fn display_window_uses_ocs_implicit_high_bits_until_diwhigh_is_written() {
    let state = RenderState {
        diwstrt: 0xFCFC,
        diwstop: 0x7F01,
        ..blank_state()
    };

    assert_eq!(state.diw_v_start(), 0x00FC);
    assert_eq!(state.diw_h_start(), 0x00FC);
    assert_eq!(state.diw_v_stop(), 0x017F);
    assert_eq!(state.diw_h_stop(), 0x0101);
}

#[test]
fn display_window_diwhigh_zero_write_selects_ecs_direct_high_bits() {
    let state = RenderState {
        diwstrt: 0xFCFC,
        diwstop: 0x7F01,
        diwhigh: DiwHigh::ecs_explicit(0),
        ..blank_state()
    };

    assert_eq!(state.diw_v_start(), 0x00FC);
    assert_eq!(state.diw_h_start(), 0x00FC);
    assert_eq!(state.diw_v_stop(), 0x007F);
    assert_eq!(state.diw_h_stop(), 0x0001);
}

#[test]
fn display_window_diwstrt_diwstop_write_reverts_diwhigh_to_implicit() {
    // An ECS DIWHIGH value only applies until the next DIWSTRT/DIWSTOP
    // write, which re-arms the OCS-implicit high bits. A stale DIWHIGH
    // must not keep shrinking the window after it is reprogrammed --
    // TurboTomato's ECS title rendered black because $00FF stayed latched
    // and pushed the vertical window start off-screen.
    let mut state = blank_state();
    apply_move(&mut state, 0x1E4, 0x00FF);
    assert_eq!(state.diwhigh, DiwHigh::ecs_explicit(0x00FF));
    apply_move(&mut state, 0x08E, 0x2C81);
    assert_eq!(state.diwhigh, DiwHigh::ocs_implicit());

    apply_move(&mut state, 0x1E4, 0x00FF);
    apply_move(&mut state, 0x090, 0x2CC1);
    assert_eq!(state.diwhigh, DiwHigh::ocs_implicit());
}

#[test]
fn clipped_overscan_rows_advance_bitplane_pointers() {
    let mut ptrs = [0x0100, 0x1000, 0, 0, 0, 0, 0, 0];
    let control = ControlState::from_render_state(&RenderState {
        bpl1mod: 4,
        bpl2mod: -2,
        ..blank_state()
    });

    advance_bitplane_ptrs_for_rows(&mut ptrs, 3, 2, 22, &control, 0, 0x001F_FFFF);

    assert_eq!(ptrs[0], 0x0100 + 3 * (22 * 2 + 4));
    assert_eq!(ptrs[1], 0x1000 + 3 * (22 * 2 - 2));
}

#[test]
fn bscan2_clipped_rows_alternate_modulos_by_line_parity() {
    let mut ptrs = [0x0100, 0x1000, 0, 0, 0, 0, 0, 0];
    let control = ControlState::from_render_state(&RenderState {
        agnus_revision: AgnusRevision::AgaAlice,
        fmode: 0x4000,
        bpl1mod: -44,
        bpl2mod: 4,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | DIW_HSTART_FB0 as u16,
        ..blank_state()
    });

    // Two rows starting on DIWSTRT's parity: BPL1MOD then BPL2MOD, the
    // same modulo for both plane groups.
    advance_bitplane_ptrs_for_rows(
        &mut ptrs,
        2,
        2,
        22,
        &control,
        PAL_VISIBLE_LINE0,
        0x001F_FFFF,
    );

    let expected = 22 * 2 + 4;
    assert_eq!(ptrs[0], 0x0100 + expected as u32);
    assert_eq!(ptrs[1], 0x1000 + expected as u32);
}

#[test]
fn display_window_uses_diwhigh_upper_vertical_bits() {
    let state = RenderState {
        diwstrt: 0x2C81,
        diwstop: 0x2D82,
        diwhigh: DiwHigh::ecs_explicit(0x0100),
        ..blank_state()
    };

    assert_eq!(state.diw_v_start(), 0x02C);
    assert_eq!(state.diw_v_stop(), 0x12D);
}

#[test]
fn line_start_diw_write_replaces_previous_horizontal_display_bounds() {
    let base = ControlState {
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | STANDARD_DIW_HSTART as u16,
        diwstop: (((PAL_VISIBLE_LINE0 + 1) as u16) << 8) | 0x00C1,
        ..ControlState::default()
    };
    let narrowed = ControlState {
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | 0x00A0,
        ..base
    };
    let segments = [ControlSegment {
        x: 0,
        control: narrowed,
    }];

    let h_rows = compute_h_window_rows(
        &[base],
        std::slice::from_ref(&segments.to_vec()),
        PAL_VISIBLE_LINE0,
    );
    let bounds = line_display_window_bounds(base, &segments, 0, PAL_VISIBLE_LINE0, &h_rows[0])
        .expect("window open");
    assert_eq!((bounds.x_start, bounds.x_stop), narrowed.display_window_x());
    assert_eq!(bounds.carried_open_ext_fb, 0);
}

#[test]
fn carried_open_h_window_extends_paint_to_fetch_origin() {
    // Chambers of Shaolin's Grandslam intro: DIWSTRT H=$C0 with DIWSTOP
    // H=$D8 (decoded $1D8, a position the beam counter never reaches), so
    // the horizontal DIW flip-flop never clears and carries open across
    // every line. The DIWSTRT match is then a no-op on hardware and the
    // standard DDF $38 picture shows in full from its fetch origin
    // (FS-UAE/vAmiga-verified; ddfprobe-diw1 is the golden render probe).
    let control = ControlState {
        diwstrt: 0x30C0,
        diwstop: 0xF8D8,
        ddfstrt: 0x0038,
        ddfstop: 0x00D0,
        bplcon0: 0x4200,
        dmacon: 0x0380,
        ..ControlState::default()
    };
    // Row 0 sits at beam line 48, the first line inside the vertical
    // window ($30..$F8).
    let h_rows = compute_h_window_rows(&[control], &[Vec::new()], 48);
    // Flip-flop never clears: the whole line is one open run.
    assert_eq!(h_rows[0].open_runs(), &[(0, FB_WIDTH)]);
    assert_eq!(h_rows[0].comparator_anchor, None);
    let bounds = line_display_window_bounds(control, &[], 0, 48, &h_rows[0]).expect("window open");
    // Paint extends left of the $C0 anchor to the fetch-derived origin
    // (the standard display left edge), restoring the hidden samples.
    assert_eq!(bounds.x_start, STANDARD_VISIBLE_X0);
    assert_eq!(bounds.x_stop, FB_WIDTH);
    let anchor_x = control.display_window_x().0;
    assert_eq!(bounds.carried_open_ext_fb, anchor_x - STANDARD_VISIBLE_X0);
}

#[test]
fn carried_open_row_that_closes_and_reopens_still_reveals_the_left_data() {
    // ECS DIWHIGH can place HSTOP left of HSTART ($91 < $C0): the
    // flip-flop opens at $C0, survives the line wrap, and closes at $91 on
    // the next line, so every row enters the framebuffer open, closes
    // early, and reopens at the anchor. The carried-in run still reveals
    // the fetched data left of the reopen anchor on hardware; only the
    // closed gap between HSTOP and HSTART is border.
    let control = ControlState {
        diwstrt: 0x30C0,
        diwstop: 0xF891,
        diwhigh: DiwHigh::ecs_explicit(0x0100),
        ddfstrt: 0x0038,
        ddfstop: 0x00D0,
        bplcon0: 0x4200,
        dmacon: 0x0380,
        ..ControlState::default()
    };
    let anchor_x = control.display_window_x().0;
    let close_x = (0x91 - 0x62) * 2;
    let h_rows = compute_h_window_rows(&[control, control], &[Vec::new(), Vec::new()], 48);
    // Row 1 entered open (carried), closed at $91, reopened at $C0.
    assert_eq!(h_rows[1].open_runs(), &[(0, close_x), (anchor_x, FB_WIDTH)]);
    assert_eq!(h_rows[1].comparator_anchor, Some(anchor_x));
    let bounds = line_display_window_bounds(control, &[], 1, 48, &h_rows[1]).expect("window open");
    // Paint still starts at the fetch origin; the per-pixel window gate
    // and the closed-interval border mask hide the closed gap.
    assert_eq!(bounds.x_start, STANDARD_VISIBLE_X0);
    assert_eq!(bounds.carried_open_ext_fb, anchor_x - STANDARD_VISIBLE_X0);
}

#[test]
fn reachable_diwstop_keeps_the_window_clip() {
    // The same $C0 window with a reachable DIWSTOP ($1C1) closes every
    // line, so the next line's DIWSTRT match is a real open transition and
    // the window clips the playfield at the anchor, exactly as before.
    let control = ControlState {
        diwstrt: 0x30C0,
        diwstop: 0xF8C1,
        ddfstrt: 0x0038,
        ddfstop: 0x00D0,
        bplcon0: 0x4200,
        dmacon: 0x0380,
        ..ControlState::default()
    };
    let h_rows = compute_h_window_rows(&[control, control], &[Vec::new(), Vec::new()], 48);
    let anchor_x = control.display_window_x().0;
    // Row 1 (a line whose predecessor closed the flip-flop) opens at the
    // comparator anchor.
    assert_eq!(h_rows[1].comparator_anchor, Some(anchor_x));
    let bounds = line_display_window_bounds(control, &[], 1, 48, &h_rows[1]).expect("window open");
    assert_eq!(bounds.x_start, anchor_x);
    assert_eq!(bounds.carried_open_ext_fb, 0);
}

#[test]
fn beam_position_converts_to_line_and_segment_x() {
    let line = PAL_VISIBLE_LINE0 + 25;
    let hpos = COPPER_WAIT_HPOS_FB0 + 16;

    assert_eq!(beam_to_framebuffer_pos(line as u32, hpos as u32), (25, 64));
}

#[test]
fn denise_horizontal_delay_aligns_copper_beam_and_display_fetch_domains() {
    let state = RenderState {
        bplcon0: 0,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | STANDARD_DIW_HSTART as u16,
        diwstop: (((PAL_VISIBLE_LINE0 + 1) as u16) << 8) | 0x00C1,
        ddfstrt: 0x0038,
        ..blank_state()
    };
    let display_left = state.display_window_x().0;
    // The standard window edge sits at 62 (2H-196, hardware-verified) with
    // the first fetched lo-res sample flush at the edge; the copper/register
    // domain position of the same beam time maps one lo-res pixel later
    // (the Agnus-fetch -> Denise-display pipeline).
    let copper_hpos = COPPER_WAIT_HPOS_FB0 + ((display_left + 2) / 4) as i32;

    assert_eq!(display_left, 62);
    assert_eq!(
        beam_to_framebuffer_pos(PAL_VISIBLE_LINE0 as u32, copper_hpos as u32),
        (0, display_left + 2)
    );
    assert_eq!(state.native_x_offset(false, 2), 0);
    assert_eq!(state.fetch_start_native_x(false, 2), 0);
}

#[test]
fn color_register_writes_use_final_output_position() {
    assert_eq!(
        color_write_framebuffer_x(COLOR_WRITE_HPOS_FB0 as u32, false),
        0
    );
    assert_eq!(
        color_write_framebuffer_x(COLOR_WRITE_HPOS_FB0 as u32, true),
        1
    );
    assert_eq!(
        color_write_framebuffer_x((COLOR_WRITE_HPOS_FB0 - 1) as u32, true),
        0,
        "Lisa's one-pixel delay applies before the left-edge clamp"
    );
    assert_eq!(
        color_write_framebuffer_x((COLOR_WRITE_HPOS_FB0 + 4) as u32, false),
        16
    );
    assert!(color_write_wraps_to_previous_output_line(2));
    assert!(!color_write_wraps_to_previous_output_line(
        DENISE_HBLANK_START_HPOS
    ));
    assert_eq!(color_write_wrapped_framebuffer_x(2, false), 704);
    assert_eq!(color_write_wrapped_framebuffer_x(2, true), 705);
    assert_eq!(
        beam_to_framebuffer_x_unclamped(COLOR_WRITE_HPOS_FB0 as u32),
        52
    );
    assert_eq!(
        sprite_palette_control_framebuffer_x(SPRITE_PALETTE_CONTROL_HPOS_FB0 as u32),
        0
    );
    assert_eq!(
        sprite_palette_control_framebuffer_x((SPRITE_PALETTE_CONTROL_HPOS_FB0 + 4) as u32),
        16
    );
}

#[test]
fn bplcon3_brdrblnk_blanks_border_when_ecsena_set() {
    let mut state = RenderState {
        bplcon0: BPLCON0_ECSENA,
        bplcon3: BPLCON3_BRDRBLNK,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | (DIW_HSTART_FB0 as u16 + 8),
        diwstop: (((PAL_VISIBLE_LINE0 + 128) as u16) << 8) | (DIW_HSTART_FB0 as u16 + 63),
        ..blank_state()
    };
    state.palette.write_ocs(0, 0x0F00);
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_control = ControlState::from_render_state(&state);
    let base_controls = [base_control; FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut fb = vec![0; FB_PIXELS];

    fill_background(
        &mut fb,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
    );

    assert_eq!(fb[0], rgb12_to_rgba8_alpha(0, false));
    assert_eq!(fb[16], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[200 * FB_WIDTH], rgb12_to_rgba8_alpha(0, false));
}

#[test]
fn bplcon3_brdrblnk_requires_ecsena_for_border_blank() {
    let mut state = RenderState {
        bplcon0: 0,
        bplcon3: BPLCON3_BRDRBLNK,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | (DIW_HSTART_FB0 as u16 + 8),
        diwstop: (((PAL_VISIBLE_LINE0 + 128) as u16) << 8) | (DIW_HSTART_FB0 as u16 + 63),
        ..blank_state()
    };
    state.palette.write_ocs(0, 0x0F00);
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_control = ControlState::from_render_state(&state);
    let base_controls = [base_control; FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut fb = vec![0; FB_PIXELS];

    fill_background(
        &mut fb,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[16], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[200 * FB_WIDTH], rgb12_to_rgba8(0x0F00));
}

#[test]
fn bplcon3_brdntran_keeps_border_opaque_for_color_key() {
    let mut state = RenderState {
        bplcon0: BPLCON0_ECSENA,
        bplcon2: BPLCON2_ZDCTEN,
        bplcon3: BPLCON3_BRDNTRAN,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | (DIW_HSTART_FB0 as u16 + 8),
        diwstop: (((PAL_VISIBLE_LINE0 + 128) as u16) << 8) | (DIW_HSTART_FB0 as u16 + 63),
        ..blank_state()
    };
    state.palette.write_ocs(0, COLOR_TRANSPARENCY_BIT | 0x0F00);
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_control = ControlState::from_render_state(&state);
    let base_controls = [base_control; FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut fb = vec![0; FB_PIXELS];

    fill_background(
        &mut fb,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[16], rgb12_to_rgba8_alpha(0x0F00, false));
}

#[test]
fn bplcon3_brdntran_makes_blank_border_opaque_black() {
    let mut state = RenderState {
        bplcon0: BPLCON0_ECSENA,
        bplcon3: BPLCON3_BRDRBLNK | BPLCON3_BRDNTRAN,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | (DIW_HSTART_FB0 as u16 + 8),
        diwstop: (((PAL_VISIBLE_LINE0 + 128) as u16) << 8) | (DIW_HSTART_FB0 as u16 + 63),
        ..blank_state()
    };
    state.palette.write_ocs(0, 0x0F00);
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_control = ControlState::from_render_state(&state);
    let base_controls = [base_control; FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut fb = vec![0; FB_PIXELS];

    fill_background(
        &mut fb,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0));
    assert_eq!(fb[16], rgb12_to_rgba8(0x0F00));
}

#[test]
fn beam_timed_bplcon3_brdrblnk_latches_until_ecsena_enables_effect() {
    let mut state = RenderState {
        bplcon0: 0,
        bplcon3: 0,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | (DIW_HSTART_FB0 as u16 + 80),
        // OCS DIWSTOP carries an implied H8, so the stop byte must map
        // inside the comparator's reach (H <= 0x1C7) for the window to
        // close; 0x99 -> 0x199 closes mid-line, leaving a left border for
        // the BRDRBLNK assertions below.
        diwstop: (((PAL_VISIBLE_LINE0 + 1) as u16) << 8) | 0x0099,
        ..blank_state()
    };
    state.palette.write_ocs(0, 0x0F00);
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [
        beam_event(
            PAL_VISIBLE_LINE0 as u32,
            (COPPER_WAIT_HPOS_FB0 + 4) as u32,
            0x0106,
            BPLCON3_BRDRBLNK,
        ),
        beam_event(
            PAL_VISIBLE_LINE0 as u32,
            (COPPER_WAIT_HPOS_FB0 + 8) as u32,
            0x0100,
            BPLCON0_ECSENA,
        ),
    ];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );
    let mut fb = vec![0; FB_PIXELS];
    fill_background(
        &mut fb,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
    );

    assert_eq!(fb[8], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[24], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[40], rgb12_to_rgba8_alpha(0, false));
}

#[test]
fn closed_interval_repaint_follows_copper_color00_writes() {
    // Copper-chunky banners (e.g. the "binary" logo's red bar) paint colour
    // bars by rewriting COLOR00 across the horizontal border, where the DIW
    // flip-flop has closed the display window. The closed-interval border
    // repaint must sample COLOR00 per colour-write, not once per control run:
    // sampling once painted the whole left border with the frame-start COLOR00
    // and dropped the copper's mid-border colour change, clipping the bar's
    // left edge back to the first control boundary.
    let control = visible_lowres_control(0);
    let mut palette = Palette::from_ocs([0; 32]);
    palette.write_ocs(0, 0x0123); // dark frame-start COLOR00
    let base_palettes = [palette];
    // Copper writes COLOR00 = red partway across the closed left border.
    let palette_segments = vec![vec![PaletteSegment {
        x: 40,
        entry: 0,
        loct: false,
        value: 0x0C00,
    }]];
    let base_controls = [control];
    let control_segments = vec![Vec::new()];
    // Flip-flop closed over [0, 100); open to the right of it.
    let h_window_rows = vec![HWindowRow {
        open_runs: vec![(100, FB_WIDTH)],
        comparator_anchor: Some(100),
    }];
    let mut fb = vec![0u32; FB_WIDTH];

    enforce_h_window_closed_intervals(
        &mut fb,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &h_window_rows,
        PAL_VISIBLE_LINE0,
        1,
    );

    let dark = background_pixel(&control, 0x0123, true);
    let red = background_pixel(&control, 0x0C00, true);
    // Border left of the copper write keeps the frame-start colour...
    assert_eq!(fb[20], dark);
    assert_eq!(fb[38], dark);
    // ...and follows the red COLOR00 write for the rest of the closed border.
    assert_eq!(fb[40], red);
    assert_eq!(fb[98], red);
    // The open interval to its right is left untouched by the repaint.
    assert_eq!(fb[100], 0);
}

#[test]
fn native_x_offset_accounts_for_diw_and_ddf_alignment() {
    let standard_hires = RenderState {
        bplcon0: 0x8000,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | DIW_HSTART_FB0 as u16,
        ddfstrt: 0x003C,
        ..blank_state()
    };
    assert_eq!(standard_hires.native_x_offset(true, 1), 0);

    // FMODE=0: the fetch gulp equals the DDF granularity, so the picture
    // follows DDFSTRT continuously. KS 2.05's insert-disk screen (DDF
    // $40, DIW $95) is a late-DDF hi-res picture whose left border shows the
    // samples fetched before the window opens: with the hi-res reference flush
    // at the 2H-196 window edge the first visible sample is 24 hi-res samples
    // into the row (Agnus/DDF vAmigaTS references improve, none regress).
    let kickstart_hires = RenderState {
        bplcon0: 0x8000,
        diwstrt: 0x6395,
        ddfstrt: 0x0040,
        ..blank_state()
    };
    assert_eq!(kickstart_hires.native_x_offset(true, 1), 24);

    // Wide FMODE fetches quantize the displayed shifter origin to the gulp
    // grid: AGA system screens program DDFSTRT $38 or $3C interchangeably
    // (same 16-cck gulp slot) and must display identically; without the
    // quantized placement the $38 screens showed the interleaved bitmap's
    // fetch overrun as a junk column inside the window's right edge.
    let wb_hires_overscan_fetch = RenderState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon0: 0x8000,
        fmode: 0x0003,
        diwstrt: 0x2C81,
        diwstop: 0x2CC1,
        ddfstrt: 0x0038,
        ..blank_state()
    };
    let wb_with_standard_ddf = RenderState {
        ddfstrt: 0x003C,
        ..wb_hires_overscan_fetch
    };
    assert_eq!(
        wb_hires_overscan_fetch.native_x_offset(true, 1),
        wb_with_standard_ddf.native_x_offset(true, 1)
    );
    assert_eq!(
        wb_hires_overscan_fetch.fetch_start_native_x(true, 1),
        wb_with_standard_ddf.fetch_start_native_x(true, 1)
    );
    // Wide-FMODE hi-res output uses the same corrected $81 display origin:
    // the AGA bitmap fills its standard window exactly flush, matching
    // FS-UAE. DDFSTRT $38 and $3C are still equivalent because both align
    // to the same wide-FMODE gulp.
    assert_eq!(wb_hires_overscan_fetch.native_x_offset(true, 1), 0);
    // The hi-res picture sits flush against the 2H-196 window edge (x = 62),
    // same as lo-res: its first fetched sample is the window's first visible
    // pixel, so no samples are consumed in the border before it opens.
    assert_eq!(wb_hires_overscan_fetch.fetch_start_native_x(true, 1), 0);

    // The placement gulp grid runs on absolute colour-clock multiples of
    // the fetch period. Lores FMODE=1 has a 16-cck gulp: DDFSTRT $30 is
    // on-grid and shares its displayed origin with the standard $38, so
    // both must display at the same position. A $18-anchored grid put
    // these modes half a gulp early, shifting the picture left with wrap
    // junk at the window's right edge.
    let pinball_lores_wide_fetch = RenderState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon0: 0x0611,
        fmode: 0x0001,
        diwstrt: 0x2C81,
        diwstop: 0x2CC1,
        ddfstrt: 0x0030,
        ..blank_state()
    };
    let pinball_with_standard_ddf = RenderState {
        ddfstrt: 0x0038,
        ..pinball_lores_wide_fetch
    };
    assert_eq!(
        pinball_lores_wide_fetch.native_x_offset(false, 2),
        pinball_with_standard_ddf.native_x_offset(false, 2)
    );
    assert_eq!(
        pinball_lores_wide_fetch.fetch_start_native_x(false, 2),
        pinball_with_standard_ddf.fetch_start_native_x(false, 2)
    );

    // The absolute gulp grid stays linear below the standard slots: a lores
    // FMODE=3 scroller (8-plane interleaved 384-px buffer, 48-byte rows) with
    // DDFSTRT $19 (masked $18, gulp slot 0) and DDFSTOP $B8 fetches six
    // 64-px gulps and hides the whole first gulp -- its scroll-seam wrap
    // columns -- left of the standard window: buffer pixel 64 is the
    // window's first visible pixel and the remaining 320 fill the window
    // flush to both edges (real-AGA and FS-UAE verified). Clamping the wide
    // placement grid to the DDF hard start $18 pushed the picture 48 px
    // right: the seam showed as a junk column at the window's left edge and
    // the rightmost 48 px of content were cropped.
    let seam_scroller = RenderState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon0: 0x0211,
        fmode: 0x0003,
        diwstrt: 0x2C81,
        diwstop: 0x2CC1,
        ddfstrt: 0x0019,
        ddfstop: 0x00B8,
        ..blank_state()
    };
    assert_eq!(seam_scroller.words_per_row(false, 320), 24);
    assert_eq!(seam_scroller.native_x_offset(false, 2), 64);
    assert_eq!(seam_scroller.fetch_start_native_x(false, 2), 0);

    let diagrom_hires = RenderState {
        bplcon0: 0x8000,
        diwstrt: 0x2C81,
        diwstop: 0x2CC1,
        ddfstrt: 0x003C,
        ..blank_state()
    };
    assert_eq!(diagrom_hires.display_window_x().0, 62);
    assert_eq!(diagrom_hires.clipped_display_pixels_before_frame(), 0);
    assert_eq!(diagrom_hires.native_x_offset(true, 1), 0);
    // Standard hi-res ($81/$3C) sits flush against the 2H-196 window edge
    // (x = 62): its first fetched sample is the window's first visible pixel,
    // so a full 40-word row fills the 640-sample window to both edges. When
    // hi-res used a +1 (0x82) reference it started 2 hi-res px inside the
    // window and clipped its rightmost fetched pixel -- the AmigaDOS window's
    // right border vanished on KS1.3 (the "binary" demo, r.adf).
    assert_eq!(diagrom_hires.fetch_start_native_x(true, 1), 0);

    let lores_extra_fetch_word = RenderState {
        bplcon0: 0,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | DIW_HSTART_FB0 as u16,
        ddfstrt: 0x0030,
        ..blank_state()
    };
    assert_eq!(lores_extra_fetch_word.native_x_offset(false, 2), 0);
    assert_eq!(lores_extra_fetch_word.fetch_start_native_x(false, 2), 15);

    let lores_late_fetch = RenderState {
        bplcon0: 0,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | DIW_HSTART_FB0 as u16,
        ddfstrt: 0x0050,
        ..blank_state()
    };
    assert_eq!(lores_late_fetch.native_x_offset(false, 2), 0);
    assert_eq!(lores_late_fetch.fetch_start_native_x(false, 2), 79);

    let lores_early_fetch_standard_window = RenderState {
        bplcon0: 0,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | STANDARD_DIW_HSTART as u16,
        ddfstrt: 0x0030,
        ddfstop: 0x00D0,
        ..blank_state()
    };
    assert_eq!(
        lores_early_fetch_standard_window.words_per_row(false, 0),
        21
    );
    // Linear DDFSTRT placement: $30 fetches one 8-cck period (16 lo-res
    // pixels) earlier than the standard $38. With the hardware window edge
    // (2H-196) the standard $81 window is flush with the $38 bitmap, so
    // the $30 offset is exactly the 16 extra samples. Hardware-verified via
    // the vAmigaTS Agnus/DIW/OLDDIW/diw1 A500 photos (OCS and ECS).
    assert_eq!(
        lores_early_fetch_standard_window.native_x_offset(false, 2),
        16
    );
}

#[test]
fn lores_early_ddf_picture_sits_linearly_left_of_standard_ddf() {
    // A lo-res FMODE=0 picture fetched at DDFSTRT $30 sits exactly 16 lo-res
    // pixels (one 8-cck fetch period) left of the same picture fetched at
    // $38; the DDF->position mapping is linear. Real hardware confirms
    // (vAmigaTS Agnus/DIW/OLDDIW/diw1 photos: DDF-$30 stripe grid exactly
    // one fetch period left of the standard grid).
    let standard = RenderState {
        bplcon0: 0,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | STANDARD_DIW_HSTART as u16,
        ddfstrt: 0x0038,
        ddfstop: 0x00D0,
        ..blank_state()
    };
    let early = RenderState {
        ddfstrt: 0x0030,
        ..standard
    };
    assert_eq!(
        early.native_x_offset(false, 2) - standard.native_x_offset(false, 2),
        16,
        "DDFSTRT $30 must sit exactly 16 lo-res px left of $38"
    );
}

#[test]
fn ddfstrt_positions_first_lowres_bitplane_word_relative_to_diwstrt() {
    let standard_lowres = RenderState {
        bplcon0: 0,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | 0x0081,
        ddfstrt: 0x0038,
        ..blank_state()
    };
    // The standard $81/$38 lo-res picture is flush with the hardware
    // window edge (both at framebuffer x = 62): no hidden sample, no inset.
    assert_eq!(standard_lowres.fetch_start_native_x(false, 2), 0);
    assert_eq!(standard_lowres.native_x_offset(false, 2), 0);

    let late_window_aligned_fetch = RenderState {
        bplcon0: 0,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | 0x00A1,
        ddfstrt: 0x0048,
        ..blank_state()
    };
    assert_eq!(late_window_aligned_fetch.fetch_start_native_x(false, 2), 0);
    assert_eq!(late_window_aligned_fetch.native_x_offset(false, 2), 0);

    let inset_fetch = RenderState {
        bplcon0: 0,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | 0x00A2,
        ddfstrt: 0x0050,
        ..blank_state()
    };
    assert_eq!(inset_fetch.fetch_start_native_x(false, 2), 15);
    assert_eq!(inset_fetch.native_x_offset(false, 2), 0);

    let late_fetch_standard_window = RenderState {
        bplcon0: 0,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | 0x0081,
        ddfstrt: 0x0048,
        ..blank_state()
    };
    assert_eq!(
        late_fetch_standard_window.fetch_start_native_x(false, 2),
        32
    );
    assert_eq!(late_fetch_standard_window.native_x_offset(false, 2), 0);
}

#[test]
fn planar_byte_lane_table_expands_every_byte_msb_first() {
    for byte in u8::MIN..=u8::MAX {
        let expected = std::array::from_fn(|bit| u8::from(byte & (0x80 >> bit) != 0));
        assert_eq!(PLANAR_BYTE_LANES[usize::from(byte)].to_le_bytes(), expected);
    }
}

#[test]
fn prepared_planar_pixels_match_word_sampler_for_all_playfield_taps() {
    let plane_words = (0..8)
        .map(|plane| {
            (0..3)
                .map(|word| 0x8421u16.rotate_left((plane * 3 + word * 5) as u32))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    // Use a partial final word as well as full words so the prepared row's
    // explicit fetched-pixel bound is covered by the comparison.
    let fetched_pixels = 43;
    let mut pixels = Vec::new();
    prepare_planar_row_pixels(&plane_words, fetched_pixels, &mut pixels);
    let word_plan = DenisePlannedPlayfieldLine::new(0, 0, 128, &plane_words, fetched_pixels);
    let pixel_plan = DenisePlannedPlayfieldLine::with_prepared_pixels(
        0,
        0,
        128,
        &plane_words,
        &pixels,
        fetched_pixels,
    );
    let delay_sets: [[i32; 8]; 3] = [
        [0; 8],
        std::array::from_fn(|plane| if plane & 1 == 0 { 3 } else { 11 }),
        std::array::from_fn(|plane| if plane & 1 == 0 { -16 } else { 5 }),
    ];

    for nplanes in 0..=8 {
        for delays in &delay_sets {
            for min_fetch_x in [0, 7, 47] {
                for native_x in 0..=64 {
                    for hold_final_fetch_sample in [false, true] {
                        let expected = word_plan.sample_prepared_with_final_fetch_hold(
                            nplanes,
                            delays,
                            min_fetch_x,
                            native_x,
                            hold_final_fetch_sample,
                        );
                        let actual = pixel_plan.sample_prepared_with_final_fetch_hold(
                            nplanes,
                            delays,
                            min_fetch_x,
                            native_x,
                            hold_final_fetch_sample,
                        );
                        assert_eq!(
                            actual, expected,
                            "nplanes={nplanes} delays={delays:?} min_fetch_x={min_fetch_x} \
                             native_x={native_x} hold={hold_final_fetch_sample}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn late_lowres_ddf_reaches_diwstop_with_undelayed_planes_active() {
    // Low-res FMODE=0 with DDFSTRT=$50 and DDFSTOP=$D0 fetches 17 words.
    // When DIWSTOP is placed exactly at the completed DDF row's right edge,
    // the final visible DIW sample still contains the undelayed even-plane
    // bit. BPLCON1 may delay the odd planes, but it must not make the even
    // planes fall one native sample past the fetched row at the right edge.
    let control = ControlState {
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x2000,
        bplcon1: 0x0009,
        diwstrt: 0x2C91,
        diwstop: 0x2CC1,
        ddfstrt: 0x0050,
        ddfstop: 0x00D0,
        ..ControlState::default()
    };
    assert_eq!(control.display_window_x(), (94, 702));
    assert_eq!(control.words_per_row(0), 17);
    assert_eq!(control.fetch_start_native_x(control.diw_h_start(), 2), 32);
    assert_eq!(control.native_x_offset(control.diw_h_start(), 2), 0);
    assert!(control.holds_final_lowres_fetch_sample_at_diwstop());

    let last_visible_x = control.display_window_x().1 - control.framebuffer_pixel_repeat();
    let output_native_x =
        (last_visible_x - control.display_window_x().0) / control.framebuffer_pixel_repeat();
    let native_x = output_native_x - control.fetch_start_native_x(control.diw_h_start(), 2);
    // With the hardware window edge (2H-196) the final visible sample is
    // the last fetched one (271), not one past the row; the DDFSTOP hold
    // keeps it available either way.
    assert_eq!(native_x, 271);

    let plane1 = vec![0; 17];
    let mut plane2 = vec![0; 17];
    plane2[16] = 0x0001;
    let planes = vec![plane1, plane2];
    let plan = DenisePlannedPlayfieldLine::new(0, 94, 702, &planes, 17 * 16);
    let delays = std::array::from_fn(|plane| control.sample_delay_for_plane(plane));
    let sample = plan.sample_prepared_with_final_fetch_hold(
        control.nplanes(),
        &delays,
        0,
        native_x,
        control.holds_final_lowres_fetch_sample_at_diwstop(),
    );

    assert!(sample.active);
    assert_eq!(sample.idx, 0x02);
}

#[test]
fn late_lowres_ddf_stop_hold_keeps_left_origin_unadvanced() {
    // The DDFSTOP hold keeps the final completed fetch visible at DIWSTOP,
    // but it must not move the whole low-res FMODE=0 row one native sample
    // to the right. A first-word bit therefore appears at the unadvanced
    // late-DDF fetch origin on the left side of the display window.
    let control = ControlState {
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x1000,
        diwstrt: 0x2C91,
        diwstop: 0x2CC1,
        ddfstrt: 0x0050,
        ddfstop: 0x00D0,
        ..ControlState::default()
    };
    assert_eq!(control.display_window_x(), (94, 702));
    assert_eq!(control.fetch_start_native_x(control.diw_h_start(), 2), 32);
    assert!(control.holds_final_lowres_fetch_sample_at_diwstop());

    let mut plane = vec![0; 17];
    plane[0] = 0x8000;
    let planes = vec![plane];
    let plan = DenisePlannedPlayfieldLine::new(0, 94, 702, &planes, 17 * 16);
    let mut palette = Palette::new();
    palette.write_ocs(1, 0x00F0);
    let mut fb = vec![0; FB_PIXELS];
    let mut playfield_mask = vec![0; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0;

    render_planned_playfield_line(
        &plan,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut CollisionLookup::new(),
        &mut IndexedOutputCache::default(),
        &mut clxdat,
        palette,
        &[],
        0,
        control,
        &[],
        0,
        control.bplcon1,
        control.bplcon0,
        false,
        0,
        0,
        &h_row_for(control),
        PAL_VISIBLE_LINE0,
        0.0,
        0,
    );

    let first_fetch_x = control.display_window_x().0
        + control.fetch_start_native_x(control.diw_h_start(), 2)
            * control.framebuffer_pixel_repeat();
    assert_eq!(first_fetch_x, 158);
    assert_eq!(fb[first_fetch_x - 2], rgb12_to_rgba8_alpha(0, false));
    assert_eq!(fb[first_fetch_x], rgb12_to_rgba8(0x00F0));
    assert_eq!(fb[first_fetch_x + 1], rgb12_to_rgba8(0x00F0));
}

#[test]
fn late_ddf_bitplane_output_starts_at_first_word_fetch() {
    let standard_ddf = ControlState {
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x1000,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | STANDARD_DIW_HSTART as u16,
        diwstop: (((PAL_VISIBLE_LINE0 + 1) as u16) << 8) | STANDARD_DIW_HSTOP as u16,
        ddfstrt: 0x0038,
        ddfstop: 0x00D0,
        ..ControlState::default()
    };
    let standard_words = standard_ddf.words_per_row(native_frame_width_for_control(standard_ddf));

    assert_eq!(
        bitplane_output_start_x(
            standard_ddf,
            &[],
            standard_ddf.display_window_x().0,
            standard_words,
            standard_ddf.dma_planes(),
        ),
        standard_ddf.display_window_x().0
    );

    let inset_ddf = ControlState {
        ddfstrt: 0x0050,
        ddfstop: 0x00D0,
        ..standard_ddf
    };
    let inset_words = inset_ddf.words_per_row(native_frame_width_for_control(inset_ddf));
    let first_word_x = bitplane_fetch_framebuffer_x(
        line_fetch_plan_for_word(inset_ddf, &[], 0, inset_ddf.dma_planes())
            .word_fetch_hpos
            .unwrap(),
    );
    let first_bpl1dat_x =
        bitplane_fetch_framebuffer_x(bitplane_fetch_hpos_for_plane(inset_ddf, 0, 0));

    assert_ne!(
        inset_ddf.fetch_start_native_x(inset_ddf.diw_h_start(), 2),
        0
    );
    assert!(first_word_x < first_bpl1dat_x);
    assert_eq!(
        bitplane_output_start_x(
            inset_ddf,
            &[],
            inset_ddf.display_window_x().0,
            inset_words,
            inset_ddf.dma_planes(),
        ),
        first_word_x
    );
}

#[test]
fn late_ddf_first_word_samples_all_planes_together() {
    let control = ControlState {
        bplcon0: 0x4000,
        ..ControlState::default()
    };
    let plane_words = [vec![0x4000], vec![0x4000], vec![0x4000], vec![0x4000]];
    let line = DenisePlannedPlayfieldLine::new(0, 0, 64, &plane_words, 16);

    assert_eq!(line.sample(control, 1).idx, 0x0F);
}

#[test]
fn bplcon1_delay_blanks_left_edge_without_shifting_row() {
    // The scroll nibble counts lo-res pixels in every resolution, so a
    // scroll of 4 blanks the same physical width - 8 framebuffer hi-res
    // pixels - whether the playfield is hi-res or lo-res.
    assert_eq!(
        left_edge_blank_pixels(ControlState {
            bplcon0: 0x8000,
            bplcon1: 4,
            bplcon2: 0,
            ..ControlState::default()
        }),
        8
    );
    assert_eq!(
        left_edge_blank_pixels(ControlState {
            bplcon0: 0,
            bplcon1: 4,
            bplcon2: 0,
            ..ControlState::default()
        }),
        8
    );
}

#[test]
fn bplcon1_delay_starts_scanline_from_empty_shifter() {
    let control = ControlState {
        bplcon0: 0x1000,
        bplcon1: 3,
        ..ControlState::default()
    };
    let plane_words = [vec![0x8000]];
    let line = DenisePlannedPlayfieldLine::new(0, 0, 32, &plane_words, 16);

    assert_eq!(
        line.sample(control, 0),
        DeniseBitplaneSample {
            idx: 0,
            nplanes: 1,
            active: true,
        }
    );
    assert_eq!(line.sample(control, 1).idx, 0);
    assert_eq!(line.sample(control, 2).idx, 0);
    assert_eq!(line.sample(control, 3).idx, 1);
}

#[test]
fn aga_extended_bplcon1_delay_blanks_until_current_line_sample() {
    let control = ControlState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon0: BPLCON0_ECSENA | 0x1000,
        bplcon1: 0x0800,
        fmode: 0x0003,
        ..ControlState::default()
    };
    assert_eq!(control.scroll_for_plane(0), 32);

    let plane_words = [vec![0x8000, 0x0000, 0x0000]];
    let line = DenisePlannedPlayfieldLine::new(0, 0, 64, &plane_words, 48);

    assert_eq!(line.sample(control, 15).idx, 0);
    assert_eq!(line.sample(control, 16).idx, 0);
    assert_eq!(line.sample(control, 31).idx, 0);
    assert_eq!(line.sample(control, 32).idx, 1);
}

#[test]
fn bplcon1_delay_drops_prefetch_samples_at_block_start() {
    let control = ControlState {
        bplcon0: 0x1000,
        bplcon1: 3,
        ..ControlState::default()
    };
    let plane_words = [vec![0xE000]];
    let line = DenisePlannedPlayfieldLine::new(0, 0, 32, &plane_words, 16);
    let delays = std::array::from_fn(|plane| control.sample_delay_for_plane(plane));

    assert_eq!(line.sample_prepared(1, &delays, 0, 3).idx, 1);
    assert_eq!(line.sample_prepared(1, &delays, 2, 3).idx, 0);
    assert_eq!(line.sample_prepared(1, &delays, 2, 5).idx, 1);
}

#[test]
fn visible_cpu_palette_write_replays_by_beam_position() {
    let mut state = blank_state();
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [cpu_event(0x45, COPPER_WAIT_HPOS_FB0 as u32, 0x0180, 0x0FFF)];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let line = (0x45 - 0x2C) as usize;
    assert_eq!(base_palettes[0][0], 0x0103);
    assert_eq!(base_palettes[line][0], 0x0103);
    assert_eq!(base_palettes[line + 1][0], 0x0FFF);
    assert_eq!(palette_segments[line].len(), 1);
    assert_eq!(palette_segments[line][0].x, 0);
    assert_eq!(palette_segments[line][0].entry, 0);
    assert_eq!(palette_segments[line][0].value, 0x0FFF);
    assert_eq!(state.palette[0], 0x0FFF);
}

#[test]
fn small_visible_cpu_palette_batch_replays_by_beam_position() {
    let mut state = blank_state();
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let mut events = Vec::new();
    for idx in 0..30 {
        events.push(cpu_event(
            0x45 + idx as u32,
            COPPER_WAIT_HPOS_FB0 as u32,
            0x0180,
            idx as u16,
        ));
    }

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let line = (0x45 - 0x2C) as usize;
    assert_eq!(base_palettes[line][0], 0x0103);
    assert_eq!(palette_segments[line].len(), 1);
    assert_eq!(palette_segments[line][0].x, 0);
    assert_eq!(palette_segments[line][0].entry, 0);
    assert_eq!(palette_segments[line][0].value, 0);
}

#[test]
fn cpu_copper_irq_palette_writes_replay_by_beam_position() {
    let mut state = blank_state();
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [cpu_copper_irq_event(
        0x45,
        COPPER_WAIT_HPOS_FB0 as u32,
        0x0180,
        0x0FFF,
    )];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let line = (0x45 - 0x2C) as usize;
    assert_eq!(base_palettes[line][0], 0x0103);
    assert_eq!(palette_segments[line].len(), 1);
    assert_eq!(palette_segments[line][0].x, 0);
    assert_eq!(palette_segments[line][0].entry, 0);
    assert_eq!(palette_segments[line][0].value, 0x0FFF);
}

#[test]
fn cpu_palette_writes_before_visible_area_update_frame_base() {
    let mut state = blank_state();
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [cpu_event(0x10, COPPER_WAIT_HPOS_FB0 as u32, 0x0180, 0x0ACE)];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    assert!(palette_segments.iter().all(Vec::is_empty));
    assert_eq!(base_palettes[0][0], 0x0ACE);
    assert_eq!(base_palettes[FB_HEIGHT - 1][0], 0x0ACE);
}

#[test]
fn cpu_banked_palette_load_lands_each_write_in_its_bplcon3_bank() {
    // Lisa resolves a COLORxx write against the BPLCON3 latch standing at
    // that write: BANK (bits 15-13) selects the block of 32 colour-table
    // entries and LOCT (bit 9) the nibble half (LOCT=0 writes both halves,
    // LOCT=1 the low nibbles only). A CPU load of the 256-entry table walks
    // BANK across the blocks and toggles LOCT between writes, all inside
    // vertical blank before the display starts, so each write has to be
    // resolved against the BPLCON3 value in force when it happened. Resolving
    // the whole load against the value the frame opened with collapses every
    // bank onto that one and destroys the entries it aliases over.
    const LOCT: u16 = 0x0200;
    let mut state = blank_state();
    state.agnus_revision = AgnusRevision::AgaAlice;
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();

    // (BANK, COLORxx index, high-nibble word, low-nibble word). BANK 0, the
    // bank the frame opens in, is deliberately never selected.
    let loads = [
        (1usize, 0usize, 0x0123u16, 0x0456u16),
        (2, 15, 0x0789, 0x0ABC),
        (7, 31, 0x0FED, 0x0CBA),
    ];
    let vblank_line = (PAL_VISIBLE_LINE0 - 20) as u32;
    let hpos = COPPER_WAIT_HPOS_FB0 as u32;
    let mut events = Vec::new();
    for (bank, idx, hi, lo) in loads {
        let bank_bits = BPLCON3_PF2OF_DEFAULT | ((bank as u16) << 13);
        let color = 0x180 + (idx as u16) * 2;
        events.push(cpu_event(vblank_line, hpos, 0x106, bank_bits));
        events.push(cpu_event(vblank_line, hpos, color, hi));
        events.push(cpu_event(vblank_line, hpos, 0x106, bank_bits | LOCT));
        events.push(cpu_event(vblank_line, hpos, color, lo));
    }

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    assert!(palette_segments.iter().all(Vec::is_empty));
    for (bank, idx, hi, lo) in loads {
        let entry = bank * 32 + idx;
        // The 8-bit components are the high nibble from the LOCT=0 write and
        // the low nibble from the LOCT=1 write.
        let expected_rgb24 = (u32::from(hi >> 8 & 0xF) << 20)
            | (u32::from(lo >> 8 & 0xF) << 16)
            | (u32::from(hi >> 4 & 0xF) << 12)
            | (u32::from(lo >> 4 & 0xF) << 8)
            | (u32::from(hi & 0xF) << 4)
            | u32::from(lo & 0xF);
        assert_eq!(
            state.palette.entry(entry).latch(),
            hi,
            "bank {bank} COLOR{idx:02} latch"
        );
        assert_eq!(
            state.palette.entry(entry).rgb24(),
            expected_rgb24,
            "bank {bank} COLOR{idx:02} merged 24-bit value"
        );
        assert_eq!(
            base_palettes[0].entry(entry).rgb24(),
            expected_rgb24,
            "bank {bank} COLOR{idx:02} in the frame base palette"
        );
    }
    // Bank 0 was never selected, so none of the load may land there.
    for (_, idx, _, _) in loads {
        assert_eq!(
            state.palette.entry(idx).latch(),
            0x0103,
            "bank 0 COLOR{idx:02} untouched"
        );
    }
}

#[test]
fn ddf_overscan_fetches_still_advance_bitplane_pointers() {
    let state = RenderState {
        ddfstrt: 0x0038,
        ddfstop: 0x00D8,
        ..blank_state()
    };
    assert_eq!(state.words_per_row(true, 640), 42);
    assert_eq!(state.words_per_row(false, 320), 21);

    // A DDFSTRT below the hardwired start keeps its raw fetch-grid anchor
    // (an armed run really starts at $10; vAmigaTS oldhwstop3/4), so the
    // word count stays linear in the raw value; only the stop is clipped
    // to the hardware stop window.
    let early_start = RenderState {
        ddfstrt: 0x0010,
        ddfstop: 0x00E0,
        ..blank_state()
    };
    assert_eq!(early_start.words_per_row(false, 320), 26);

    let ocs_equal = RenderState {
        ddfstrt: 0x0038,
        ddfstop: 0x0038,
        ..blank_state()
    };
    assert_eq!(ocs_equal.words_per_row(false, 320), 21);

    let lores_partial_stop = RenderState {
        ddfstrt: 0x004A,
        ddfstop: 0x00B6,
        ..blank_state()
    };
    assert_eq!(lores_partial_stop.words_per_row(false, 320), 15);

    let lores_odd_stop = RenderState {
        ddfstrt: 0x0064,
        ddfstop: 0x00A5,
        ..blank_state()
    };
    assert_eq!(lores_odd_stop.words_per_row(false, 320), 9);

    let lores_four_cck_start = RenderState {
        ddfstrt: 0x0034,
        ddfstop: 0x00D4,
        ..blank_state()
    };
    assert_eq!(lores_four_cck_start.words_per_row(false, 320), 21);

    let lores_second_half_stop = RenderState {
        ddfstrt: 0x0028,
        ddfstop: 0x00D4,
        ..blank_state()
    };
    assert_eq!(lores_second_half_stop.words_per_row(false, 320), 23);

    let ecs_equal = RenderState {
        agnus_revision: AgnusRevision::Ecs8372Rev4,
        ddfstrt: 0x0038,
        ddfstop: 0x0038,
        ..blank_state()
    };
    assert_eq!(ecs_equal.words_per_row(false, 320), 1);
}

#[test]
fn six_plane_non_ham_non_dual_playfield_selects_extra_half_brite() {
    let state = ControlState {
        bplcon0: 0x6000,
        bplcon1: 0,
        bplcon2: 0,
        ..ControlState::default()
    };
    assert!(state.extra_half_brite());

    let ham = ControlState {
        bplcon0: 0x6800,
        bplcon1: 0,
        bplcon2: 0,
        ..ControlState::default()
    };
    assert!(!ham.extra_half_brite());

    let dual_playfield = ControlState {
        bplcon0: 0x6400,
        bplcon1: 0,
        bplcon2: 0,
        ..ControlState::default()
    };
    assert!(!dual_playfield.extra_half_brite());

    let kill_ehb = ControlState {
        bplcon0: 0x6000,
        bplcon1: 0,
        bplcon2: 0x0200,
        ..ControlState::default()
    };
    assert!(!kill_ehb.extra_half_brite());
}

#[test]
fn six_plane_ham_selects_hold_and_modify() {
    let state = ControlState {
        bplcon0: 0x6800,
        bplcon1: 0,
        bplcon2: 0,
        ..ControlState::default()
    };
    assert!(state.hold_and_modify());
    assert!(!state.extra_half_brite());

    let five_plane_ham_bit = ControlState {
        bplcon0: 0x5800,
        bplcon1: 0,
        bplcon2: 0,
        ..ControlState::default()
    };
    assert!(five_plane_ham_bit.hold_and_modify());
}

#[test]
fn shres_limits_bitplane_depth_and_disables_ham() {
    let state = ControlState {
        bplcon0: BPLCON0_SHRES | 0x6800,
        bplcon1: 0,
        bplcon2: 0,
        ..ControlState::default()
    };

    assert_eq!(state.nplanes(), 2);
    // Agnus's SHRES fetch-unit table schedules at most 2 plane streams;
    // an overprogrammed BPU (here 6) fetches nothing at all (the same
    // hardware rule as hi-res BPU>4, invplanes1 photo).
    assert_eq!(state.dma_planes(), 0);
    assert!(!state.hold_and_modify());
}

#[test]
fn ocs_lowres_bpu7_renders_six_latched_planes_with_four_dma_planes() {
    let state = ControlState {
        bplcon0: 0x7800,
        bplcon1: 0,
        bplcon2: 0,
        ..ControlState::default()
    };
    assert_eq!(state.nplanes(), 6);
    assert_eq!(state.dma_planes(), 4);
    assert!(state.hold_and_modify());
    assert!(!state.extra_half_brite());
}

#[test]
fn shres_bitplane_fetch_uses_four_words_per_fetch_slot() {
    let control = ControlState {
        agnus_revision: AgnusRevision::Ecs8372Rev4,
        bplcon0: BPLCON0_SHRES | 0x2000,
        ddfstrt: 0x0038,
        ddfstop: 0x0038,
        ..ControlState::default()
    };

    assert_eq!(
        control.words_per_row(native_frame_width_for_control(control)),
        4
    );
}

#[test]
fn display_line_fetch_plan_records_lowres_bpu7_four_dma_slots_in_beam_order() {
    let control = ControlState {
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x7800,
        ddfstrt: 0x0038,
        ddfstop: 0x0040,
        ..ControlState::default()
    };
    let segments = [ControlSegment { x: 0, control }];

    let plans = line_fetch_plans_for_line(control, &segments, 2, control.dma_planes());

    assert_eq!(plans[0].word_fetch_hpos, Some(0x0038));
    assert_eq!(plans[1].word_fetch_hpos, Some(0x0040));
    assert_eq!(
        plans[0].iter().collect::<Vec<_>>(),
        vec![(0x0039, 3), (0x003B, 1), (0x003D, 2), (0x003F, 0)]
    );
    assert_eq!(
        plans[1].iter().collect::<Vec<_>>(),
        vec![(0x0041, 3), (0x0043, 1), (0x0045, 2), (0x0047, 0)]
    );
}

#[test]
fn display_line_plan_records_words_registers_and_sprite_slots_in_beam_order() {
    let control = ControlState {
        dmacon: DMACON_DMAEN | DMACON_BPLEN | DMACON_SPREN,
        bplcon0: 0x7800,
        ddfstrt: 0x0038,
        ddfstop: 0x0040,
        ..ControlState::default()
    };
    let row_words = [
        vec![0x1000, 0x1001],
        vec![0x2000, 0x2001],
        vec![0x3000, 0x3001],
        vec![0x4000, 0x4001],
        vec![0x5000, 0x5001],
        vec![0x6000, 0x6001],
        Vec::new(),
        Vec::new(),
    ];
    let fetch_plans = line_fetch_plans_for_line(control, &[], 2, control.dma_planes());
    let register_events = [DisplayLinePlanEvent::BpldatWrite {
        hpos: 0x003E,
        x: beam_to_framebuffer_x_unclamped(0x003E),
        plane: 5,
        value: 0xAAAA,
    }];
    let sprite_lines = [CapturedSpriteLine {
        sprite: 2,
        hstart: 0x90,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0x8000,
        datb: 0x4000,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];
    let plan = DisplayLinePlan::new(
        0,
        PAL_VISIBLE_LINE0 as u32,
        0,
        64,
        control.nplanes(),
        control.dma_planes(),
        2,
        &fetch_plans,
        &row_words,
        &register_events,
        &sprite_lines,
        control,
    );
    let events = plan.collect_events();

    assert!(events.windows(2).all(|pair| {
        (pair[0].hpos(), pair[0].beam_order()) <= (pair[1].hpos(), pair[1].beam_order())
    }));
    assert!(events.contains(&DisplayLinePlanEvent::SpriteSlot {
        hpos: SPRITE_DMA_PAIR_CAPTURE_HPOS[1],
        sprite: 2,
        hstart: 0x90,
        data: 0x8000,
        datb: 0x4000,
        attached: false,
    }));
    assert!(events.contains(&DisplayLinePlanEvent::BitplaneDmaFetch {
        hpos: 0x003F,
        word_idx: 0,
        plane: 0,
        word: 0x1000,
    }));
    assert!(events.contains(&DisplayLinePlanEvent::LatchedBitplaneWord {
        hpos: 0x003F,
        word_idx: 0,
        plane: 4,
        word: 0x5000,
    }));
    assert!(events.contains(&DisplayLinePlanEvent::BpldatWrite {
        hpos: 0x003E,
        x: beam_to_framebuffer_x_unclamped(0x003E),
        plane: 5,
        value: 0xAAAA,
    }));
}

#[test]
fn ehb_palette_indexes_use_half_bright_base_color() {
    let mut palette = Palette::new();
    palette.write_ocs(3, 0x0E86);

    assert_eq!(half_brite_rgb12(0x0E86), 0x0743);
    assert_eq!(palette_index_to_rgb12(&palette, 0x23, true), 0x0743);
    assert_eq!(palette_index_to_rgb12(&palette, 0x23, false), 0x0E86);
}

fn render_sprite_dma_test_frame(
    state: &RenderState,
    ram: &[u8],
    refreshes: [SpritePointerRefresh; 8],
) -> Vec<u32> {
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites_with_manual_lines(
        state,
        ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        refreshes,
        &[],
        false,
        None,
    );

    fb
}

#[test]
fn captured_sprite_dma_blocks_sprite_seven_when_ddfstrt_uses_early_fetch_slot() {
    let mut state = blank_state();
    state.dmacon = DMACON_DMAEN | DMACON_SPREN | DMACON_BPLEN;
    state.bplcon0 = 0x1000;
    state.ddfstrt = 0x0028;
    state.ddfstop = 0x0038;
    state.palette.write_ocs(29, 0x0F00);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 7,
        hstart: SPRITE_HSTART_FB0,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0x8000,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &captured,
        true,
    );
    assert_eq!(fb[0], rgb12_to_rgba8(0x0000));

    state.ddfstrt = 0x0038;
    state.ddfstop = 0x0038;
    let base_palettes = [state.palette; FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    fb.fill(rgb12_to_rgba8(0));
    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &captured,
        true,
    );
    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
}

#[test]
fn bplcon3_spres_default_uses_ecs_sprite_width_for_hires_playfield() {
    let mut state = RenderState {
        dmacon: DMACON_DMAEN | DMACON_SPREN,
        bplcon0: 0x8000,
        ..blank_state()
    };
    state.palette.write_ocs(17, 0x0F00);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: SPRITE_HSTART_FB0,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0x8000,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &captured,
        true,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[1], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[2], rgb12_to_rgba8(0));
}

#[test]
fn bplcon3_spres_default_upgrades_to_70ns_when_shres_is_set() {
    let mut state = RenderState {
        dmacon: DMACON_DMAEN | DMACON_SPREN,
        bplcon0: BPLCON0_SHRES,
        ..blank_state()
    };
    state.palette.write_ocs(17, 0x0F00);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: SPRITE_HSTART_FB0,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0x8000,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &captured,
        true,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[1], rgb12_to_rgba8(0));
}

#[test]
fn shres_sprite_control_bit4_adds_70ns_horizontal_offset() {
    let (pos, ctl) = sprite_control_words(PAL_VISIBLE_LINE0 as u16, 0x2D, DIW_HSTART_FB0 as u16);
    let mut state = RenderState {
        dmacon: DMACON_DMAEN | DMACON_SPREN,
        bplcon0: BPLCON0_SHRES,
        ..blank_state()
    };
    state.palette.write_ocs(17, 0x0F00);
    state.sprpos[0] = pos;
    state.spr_hw_pos[0] = pos;
    state.sprctl[0] = ctl | 0x0010;
    state.spr_hw_ctl[0] = ctl | 0x0010;
    state.sprdata[0] = 0x8000;
    state.spr_hw_data[0] = 0x8000;
    state.spr_armed[0] = true;
    state.spr_hw_armed[0] = true;
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &[],
        false,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0));
    assert_eq!(fb[1], rgb12_to_rgba8(0x0F00));
}

#[test]
fn bplcon3_spres_hires_draws_one_framebuffer_pixel_per_sprite_bit() {
    let mut state = RenderState {
        dmacon: DMACON_DMAEN | DMACON_SPREN,
        bplcon3: BPLCON3_SPRES_HIRES,
        ..blank_state()
    };
    state.palette.write_ocs(17, 0x0F00);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: SPRITE_HSTART_FB0,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0x8000,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &captured,
        true,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[1], rgb12_to_rgba8(0));
}

#[test]
fn bplcon3_spres_hires_applies_to_attached_sprite_pairs() {
    let mut state = RenderState {
        dmacon: DMACON_DMAEN | DMACON_SPREN,
        bplcon3: BPLCON3_SPRES_HIRES,
        ..blank_state()
    };
    state.palette.write_ocs(21, 0x00F0);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let captured = [
        CapturedSpriteLine {
            sprite: 0,
            hstart: SPRITE_HSTART_FB0,
            hsub_70ns: false,
            beam_y: PAL_VISIBLE_LINE0,
            data: 0x8000,
            datb: 0,
            attached: false,
            data_ext: [0; 3],
            datb_ext: [0; 3],
            width_words: 1,
        },
        CapturedSpriteLine {
            sprite: 1,
            hstart: SPRITE_HSTART_FB0,
            hsub_70ns: false,
            beam_y: PAL_VISIBLE_LINE0,
            data: 0x8000,
            datb: 0,
            attached: true,
            data_ext: [0; 3],
            datb_ext: [0; 3],
            width_words: 1,
        },
    ];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &captured,
        true,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x00F0));
    assert_eq!(fb[1], rgb12_to_rgba8(0));
}

#[test]
fn sprite_dma_ignores_unrefreshed_stale_sprite_pointer() {
    let mut ram = vec![0u8; 512 * 1024];
    let sprite_ptr = 0x0100usize;
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 1,
        DIW_HSTART_FB0 as u16,
    );
    put_word(&mut ram, sprite_ptr, pos);
    put_word(&mut ram, sprite_ptr + 2, ctl);
    put_word(&mut ram, sprite_ptr + 4, 0x8000);
    put_word(&mut ram, sprite_ptr + 6, 0);
    put_word(&mut ram, sprite_ptr + 8, 0);
    put_word(&mut ram, sprite_ptr + 10, 0);

    let mut state = blank_state();
    state.dmacon = DMACON_DMAEN | DMACON_SPREN;
    state.sprpt[0] = sprite_ptr as u32;
    state.palette.write_ocs(17, 0x0F00);
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &[],
        false,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0));
}

#[test]
fn manual_sprite_data_writes_affect_only_later_beam_lines() {
    let mut initial_state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 8,
        DIW_HSTART_FB0 as u16,
    );
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.palette.write_ocs(17, 0x0F00);

    let write_line = PAL_VISIBLE_LINE0 + 4;
    let events = [cpu_event(
        write_line as u32,
        COPPER_WAIT_HPOS_FB0 as u32,
        0x144,
        0x8000,
    )];
    let manual_sprite_lines = manual_sprite_lines_from_events(&initial_state, &events);
    assert!(manual_sprite_lines[0]
        .iter()
        .all(|line| line.beam_y >= write_line));

    let base_controls = [ControlState::from_render_state(&initial_state); FB_HEIGHT];
    let mut render_state = initial_state;
    apply_move(&mut render_state, 0x144, 0x8000);
    let ram = vec![0u8; 512 * 1024];
    let base_palettes = [render_state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites_with_manual_lines(
        &render_state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        sprite_pointer_refreshes_from_mask([false; 8]),
        &[],
        false,
        Some(&manual_sprite_lines),
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0));
    assert_eq!(fb[4 * FB_WIDTH], rgb12_to_rgba8(0x0F00));
}

#[test]
fn armed_sprite_latch_keeps_serializing_in_frames_without_sprite_writes() {
    // Denise has no vertical comparator: a sprite armed by a SPRxDATA write
    // stays armed across frame boundaries and serializes at its POS/CTL
    // position on every line until SPRxCTL disarms it. A frame with NO
    // sprite register writes at all must therefore still emit the latched
    // output while sprite DMA is idle (the live render path passes
    // include_latched_sprite_state = !sprite_dma_observed). Software arms a
    // masking bar once at scene init and leaves it alone for the whole
    // scene; a DATA-armed vertical bar covering a raced copper-chunky
    // column edge is the gen-x mosaic regression example.
    // The bar's POS/CTL span the display vertically, like the demo's
    // (vstart $28, vstop $130); the latched replay still consults that
    // window, so a full-scene bar needs a full-coverage window.
    let mut initial_state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + FB_HEIGHT as u16,
        DIW_HSTART_FB0 as u16 + 40,
    );
    initial_state.sprpos[7] = pos;
    initial_state.spr_hw_pos[7] = pos;
    initial_state.sprctl[7] = ctl;
    initial_state.spr_hw_ctl[7] = ctl;
    initial_state.sprdata[7] = 0xF000;
    initial_state.spr_hw_data[7] = 0xF000;
    initial_state.sprdatb[7] = 0xF000;
    initial_state.spr_hw_datb[7] = 0xF000;
    initial_state.spr_armed[7] = true;
    initial_state.spr_hw_armed[7] = true;

    // Sprite DMA idle: the latch emits on every line of the window even
    // though this frame carries no sprite register writes.
    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[],
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        true,
        false,
    );
    let line_count = manual_sprite_lines[7]
        .iter()
        .filter(|line| line.data == 0xF000 && line.datb == 0xF000)
        .count();
    assert!(
        line_count >= FB_HEIGHT / 2,
        "an armed sprite latch must serialize on every line of a frame \
         without sprite writes (got {line_count} lines)"
    );

    // With sprite DMA observed this frame, captured DMA lines own the
    // channel's output and the carried latch stays silent.
    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[],
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        false,
        true,
    );
    assert!(manual_sprite_lines[7].is_empty());
}

#[test]
fn latched_sprite_vstart_equal_vstop_is_empty() {
    let mut initial_state = blank_state();
    let beam_y = PAL_VISIBLE_LINE0;
    let (pos, ctl) = sprite_control_words(beam_y as u16, beam_y as u16, DIW_HSTART_FB0 as u16);
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.sprdata[0] = 0xFFFF;
    initial_state.spr_hw_data[0] = 0xFFFF;
    initial_state.spr_armed[0] = true;
    initial_state.spr_hw_armed[0] = true;

    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[],
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        true,
        true,
    );

    assert!(manual_sprite_lines[0].is_empty());
}

#[test]
fn direct_sprite_data_write_ignores_dma_vertical_window() {
    let mut initial_state = blank_state();
    let beam_y = PAL_VISIBLE_LINE0 + 12;
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16,
        DIW_HSTART_FB0 as u16,
    );
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;

    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[cpu_event(
            beam_y as u32,
            COPPER_WAIT_HPOS_FB0 as u32,
            0x144,
            0xFFFF,
        )],
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        false,
        true,
    );

    assert!(manual_sprite_lines[0]
        .iter()
        .any(|line| line.beam_y == beam_y));
}

#[test]
fn manual_sprite_data_write_replicates_words_across_fmode_wide_register() {
    let mut initial_state = blank_state();
    initial_state.agnus_revision = AgnusRevision::AgaAlice;
    initial_state.fmode = 0x000C; // SPR32 | SPAGEM: 64-pixel sprites
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 4,
        DIW_HSTART_FB0 as u16,
    );
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;

    let events = [
        cpu_event(PAL_VISIBLE_LINE0 as u32, 0, 0x146, 0x00FF),
        cpu_event(PAL_VISIBLE_LINE0 as u32, 0, 0x144, 0x8001),
    ];
    let manual_sprite_lines = manual_sprite_lines_from_events(&initial_state, &events);
    let line = manual_sprite_lines[0]
        .iter()
        .find(|line| line.beam_y == PAL_VISIBLE_LINE0 + 1)
        .expect("armed manual sprite line");
    assert_eq!(line.width_words, 4);
    assert_eq!(line.data, 0x8001);
    assert_eq!(line.data_ext, [0x8001; 3]);
    assert_eq!(line.datb, 0x00FF);
    assert_eq!(line.datb_ext, [0x00FF; 3]);
}

#[test]
fn manual_sprite_width_stays_one_word_without_aga_lisa() {
    let mut initial_state = blank_state();
    // FMODE can only be nonzero on Alice, but the manual sprite replay
    // must not widen on a pre-AGA revision even if state carries a value.
    initial_state.fmode = 0x000C;
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 4,
        DIW_HSTART_FB0 as u16,
    );
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;

    let events = [cpu_event(PAL_VISIBLE_LINE0 as u32, 0, 0x144, 0x8001)];
    let manual_sprite_lines = manual_sprite_lines_from_events(&initial_state, &events);
    let line = manual_sprite_lines[0]
        .iter()
        .find(|line| line.beam_y == PAL_VISIBLE_LINE0 + 1)
        .expect("armed manual sprite line");
    assert_eq!(line.width_words, 1);
    assert_eq!(line.data_ext, [0; 3]);
}

#[test]
fn manual_sprite_fmode_event_changes_width_for_later_lines() {
    let mut initial_state = blank_state();
    initial_state.agnus_revision = AgnusRevision::AgaAlice;
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 8,
        DIW_HSTART_FB0 as u16,
    );
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;

    let fmode_line = PAL_VISIBLE_LINE0 + 4;
    let events = [
        cpu_event(PAL_VISIBLE_LINE0 as u32, 0, 0x144, 0x8001),
        cpu_event(fmode_line as u32, 0, 0x1FC, 0x0004),
    ];
    let manual_sprite_lines = manual_sprite_lines_from_events(&initial_state, &events);
    for line in &manual_sprite_lines[0] {
        if line.beam_y < fmode_line {
            assert_eq!(line.width_words, 1, "beam_y={}", line.beam_y);
        } else {
            assert_eq!(line.width_words, 2, "beam_y={}", line.beam_y);
            assert_eq!(line.data_ext[0], 0x8001);
        }
    }
    assert!(manual_sprite_lines[0]
        .iter()
        .any(|line| line.beam_y >= fmode_line));
}

#[test]
fn sprite_dma_capture_suppresses_latched_manual_sprite_spans() {
    let mut initial_state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 8,
        DIW_HSTART_FB0 as u16,
    );
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.sprdata[0] = 0xFFFF;
    initial_state.spr_hw_data[0] = 0xFFFF;
    initial_state.spr_armed[0] = true;
    initial_state.spr_hw_armed[0] = true;

    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[],
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        false,
        true,
    );

    assert!(manual_sprite_lines[0].is_empty());
}

#[test]
fn manual_sprite_replay_does_not_seed_from_frame_start_data_latch() {
    let mut initial_state = blank_state();
    let (pos, ctl) = sprite_control_words(PAL_VISIBLE_LINE0 as u16, 0, DIW_HSTART_FB0 as u16);
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.sprdata[0] = 0xFFFF;
    initial_state.spr_hw_data[0] = 0xFFFF;
    initial_state.spr_armed[0] = true;
    initial_state.spr_hw_armed[0] = true;

    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[],
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        false,
        true,
    );

    assert!(manual_sprite_lines[0].is_empty());
}

#[test]
fn pre_dma_position_write_does_not_preserve_frame_start_latch_when_dma_observed() {
    let mut initial_state = blank_state();
    let beam_y = PAL_VISIBLE_LINE0;
    let (old_pos, ctl) =
        sprite_control_words(beam_y as u16, beam_y as u16 + 1, DIW_HSTART_FB0 as u16);
    let pre_dma_hstart = (DIW_HSTART_FB0 + 16) as u16;
    let (pre_dma_pos, _) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, pre_dma_hstart);
    let post_dma_hstart = (DIW_HSTART_FB0 + 32) as u16;
    let (post_dma_pos, _) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, post_dma_hstart);
    initial_state.sprpos[0] = old_pos;
    initial_state.spr_hw_pos[0] = old_pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.sprdata[0] = 0x8000;
    initial_state.spr_hw_data[0] = 0x8000;
    initial_state.spr_armed[0] = true;
    initial_state.spr_hw_armed[0] = true;

    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[
            cpu_event(
                beam_y as u32,
                SPRITE_DMA_PAIR_CAPTURE_HPOS[0] - 1,
                0x140,
                pre_dma_pos,
            ),
            cpu_event(
                beam_y as u32,
                SPRITE_DMA_PAIR_CAPTURE_HPOS[0] + 1,
                0x140,
                post_dma_pos,
            ),
        ],
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        false,
        true,
    );

    assert!(manual_sprite_lines[0].is_empty());
}

#[test]
fn post_dma_position_write_does_not_reuse_frame_start_latch_when_dma_observed() {
    let mut initial_state = blank_state();
    let beam_y = PAL_VISIBLE_LINE0;
    let (old_pos, ctl) =
        sprite_control_words(beam_y as u16, beam_y as u16 + 1, DIW_HSTART_FB0 as u16);
    let reused_hstart = (DIW_HSTART_FB0 + 32) as u16;
    let (new_pos, _) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, reused_hstart);
    initial_state.sprpos[0] = old_pos;
    initial_state.spr_hw_pos[0] = old_pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.sprdata[0] = 0x8000;
    initial_state.spr_hw_data[0] = 0x8000;
    initial_state.spr_armed[0] = true;
    initial_state.spr_hw_armed[0] = true;

    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[cpu_event(
            beam_y as u32,
            SPRITE_DMA_PAIR_CAPTURE_HPOS[0] + 1,
            0x140,
            new_pos,
        )],
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        false,
        true,
    );

    assert!(manual_sprite_lines[0].is_empty());
}

#[test]
fn same_line_position_retime_does_not_reuse_frame_start_latch_when_dma_observed() {
    let mut initial_state = blank_state();
    let beam_y = 99;
    initial_state.sprpos[3] = 0x5020;
    initial_state.spr_hw_pos[3] = 0x5020;
    initial_state.sprctl[3] = 0x0602;
    initial_state.spr_hw_ctl[3] = 0x0602;
    initial_state.sprdata[3] = 0xE92D;
    initial_state.spr_hw_data[3] = 0xE92D;
    initial_state.sprdatb[3] = 0x16FF;
    initial_state.spr_hw_datb[3] = 0x16FF;
    initial_state.spr_armed[3] = true;
    initial_state.spr_hw_armed[3] = true;

    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[
            cpu_event(beam_y as u32, 8, 0x158, 0x5020),
            cpu_event(beam_y as u32, 64, 0x158, 0x503C),
        ],
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        false,
        true,
    );

    assert!(manual_sprite_lines[3].is_empty());
}

#[test]
fn pre_visible_data_write_seeds_latch_without_direct_output_guard() {
    let mut initial_state = blank_state();
    initial_state.sprpos[3] = 0x5020;
    initial_state.spr_hw_pos[3] = 0x5020;

    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[
            cpu_event(0, 78, 0x15A, 0x0602),
            cpu_event(0, 110, 0x15E, 0x16FF),
            cpu_event(0, 142, 0x15C, 0xE92D),
            cpu_event(99, 8, 0x158, 0x5020),
            cpu_event(99, 64, 0x158, 0x503C),
        ],
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        false,
        true,
    );

    assert!(manual_sprite_lines[3].iter().any(|line| {
        line.beam_y == 99 && line.hstart == 0x78 && line.data == 0xE92D && line.datb == 0x16FF
    }));
}

#[test]
fn vblank_data_write_arms_manual_sprite_for_all_lines_without_sprite_dma() {
    // With sprite DMA idle, a vertical-blank arm sequence (SPRxPOS, SPRxCTL,
    // SPRxDATA, SPRxDATB) must behave like Denise: the CTL write disarms, the
    // DATA write re-arms, and the armed serializer fires at HSTART on every
    // line. VSTART == VSTOP is irrelevant without DMA (Denise has no vertical
    // comparator). Regression example: Gen-X's edge-masking line sprites,
    // armed once per frame during vblank.
    let initial_state = blank_state();

    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[
            cpu_event(0, 34, 0x140, 0x2C7E),
            cpu_event(0, 38, 0x142, 0x2C00),
            cpu_event(0, 50, 0x144, 0x0001),
            cpu_event(0, 54, 0x146, 0xFFFE),
        ],
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        false,
        false,
    );

    let hstart = 0x00FC;
    for beam_y in [
        PAL_VISIBLE_LINE0,
        PAL_VISIBLE_LINE0 + FB_HEIGHT as i32 / 2,
        PAL_VISIBLE_LINE0 + FB_HEIGHT as i32 - 1,
    ] {
        assert!(
            manual_sprite_lines[0].iter().any(|line| {
                line.beam_y == beam_y
                    && line.hstart == hstart
                    && line.data == 0x0001
                    && line.datb == 0xFFFE
            }),
            "expected armed manual sprite line at beam_y={beam_y}"
        );
    }
}

#[test]
fn early_position_write_keeps_manual_sprite_armed_without_sprite_dma() {
    // SPRxPOS never disarms a sprite on Denise; the early same-line POS guard
    // reconciles unseen DMA writes and must not fire when no sprite DMA ran.
    let initial_state = blank_state();
    let beam_y = PAL_VISIBLE_LINE0;
    let (pos, ctl) = sprite_control_words(beam_y as u16, beam_y as u16, DIW_HSTART_FB0 as u16);
    let (moved_pos, _) =
        sprite_control_words(beam_y as u16, beam_y as u16, (DIW_HSTART_FB0 + 32) as u16);

    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[
            cpu_event(0, 34, 0x140, pos),
            cpu_event(0, 38, 0x142, ctl),
            cpu_event(0, 50, 0x144, 0x8000),
            cpu_event((beam_y + 1) as u32, 8, 0x140, moved_pos),
        ],
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        false,
        false,
    );

    assert!(manual_sprite_lines[0].iter().any(|line| {
        line.beam_y == beam_y + 1
            && line.hstart == reg_hstart(DIW_HSTART_FB0 + 32)
            && line.data == 0x8000
    }));
}

#[test]
fn manual_sprite_replay_starts_from_beam_timed_data_write() {
    let mut initial_state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 4,
        DIW_HSTART_FB0 as u16,
    );
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;

    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[cpu_event(
            PAL_VISIBLE_LINE0 as u32,
            COPPER_WAIT_HPOS_FB0 as u32,
            0x144,
            0xFFFF,
        )],
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        false,
        true,
    );

    assert!(manual_sprite_lines[0]
        .iter()
        .any(|line| line.beam_y == PAL_VISIBLE_LINE0));
}

#[test]
fn early_line_position_write_does_not_reuse_previous_manual_sprite_data() {
    let mut initial_state = blank_state();
    let beam_y = PAL_VISIBLE_LINE0;
    let (pos, ctl) = sprite_control_words(beam_y as u16, beam_y as u16 + 4, DIW_HSTART_FB0 as u16);
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;

    let (next_pos, _) = sprite_control_words(
        (beam_y + 1) as u16,
        (beam_y + 2) as u16,
        (DIW_HSTART_FB0 + 32) as u16,
    );
    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[
            cpu_event(beam_y as u32, COPPER_WAIT_HPOS_FB0 as u32, 0x144, 0xFFFF),
            cpu_event((beam_y + 1) as u32, 4, 0x140, next_pos),
        ],
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        false,
        true,
    );

    assert!(manual_sprite_lines[0]
        .iter()
        .any(|line| line.beam_y == beam_y && line.data == 0xFFFF));
    assert!(manual_sprite_lines[0]
        .iter()
        .all(|line| line.beam_y != beam_y + 1));
}

#[test]
fn dma_seeded_sprite_reuse_keeps_later_register_data_on_same_line() {
    let beam_y = PAL_VISIBLE_LINE0;
    let mut manual_lines = vec![Vec::new(); 8];
    manual_lines[0].push(SpriteLine {
        hstart: SPRITE_HSTART_FB0 + 96,
        hsub_70ns: false,
        beam_y,
        data: 0xFFFF,
        datb: 0,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
        attached: false,
        x_start: 208,
        x_stop: FB_WIDTH,
    });

    let mut dma_seeded = vec![Vec::new(); 8];
    dma_seeded[0].push(SpriteLine {
        hstart: SPRITE_HSTART_FB0 + 96,
        hsub_70ns: false,
        beam_y,
        data: 0x8000,
        datb: 0,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
        attached: false,
        x_start: 112,
        x_stop: 240,
    });

    merge_dma_seeded_manual_sprite_lines(&mut manual_lines, dma_seeded);

    assert!(manual_lines[0].iter().any(|line| {
        line.beam_y == beam_y
            && line.data == 0xFFFF
            && line.x_start == 208
            && line.x_stop == FB_WIDTH
    }));
    assert!(manual_lines[0].iter().any(|line| {
        line.beam_y == beam_y && line.data == 0x8000 && line.x_start == 112 && line.x_stop == 208
    }));
    assert!(manual_lines[0]
        .iter()
        .all(|line| line.data != 0x8000 || line.x_stop <= 208));
}

#[test]
fn held_sprite_after_dma_disable_persists_past_descriptor_vstop() {
    let mut initial_state = blank_state();
    let held_vstart = PAL_VISIBLE_LINE0;
    let held_vstop = PAL_VISIBLE_LINE0 + 4;
    let live_vstart = PAL_VISIBLE_LINE0 + 32;
    let live_vstop = PAL_VISIBLE_LINE0 + 40;
    let live_hstart = DIW_HSTART_FB0 + 8;
    let (pos, ctl) =
        sprite_control_words(live_vstart as u16, live_vstop as u16, live_hstart as u16);
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;

    let mut held = [None; 8];
    held[0] = Some(HeldSpriteLine {
        line: CapturedSpriteLine {
            sprite: 0,
            hstart: SPRITE_HSTART_FB0,
            hsub_70ns: false,
            beam_y: held_vstart,
            data: 0x8000,
            datb: 0,
            data_ext: [0; 3],
            datb_ext: [0; 3],
            width_words: 1,
            attached: false,
        },
        vstart: held_vstart,
        vstop: held_vstop,
    });

    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[cpu_event(
            held_vstart as u32,
            COPPER_WAIT_HPOS_FB0 as u32,
            0x140,
            pos,
        )],
        &held,
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        true,
        true,
    );

    let line = manual_sprite_lines[0]
        .iter()
        .find(|line| line.beam_y == held_vstart)
        .expect("held sprite remains visible in its DMA vertical window");
    assert_eq!(line.hstart, reg_hstart(live_hstart));
    assert_eq!(line.x_start, 0);
    assert!(manual_sprite_lines[0]
        .iter()
        .any(|line| line.beam_y == held_vstop && line.hstart == reg_hstart(live_hstart)));
    assert!(manual_sprite_lines[0]
        .iter()
        .any(|line| line.beam_y > held_vstop && line.hstart == reg_hstart(live_hstart)));
}

#[test]
fn held_sprite_starts_from_dma_loaded_position_and_control() {
    let initial_state = blank_state();
    let held_vstart = PAL_VISIBLE_LINE0;
    let held_hstart = DIW_HSTART_FB0 + 16;

    let mut held = [None; 8];
    held[1] = Some(HeldSpriteLine {
        line: CapturedSpriteLine {
            sprite: 1,
            hstart: held_hstart,
            hsub_70ns: true,
            beam_y: held_vstart,
            data: 0x8000,
            datb: 0,
            data_ext: [0; 3],
            datb_ext: [0; 3],
            width_words: 1,
            attached: true,
        },
        vstart: held_vstart,
        vstop: held_vstart + 1,
    });

    let manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &initial_state,
        &[],
        &held,
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        true,
        true,
    );

    let line = manual_sprite_lines[1]
        .iter()
        .find(|line| line.beam_y == held_vstart)
        .expect("held sprite keeps DMA-loaded position without a register write");
    assert_eq!(line.hstart, held_hstart);
    assert!(line.hsub_70ns);
    assert!(line.attached);
}

#[test]
fn manual_sprite_position_write_before_hstart_uses_sprite_compare_domain() {
    let mut initial_state = blank_state();
    let beam_y = PAL_VISIBLE_LINE0;
    let old_hstart = 384;
    let new_hstart = 192;
    let (old_pos, old_ctl) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, old_hstart);
    let (new_pos, _) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, new_hstart);
    initial_state.sprpos[2] = old_pos;
    initial_state.spr_hw_pos[2] = old_pos;
    initial_state.sprctl[2] = old_ctl;
    initial_state.spr_hw_ctl[2] = old_ctl;
    initial_state.sprdata[2] = 0xFFFF;
    initial_state.spr_hw_data[2] = 0xFFFF;
    initial_state.spr_armed[2] = true;
    initial_state.spr_hw_armed[2] = true;

    // SPRxPOS writes update the Denise sprite comparator before the
    // general colour-output register domain reaches the same beam hpos.
    // The repositioned sprite must not be clipped to that later output x.
    let event_hpos = 96;
    let manual_sprite_lines = manual_sprite_lines_from_events(
        &initial_state,
        &[cpu_event(beam_y as u32, event_hpos, 0x150, new_pos)],
    );

    let old_line = manual_sprite_lines[2]
        .iter()
        .find(|line| line.beam_y == beam_y && line.hstart == reg_hstart(old_hstart as i32))
        .expect("old position interval");
    let new_line = manual_sprite_lines[2]
        .iter()
        .find(|line| line.beam_y == beam_y && line.hstart == reg_hstart(new_hstart as i32))
        .expect("new position interval");
    let sprite_position_x = sprite_position_write_framebuffer_x(event_hpos);
    let colour_output_x = ((event_hpos as i32 - COPPER_WAIT_HPOS_FB0) * 4) as usize;

    assert_eq!(old_line.x_stop, sprite_position_x);
    assert_eq!(new_line.x_start, sprite_position_x);
    assert_ne!(new_line.x_start, colour_output_x);
}

#[test]
fn copper_sprite_position_write_repositions_four_cck_later_than_cpu() {
    // The Copper WAIT-comparator lookahead model advances the Copper's
    // bus-cycle bookkeeping four colour clocks so register VALUE landings
    // match the vAmiga trace. The horizontal sprite comparator reload is a
    // fixed Denise pipeline measured from the real bus write, so a
    // copper-driven sprite multiplexer's reposition interval must carry that
    // lookahead back out and land four colour clocks (16 framebuffer units)
    // to the right of where a CPU write at the same recorded hpos would.
    // Regression example: the Desire "Hamazing" HAM sprite-multiplexer wipes,
    // whose per-line SPRxPOS repositions otherwise fall into the wrong
    // interval and leave horizontal streaks trailing the reveal.
    let event_hpos = 96;
    let cpu_x = sprite_position_write_framebuffer_x_from(event_hpos, false);
    let copper_x = sprite_position_write_framebuffer_x_from(event_hpos, true);
    assert_eq!(copper_x, cpu_x + 16);
    assert_eq!(
        copper_x,
        sprite_position_write_framebuffer_x(event_hpos + 4)
    );

    // The same offset carries into the event-driven reposition interval: a
    // copper reposition abuts 16 framebuffer units right of the CPU one.
    let mut initial_state = blank_state();
    let beam_y = PAL_VISIBLE_LINE0;
    let old_hstart = 200;
    let new_hstart = 260;
    let (old_pos, old_ctl) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, old_hstart);
    let (new_pos, _) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, new_hstart);
    initial_state.sprpos[0] = old_pos;
    initial_state.spr_hw_pos[0] = old_pos;
    initial_state.sprctl[0] = old_ctl;
    initial_state.spr_hw_ctl[0] = old_ctl;
    initial_state.sprdata[0] = 0xFFFF;
    initial_state.spr_hw_data[0] = 0xFFFF;
    initial_state.spr_armed[0] = true;
    initial_state.spr_hw_armed[0] = true;

    let cpu_lines = manual_sprite_lines_from_events(
        &initial_state,
        &[cpu_event(beam_y as u32, 96, 0x140, new_pos)],
    );
    let copper_lines = manual_sprite_lines_from_events(
        &initial_state,
        &[beam_event(beam_y as u32, 96, 0x140, new_pos)],
    );
    let cpu_new = cpu_lines[0]
        .iter()
        .find(|line| line.hstart == reg_hstart(new_hstart as i32))
        .expect("cpu new-position interval");
    let copper_new = copper_lines[0]
        .iter()
        .find(|line| line.hstart == reg_hstart(new_hstart as i32))
        .expect("copper new-position interval");
    assert_eq!(copper_new.x_start, cpu_new.x_start + 16);
}

#[test]
fn manual_sprite_position_writes_use_denise_compare_lag() {
    let mut initial_state = blank_state();
    let beam_y = PAL_VISIBLE_LINE0;
    let initial_hstart = 64;
    let first_hstart = 114;
    let second_hstart = 130;
    let (initial_pos, ctl) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, initial_hstart);
    let (first_pos, _) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, first_hstart);
    let (second_pos, _) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, second_hstart);
    initial_state.sprpos[0] = initial_pos;
    initial_state.spr_hw_pos[0] = initial_pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.sprdata[0] = 0xFFFF;
    initial_state.spr_hw_data[0] = 0xFFFF;
    initial_state.spr_armed[0] = true;
    initial_state.spr_hw_armed[0] = true;

    let first_hpos = 64;
    let second_hpos = 72;
    let manual_sprite_lines = manual_sprite_lines_from_events(
        &initial_state,
        &[
            cpu_event(beam_y as u32, first_hpos, 0x140, first_pos),
            cpu_event(beam_y as u32, second_hpos, 0x140, second_pos),
        ],
    );

    let first_line = manual_sprite_lines[0]
        .iter()
        .find(|line| line.beam_y == beam_y && line.hstart == reg_hstart(first_hstart as i32))
        .expect("first position interval");
    let second_line = manual_sprite_lines[0]
        .iter()
        .find(|line| line.beam_y == beam_y && line.hstart == reg_hstart(second_hstart as i32))
        .expect("second position interval");
    let first_base_x = ((first_hstart as i32 - DIW_HSTART_FB0) * 2) as usize;
    let second_base_x = ((second_hstart as i32 - DIW_HSTART_FB0) * 2) as usize;
    let base_control = ControlState::from_render_state(&initial_state);

    assert_eq!(first_line.x_start, first_base_x);
    assert!(first_line.x_stop > second_base_x);
    assert_eq!(second_line.x_start, second_base_x);
    assert_eq!(
        first_line.x_start,
        sprite_position_write_framebuffer_x(first_hpos)
    );
    assert_eq!(
        second_line.x_start,
        sprite_position_write_framebuffer_x(second_hpos)
    );
    assert_eq!(
        sprite_line_pixel_bits_at(first_line, second_base_x as i32 - 1, base_control, &[]),
        1
    );
    assert_eq!(
        sprite_line_pixel_bits_at(second_line, second_base_x as i32, base_control, &[]),
        1
    );
}

#[test]
fn manual_sprite_position_write_does_not_truncate_started_word() {
    let mut initial_state = blank_state();
    let beam_y = PAL_VISIBLE_LINE0;
    let initial_hstart = 64;
    let first_hstart = 126;
    let second_hstart = 142;
    let (initial_pos, ctl) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, initial_hstart);
    let (first_pos, _) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, first_hstart);
    let (second_pos, _) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, second_hstart);
    initial_state.sprpos[0] = initial_pos;
    initial_state.spr_hw_pos[0] = initial_pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.sprdata[0] = 0xFFFF;
    initial_state.spr_hw_data[0] = 0xFFFF;
    initial_state.spr_armed[0] = true;
    initial_state.spr_hw_armed[0] = true;

    let manual_sprite_lines = manual_sprite_lines_from_events(
        &initial_state,
        &[
            cpu_event(beam_y as u32, 64, 0x140, first_pos),
            cpu_event(beam_y as u32, 72, 0x140, second_pos),
        ],
    );

    let first_line = manual_sprite_lines[0]
        .iter()
        .find(|line| line.beam_y == beam_y && line.hstart == reg_hstart(first_hstart as i32))
        .expect("first position interval");
    let second_line = manual_sprite_lines[0]
        .iter()
        .find(|line| line.beam_y == beam_y && line.hstart == reg_hstart(second_hstart as i32))
        .expect("second position interval");
    let first_base_x = (first_hstart as i32 - DIW_HSTART_FB0) * 2;
    let second_base_x = (second_hstart as i32 - DIW_HSTART_FB0) * 2;
    let second_write_x = sprite_position_write_framebuffer_x(72) as i32;
    let base_control = ControlState::from_render_state(&initial_state);

    assert!(second_write_x > first_base_x);
    assert!(second_write_x < second_base_x);
    assert!(first_line.x_stop as i32 > second_base_x);
    assert_eq!(
        sprite_line_pixel_bits_at(first_line, second_write_x + 2, base_control, &[]),
        1,
        "a POS write must not cut off a word that has already started"
    );
    assert_eq!(
        sprite_line_pixel_bits_at(first_line, second_base_x - 1, base_control, &[]),
        1
    );
    assert_eq!(
        sprite_line_pixel_bits_at(second_line, second_base_x, base_control, &[]),
        1
    );
}

#[test]
fn manual_sprite_position_write_on_compare_boundary_preserves_started_word() {
    let mut initial_state = blank_state();
    let beam_y = PAL_VISIBLE_LINE0;
    let initial_hstart = 64;
    let first_hstart = 126;
    let second_hstart = 142;
    let (initial_pos, ctl) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, initial_hstart);
    let (first_pos, _) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, first_hstart);
    let (second_pos, _) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, second_hstart);
    initial_state.sprpos[0] = initial_pos;
    initial_state.spr_hw_pos[0] = initial_pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.sprdata[0] = 0xFFFF;
    initial_state.spr_hw_data[0] = 0xFFFF;
    initial_state.spr_armed[0] = true;
    initial_state.spr_hw_armed[0] = true;

    let boundary_hpos = u32::from(first_hstart / 2) + SPRITE_REGISTER_WRITE_PIPELINE_CCK;
    let manual_sprite_lines = manual_sprite_lines_from_events(
        &initial_state,
        &[
            cpu_event(beam_y as u32, 64, 0x140, first_pos),
            cpu_event(beam_y as u32, boundary_hpos, 0x140, second_pos),
        ],
    );

    let first_line = manual_sprite_lines[0]
        .iter()
        .find(|line| line.beam_y == beam_y && line.hstart == reg_hstart(first_hstart as i32))
        .expect("first position interval");
    let base_control = ControlState::from_render_state(&initial_state);
    let first_base_x = (first_hstart as i32 - DIW_HSTART_FB0) * 2;

    assert_eq!(
        sprite_position_write_framebuffer_x(boundary_hpos),
        first_base_x as usize
    );
    assert_eq!(
        sprite_line_pixel_bits_at(first_line, first_base_x, base_control, &[]),
        1,
        "a POS write on the comparator boundary must not cancel the word"
    );
}

#[test]
fn manual_sprite_data_write_before_compare_uses_sprite_compare_domain() {
    let mut initial_state = blank_state();
    let beam_y = PAL_VISIBLE_LINE0;
    let hstart = 240;
    let (pos, ctl) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, hstart);
    initial_state.sprpos[2] = pos;
    initial_state.spr_hw_pos[2] = pos;
    initial_state.sprctl[2] = ctl;
    initial_state.spr_hw_ctl[2] = ctl;
    initial_state.palette.write_ocs(25, 0x0F00);

    let event_hpos = 116;
    let data_x = sprite_data_write_framebuffer_x(event_hpos);
    let colour_output_x = beam_to_framebuffer_x_unclamped(event_hpos) as usize;
    let base_x =
        sprite_nominal_base_framebuffer_x(pos, ctl, initial_state.bplcon0, initial_state.fmode)
            as usize;
    assert!(data_x < base_x);
    assert!(colour_output_x > base_x);

    let manual_sprite_lines = manual_sprite_lines_from_events(
        &initial_state,
        &[cpu_event(beam_y as u32, event_hpos, 0x154, 0x8000)],
    );

    let line = manual_sprite_lines[2]
        .iter()
        .find(|line| line.beam_y == beam_y)
        .expect("data write before the comparator affects the current scanline");
    assert_eq!(line.x_start, data_x);
    assert_eq!(line.data, 0x8000);
    assert_eq!(
        sprite_line_pixel_bits_at(
            line,
            base_x as i32,
            ControlState::from_render_state(&initial_state),
            &[],
        ),
        1
    );
}

#[test]
fn attached_manual_sprite_data_write_before_compare_uses_sprite_compare_domain() {
    let mut initial_state = blank_state();
    let beam_y = PAL_VISIBLE_LINE0;
    let hstart = 240;
    let (pos, ctl) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, hstart);
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.sprpos[1] = pos;
    initial_state.spr_hw_pos[1] = pos;
    initial_state.sprctl[1] = ctl | 0x0080;
    initial_state.spr_hw_ctl[1] = ctl | 0x0080;
    initial_state.palette.write_ocs(20, 0x0F00);

    let event_hpos = 116;
    let data_x = sprite_data_write_framebuffer_x(event_hpos);
    let colour_output_x = beam_to_framebuffer_x_unclamped(event_hpos) as usize;
    let base_x =
        sprite_nominal_base_framebuffer_x(pos, ctl, initial_state.bplcon0, initial_state.fmode)
            as usize;
    assert!(data_x < base_x);
    assert!(colour_output_x > base_x);

    let events = [
        cpu_event(beam_y as u32, event_hpos, 0x144, 0),
        cpu_event(beam_y as u32, event_hpos, 0x14C, 0x8000),
    ];
    let manual_sprite_lines = manual_sprite_lines_from_events(&initial_state, &events);

    let base_controls = [ControlState::from_render_state(&initial_state); FB_HEIGHT];
    let mut render_state = initial_state;
    apply_move(&mut render_state, 0x144, 0);
    apply_move(&mut render_state, 0x14C, 0x8000);
    let ram = vec![0u8; 512 * 1024];
    let base_palettes = [render_state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites_with_manual_lines(
        &render_state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        sprite_pointer_refreshes_from_mask([false; 8]),
        &[],
        false,
        Some(&manual_sprite_lines),
    );

    assert_eq!(fb[base_x], rgb12_to_rgba8(0x0F00));
}

#[test]
fn manual_sprite_data_write_after_compare_waits_for_next_scanline() {
    let mut initial_state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 1,
        DIW_HSTART_FB0 as u16,
    );
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.palette.write_ocs(17, 0x0F00);

    let after_compare_hpos = (DIW_HSTART_FB0 as u32 / 2) + SPRITE_REGISTER_WRITE_PIPELINE_CCK + 2;
    assert!(sprite_data_write_framebuffer_x(after_compare_hpos) > 0);
    let events = [cpu_event(
        PAL_VISIBLE_LINE0 as u32,
        after_compare_hpos,
        0x144,
        0xA000,
    )];
    let manual_sprite_lines = manual_sprite_lines_from_events(&initial_state, &events);
    assert!(manual_sprite_lines[0]
        .iter()
        .all(|line| line.beam_y != PAL_VISIBLE_LINE0));
    assert!(manual_sprite_lines[0]
        .iter()
        .any(|line| line.beam_y == PAL_VISIBLE_LINE0 + 1 && line.x_start == 0));

    let base_controls = [ControlState::from_render_state(&initial_state); FB_HEIGHT];
    let mut render_state = initial_state;
    apply_move(&mut render_state, 0x144, 0xA000);
    let ram = vec![0u8; 512 * 1024];
    let base_palettes = [render_state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites_with_manual_lines(
        &render_state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        sprite_pointer_refreshes_from_mask([false; 8]),
        &[],
        false,
        Some(&manual_sprite_lines),
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0));
    assert_eq!(fb[4], rgb12_to_rgba8(0));
    assert_eq!(fb[FB_WIDTH], rgb12_to_rgba8(0x0F00));
}

#[test]
fn manual_sprite_datb_write_after_compare_waits_for_next_scanline() {
    let mut initial_state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 1,
        DIW_HSTART_FB0 as u16,
    );
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.palette.write_ocs(17, 0x0F00);
    initial_state.palette.write_ocs(19, 0x00F0);

    let after_compare_hpos = (DIW_HSTART_FB0 as u32 / 2) + SPRITE_REGISTER_WRITE_PIPELINE_CCK + 2;
    assert!(sprite_data_write_framebuffer_x(after_compare_hpos) > 0);
    let events = [
        cpu_event(
            PAL_VISIBLE_LINE0 as u32,
            COPPER_WAIT_HPOS_FB0 as u32,
            0x144,
            0xFFFF,
        ),
        cpu_event(PAL_VISIBLE_LINE0 as u32, after_compare_hpos, 0x146, 0xFFFF),
    ];
    let manual_sprite_lines = manual_sprite_lines_from_events(&initial_state, &events);

    let base_controls = [ControlState::from_render_state(&initial_state); FB_HEIGHT];
    let mut render_state = initial_state;
    apply_move(&mut render_state, 0x144, 0xFFFF);
    apply_move(&mut render_state, 0x146, 0xFFFF);
    let ram = vec![0u8; 512 * 1024];
    let base_palettes = [render_state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites_with_manual_lines(
        &render_state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        sprite_pointer_refreshes_from_mask([false; 8]),
        &[],
        false,
        Some(&manual_sprite_lines),
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[4], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[FB_WIDTH], rgb12_to_rgba8(0x00F0));
}

#[test]
fn manual_sprite_control_write_after_compare_preserves_loaded_word() {
    let mut initial_state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 1,
        DIW_HSTART_FB0 as u16,
    );
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.sprdata[0] = 0xFFFF;
    initial_state.spr_hw_data[0] = 0xFFFF;
    initial_state.spr_armed[0] = true;
    initial_state.spr_hw_armed[0] = true;
    initial_state.palette.write_ocs(17, 0x0F00);

    let events = [
        cpu_event(
            PAL_VISIBLE_LINE0 as u32,
            (COPPER_WAIT_HPOS_FB0 + 2) as u32,
            0x142,
            ctl,
        ),
        cpu_event(
            PAL_VISIBLE_LINE0 as u32,
            (COPPER_WAIT_HPOS_FB0 + 4) as u32,
            0x144,
            0xFFFF,
        ),
    ];
    let manual_sprite_lines = manual_sprite_lines_from_events(&initial_state, &events);

    let base_controls = [ControlState::from_render_state(&initial_state); FB_HEIGHT];
    let mut render_state = initial_state;
    apply_move(&mut render_state, 0x142, ctl);
    apply_move(&mut render_state, 0x144, 0xFFFF);
    let ram = vec![0u8; 512 * 1024];
    let base_palettes = [render_state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites_with_manual_lines(
        &render_state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        sprite_pointer_refreshes_from_mask([false; 8]),
        &[],
        false,
        Some(&manual_sprite_lines),
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[7], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[8], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[15], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[16], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[32], rgb12_to_rgba8(0));
}

#[test]
fn attached_manual_sprite_writes_draw_odd_bits_without_even_data_bits() {
    let mut initial_state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 1,
        DIW_HSTART_FB0 as u16,
    );
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.sprpos[1] = pos;
    initial_state.spr_hw_pos[1] = pos;
    initial_state.sprctl[1] = ctl | 0x0080;
    initial_state.spr_hw_ctl[1] = ctl | 0x0080;
    initial_state.palette.write_ocs(20, 0x0F00);

    let events = [
        cpu_event(
            PAL_VISIBLE_LINE0 as u32,
            COPPER_WAIT_HPOS_FB0 as u32,
            0x144,
            0,
        ),
        cpu_event(
            PAL_VISIBLE_LINE0 as u32,
            COPPER_WAIT_HPOS_FB0 as u32,
            0x14C,
            0x8000,
        ),
    ];
    let manual_sprite_lines = manual_sprite_lines_from_events(&initial_state, &events);

    let base_controls = [ControlState::from_render_state(&initial_state); FB_HEIGHT];
    let mut render_state = initial_state;
    apply_move(&mut render_state, 0x144, 0);
    apply_move(&mut render_state, 0x14C, 0x8000);
    let ram = vec![0u8; 512 * 1024];
    let base_palettes = [render_state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites_with_manual_lines(
        &render_state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        sprite_pointer_refreshes_from_mask([false; 8]),
        &[],
        false,
        Some(&manual_sprite_lines),
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
}

#[test]
fn attached_manual_sprite_data_after_compare_waits_for_next_scanline() {
    let mut initial_state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 1,
        DIW_HSTART_FB0 as u16,
    );
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.sprpos[1] = pos;
    initial_state.spr_hw_pos[1] = pos;
    initial_state.sprctl[1] = ctl | 0x0080;
    initial_state.spr_hw_ctl[1] = ctl | 0x0080;
    initial_state.palette.write_ocs(20, 0x0F00);

    let after_compare_hpos = (DIW_HSTART_FB0 as u32 / 2) + SPRITE_REGISTER_WRITE_PIPELINE_CCK + 2;
    assert!(sprite_data_write_framebuffer_x(after_compare_hpos) > 0);
    let events = [
        cpu_event(
            PAL_VISIBLE_LINE0 as u32,
            COPPER_WAIT_HPOS_FB0 as u32,
            0x144,
            0,
        ),
        cpu_event(
            PAL_VISIBLE_LINE0 as u32,
            COPPER_WAIT_HPOS_FB0 as u32,
            0x14C,
            0x8000,
        ),
        cpu_event(PAL_VISIBLE_LINE0 as u32, after_compare_hpos, 0x14C, 0x2000),
    ];
    let manual_sprite_lines = manual_sprite_lines_from_events(&initial_state, &events);

    let base_controls = [ControlState::from_render_state(&initial_state); FB_HEIGHT];
    let mut render_state = initial_state;
    apply_move(&mut render_state, 0x144, 0);
    apply_move(&mut render_state, 0x14C, 0x2000);
    let ram = vec![0u8; 512 * 1024];
    let base_palettes = [render_state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites_with_manual_lines(
        &render_state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        sprite_pointer_refreshes_from_mask([false; 8]),
        &[],
        false,
        Some(&manual_sprite_lines),
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[4], rgb12_to_rgba8(0));
}

#[test]
fn attached_manual_sprite_data_after_compare_preserves_loaded_even_pixels() {
    let mut initial_state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 1,
        DIW_HSTART_FB0 as u16,
    );
    initial_state.sprpos[0] = pos;
    initial_state.spr_hw_pos[0] = pos;
    initial_state.sprctl[0] = ctl;
    initial_state.spr_hw_ctl[0] = ctl;
    initial_state.sprdata[0] = 0xA000;
    initial_state.spr_hw_data[0] = 0xA000;
    initial_state.spr_armed[0] = true;
    initial_state.spr_hw_armed[0] = true;
    initial_state.sprpos[1] = pos;
    initial_state.spr_hw_pos[1] = pos;
    initial_state.sprctl[1] = ctl | 0x0080;
    initial_state.spr_hw_ctl[1] = ctl | 0x0080;
    initial_state.palette.write_ocs(17, 0x0F00);
    initial_state.palette.write_ocs(21, 0x00F0);

    let after_compare_hpos = (DIW_HSTART_FB0 as u32 / 2) + SPRITE_REGISTER_WRITE_PIPELINE_CCK + 2;
    assert!(sprite_data_write_framebuffer_x(after_compare_hpos) > 0);
    let events = [cpu_event(
        PAL_VISIBLE_LINE0 as u32,
        after_compare_hpos,
        0x14C,
        0x2000,
    )];
    let manual_sprite_lines = manual_sprite_lines_from_events(&initial_state, &events);

    let base_controls = [ControlState::from_render_state(&initial_state); FB_HEIGHT];
    let mut render_state = initial_state;
    apply_move(&mut render_state, 0x14C, 0x2000);
    let ram = vec![0u8; 512 * 1024];
    let base_palettes = [render_state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites_with_manual_lines(
        &render_state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        sprite_pointer_refreshes_from_mask([false; 8]),
        &[],
        false,
        Some(&manual_sprite_lines),
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[4], rgb12_to_rgba8(0x0F00));
}

#[test]
fn disabled_sprite_dma_ignores_stale_sprite_pointers() {
    let mut ram = vec![0u8; 512 * 1024];
    let sprite_ptr = 0x0100usize;
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 1,
        DIW_HSTART_FB0 as u16,
    );
    put_word(&mut ram, sprite_ptr, pos);
    put_word(&mut ram, sprite_ptr + 2, ctl);
    put_word(&mut ram, sprite_ptr + 4, 0x8000);
    put_word(&mut ram, sprite_ptr + 6, 0);
    put_word(&mut ram, sprite_ptr + 8, 0);
    put_word(&mut ram, sprite_ptr + 10, 0);

    let mut state = blank_state();
    state.sprpt[0] = sprite_ptr as u32;
    state.palette.write_ocs(17, 0x0F00);
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &[],
        false,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0));
}

#[test]
fn sprites_wait_for_first_bpl1dat_display_enable_on_scanline() {
    let mut state = blank_state();
    state.palette.write_ocs(17, 0x0F00);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: SPRITE_HSTART_FB0,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0xFFFF,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];
    let mut display_enable_x = [None; FB_HEIGHT];
    display_enable_x[0] = Some(4);
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites_with_manual_lines_and_writes(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &display_enable_x,
        &playfield_mask,
        &mut collision_pixels,
        &captured,
        true,
        None,
        PAL_VISIBLE_LINE0,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0));
    assert_eq!(fb[3], rgb12_to_rgba8(0));
    assert_eq!(fb[4], rgb12_to_rgba8(0x0F00));
}

#[test]
fn manual_bpl1dat_display_enable_allows_sprites_on_vertically_closed_diw_line() {
    let mut state = RenderState {
        dmacon: DMACON_DMAEN | DMACON_SPREN,
        diwstrt: (((PAL_VISIBLE_LINE0 + 10) as u16) << 8) | DIW_HSTART_FB0 as u16,
        diwstop: (((PAL_VISIBLE_LINE0 + 20) as u16) << 8) | 0x00C1,
        ..blank_state()
    };
    state.palette.write_ocs(17, 0x0F00);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: SPRITE_HSTART_FB0,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0x8000,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];

    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let display_disabled = [None; FB_HEIGHT];
    render_sprites_with_manual_lines_and_writes(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &display_disabled,
        &playfield_mask,
        &mut collision_pixels,
        &captured,
        true,
        None,
        PAL_VISIBLE_LINE0,
    );
    assert_eq!(fb[0], rgb12_to_rgba8(0));

    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let mut display_enabled = [None; FB_HEIGHT];
    display_enabled[0] = Some(0);
    render_sprites_with_manual_lines_and_writes(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &display_enabled,
        &playfield_mask,
        &mut collision_pixels,
        &captured,
        true,
        None,
        PAL_VISIBLE_LINE0,
    );
    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
}

#[test]
fn brdsprt_bypasses_first_bpl1dat_display_enable_gate() {
    let mut state = RenderState {
        bplcon0: BPLCON0_ECSENA,
        bplcon3: BPLCON3_BRDSPRT,
        ..blank_state()
    };
    state.palette.write_ocs(17, 0x0F00);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: SPRITE_HSTART_FB0,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0x8000,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];
    let display_enable_x = [None; FB_HEIGHT];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites_with_manual_lines_and_writes(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &display_enable_x,
        &playfield_mask,
        &mut collision_pixels,
        &captured,
        true,
        None,
        PAL_VISIBLE_LINE0,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
}

#[test]
fn brdrblnk_suppresses_brdsprt_display_enable_bypass() {
    let mut state = RenderState {
        bplcon0: BPLCON0_ECSENA,
        bplcon3: BPLCON3_BRDSPRT | BPLCON3_BRDRBLNK,
        ..blank_state()
    };
    state.palette.write_ocs(17, 0x0F00);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: SPRITE_HSTART_FB0,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0x8000,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];
    let display_enable_x = [None; FB_HEIGHT];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites_with_manual_lines_and_writes(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &display_enable_x,
        &playfield_mask,
        &mut collision_pixels,
        &captured,
        true,
        None,
        PAL_VISIBLE_LINE0,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0));
}

#[test]
fn bplcon3_brdsprt_allows_sprites_in_border_when_ecsena_set() {
    let mut state = RenderState {
        dmacon: DMACON_DMAEN | DMACON_SPREN,
        bplcon0: BPLCON0_ECSENA,
        bplcon3: BPLCON3_BRDSPRT,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | (DIW_HSTART_FB0 as u16 + 8),
        diwstop: (((PAL_VISIBLE_LINE0 + 128) as u16) << 8) | (DIW_HSTART_FB0 as u16 + 63),
        ..blank_state()
    };
    state.palette.write_ocs(17, 0x0F00);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: SPRITE_HSTART_FB0,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0x8000,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &captured,
        true,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[1], rgb12_to_rgba8(0x0F00));
}

#[test]
fn bplcon3_brdsprt_requires_ecsena_for_border_sprites() {
    let mut state = RenderState {
        dmacon: DMACON_DMAEN | DMACON_SPREN,
        bplcon0: 0,
        bplcon3: BPLCON3_BRDSPRT,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | (DIW_HSTART_FB0 as u16 + 8),
        diwstop: (((PAL_VISIBLE_LINE0 + 128) as u16) << 8) | (DIW_HSTART_FB0 as u16 + 63),
        ..blank_state()
    };
    state.palette.write_ocs(17, 0x0F00);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: SPRITE_HSTART_FB0,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0x8000,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &captured,
        true,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0));
    assert_eq!(fb[1], rgb12_to_rgba8(0));
}

#[test]
fn beam_timed_bplcon3_brdsprt_latches_until_ecsena_enables_effect() {
    let mut state = RenderState {
        dmacon: DMACON_DMAEN | DMACON_SPREN,
        bplcon0: 0,
        bplcon3: 0,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | (DIW_HSTART_FB0 as u16 + 80),
        diwstop: (((PAL_VISIBLE_LINE0 + 128) as u16) << 8) | (DIW_HSTART_FB0 as u16 + 120),
        ..blank_state()
    };
    state.palette.write_ocs(17, 0x0F00);
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [
        beam_event(
            PAL_VISIBLE_LINE0 as u32,
            (COPPER_WAIT_HPOS_FB0 + 4) as u32,
            0x0106,
            BPLCON3_BRDSPRT,
        ),
        beam_event(
            PAL_VISIBLE_LINE0 as u32,
            (COPPER_WAIT_HPOS_FB0 + 8) as u32,
            0x0100,
            BPLCON0_ECSENA,
        ),
    ];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let ram = vec![0; 64];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: SPRITE_HSTART_FB0 + 8,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0xFFFF,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &captured,
        true,
    );

    assert_eq!(fb[24], rgb12_to_rgba8(0));
    assert_eq!(fb[40], rgb12_to_rgba8(0x0F00));
}

#[test]
fn sprites_use_beam_timed_display_window_control() {
    let mut state = blank_state();
    state.dmacon = DMACON_DMAEN | DMACON_SPREN;
    state.diwstrt = ((PAL_VISIBLE_LINE0 as u16) << 8) | DIW_HSTART_FB0 as u16;
    state.diwstop = (((PAL_VISIBLE_LINE0 + 1) as u16) << 8) | 0x00C1;
    state.palette.write_ocs(17, 0x0F00);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_control = ControlState::from_render_state(&state);
    let base_controls = [base_control; FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut later_control = base_control;
    later_control.diwstrt = ((PAL_VISIBLE_LINE0 as u16) << 8) | (DIW_HSTART_FB0 as u16 + 2);
    control_segments[0].push(ControlSegment {
        x: 2,
        control: later_control,
    });
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: SPRITE_HSTART_FB0,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0xC000,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &captured,
        true,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[1], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[2], rgb12_to_rgba8(0));
    assert_eq!(fb[3], rgb12_to_rgba8(0));
}

#[test]
fn captured_sprite_dma_lines_render_without_reparsing_frame_ram() {
    let mut state = blank_state();
    state.dmacon = DMACON_DMAEN | DMACON_SPREN;
    state.palette.write_ocs(17, 0x0F00);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: SPRITE_HSTART_FB0,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0x8000,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &captured,
        true,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[1], rgb12_to_rgba8(0x0F00));
}

#[test]
fn dma_loaded_sprite_data_rearms_on_same_line_position_write() {
    let mut state = blank_state();
    state.dmacon = DMACON_DMAEN | DMACON_SPREN;
    state.palette.write_ocs(17, 0x0F00);
    state.palette.write_ocs(18, 0x00F0);
    state.sprdatb[0] = 0x8000;
    state.spr_hw_datb[0] = 0x8000;
    state.spr_armed[0] = true;
    state.spr_hw_armed[0] = true;
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let beam_y = PAL_VISIBLE_LINE0;
    let initial_hstart = DIW_HSTART_FB0 as u16;
    // Odd output position: with only SPRxPOS rewritten, H0 stays in the old
    // CTL word, so the register value must be even to survive a POS-only
    // reposition intact.
    let reused_hstart = initial_hstart + 65;
    let (reused_pos, _) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, reused_hstart);
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: i32::from(initial_hstart),
        hsub_70ns: false,
        beam_y,
        data: 0x8000,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];
    let events = [beam_event(
        beam_y as u32,
        (COPPER_WAIT_HPOS_FB0 + 4) as u32,
        0x0140,
        reused_pos,
    )];
    let mut manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &state,
        &events,
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        false,
        true,
    );

    assert!(manual_sprite_lines[0].is_empty());

    let dma_seeded_lines = manual_sprite_lines_from_captured_dma_reuse(
        &state,
        &events,
        &captured,
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
    );
    assert!(dma_seeded_lines[0].iter().any(|line| {
        line.beam_y == beam_y
            && line.hstart == reg_hstart(i32::from(reused_hstart))
            && line.data == 0x8000
    }));
    assert!(dma_seeded_lines[0].iter().all(|line| line.datb == 0));
    merge_dma_seeded_manual_sprite_lines(&mut manual_sprite_lines, dma_seeded_lines);

    render_sprites_with_manual_lines(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        sprite_pointer_refreshes_from_mask([false; 8]),
        &captured,
        true,
        Some(&manual_sprite_lines),
    );

    let reused_x = sprite_base_framebuffer_x(
        reg_hstart(i32::from(reused_hstart)),
        false,
        base_controls[0],
        &[],
    );
    assert_eq!(fb[reused_x as usize], rgb12_to_rgba8(0x0F00));
    assert_ne!(fb[reused_x as usize], rgb12_to_rgba8(0x00F0));
}

/// A DMA fetch arms Denise, but the serializer only loads the latched words
/// at HSTART: a SPRxCTL write in between clears the armed bit, so the fetched
/// line is never displayed -- not at its own HSTART, and not at a position a
/// later same-line SPRxPOS write moves it to (SPRxPOS never arms). This is the
/// Hybris status-panel idiom: the panel's multiplexed channels are retired
/// with SPRxCTL writes at the start of the line below the panel, where sprite
/// DMA is still fetching.
#[test]
fn sprite_ctl_write_before_hstart_cancels_dma_loaded_line() {
    let (captured, events, reused_hstart) =
        dma_loaded_line_with_ctl_write_at(COPPER_WAIT_HPOS_FB0 as u32);

    // Nothing survives: neither the captured fetch nor a reuse of its data.
    assert!(retain_armed_captured_sprite_lines(&captured, &events).is_empty());
    let (fb, base_control) = render_dma_loaded_line_with_events(&captured, &events);
    for hstart in [captured[0].hstart, reg_hstart(i32::from(reused_hstart))] {
        let x = sprite_base_framebuffer_x(hstart, false, base_control, &[]);
        assert_eq!(fb[x as usize], rgb12_to_rgba8(0), "hstart {hstart}");
    }
}

/// The mirror image: a SPRxCTL write past HSTART cannot recall pixels the
/// serializer has already started shifting out, so the fetched line stays.
#[test]
fn sprite_ctl_write_after_hstart_keeps_dma_loaded_line() {
    let hstart_hpos = (DIW_HSTART_FB0 / 2 + 8) as u32;
    let (captured, events, _) = dma_loaded_line_with_ctl_write_at(hstart_hpos);

    assert_eq!(
        retain_armed_captured_sprite_lines(&captured, &events).len(),
        1
    );
    let (fb, base_control) = render_dma_loaded_line_with_events(&captured, &events);
    let x = sprite_base_framebuffer_x(captured[0].hstart, false, base_control, &[]);
    assert_eq!(fb[x as usize], rgb12_to_rgba8(0x0F00));
}

/// The same disarm, seen through the DMA-data reuse path alone: the fetch
/// lands with HSTART left of the disarm (so the fetched line's own comparator
/// already fired and it is not itself cancelled), and the Copper then disarms
/// and repositions the channel to the right of it. Nothing may appear at the
/// new position -- SPRxPOS reuses DMA-loaded data only while the channel is
/// still armed. Hybris's panel does exactly this on one of its two channels.
#[test]
fn sprite_ctl_write_stops_dma_data_reuse_by_later_position_write() {
    let beam_y = PAL_VISIBLE_LINE0;
    // HSTART before the sprite's fetch slot: its own output is off-screen
    // left, so only the repositioned copy could be visible.
    let fetched_hstart = 20i32;
    let reused_hstart = DIW_HSTART_FB0 as u16 + 100;
    // CTL carries the fetched line's own register-decoded HSTART, so build
    // it from the register parts rather than the output-position helper.
    let (_, ctl) =
        sprite_control_words_from_parts(beam_y, beam_y + 1, fetched_hstart, false, false);
    let (reused_pos, _) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, reused_hstart);
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: fetched_hstart,
        hsub_70ns: false,
        beam_y,
        // A full opaque word, so any surviving output is unmissable wherever
        // the reposition lands it.
        data: 0xFFFF,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];
    let events = [
        beam_event(beam_y as u32, COPPER_WAIT_HPOS_FB0 as u32, 0x0142, ctl),
        beam_event(
            beam_y as u32,
            COPPER_WAIT_HPOS_FB0 as u32 + 4,
            0x0140,
            reused_pos,
        ),
    ];

    // The fetched line survives the filter (its comparator fired before the
    // disarm) but must not be re-serialized at the repositioned HSTART, so
    // the whole scanline stays background: the fetch's own output sits off
    // the left edge and the reposition may not resurrect it.
    assert_eq!(
        retain_armed_captured_sprite_lines(&captured, &events).len(),
        1
    );
    let (fb, base_control) = render_dma_loaded_line_with_events(&captured, &events);
    let row = (beam_y - PAL_VISIBLE_LINE0) as usize * FB_WIDTH;
    assert!(
        fb[row..row + FB_WIDTH]
            .iter()
            .all(|&px| px == rgb12_to_rgba8(0)),
        "repositioned HSTART {} painted after the disarm",
        sprite_base_framebuffer_x(
            reg_hstart(i32::from(reused_hstart)),
            false,
            base_control,
            &[]
        )
    );
}

/// One DMA-loaded sprite line on the first visible line, with a Copper
/// SPRxCTL write at `ctl_hpos` (the channel's own control words, so the only
/// effect is the disarm) followed by a SPRxPOS reposition four colour clocks
/// later. Returns the captured line, the events, and the repositioned hstart.
fn dma_loaded_line_with_ctl_write_at(
    ctl_hpos: u32,
) -> ([CapturedSpriteLine; 1], [BeamRegisterWrite; 2], u16) {
    let beam_y = PAL_VISIBLE_LINE0;
    let hstart = DIW_HSTART_FB0 as u16;
    let reused_hstart = hstart + 65;
    let (_, ctl) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, hstart);
    let (reused_pos, _) = sprite_control_words(beam_y as u16, beam_y as u16 + 1, reused_hstart);
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: reg_hstart(i32::from(hstart)),
        hsub_70ns: false,
        beam_y,
        data: 0x8000,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];
    let events = [
        beam_event(beam_y as u32, ctl_hpos, 0x0142, ctl),
        beam_event(beam_y as u32, ctl_hpos + 4, 0x0140, reused_pos),
    ];
    (captured, events, reused_hstart)
}

/// Render `captured` under `events` the way the frame renderer does: the
/// beam-timed manual replay, then the DMA-loaded reuse replay merged into it,
/// against the captured lines that are still armed at their HSTART.
fn render_dma_loaded_line_with_events(
    captured: &[CapturedSpriteLine],
    events: &[BeamRegisterWrite],
) -> (Vec<u32>, ControlState) {
    let mut state = blank_state();
    state.dmacon = DMACON_DMAEN | DMACON_SPREN;
    state.palette.write_ocs(17, 0x0F00);
    state.palette.write_ocs(18, 0x00F0);
    state.sprdatb[0] = 0x8000;
    state.spr_hw_datb[0] = 0x8000;
    state.spr_armed[0] = true;
    state.spr_hw_armed[0] = true;
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    let armed = retain_armed_captured_sprite_lines(captured, events);
    let mut manual_sprite_lines = manual_sprite_lines_from_events_with_visible_line0(
        &state,
        events,
        &[None; 8],
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
        false,
        true,
    );
    let dma_seeded_lines = manual_sprite_lines_from_captured_dma_reuse(
        &state,
        events,
        &armed,
        PAL_VISIBLE_LINE0,
        FB_HEIGHT,
    );
    merge_dma_seeded_manual_sprite_lines(&mut manual_sprite_lines, dma_seeded_lines);
    render_sprites_with_manual_lines(
        &state,
        &[],
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        sprite_pointer_refreshes_from_mask([false; 8]),
        &armed,
        true,
        Some(&manual_sprite_lines),
    );
    (fb, base_controls[0])
}

#[test]
fn sprite_register_data_write_after_compare_preserves_dma_latch_on_same_beam_line() {
    let mut state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 1,
        DIW_HSTART_FB0 as u16,
    );
    state.dmacon = DMACON_DMAEN | DMACON_SPREN;
    state.sprpos[0] = pos;
    state.spr_hw_pos[0] = pos;
    state.sprctl[0] = ctl;
    state.spr_hw_ctl[0] = ctl;
    state.palette.write_ocs(17, 0x0F00);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 0,
        hstart: SPRITE_HSTART_FB0,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0xFFFF,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];
    let after_compare_hpos = (DIW_HSTART_FB0 as u32 / 2) + SPRITE_REGISTER_WRITE_PIPELINE_CCK + 2;
    assert!(sprite_data_write_framebuffer_x(after_compare_hpos) > 0);
    let manual_sprite_lines = manual_sprite_lines_from_events(
        &state,
        &[beam_event(
            PAL_VISIBLE_LINE0 as u32,
            after_compare_hpos,
            0x0144,
            0x0000,
        )],
    );

    render_sprites_with_manual_lines(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        sprite_pointer_refreshes_from_mask([false; 8]),
        &captured,
        true,
        Some(&manual_sprite_lines),
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[7], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[8], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[31], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[32], rgb12_to_rgba8(0));
}

#[test]
fn dual_playfield_uses_separate_palette_banks_and_priority() {
    let pf1_priority = ControlState {
        bplcon0: 0x6400,
        bplcon1: 0,
        bplcon2: 0,
        bplcon3: BPLCON3_PF2OF_DEFAULT,
        ..ControlState::default()
    };
    assert!(pf1_priority.dual_playfield());
    assert_eq!(dual_playfield_palette_index(0b010101, pf1_priority), 7);
    assert_eq!(dual_playfield_palette_index(0b101010, pf1_priority), 15);
    assert_eq!(dual_playfield_palette_index(0b000011, pf1_priority), 1);

    let pf2_priority = ControlState {
        bplcon0: 0x6400,
        bplcon1: 0,
        bplcon2: 0x0040,
        bplcon3: BPLCON3_PF2OF_DEFAULT,
        ..ControlState::default()
    };
    assert_eq!(dual_playfield_palette_index(0b000011, pf2_priority), 9);
}

#[test]
fn dual_playfield_out_of_range_priority_draws_the_field_transparent_on_denise() {
    // A dual playfield whose BPLCON2 priority code is programmed out of range
    // (5-7) is drawn transparent on Denise: the winning field's pixels
    // collapse to the background instead of revealing the field behind it.
    // Photographed on an A500 (vAmigaTS Denise/Registers/BPLCON0/invprio1,
    // PF2 code 7, background visible between the bars). Valid codes (0-4) are
    // unaffected, so real dual-playfield content is unchanged.
    let invalid_pf2 = ControlState {
        bplcon0: 0x6400, // 4 planes, dual playfield
        bplcon2: 0x0038, // PF2 priority code 7 (out of range), PF1 code 0
        bplcon3: BPLCON3_PF2OF_DEFAULT,
        ..ControlState::default()
    };
    // A PF2-only pixel is transparent (the winning field's priority is out of
    // range); a PF1-only pixel still resolves (PF1 code 0 is valid).
    assert_eq!(dual_playfield_pixel(0b0000_0010, invalid_pf2), (0, 0));
    assert_eq!(dual_playfield_pixel(0b0000_0001, invalid_pf2), (1, 1));

    // With a valid PF2 priority the same PF2 pixel resolves normally.
    let valid = ControlState {
        bplcon2: 0x0004, // PF2 code 0, PF1 code 4 (both valid)
        ..invalid_pf2
    };
    assert_eq!(dual_playfield_pixel(0b0000_0010, valid), (2, 9));
}

#[test]
fn dual_playfield_out_of_range_priority_still_draws_on_lisa() {
    // Lisa does not inherit Denise's out-of-range-priority quirk. Alfred
    // Chicken programs BPLCON2 = 0x003F -- both codes 7 -- for its whole
    // in-game display and draws an eight-plane dual playfield on real AGA
    // hardware; blanking it there left the level invisible (issue #416).
    let aga = ControlState {
        bplcon0: 0x6400,
        bplcon2: 0x003F, // both priority codes 7
        bplcon3: BPLCON3_PF2OF_DEFAULT,
        agnus_revision: AgnusRevision::AgaAlice,
        ..ControlState::default()
    };
    assert_eq!(dual_playfield_pixel(0b0000_0010, aga), (2, 9));
    assert_eq!(dual_playfield_pixel(0b0000_0001, aga), (1, 1));
    assert_eq!(dual_playfield_pixel(0, aga), (0, 0));

    // The same pixels resolve identically with an in-range code, which is
    // what "the code does not affect colour resolution on Lisa" means.
    let aga_valid = ControlState {
        bplcon2: 0x0004,
        ..aga
    };
    assert_eq!(dual_playfield_pixel(0b0000_0010, aga_valid), (2, 9));
    assert_eq!(dual_playfield_pixel(0b0000_0001, aga_valid), (1, 1));

    // On either chip the code still saturates in the sprite comparison,
    // where it counts the sprite pairs that pass in front of the playfield:
    // 5-7 put all four pairs in front exactly as 4 does, while 3 leaves the
    // last pair behind.
    for code in 4u16..=7 {
        let control = ControlState {
            bplcon2: code,
            ..aga
        };
        for group in 0..4 {
            assert!(
                sprite_has_priority(group * 2, 1, control),
                "code {code} group {group}"
            );
        }
    }
    let code_three = ControlState { bplcon2: 3, ..aga };
    assert!(sprite_has_priority(4, 1, code_three));
    assert!(!sprite_has_priority(6, 1, code_three));
}

#[test]
fn single_playfield_out_of_range_priority_eliminates_low_bitplanes() {
    // A single playfield with an out-of-range BPLCON2 PF2 priority code keeps
    // only bitplanes 5-6 wherever bitplane 5 is set (eliminating the four low
    // planes) and forces background sprite priority (vAmiga translateSPF).
    // Valid codes leave the pixel untouched.
    let mut palette = Palette::new();
    palette.write_ocs(0x10, 0x0F00);
    palette.write_ocs(0x13, 0x000F);

    let invalid = ControlState {
        bplcon0: 0x5000, // 5 planes, single playfield (no HAM/dual/EHB)
        bplcon2: 0x0038, // PF2 priority code 7 (invalid)
        ..ControlState::default()
    };
    let valid = ControlState {
        bplcon2: 0x0004, // PF1 code 4, PF2 code 0 (both valid)
        ..invalid
    };

    // Planes 1,2,5 set: the invalid priority eliminates planes 1-2, leaving
    // the plane-5-only index, so the pixel matches a valid render of 0x10.
    let mut h = 0u32;
    let invalid_13 = denise_playfield_output(invalid, &palette, 0x13, &mut h);
    let mut h = 0u32;
    let valid_10 = denise_playfield_output(valid, &palette, 0x10, &mut h);
    let mut h = 0u32;
    let valid_13 = denise_playfield_output(valid, &palette, 0x13, &mut h);
    assert_eq!(invalid_13.color, valid_10.color);
    assert_ne!(invalid_13.color, valid_13.color);
    assert_eq!(invalid_13.pf_mask, 0);
    assert_eq!(valid_13.pf_mask, 2);
}

#[test]
fn aga_dual_playfield_decodes_bitplane7_into_pf1_fourth_bit() {
    // AGA Lisa dual playfield gives each field four bits: bitplane 7
    // becomes PF1's high bit (palette entries 8..15) and bitplane 8
    // PF2's. Pre-AGA chips decode only three bits per field. Zool
    // (A1200) draws its sprite-cel character into a 7-plane dual
    // playfield: the black body lives at PF1 index 11, which collapses
    // to index 3 (orange) when bitplane 7 is dropped.
    let aga = ControlState {
        bplcon0: 0x7400,
        bplcon3: BPLCON3_PF2OF_DEFAULT,
        agnus_revision: AgnusRevision::AgaAlice,
        ..ControlState::default()
    };
    assert!(aga.aga() && aga.dual_playfield());
    // Bitplanes 1,3,7 set -> PF1 = 0b1011 = 11, PF2 empty.
    assert_eq!(dual_playfield_palette_index(0b0100_0101, aga), 11);
    // Bitplane 8 set -> PF2 high bit; PF2 = 0b1000 = 8, plus the
    // default PF2OF offset of 8 -> palette entry 16.
    assert_eq!(dual_playfield_palette_index(0b1000_0000, aga), 16);

    // The same indices on OCS keep the three-bit decode (bits 6,7 are
    // never carried by <=6 plane hardware, so they are ignored).
    let ocs = ControlState {
        bplcon0: 0x6400,
        ..ControlState::default()
    };
    assert!(!ocs.aga() && ocs.dual_playfield());
    assert_eq!(dual_playfield_palette_index(0b0100_0101, ocs), 3);
}

#[test]
fn bplcon3_pf2of_selects_dual_playfield_pf2_palette_offset() {
    let control = |bplcon3| ControlState {
        bplcon0: 0x6400,
        bplcon1: 0,
        bplcon2: 0x0040,
        bplcon3,
        ..ControlState::default()
    };

    assert_eq!(dual_playfield_palette_index(0b000011, control(0x0000)), 1);
    assert_eq!(dual_playfield_palette_index(0b000011, control(0x0400)), 3);
    assert_eq!(dual_playfield_palette_index(0b000011, control(0x0800)), 5);
    assert_eq!(dual_playfield_palette_index(0b000011, control(0x1000)), 17);
}

#[test]
fn bplcon1_exposes_separate_playfield_scroll_nibbles() {
    let control = ControlState {
        bplcon0: 0x6400,
        bplcon1: 0x00A3,
        bplcon2: 0,
        ..ControlState::default()
    };

    assert_eq!(control.pf1_scroll(), 3);
    assert_eq!(control.pf2_scroll(), 10);
    assert_eq!(control.scroll_for_plane(0), 3);
    assert_eq!(control.scroll_for_plane(1), 10);
}

#[test]
fn covered_scroll_catches_floor_reload_slot_for_off_grid_ddfstrt() {
    // Lo-res FMODE=0 fetch with DDFSTRT $66: 6 cck past the 8-cck reload
    // grid, i.e. the data is 12 native px late for its own slot. With
    // scroll 0 it waits for the next slot (round-up placement, no advance);
    // a BPLCON1 delay that covers the lateness (S >= 12) catches the floor
    // slot one full gulp (16 native px) earlier. vAmiga-verified with the
    // ddfprobe-phase/-phase2 probes; regression example: Rampage's dot-cube
    // pan (DDFSTRT $66->$68 against a scroll wrap $FF->$00) jumps 16 px
    // without the advance.
    // ECS Agnus: the DDFSTRT comparator has 2-cck resolution, so $66 keeps
    // its phase-6 position (OCS masks to 4-cck, making the lateness 8 px).
    let control = |bplcon1: u16| ControlState {
        agnus_revision: AgnusRevision::Ecs8372Rev4,
        bplcon0: 0x2200, // 2 planes, lo-res
        ddfstrt: 0x0066,
        ddfstop: 0x00C8,
        bplcon1,
        ..ControlState::default()
    };

    // Scroll 0: no advance, delays are the plain scroll values.
    assert_eq!(control(0x0000).row_reload_advance(), 0);
    assert_eq!(control(0x0000).sample_delay_for_plane(0), 0);
    // Scroll 15 on both playfields covers the 12 px lateness.
    assert_eq!(control(0x00FF).row_reload_advance(), 16);
    assert_eq!(control(0x00FF).sample_delay_for_plane(0), 15);
    assert_eq!(control(0x00FF).sample_delay_for_plane(1), 15);
    // Scroll 11 does not cover 12 px: round-up placement stands.
    assert_eq!(control(0x00BB).row_reload_advance(), 0);
    assert_eq!(control(0x00BB).sample_delay_for_plane(0), 11);
    // Split scrolls: PF2 (odd planes) covered, PF1 not. The row origin
    // extends by PF2's advance; PF1 planes rebase one gulp later so their
    // placement is unchanged.
    assert_eq!(control(0x00F0).row_reload_advance(), 16);
    assert_eq!(control(0x00F0).sample_delay_for_plane(0), 16);
    assert_eq!(control(0x00F0).sample_delay_for_plane(1), 15);
    // On-grid DDFSTRT never advances, whatever the scroll.
    let on_grid = ControlState {
        agnus_revision: AgnusRevision::Ecs8372Rev4,
        bplcon0: 0x2200,
        ddfstrt: 0x0068,
        ddfstop: 0x00C8,
        bplcon1: 0x00FF,
        ..ControlState::default()
    };
    assert_eq!(on_grid.row_reload_advance(), 0);
    assert_eq!(on_grid.sample_delay_for_plane(0), 15);
}

#[test]
fn wide_fmode_scroll_folds_into_the_early_masked_fetch_start() {
    // Lo-res 32-bit fetch (FMODE BPL32) with DDFSTRT $24: Agnus masks the
    // start down to the 16-cck fetch-unit grid ($20), so the data arrives
    // 4 cck (8 lo-res px) EARLY relative to the programmed start. Denise's
    // reload comparator runs on the absolute hpos gulp grid, so the fold
    // boundary is the data-arrival phase: earliness (8 px) plus the 8-cck
    // fetch-to-comparator pipeline (16 px) = 24. Scroll taps at or past
    // the boundary sit one full gulp left of taps below it.
    // Calibrated against Alien Breed II AGA's playfield scroller, which
    // pairs the folded BPLCON1 taps ($CC99..$CCFF, taps 25..31) with a
    // one-gulp bitplane-pointer step; without the fold the pan jumps 32 px
    // for 4 of every 16 frames (issue #248).
    let control = |bplcon1: u16| ControlState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon0: 0x6200, // 6 planes, lo-res
        ddfstrt: 0x0024,
        ddfstop: 0x00D4,
        fmode: 0x0005, // BPL32 | SPR32
        bplcon1,
        ..ControlState::default()
    };

    // $CC77: raw 55 -> masked tap 23, outside the fold window: no advance.
    assert_eq!(control(0xCC77).row_reload_advance(), 0);
    assert_eq!(control(0xCC77).sample_delay_for_plane(0), 23);
    // $CC99: raw 57 -> masked tap 25 >= 32 - 8: one gulp (32 px) left.
    assert_eq!(control(0xCC99).row_reload_advance(), 32);
    assert_eq!(control(0xCC99).sample_delay_for_plane(0), 25);
    assert_eq!(control(0xCC99).sample_delay_for_plane(1), 25);
    // $CCFF: raw 63 -> tap 31 folds; past the wrap, raw 1 does not.
    assert_eq!(control(0xCCFF).row_reload_advance(), 32);
    assert_eq!(control(0x0011).row_reload_advance(), 0);
    // An on-grid start still folds from the pipeline alone: the data
    // arrives 16 px past the comparator grid point, so taps 16..31 catch
    // the reload one cell early (FS-UAE-verified, ddfprobe-agafold2
    // band 13).
    let on_grid = ControlState {
        ddfstrt: 0x0020,
        ..control(0xCC99)
    };
    assert_eq!(on_grid.row_reload_advance(), 32);
    assert_eq!(on_grid.sample_delay_for_plane(0), 25);
    let on_grid_low = ControlState {
        ddfstrt: 0x0020,
        ..control(0x00FF)
    };
    assert_eq!(on_grid_low.row_reload_advance(), 0);
}

#[test]
fn wide_fmode_fold_boundary_saturates_past_the_tap_range() {
    // SANITY Roots II AGA's swirl and kaleidoscope screens (issue #371):
    // lo-res 64-bit fetch (FMODE=3, 32-cck unit, 64-px gulps) with DDFSTRT
    // $58 or $38, both 24 cck past the unit grid. The 48 px of earliness
    // plus the 16-px fetch-to-comparator pipeline puts the fold boundary
    // past the top of the 0..63 tap range, so no tap folds and the demo's
    // AGA scrolls (pf1 17 / pf2 16 on the swirl, per-line copper taps up
    // to 43 on the kaleidoscope) render linearly. The previous
    // last-earliness-window rule folded every tap >= 16 here, pulling the
    // swirl a full gulp left and shearing the kaleidoscope line by line.
    // FS-UAE-verified band by band (ddfprobe-agafold2).
    let control = |ddfstrt: u16, bplcon1: u16| ControlState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon0: 0x0210, // 8 planes, lo-res
        ddfstrt,
        ddfstop: 0x0090,
        fmode: 0x0003, // BPL64
        bplcon1,
        ..ControlState::default()
    };

    // The swirl screen's live BPLCON1 $4401: pf1 tap 17, pf2 tap 16.
    assert_eq!(control(0x0058, 0x4401).row_reload_advance(), 0);
    assert_eq!(control(0x0058, 0x4401).sample_delay_for_plane(0), 17);
    assert_eq!(control(0x0058, 0x4401).sample_delay_for_plane(1), 16);
    // The kaleidoscope's per-line extremes ($48EB: pf1 43, pf2 30) and the
    // top of the tap range stay linear too.
    assert_eq!(control(0x0038, 0x48EB).row_reload_advance(), 0);
    assert_eq!(control(0x0038, 0x48EB).sample_delay_for_plane(0), 43);
    assert_eq!(control(0x0038, 0x48EB).sample_delay_for_plane(1), 30);
    assert_eq!(control(0x0058, 0xCCFF).row_reload_advance(), 0);
    // The boundary saturates instead of wrapping: a start 28 cck past the
    // grid ($5C) has boundary 56 + 16 = 72, also past the range, so even
    // the top taps stay linear (FS-UAE bands 7..9, which the wrapped
    // model folded).
    assert_eq!(control(0x005C, 0x8877).row_reload_advance(), 0);
    assert_eq!(control(0x005C, 0xCC88).row_reload_advance(), 0);
    // An on-grid start folds from the 16-px pipeline alone: tap 8 stays,
    // tap 56 sits one gulp left (FS-UAE bands 10/13).
    assert_eq!(control(0x0040, 0x0088).row_reload_advance(), 0);
    assert_eq!(control(0x0040, 0xCC88).row_reload_advance(), 64);
    // Mid phases keep their folds: $48 (earliness 16, boundary 32) and
    // $50 (earliness 32, boundary 48) fold tap 56 and 48, and $44
    // (earliness 8, boundary 24) folds tap 55 (FS-UAE bands 11/12/15).
    assert_eq!(control(0x0048, 0xCC88).row_reload_advance(), 64);
    assert_eq!(control(0x0050, 0xCC00).row_reload_advance(), 64);
    assert_eq!(control(0x0044, 0xCC77).row_reload_advance(), 64);
}

#[test]
fn aga_bplcon1_decodes_expanded_scroll_fields() {
    let control = ControlState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon0: 0x0010,
        bplcon1: 0xC8C2,
        fmode: 0x0003,
        ..ControlState::default()
    };

    // BPLCON1 bit layout on Lisa:
    // PF1 H0..H7 = bits 8,9,0,1,2,3,10,11.
    // PF2 H0..H7 = bits 12,13,4,5,6,7,14,15.
    // This frame uses 136 and 240 super-hires pixels respectively; in
    // lo-res output those are 34 and 60 native samples.
    assert_eq!(control.pf1_scroll(), 34);
    assert_eq!(control.pf2_scroll(), 60);
    assert_eq!(control.scroll_for_plane(0), 34);
    assert_eq!(control.scroll_for_plane(1), 60);
}

#[test]
fn aga_bplcon1_preserves_classic_lores_scroll_nibbles() {
    let control = ControlState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon0: 0x0010,
        bplcon1: 0x00A3,
        fmode: 0,
        ..ControlState::default()
    };

    assert_eq!(control.pf1_scroll(), 3);
    assert_eq!(control.pf2_scroll(), 10);
}

#[test]
fn aga_bplcon1_masks_scroll_range_by_fetch_width() {
    let control = |fmode| ControlState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon0: 0x0010,
        bplcon1: 0xCC00,
        fmode,
        ..ControlState::default()
    };

    assert_eq!(control(0x0000).pf1_scroll(), 0);
    assert_eq!(control(0x0001).pf1_scroll(), 16);
    assert_eq!(control(0x0003).pf1_scroll(), 48);
}

#[test]
fn bplcon1_scroll_nibbles_apply_to_odd_even_planes_without_dual_playfield() {
    let control = ControlState {
        bplcon0: 0x2200,
        bplcon1: 0x0020,
        bplcon2: 0,
        ..ControlState::default()
    };

    assert!(!control.dual_playfield());
    assert_eq!(control.scroll_for_plane(0), 0);
    assert_eq!(control.scroll_for_plane(1), 2);
}

#[test]
fn classic_bplcon1_scroll_nibble_counts_lores_pixels_at_every_resolution() {
    // The OCS/ECS scroll nibble is a lo-res pixel count: Denise reloads the
    // shifter when the low nibble bits match its pixel counter, so one step
    // spans one lo-res pixel in native samples (1 lo-res / 2 hi-res /
    // 4 super-hi-res) and the comparison narrows with the word cadence
    // (hi-res compares 3 bits, super-hi-res 2). Regression example: the
    // Kickstart 2.05 insert-disk screen (hi-res, BPLCON1 $44) sat one
    // colour clock left of hardware when the hi-res scroll was halved,
    // clipping the first text column and leaking the negative-modulo
    // overlap words into the window's right edge.
    let control = |bplcon0: u16, bplcon1: u16| ControlState {
        bplcon0,
        bplcon1,
        ..ControlState::default()
    };

    // Lo-res: full nibble, one native sample per lo-res pixel.
    assert_eq!(control(0x2200, 0x0044).scroll_for_plane(0), 4);
    assert_eq!(control(0x2200, 0x0044).scroll_for_plane(1), 4);
    assert_eq!(control(0x2200, 0x00FF).scroll_for_plane(0), 15);

    // Hi-res: two native samples per lo-res pixel, nibble bit 3 ignored.
    assert_eq!(control(0xA200, 0x0044).scroll_for_plane(0), 8);
    assert_eq!(control(0xA200, 0x0044).scroll_for_plane(1), 8);
    assert_eq!(control(0xA200, 0x0077).scroll_for_plane(0), 14);
    assert_eq!(control(0xA200, 0x0088).scroll_for_plane(0), 0);

    // ECS super-hi-res: four native samples per lo-res pixel, two nibble
    // bits compared.
    let shres = 0x2200 | BPLCON0_SHRES;
    assert_eq!(control(shres, 0x0033).scroll_for_plane(0), 12);
    assert_eq!(control(shres, 0x0044).scroll_for_plane(0), 0);
}

#[test]
fn bplcon2_priority_codes_place_sprite_groups_against_playfields() {
    for playfield in 1..=2 {
        for priority_code in 0u16..=7 {
            let bplcon2 = if playfield == 1 {
                priority_code
            } else {
                priority_code << 3
            };
            let control = ControlState {
                bplcon0: 0,
                bplcon1: 0,
                bplcon2,
                ..ControlState::default()
            };
            let visible_groups = priority_code.min(4) as usize;
            for group in 0..4 {
                let sprite = group * 2;
                assert_eq!(
                    sprite_has_priority(sprite, playfield, control),
                    group < visible_groups,
                    "playfield={playfield} priority_code={priority_code} sprite_group={group}"
                );
            }
        }
    }
}

#[test]
fn attached_manual_sprites_use_four_bit_color_indexes() {
    let mut state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 1,
        DIW_HSTART_FB0 as u16,
    );
    state.sprpos[0] = pos;
    state.spr_hw_pos[0] = pos;
    state.sprctl[0] = ctl;
    state.spr_hw_ctl[0] = ctl;
    state.sprdata[0] = 0x8000;
    state.spr_hw_data[0] = 0x8000;
    state.sprdatb[0] = 0;
    state.spr_hw_datb[0] = 0;
    state.spr_armed[0] = true;
    state.spr_hw_armed[0] = true;
    state.sprpos[1] = pos;
    state.spr_hw_pos[1] = pos;
    state.sprctl[1] = ctl | 0x0080;
    state.spr_hw_ctl[1] = ctl | 0x0080;
    state.sprdata[1] = 0x8000;
    state.spr_hw_data[1] = 0x8000;
    state.sprdatb[1] = 0;
    state.spr_hw_datb[1] = 0;
    state.spr_armed[1] = true;
    state.spr_hw_armed[1] = true;
    state.palette.write_ocs(21, 0x00F0);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &[],
        false,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x00F0));
}

#[test]
fn attached_manual_sprites_draw_odd_bits_without_even_data_bits() {
    let mut state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 1,
        DIW_HSTART_FB0 as u16,
    );
    state.sprpos[0] = pos;
    state.spr_hw_pos[0] = pos;
    state.sprctl[0] = ctl;
    state.spr_hw_ctl[0] = ctl;
    state.sprdata[0] = 0;
    state.spr_hw_data[0] = 0;
    state.sprdatb[0] = 0;
    state.spr_hw_datb[0] = 0;
    state.spr_armed[0] = true;
    state.spr_hw_armed[0] = true;
    state.sprpos[1] = pos;
    state.spr_hw_pos[1] = pos;
    state.sprctl[1] = ctl | 0x0080;
    state.spr_hw_ctl[1] = ctl | 0x0080;
    state.sprdata[1] = 0x8000;
    state.spr_hw_data[1] = 0x8000;
    state.sprdatb[1] = 0;
    state.spr_hw_datb[1] = 0;
    state.spr_armed[1] = true;
    state.spr_hw_armed[1] = true;
    state.palette.write_ocs(20, 0x00F0);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &[],
        false,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x00F0));
}

#[test]
fn attached_manual_sprite_pair_uses_even_control_attach_bit() {
    let mut state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 1,
        DIW_HSTART_FB0 as u16,
    );
    state.sprpos[0] = pos;
    state.spr_hw_pos[0] = pos;
    state.sprctl[0] = ctl | 0x0080;
    state.spr_hw_ctl[0] = ctl | 0x0080;
    state.sprdata[0] = 0x8000;
    state.spr_hw_data[0] = 0x8000;
    state.sprdatb[0] = 0;
    state.spr_hw_datb[0] = 0;
    state.spr_armed[0] = true;
    state.spr_hw_armed[0] = true;
    state.sprpos[1] = pos;
    state.spr_hw_pos[1] = pos;
    state.sprctl[1] = ctl;
    state.spr_hw_ctl[1] = ctl;
    state.sprdata[1] = 0x8000;
    state.spr_hw_data[1] = 0x8000;
    state.sprdatb[1] = 0;
    state.spr_hw_datb[1] = 0;
    state.spr_armed[1] = true;
    state.spr_hw_armed[1] = true;
    state.palette.write_ocs(21, 0x00F0);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &[],
        false,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x00F0));
}

#[test]
fn attached_manual_sprite_pair_decodes_odd_pixels_without_even_line() {
    let mut state = blank_state();
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 1,
        DIW_HSTART_FB0 as u16,
    );
    state.sprpos[1] = pos;
    state.spr_hw_pos[1] = pos;
    state.sprctl[1] = ctl | 0x0080;
    state.spr_hw_ctl[1] = ctl | 0x0080;
    state.sprdata[1] = 0x8000;
    state.spr_hw_data[1] = 0x8000;
    state.sprdatb[1] = 0;
    state.spr_hw_datb[1] = 0;
    state.spr_armed[1] = true;
    state.spr_hw_armed[1] = true;
    state.palette.write_ocs(20, 0x00F0);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];

    render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &[],
        false,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x00F0));
}

#[test]
fn sprite_dma_reuse_stops_at_null_control_block() {
    let mut ram = vec![0u8; 512 * 1024];
    let sprite_ptr = 0x0100usize;
    let (pos, ctl) = sprite_control_words(
        PAL_VISIBLE_LINE0 as u16,
        PAL_VISIBLE_LINE0 as u16 + 1,
        DIW_HSTART_FB0 as u16,
    );
    put_word(&mut ram, sprite_ptr, 0);
    put_word(&mut ram, sprite_ptr + 2, 0);
    put_word(&mut ram, sprite_ptr + 4, pos);
    put_word(&mut ram, sprite_ptr + 6, ctl);
    put_word(&mut ram, sprite_ptr + 8, 0x8000);
    put_word(&mut ram, sprite_ptr + 10, 0);
    put_word(&mut ram, sprite_ptr + 12, 0);
    put_word(&mut ram, sprite_ptr + 14, 0);

    let mut state = blank_state();
    state.dmacon = DMACON_DMAEN | DMACON_SPREN;
    state.sprpt[0] = sprite_ptr as u32;
    state.palette.write_ocs(17, 0x0F00);
    let mut sprite_ptr_refreshed = [false; 8];
    sprite_ptr_refreshed[0] = true;

    let fb = render_sprite_dma_test_frame(
        &state,
        &ram,
        sprite_pointer_refreshes_from_mask(sprite_ptr_refreshed),
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0));
}

#[test]
fn collision_pixel_honors_clxcon_match_bits() {
    let collision = collision_pixel(0b000011, 0x00C3, 0, false);
    assert!(collision.pf1_match());
    assert!(collision.pf2_match());

    let mismatch = collision_pixel(0b000010, 0x00C3, 0, false);
    assert!(!mismatch.pf1_match());
    assert!(mismatch.pf2_match());
}

#[test]
fn collision_pixel_single_playfield_odd_match_requires_even_match() {
    let single = collision_pixel(0b000001, 0x0083, 0, false);
    assert!(!single.pf1_match());
    assert!(!single.pf2_match());

    let dual = collision_pixel(0b000001, 0x0083, 0, true);
    assert!(dual.pf1_match());
    assert!(!dual.pf2_match());
}

/// Plan 3.4: AGA planes 7-8 collision control comes from CLXCON2
/// (ENBP7/ENBP8 in bits 6-7, MVBP7/MVBP8 in bits 0-1).
#[test]
fn collision_pixel_planes_7_and_8_use_clxcon2() {
    // Plane 7 (bit 6) enabled, must be set: pixel with bit 6 matches.
    let hit = collision_pixel(0b0100_0000, 0, 0x0041, false);
    assert!(hit.pf1_match());
    let miss = collision_pixel(0, 0, 0x0041, false);
    assert!(!miss.pf1_match());

    // Plane 8 (bit 7) enabled, must be clear: a set bit 7 mismatches.
    let even_hit = collision_pixel(0, 0, 0x0080, false);
    assert!(even_hit.pf2_match());
    let even_miss = collision_pixel(0b1000_0000, 0, 0x0080, false);
    assert!(!even_miss.pf2_match());

    // With CLXCON2 clear, planes 7-8 never gate the match (and the
    // sprite-enable bits of CLXCON are not misread for them).
    let ignore = collision_pixel(0b1100_0000, 0xF000, 0, false);
    assert!(ignore.pf1_match() && ignore.pf2_match());
}

#[test]
fn collision_pixel_disabled_planes_match_continuously() {
    let collision = collision_pixel(0, 0, 0, false);
    assert!(collision.pf1_match());
    assert!(collision.pf2_match());
    assert_eq!(collision.clxdat_bits(), 1);
}

#[test]
fn collision_match_gates_on_enabled_planes_beyond_the_bpu_count() {
    // A one-bitplane playfield pixel (only bitplane 1 set) with CLXCON
    // enabling all six planes at match value 1 must NOT match: the absent
    // planes 2-6 read 0, and every ENABLED plane participates in the compare
    // regardless of the current BPU count (vAmiga checkS2PCollisions:
    // `(dBuffer & enbp) == (mvbp & enbp)`). Copperline previously only checked
    // planes up to the fetched count and so spuriously matched.
    let all_planes_match_one = collision_pixel(0b000001, 0x0FFF, 0, false);
    assert!(!all_planes_match_one.pf1_match());
    assert!(!all_planes_match_one.pf2_match());

    // Setting the absent planes' match value to 0 (CLXCON 0x0FC1) lets the
    // zero-read absent planes match, so the one-plane pixel collides.
    let only_plane1_match = collision_pixel(0b000001, 0x0FC1, 0, false);
    assert!(only_plane1_match.pf1_match());
    assert!(only_plane1_match.pf2_match());
}

#[test]
fn generated_playfield_pixels_feed_playfield_and_sprite_clxdat() {
    let control = ControlState {
        bplcon0: 0x2400,
        ..ControlState::default()
    };
    let sample = DeniseBitplaneSample {
        idx: 0b000011,
        nplanes: 2,
        active: true,
    };
    let mut playfield_mask = vec![0u8; 1];
    let mut collision_pixels = vec![CollisionPixel::default(); 1];
    let mut clxdat = 0u16;

    record_generated_playfield_collision_pixel(
        &mut playfield_mask,
        &mut collision_pixels,
        &mut clxdat,
        0,
        sample,
        control,
    );

    assert_eq!(clxdat & 0x0001, 0x0001);
    assert_eq!(playfield_mask[0], 0x03);

    let mut sprite_group_mask = vec![0u8; 1];
    let sprite_clxdat = generated_sprite_collision_bits(
        0,
        0,
        control.clxcon,
        &mut sprite_group_mask,
        &mut collision_pixels,
        &playfield_mask,
    );
    assert_eq!(sprite_clxdat & 0x0022, 0x0022);
}

#[test]
fn denise_planned_playfield_line_applies_word_phase_scroll_and_plane_count() {
    let row_words = vec![vec![0x8000], vec![0x4000], vec![0x2000]];
    let line_plan = DenisePlannedPlayfieldLine::new(0, 0, 16, &row_words, 16);
    let mut control = ControlState {
        bplcon0: 0x3000,
        ..ControlState::default()
    };

    assert_eq!(
        line_plan.sample(control, 0),
        DeniseBitplaneSample {
            idx: 0x01,
            nplanes: 3,
            active: true,
        }
    );
    assert_eq!(
        line_plan.sample(control, 1),
        DeniseBitplaneSample {
            idx: 0x02,
            nplanes: 3,
            active: true,
        }
    );

    control.bplcon1 = 0x0001;
    assert_eq!(
        line_plan.sample(control, 0),
        DeniseBitplaneSample {
            idx: 0x00,
            nplanes: 3,
            active: true,
        }
    );
    assert_eq!(
        line_plan.sample(control, 1),
        DeniseBitplaneSample {
            idx: 0x03,
            nplanes: 3,
            active: true,
        }
    );

    control.bplcon0 = 0x1000;
    assert_eq!(
        line_plan.sample(control, 1),
        DeniseBitplaneSample {
            idx: 0x01,
            nplanes: 1,
            active: true,
        }
    );
}

#[test]
fn planned_ham_dma_uses_current_bitplane_sample_at_fetch_edge() {
    let mut row_words = vec![vec![0; 1]; 6];
    for words in row_words.iter_mut().take(4) {
        words[0] |= 0x8000;
    }
    let line_plan = DenisePlannedPlayfieldLine::new(0, 68, 72, &row_words, 1);
    let control = visible_lowres_control(0x7800);
    let mut palette = Palette::new();
    palette.write_ocs(15, 0x0123);
    let mut fb = vec![0; FB_PIXELS];
    let mut playfield_mask = vec![0; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0;

    render_planned_playfield_line(
        &line_plan,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut CollisionLookup::new(),
        &mut IndexedOutputCache::default(),
        &mut clxdat,
        palette,
        &[],
        0,
        control,
        &[],
        0,
        control.bplcon1,
        control.bplcon0,
        false,
        0,
        0,
        &h_row_for(control),
        PAL_VISIBLE_LINE0,
        0.0,
        0,
    );

    assert_eq!(fb[68], rgb12_to_rgba8(0x0123));
    assert_eq!(fb[69], rgb12_to_rgba8(0x0123));
    assert_eq!(fb[70], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[71], rgb12_to_rgba8(0x0000));
    assert_eq!(&playfield_mask[68..72], &[0x02, 0x02, 0x00, 0x00]);
}

#[test]
fn planned_ham_dma_advances_hold_through_edge_fetch_phase() {
    let mut row_words = vec![vec![0; 1]; 6];
    row_words[0][0] |= 0x8000; // native x 0: direct palette entry 1
    row_words[1][0] |= 0x4000; // native x 1: HAM blue := 2
    row_words[4][0] |= 0x4000;
    // DIWSTRT one lores px right of standard ($82): the window opens one
    // sample into the fetched stream, so sample 0 is the hidden edge sample
    // whose HAM hold advance this test pins.
    let line_plan = DenisePlannedPlayfieldLine::new(0, 64, 66, &row_words, 2);
    let mut control = visible_lowres_control(0x6800);
    control.diwstrt = ((PAL_VISIBLE_LINE0 as u16) << 8) | (STANDARD_DIW_HSTART as u16 + 1);
    control.diwstop = (((PAL_VISIBLE_LINE0 + 1) as u16) << 8) | (STANDARD_DIW_HSTART as u16 + 2);
    let mut palette = Palette::new();
    palette.write_ocs(1, 0x0123);
    let mut fb = vec![0; FB_PIXELS];
    let mut playfield_mask = vec![0; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0;

    render_planned_playfield_line(
        &line_plan,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut CollisionLookup::new(),
        &mut IndexedOutputCache::default(),
        &mut clxdat,
        palette,
        &[],
        0,
        control,
        &[],
        0,
        control.bplcon1,
        control.bplcon0,
        false,
        0,
        0,
        &h_row_for(control),
        PAL_VISIBLE_LINE0,
        0.0,
        0,
    );

    assert_eq!(control.native_x_offset(control.diw_h_start(), 2), 1);
    assert_eq!(fb[64], rgb12_to_rgba8(0x0122));
    assert_eq!(fb[65], rgb12_to_rgba8(0x0122));
    assert_eq!(&playfield_mask[62..64], &[0x00, 0x00]);
}

#[test]
fn planned_ham_dma_carries_early_ddf_history_across_diw_open() {
    // Denise's HAM accumulator advances on every shifted sample; DIW only
    // selects between border and playfield output. With DDFSTRT one fetch
    // period before the window, the hidden samples must seed the hold colour
    // the first visible pixel modifies. Regression example: the Lemmings 2
    // FES demo's DMA Design logo (352px overscan HAM, DDFSTRT $30, DIW
    // HSTART $79) opens every line with a set-palette pixel in the hidden
    // span; dropping it turned the left edge into black-and-green streaks.
    let mut row_words = vec![vec![0; 2]; 6];
    row_words[0][0] |= 0x8000; // native x 0: direct palette entry 1 (hidden)
    row_words[0][0] |= 0x7FFF; // native x 1..15: HAM blue := $B (hidden)
    row_words[1][0] |= 0x7FFF;
    row_words[3][0] |= 0x7FFF;
    row_words[4][0] |= 0x7FFF;
    row_words[2][1] |= 0x8000; // native x 16: HAM green := $C (first visible)
    row_words[3][1] |= 0x8000;
    row_words[4][1] |= 0x8000;
    row_words[5][1] |= 0x8000;
    let line_plan = DenisePlannedPlayfieldLine::new(0, 62, 64, &row_words, 32);
    let mut control = visible_lowres_control(0x6800);
    control.diwstrt = ((PAL_VISIBLE_LINE0 as u16) << 8) | STANDARD_DIW_HSTART as u16;
    control.diwstop = (((PAL_VISIBLE_LINE0 + 1) as u16) << 8) | (STANDARD_DIW_HSTART as u16 + 1);
    control.ddfstrt = 0x0030;
    control.ddfstop = 0x00D0;
    let mut palette = Palette::new();
    palette.write_ocs(1, 0x0123);
    let mut fb = vec![0; FB_PIXELS];
    let mut playfield_mask = vec![0; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0;

    render_planned_playfield_line(
        &line_plan,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut CollisionLookup::new(),
        &mut IndexedOutputCache::default(),
        &mut clxdat,
        palette,
        &[],
        0,
        control,
        &[],
        0,
        control.bplcon1,
        control.bplcon0,
        false,
        0,
        0,
        &h_row_for(control),
        PAL_VISIBLE_LINE0,
        0.0,
        0,
    );

    // 16 = one 8-cck fetch period before the standard $38 origin: DDFSTRT
    // placement is linear and the hardware window edge (2H-196) is flush
    // with the standard-DDF picture (vAmigaTS Agnus/DIW/OLDDIW/diw1 photos).
    assert_eq!(control.native_x_offset(control.diw_h_start(), 2), 16);
    // Hidden history: pal[1]=$123, then blue:=$B fifteen times -> $12B held
    // at the window edge; the visible green:=$C pixel lands on $1CB.
    assert_eq!(fb[62], rgb12_to_rgba8(0x01CB));
    assert_eq!(fb[63], rgb12_to_rgba8(0x01CB));
    assert_eq!(&playfield_mask[62..64], &[0x02, 0x02]);
}

#[test]
fn bplcon1_write_at_diw_right_edge_does_not_retap_current_ham_line() {
    let mut row_words = vec![vec![0; 1]; 6];
    row_words[0][0] |= 0x4000; // native x 1: direct palette entry 1
    let line_plan = DenisePlannedPlayfieldLine::new(0, 62, 94, &row_words, 16);
    let mut control = visible_lowres_control(0x6800);
    control.diwstrt = ((PAL_VISIBLE_LINE0 as u16) << 8) | STANDARD_DIW_HSTART as u16;
    control.diwstop = (((PAL_VISIBLE_LINE0 + 1) as u16) << 8) | (STANDARD_DIW_HSTART as u16 + 16);
    control.diwhigh = DiwHigh::ecs_explicit(0);
    let mut retapped_control = control;
    retapped_control.bplcon1 = 0x0004;
    let control_segments = [ControlSegment {
        x: control.display_window_x().1,
        control: retapped_control,
    }];
    let mut palette = Palette::new();
    palette.write_ocs(1, 0x0123);
    let mut fb = vec![0; FB_PIXELS];
    let mut playfield_mask = vec![0; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0;

    render_planned_playfield_line(
        &line_plan,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut CollisionLookup::new(),
        &mut IndexedOutputCache::default(),
        &mut clxdat,
        palette,
        &[],
        0,
        control,
        &control_segments,
        0,
        control.bplcon1,
        control.bplcon0,
        false,
        0,
        0,
        &h_row_for(control),
        PAL_VISIBLE_LINE0,
        0.0,
        0,
    );

    assert_eq!(control.display_window_x(), (62, 94));
    assert_eq!(fb[64], rgb12_to_rgba8(0x0123));
    assert_eq!(fb[65], rgb12_to_rgba8(0x0123));
}

/// BPLCON0's HAM select feeds Denise's colour-selection stage, the same stage
/// a COLORxx write feeds, so a mid-line HAM change takes effect
/// [`DENISE_HAM_SELECT_PIPELINE_FB`] framebuffer pixels left of the generic
/// register/beam domain the rest of the control state is sampled in.
///
/// Hardware reference: vAmiga `Denise::setBPLCON0` records the HAM change one
/// colour clock after the carrying bus slot and then backs it up by one colour
/// clock, landing on the same pixel `Denise::pokeCOLORxx` uses. Cross-checked
/// end to end by the vAmiga-verified `hamprobe-select` golden render.
#[test]
fn ham_select_lands_in_the_colour_write_domain() {
    // Index $1F on every fetched pixel: HAM decodes it as modify-blue $F over
    // the black background, the plain index path as palette entry 31.
    let mut row_words = vec![vec![0u16; 4]; 6];
    for plane in row_words.iter_mut().take(5) {
        plane.fill(0xFFFF);
    }
    let x_start = 62;
    let x_stop = x_start + 128;
    let line_plan = DenisePlannedPlayfieldLine::new(0, x_start, x_stop, &row_words, 64);
    let mut control = visible_lowres_control(0x6800); // 6 planes, lo-res, HAM
    control.diwstrt = ((PAL_VISIBLE_LINE0 as u16) << 8) | DIW_HSTART_FETCH_REFERENCE_LORES as u16;
    control.diwstop =
        (((PAL_VISIBLE_LINE0 + 1) as u16) << 8) | (DIW_HSTART_FETCH_REFERENCE_LORES as u16 + 64);
    let mut ham_off = control;
    ham_off.bplcon0 &= !0x0800;
    let segment_x = x_start + 100;
    let control_segments = [ControlSegment {
        x: segment_x,
        control: ham_off,
    }];
    let mut palette = Palette::new();
    palette.write_ocs(31, 0x00F0);
    let mut fb = vec![0; FB_PIXELS];
    let mut playfield_mask = vec![0; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0;

    render_planned_playfield_line(
        &line_plan,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut CollisionLookup::new(),
        &mut IndexedOutputCache::default(),
        &mut clxdat,
        palette,
        &[],
        0,
        control,
        &control_segments,
        0,
        control.bplcon1,
        control.bplcon0,
        false,
        0,
        0,
        &h_row_for(control),
        PAL_VISIBLE_LINE0,
        0.0,
        0,
    );

    let effect_x = segment_x - DENISE_HAM_SELECT_PIPELINE_FB;
    assert_eq!(fb[effect_x - 1], rgb12_to_rgba8(0x000F));
    assert_eq!(fb[effect_x], rgb12_to_rgba8(0x00F0));
    // The generic register domain is where the change used to land.
    assert_eq!(fb[segment_x - 1], rgb12_to_rgba8(0x00F0));
}

#[test]
fn bplcon2_color_key_uses_color_register_transparency_bit() {
    let row_words = vec![vec![0x8000]];
    let line_plan = DenisePlannedPlayfieldLine::new(0, 68, 70, &row_words, 16);
    let mut control = visible_lowres_control(0x1000);
    control.bplcon2 = BPLCON2_ZDCTEN;
    let mut palette = Palette::new();
    palette.write_ocs(1, COLOR_TRANSPARENCY_BIT | 0x0F00);
    let mut fb = vec![0; FB_PIXELS];
    let mut playfield_mask = vec![0; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0;

    render_planned_playfield_line(
        &line_plan,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut CollisionLookup::new(),
        &mut IndexedOutputCache::default(),
        &mut clxdat,
        palette,
        &[],
        0,
        control,
        &[],
        0,
        control.bplcon1,
        control.bplcon0,
        false,
        0,
        0,
        &h_row_for(control),
        PAL_VISIBLE_LINE0,
        0.0,
        0,
    );

    assert_eq!(fb[68], rgb12_to_rgba8_alpha(0x0F00, false));
    assert_eq!(&playfield_mask[68..70], &[0x02, 0x02]);
}

#[test]
fn bplcon2_bitplane_key_uses_selected_bitplane_sample() {
    let row_words = vec![vec![0x8000], vec![0x4000]];
    let line_plan = DenisePlannedPlayfieldLine::new(0, 68, 72, &row_words, 16);
    let mut control = visible_lowres_control(0x2000);
    control.bplcon2 = BPLCON2_ZDBPEN | (1 << BPLCON2_ZDBPSEL_SHIFT);
    let mut palette = Palette::new();
    palette.write_ocs(1, 0x0F00);
    palette.write_ocs(2, 0x00F0);
    let mut fb = vec![0; FB_PIXELS];
    let mut playfield_mask = vec![0; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0;

    render_planned_playfield_line(
        &line_plan,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut CollisionLookup::new(),
        &mut IndexedOutputCache::default(),
        &mut clxdat,
        palette,
        &[],
        0,
        control,
        &[],
        0,
        control.bplcon1,
        control.bplcon0,
        false,
        0,
        0,
        &h_row_for(control),
        PAL_VISIBLE_LINE0,
        0.0,
        0,
    );

    assert_eq!(fb[68], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[70], rgb12_to_rgba8_alpha(0x00F0, false));
}

#[test]
fn bplcon3_zdclken_disables_internal_genlock_keys() {
    let row_words = vec![vec![0x8000]];
    let line_plan = DenisePlannedPlayfieldLine::new(0, 68, 70, &row_words, 16);
    let mut control = visible_lowres_control(BPLCON0_ECSENA | 0x1000);
    control.bplcon2 = BPLCON2_ZDCTEN;
    control.bplcon3 = BPLCON3_ZDCLKEN;
    let mut palette = Palette::new();
    palette.write_ocs(1, COLOR_TRANSPARENCY_BIT | 0x0F00);
    let mut fb = vec![0; FB_PIXELS];
    let mut playfield_mask = vec![0; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0;

    render_planned_playfield_line(
        &line_plan,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut CollisionLookup::new(),
        &mut IndexedOutputCache::default(),
        &mut clxdat,
        palette,
        &[],
        0,
        control,
        &[],
        0,
        control.bplcon1,
        control.bplcon0,
        false,
        0,
        0,
        &h_row_for(control),
        PAL_VISIBLE_LINE0,
        0.0,
        0,
    );

    assert_eq!(fb[68], rgb12_to_rgba8(0x0F00));
    assert_eq!(&playfield_mask[68..70], &[0x02, 0x02]);
}

#[test]
fn planned_playfield_line_feeds_clxdat_from_rendered_dual_playfield_sample() {
    let row_words = vec![vec![0x8000], vec![0x8000]];
    let line_plan = DenisePlannedPlayfieldLine::new(0, 68, 70, &row_words, 16);
    let control = visible_lowres_control(0x2400);
    let mut fb = vec![0; FB_PIXELS];
    let mut playfield_mask = vec![0; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0;

    render_planned_playfield_line(
        &line_plan,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut CollisionLookup::new(),
        &mut IndexedOutputCache::default(),
        &mut clxdat,
        Palette::new(),
        &[],
        0,
        control,
        &[],
        0,
        control.bplcon1,
        control.bplcon0,
        false,
        0,
        0,
        &h_row_for(control),
        PAL_VISIBLE_LINE0,
        0.0,
        0,
    );

    assert_eq!(clxdat & 0x0001, 0x0001);
    assert_eq!(&playfield_mask[68..70], &[0x03, 0x03]);
    assert_eq!(collision_pixels[68].playfield_mask(), 0b11);
}

#[test]
fn denise_manual_bitplane_shifter_uses_bpldat_latches_and_delay() {
    let shifter = DeniseManualBitplaneShifter::new([0x8000, 0x4000, 0, 0, 0, 0, 0, 0], 16);
    let mut control = ControlState {
        bplcon0: 0x2000,
        ..ControlState::default()
    };

    assert_eq!(
        shifter.sample(control, 0),
        Some(DeniseBitplaneSample {
            idx: 0x01,
            nplanes: 2,
            active: true,
        })
    );
    assert_eq!(
        shifter.sample(control, 1),
        Some(DeniseBitplaneSample {
            idx: 0x02,
            nplanes: 2,
            active: true,
        })
    );
    assert_eq!(shifter.sample(control, 16), None);

    control.bplcon1 = 0x0001;
    assert_eq!(
        shifter.sample(control, 0),
        Some(DeniseBitplaneSample {
            idx: 0x00,
            nplanes: 2,
            active: true,
        })
    );
    assert_eq!(
        shifter.sample(control, 1),
        Some(DeniseBitplaneSample {
            idx: 0x03,
            nplanes: 2,
            active: true,
        })
    );
}

#[test]
fn ham8_control_bits_are_the_two_lowest_planes() {
    let mut palette = Palette::new();
    palette.write_entry(5, false, 0x0123);
    palette.write_entry(5, true, 0x0456); // 24-bit entry 5 = 0x142536
                                          // Set: control bits (pixel bits 0-1) = 00, palette index in the
                                          // top six bits.
    let set = ham8_rgb24(&palette, 5 << 2, 0);
    assert_eq!(set, 0x0014_2536);
    // Modify blue (01): value bits replace the top six bits of the
    // component, the low two bits hold.
    let blue = ham8_rgb24(&palette, 0b1010_1001, set);
    assert_eq!(blue, 0x0014_25AA); // 0xA8 | (0x36 & 0x03)
                                   // Modify red (10).
    let red = ham8_rgb24(&palette, 0b1111_1110, blue);
    assert_eq!(red, 0x00FC_25AA);
    // Modify green (11).
    let green = ham8_rgb24(&palette, 0b0100_0111, red);
    assert_eq!(green, 0x00FC_45AA); // 0x44 | (0x25 & 0x03)
}

#[test]
fn denise_playfield_output_selects_ehb_ham_and_dual_playfield_colors() {
    let mut palette = Palette::new();
    palette.write_ocs(1, 0x0E86);
    palette.write_ocs(2, 0x0123);
    palette.write_ocs(9, 0x0456);
    let mut ham_color = rgb12_to_rgb24(0x0ABC);

    let ehb = ControlState {
        bplcon0: 0x6000,
        ..ControlState::default()
    };
    assert_eq!(
        denise_playfield_output(ehb, &palette, 0x21, &mut ham_color),
        DenisePlayfieldOutput {
            color: rgb12_to_rgb24(0x0743),
            color_latch: 0x0E86,
            pf_mask: 2,
        }
    );

    let ham = ControlState {
        bplcon0: 0x6800,
        ..ControlState::default()
    };
    assert_eq!(
        denise_playfield_output(ham, &palette, 0x2F, &mut ham_color).color,
        rgb12_to_rgb24(0x0F43)
    );

    let dual = ControlState {
        bplcon0: 0x2400,
        bplcon3: BPLCON3_PF2OF_DEFAULT,
        ..ControlState::default()
    };
    assert_eq!(
        denise_playfield_output(dual, &palette, 0x02, &mut ham_color),
        DenisePlayfieldOutput {
            color: rgb12_to_rgb24(0x0456),
            color_latch: 0x0456,
            pf_mask: 2,
        }
    );
}

#[test]
fn indexed_output_cache_matches_history_independent_color_resolution() {
    let mut palette = Palette::new();
    for idx in 0..palette.len() {
        let hi = (((idx * 7) as u16) & 0x0F00)
            | (((idx * 11) as u16) & 0x00F0)
            | ((idx * 13) as u16 & 0x000F);
        let lo = (((idx * 3) as u16) & 0x0F00)
            | (((idx * 5) as u16) & 0x00F0)
            | ((idx * 9) as u16 & 0x000F);
        palette.write_entry(idx, false, hi);
        palette.write_entry(idx, true, lo);
    }
    let controls = [
        ControlState {
            bplcon0: 0x6000, // OCS EHB
            ..ControlState::default()
        },
        ControlState {
            bplcon0: 0x2400, // OCS dual playfield
            bplcon3: BPLCON3_PF2OF_DEFAULT,
            ..ControlState::default()
        },
        ControlState {
            agnus_revision: AgnusRevision::AgaAlice,
            bplcon0: 0x0010, // AGA 8-plane indexed
            bplcon4: 0x5A00, // BPLAM
            ..ControlState::default()
        },
        ControlState {
            agnus_revision: AgnusRevision::AgaAlice,
            bplcon0: 0x6000, // AGA EHB
            ..ControlState::default()
        },
    ];
    let mut cache = IndexedOutputCache::default();

    for control in controls {
        let outputs = cache.outputs(control, &palette);
        for idx in u8::MIN..=u8::MAX {
            let mut expected_history = 0x0012_3456;
            let expected = denise_playfield_output(control, &palette, idx, &mut expected_history);
            let mut cached_history = 0x0065_4321;
            let cached = cached_indexed_output(outputs, idx, &mut cached_history);
            assert_eq!(cached, expected, "index {idx:#04x}, control {control:?}");
            assert_eq!(cached_history, expected.color);
        }
    }
}

#[test]
fn ham_dual_playfield_runs_the_resolved_index_through_ham() {
    // The invalid HAM + dual-playfield combination (both BPLCON0 bits set --
    // no real software does this) resolves the dual-playfield colour index
    // and then runs it through the HAM logic: the HAM control code comes from
    // the raw plane 5/6 bits while the value nibble is the resolved index.
    // Matches vAmiga (translateDPF writes the resolved index, colorizeHAM
    // takes the control from the raw bitplane bits); exact on vAmigaTS
    // Denise/BPLCON0/modes4.
    let mut palette = Palette::new();
    palette.write_ocs(9, 0x0456);
    let control = ControlState {
        bplcon0: 0x6C00, // BPU6 + HAM + dual playfield
        bplcon3: BPLCON3_PF2OF_DEFAULT,
        ..ControlState::default()
    };
    assert!(control.hold_and_modify() && control.dual_playfield());

    // Planes 5 and 6 clear -> HAM "set": plane 2 resolves to PF2 index 1 at
    // the default PF2 palette offset 8 (entry 9), shown directly, exactly as
    // the plain dual-playfield path would.
    let mut ham_color = rgb12_to_rgb24(0x0ABC);
    assert_eq!(
        denise_playfield_output(control, &palette, 0x02, &mut ham_color),
        DenisePlayfieldOutput {
            color: rgb12_to_rgb24(0x0456),
            color_latch: 0x0456,
            pf_mask: 2,
        }
    );

    // Plane 5 set supplies the HAM "modify blue" control code; the value
    // nibble is the resolved PF1 index (planes 1/3/5 -> plane 5 only -> 4),
    // so only the blue nibble of the held colour changes.
    let mut ham_color = rgb12_to_rgb24(0x0ABC);
    assert_eq!(
        denise_playfield_output(control, &palette, 0x12, &mut ham_color).color,
        rgb12_to_rgb24(0x0AB4),
    );
}

/// Plan 3.3: the Lisa resolution path. BPLAM XORs the pixel index,
/// HAM8 modifies six bits per component, EHB halves in 8-bit space,
/// and palette lookups read the full 24-bit banked store.
#[test]
fn aga_playfield_output_resolves_ham8_ehb_and_bplam() {
    let mut palette = Palette::new();
    palette.write_banked(0, 1, false, 0x0123);
    palette.write_banked(0, 1, true, 0x0456);
    let aga = ControlState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon0: 0x0010, // BPU3: 8 planes
        ..ControlState::default()
    };
    let mut ham = 0u32;

    // Plain palette lookup composes high and low nibbles.
    let out = denise_playfield_output(aga, &palette, 0x01, &mut ham);
    assert_eq!(out.color, 0x0014_2536);

    // HAM8 with 8 planes: control bits live in the two lowest planes,
    // the value in the top six. Control 01 modifies blue.
    let ham8 = ControlState {
        bplcon0: 0x0810,
        ..aga
    };
    ham = 0x00AA_BBCC;
    let out = denise_playfield_output(ham8, &palette, (0x3F << 2) | 0x01, &mut ham);
    assert_eq!(out.color, 0x00AA_BBFC, "blue := 111111<<2 | old low bits");
    let out = denise_playfield_output(ham8, &palette, (0x15 << 2) | 0x02, &mut ham);
    assert_eq!(
        out.color, 0x0056_BBFC,
        "red := 010101<<2 | old low-bit pair"
    );

    // BPLAM XORs the index before lookup: index 0 becomes 1.
    let masked = ControlState {
        bplcon4: 0x0100,
        ..aga
    };
    let mut ham2 = 0u32;
    let out = denise_playfield_output(masked, &palette, 0x00, &mut ham2);
    assert_eq!(out.color, 0x0014_2536);

    // AGA EHB: entry halved per 8-bit component.
    let mut ehb_palette = Palette::new();
    ehb_palette.write_banked(0, 1, false, 0x0FFF);
    ehb_palette.write_banked(0, 1, true, 0x0EEE);
    let ehb = ControlState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon0: 0x6000, // 6 planes, no HAM
        ..ControlState::default()
    };
    let mut ham3 = 0u32;
    let out = denise_playfield_output(ehb, &ehb_palette, 0x21, &mut ham3);
    assert_eq!(out.color, (0x00FE_FEFE >> 1) & 0x007F_7F7F);
}

#[test]
fn shres_playfield_output_resolves_each_35ns_sample_through_the_palette() {
    let mut palette = Palette::new();
    palette.write_ocs(1, 0x00F0);
    palette.write_ocs(4, 0x0F00);
    palette.write_ocs(5, 0x000F);
    let control = ControlState::default();
    let mut ham_color = 0;

    // A solid run of colour 1 keeps colour 1: each 35 ns half resolves
    // palette[1], never a pair-encoded entry.
    let (left, right) = denise_shres_playfield_output_pair(control, &palette, 1, 1, &mut ham_color);
    assert_eq!(
        blend_shres_outputs(left, right),
        DenisePlayfieldOutput {
            color: rgb12_to_rgb24(0x00F0),
            color_latch: 0x00F0,
            pf_mask: 2,
        }
    );
    // A background/colour-1 pair blends the two resolved colours into the
    // 70 ns framebuffer pixel (the classic-pitch canvas path); a 35 ns
    // canvas emits the halves separately.
    let (left, right) = denise_shres_playfield_output_pair(control, &palette, 0, 1, &mut ham_color);
    assert_eq!(left.color, rgb12_to_rgb24(0x0000));
    assert_eq!(right.color, rgb12_to_rgb24(0x00F0));
    assert_eq!(
        blend_shres_outputs(left, right),
        DenisePlayfieldOutput {
            color: rgb24_blend_halves(rgb12_to_rgb24(0x0000), rgb12_to_rgb24(0x00F0)),
            color_latch: 0x00F0,
            pf_mask: 2,
        }
    );
}

#[test]
fn canvas_scale_doubles_only_for_programmable_shres_frames() {
    let shres_write = BeamRegisterWrite {
        vpos: 100,
        hpos: 20,
        offset: 0x100,
        value: 0x4240,
        source: BeamWriteSource::Copper,
    };
    let lores_write = BeamRegisterWrite {
        value: 0x4200,
        ..shres_write
    };
    // Standard scans never double, SHRES or not.
    assert_eq!(canvas_scale_for(false, 0x4240, &[]), 1);
    assert_eq!(canvas_scale_for(false, 0x4200, &[shres_write]), 1);
    // Programmable scans double when SHRES is active at the frame start or
    // arrives mid-frame.
    assert_eq!(canvas_scale_for(true, 0x4240, &[]), 2);
    assert_eq!(canvas_scale_for(true, 0x4200, &[shres_write]), 2);
    assert_eq!(canvas_scale_for(true, 0x4200, &[lores_write]), 1);
}

#[test]
fn aga_shres_playfield_output_keeps_four_plane_indices() {
    // Regression: the Debian/m68k amifb console (SHRES, 4 bitplanes,
    // FMODE=3) rendered index 14 (yellow) as index 10 (light green) and
    // index 7 (grey) as index 15 (white) while the pair encoding truncated
    // every 35 ns sample to two planes.
    let mut palette = Palette::new();
    palette.write_ocs(7, 0x0AAA);
    palette.write_ocs(10, 0x05F5);
    palette.write_ocs(14, 0x0FF5);
    palette.write_ocs(15, 0x0FFF);
    let control = ControlState {
        agnus_revision: AgnusRevision::AgaAlice,
        bplcon0: 0x4240, // 4 planes, SHRES
        ..ControlState::default()
    };
    let mut ham_color = 0;

    let (left, right) =
        denise_shres_playfield_output_pair(control, &palette, 14, 14, &mut ham_color);
    assert_eq!(
        blend_shres_outputs(left, right).color,
        palette.rgb24(14) & 0x00FF_FFFF
    );
    let (left, right) = denise_shres_playfield_output_pair(control, &palette, 7, 7, &mut ham_color);
    assert_eq!(
        blend_shres_outputs(left, right).color,
        palette.rgb24(7) & 0x00FF_FFFF
    );
}

#[test]
fn clxcon_odd_sprite_enable_bits_or_odd_sprites_with_even_partner() {
    let mut sprite_group_mask = vec![0u8; 1];
    let mut collision_pixels = vec![CollisionPixel::default(); 1];
    let playfield_mask = vec![0u8; 1];

    assert_eq!(
        generated_sprite_collision_bits(
            1,
            0,
            0,
            &mut sprite_group_mask,
            &mut collision_pixels,
            &playfield_mask
        ),
        0
    );
    assert_eq!(sprite_group_mask[0], 0);

    assert_eq!(
        generated_sprite_collision_bits(
            0,
            0,
            0,
            &mut sprite_group_mask,
            &mut collision_pixels,
            &playfield_mask
        ),
        0
    );
    assert_eq!(sprite_group_mask[0], 0b0001);

    assert_eq!(
        generated_sprite_collision_bits(
            3,
            0,
            0,
            &mut sprite_group_mask,
            &mut collision_pixels,
            &playfield_mask
        ),
        0
    );
    assert_eq!(sprite_group_mask[0], 0b0001);

    assert_eq!(
        generated_sprite_collision_bits(
            3,
            0,
            1 << 13,
            &mut sprite_group_mask,
            &mut collision_pixels,
            &playfield_mask
        ),
        1 << 9
    );
    assert_eq!(sprite_group_mask[0], 0b0011);
}

#[test]
fn attached_sprite_pair_collision_groups_or_odd_pixels_through_clxcon() {
    let mut sprite_group_mask = vec![0b0010u8; 1];
    let mut collision_pixels = vec![CollisionPixel::default(); 1];
    let playfield_mask = vec![0u8; 1];

    assert_eq!(
        generated_sprite_pair_collision_bits(
            0,
            0,
            0,
            false,
            true,
            &mut sprite_group_mask,
            &mut collision_pixels,
            &playfield_mask,
        ),
        0
    );
    assert_eq!(sprite_group_mask[0], 0b0010);

    assert_eq!(
        generated_sprite_pair_collision_bits(
            0,
            0,
            1 << 12,
            false,
            true,
            &mut sprite_group_mask,
            &mut collision_pixels,
            &playfield_mask,
        ),
        1 << 9
    );
    assert_eq!(sprite_group_mask[0], 0b0011);
}

#[test]
fn ham6_pixels_modify_previous_rgb_components() {
    let mut palette = Palette::new();
    palette.write_ocs(3, 0x0123);

    let direct = ham6_rgb12(&palette, 0x03, 0x0FFF);
    assert_eq!(direct, 0x0123);
    assert_eq!(ham6_rgb12(&palette, 0x14, direct), 0x0124);
    assert_eq!(ham6_rgb12(&palette, 0x25, direct), 0x0523);
    assert_eq!(ham6_rgb12(&palette, 0x36, direct), 0x0163);
}

#[test]
fn manual_ham_bitplane_word_delays_select_by_one_pixel() {
    let mut palette = Palette::new();
    palette.write_ocs(1, 0x0F00);
    let control = ControlState {
        bplcon0: 0xE800,
        ..ControlState::default()
    };
    let base_palettes = [palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [control; FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let mut playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0u16;
    let mut planes = [0u16; 8];
    planes[0] = 0x8000;
    let segments = [ManualBplSegment {
        line: 0,
        hpos: 0,
        x: 0,
        planes,
        palette,
    }];

    render_manual_bpl_segments(
        &segments,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut clxdat,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0));
    assert_eq!(fb[1], rgb12_to_rgba8(0x0F00));
}

#[test]
fn ham_hold_resets_while_display_window_is_closed() {
    let mut palette = Palette::new();
    palette.write_ocs(1, 0x0F00);
    let control = ControlState {
        bplcon0: 0xE800,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | (DIW_HSTART_FB0 as u16 + 1),
        diwstop: ((PAL_VISIBLE_LINE0 as u16 + 1) << 8) | 0x00C1,
        ..ControlState::default()
    };
    let base_palettes = [palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [control; FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let mut playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0u16;
    let mut planes = [0u16; 8];
    planes[0] = 0x4000;
    let segments = [ManualBplSegment {
        line: 0,
        hpos: 0,
        x: 0,
        planes,
        palette,
    }];

    render_manual_bpl_segments(
        &segments,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut clxdat,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
    );

    assert_eq!(fb[2], rgb12_to_rgba8(0));
}

#[test]
fn ham_pipeline_uses_palette_at_output_pixel_boundary() {
    let mut palette = Palette::new();
    palette.write_ocs(1, 0x0F00);
    let control = ControlState {
        bplcon0: 0xE800,
        ..ControlState::default()
    };
    let base_palettes = [palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    palette_segments[0].push(PaletteSegment {
        x: 1,
        entry: 1,
        loct: false,
        value: 0x00F0,
    });
    let base_controls = [control; FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let mut playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0u16;
    let mut planes = [0u16; 8];
    planes[0] = 0x8000;
    let segments = [ManualBplSegment {
        line: 0,
        hpos: 0,
        x: 0,
        planes,
        palette,
    }];

    render_manual_bpl_segments(
        &segments,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut clxdat,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
    );

    assert_eq!(fb[1], rgb12_to_rgba8(0x00F0));
}

#[test]
fn render_events_replay_palette_segments_by_beam_position() {
    let mut state = blank_state();
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [
        beam_event(0x45, COPPER_WAIT_HPOS_FB0 as u32, 0x0180, 0x0402),
        beam_event(0x47, COPPER_WAIT_HPOS_FB0 as u32, 0x0180, 0x0103),
    ];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let maroon_line = (0x45 - 0x2C) as usize;
    let border_line = (0x47 - 0x2C) as usize;
    assert_eq!(palette_segments[maroon_line][0].entry, 0);
    assert_eq!(palette_segments[maroon_line][0].value, 0x0402);
    assert_eq!(base_palettes[maroon_line + 1][0], 0x0402);
    assert_eq!(palette_segments[border_line][0].entry, 0);
    assert_eq!(palette_segments[border_line][0].value, 0x0103);
    assert_eq!(base_palettes[border_line + 1][0], 0x0103);
}

#[test]
fn color00_overscan_write_does_not_backfill_row_start() {
    let mut state = blank_state();
    state.palette.write_ocs(0, 0x0000);
    state.diwstrt = ((PAL_VISIBLE_LINE0 as u16) << 8) | STANDARD_DIW_HSTART as u16;
    state.diwstop = (((PAL_VISIBLE_LINE0 + 1) as u16) << 8) | STANDARD_DIW_HSTOP as u16;
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let beam_y = PAL_VISIBLE_LINE0 as u32;
    let events = [
        beam_event(beam_y, 68, 0x0180, 0x087A),
        beam_event(beam_y, 76, 0x0180, 0x0000),
    ];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let x_on = color_write_framebuffer_x(68, false);
    let x_off = color_write_framebuffer_x(76, false);
    assert_eq!(palette_segments[0][0].x, x_on);
    assert_eq!(palette_segments[0][0].value, 0x087A);
    assert_eq!(palette_segments[0][1].x, x_off);
    assert_eq!(palette_segments[0][1].value, 0x0000);
    assert_eq!(base_palettes[0][0], 0x0000);

    let mut fb = vec![0; FB_PIXELS];
    fill_background(
        &mut fb,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
    );

    assert_eq!(fb[0], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[x_on - 1], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[x_on], rgb12_to_rgba8(0x087A));
    assert_eq!(fb[x_off - 1], rgb12_to_rgba8(0x087A));
    assert_eq!(fb[x_off], rgb12_to_rgba8(0x0000));
}

#[test]
fn pre_hblank_color_write_updates_previous_row_tail_and_next_row_base() {
    let mut state = blank_state();
    state.palette.write_ocs(0, 0x0000);
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let beam_y = PAL_VISIBLE_LINE0 as u32 + 1;
    let events = [beam_event(beam_y, 2, 0x0180, 0x0123)];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    assert_eq!(base_palettes[0][0], 0x0000);
    assert_eq!(base_palettes[1][0], 0x0123);
    assert_eq!(palette_segments[0].len(), 1);
    assert_eq!(
        palette_segments[0][0].x,
        color_write_wrapped_framebuffer_x(2, false)
    );
    assert_eq!(palette_segments[0][0].value, 0x0123);

    let mut fb = vec![0; FB_PIXELS];
    fill_background(
        &mut fb,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
    );

    let wrapped_x = color_write_wrapped_framebuffer_x(2, false);
    assert_eq!(fb[wrapped_x - 1], rgb12_to_rgba8(0x0000));
    assert_eq!(fb[wrapped_x], rgb12_to_rgba8(0x0123));
    assert_eq!(fb[FB_WIDTH], rgb12_to_rgba8(0x0123));
}

#[test]
fn render_events_sample_bplcon_control_at_beam_positions() {
    let mut state = blank_state();
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [
        beam_event(0x50, COPPER_WAIT_HPOS_FB0 as u32, 0x0100, 0x6800),
        beam_event(0x52, COPPER_WAIT_HPOS_FB0 as u32, 0x0100, 0x4000),
    ];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let ham_line = (0x50 - 0x2C) as usize;
    let direct_line = (0x52 - 0x2C) as usize;
    assert_eq!(control_segments[ham_line][0].control.bplcon0, 0x6800);
    assert_eq!(base_controls[ham_line + 1].bplcon0, 0x6800);
    assert_eq!(control_segments[direct_line][0].control.bplcon0, 0x4000);
    assert_eq!(base_controls[direct_line + 1].bplcon0, 0x4000);
}

#[test]
fn aga_bplcon4_splits_sprite_base_from_bitplane_xor_timing() {
    let mut state = blank_state();
    state.agnus_revision = AgnusRevision::AgaAlice;
    state.bplcon4 = 0xAA09;
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let hpos = (COPPER_WAIT_HPOS_FB0 + 20) as u32;
    let events = [beam_event(0x50, hpos, 0x010C, 0x5507)];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let line = (0x50 - 0x2C) as usize;
    let sprite_x = sprite_palette_control_framebuffer_x(hpos);
    assert_eq!(
        sprite_x,
        color_write_framebuffer_x(hpos, true).saturating_sub(5)
    );
    let beam_x = beam_to_framebuffer_x_unclamped(hpos) as usize;
    assert!(sprite_x < beam_x);
    assert_eq!(control_segments[line].len(), 2);
    assert_eq!(control_segments[line][0].x, sprite_x);
    assert_eq!(control_segments[line][0].control.bplcon4, 0xAA07);
    assert_eq!(control_segments[line][1].x, beam_x);
    assert_eq!(control_segments[line][1].control.bplcon4, 0x5507);
    assert_eq!(
        control_at_x(base_controls[line], &control_segments[line], beam_x - 1).bplcon4,
        0xAA07
    );
    assert_eq!(
        control_at_x(base_controls[line], &control_segments[line], beam_x).bplcon4,
        0x5507
    );
}

#[test]
fn ddf_events_update_later_bitplane_fetch_geometry() {
    let mut state = RenderState {
        agnus_revision: AgnusRevision::Ecs8372Rev4,
        ddfstrt: 0x0038,
        ddfstop: 0x0038,
        ..blank_state()
    };
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [beam_event(
        0x50,
        COPPER_WAIT_HPOS_FB0 as u32,
        0x0094,
        0x0040,
    )];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let line = (0x50 - 0x2C) as usize;
    assert_eq!(base_controls[line - 1].words_per_row(320), 1);
    assert_eq!(control_segments[line][0].control.words_per_row(320), 2);
    assert_eq!(base_controls[line + 1].words_per_row(320), 2);
}

#[test]
fn display_plan_events_record_beam_timed_palette_control_and_bpldat_writes() {
    let mut state = RenderState {
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x1000,
        ddfstrt: 0x0038,
        ddfstop: 0x0038,
        ..blank_state()
    };
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let mut display_line_events = vec![Vec::new(); FB_HEIGHT];
    let events = [
        beam_event(0x50, 0x0040, 0x0180, 0x0ABC),
        beam_event(0x50, 0x0042, 0x0102, 0x0004),
        beam_event(0x50, 0x0044, 0x0108, 0x0002),
        beam_event(0x50, 0x0046, 0x0116, 0x8000),
    ];

    apply_render_events_and_collect_display_plan_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
        Some(&mut display_line_events),
    );

    let line = (0x50 - 0x2C) as usize;
    assert!(
        display_line_events[line].contains(&DisplayLinePlanEvent::PaletteChange {
            hpos: 0x0040,
            x: color_write_framebuffer_x(0x0040, false),
            palette: {
                let mut palette = Palette::from_ocs([0x0103; 32]);
                palette.write_ocs(0, 0x0ABC);
                Box::new(palette)
            },
        })
    );
    assert!(display_line_events[line].iter().any(|event| matches!(
        event,
        DisplayLinePlanEvent::ControlChange {
            hpos: 0x0042,
            x: 104,
            control,
        } if control.bplcon1 == 0x0004
    )));
    assert!(display_line_events[line].iter().any(|event| matches!(
        event,
        DisplayLinePlanEvent::ControlChange {
            hpos: 0x0044,
            x: 112,
            control,
        } if control.bpl1mod == 0x0002
    )));
    // The write's batch snaps to the serialiser word grid: lores slots sit at
    // x = 30 (mod 32), and the first slot at/after this write's landing is 94.
    assert!(
        display_line_events[line].contains(&DisplayLinePlanEvent::BpldatWrite {
            hpos: 0x0046,
            x: 94,
            plane: 3,
            value: 0x8000,
        })
    );
}

#[test]
fn events_above_the_visible_area_fold_to_line_zero_start_state() {
    // The boot ROM copper list programs the display window, BPLCON0 and
    // FMODE on the line *before* the window opens (vpos 0x2B for a
    // standard 0x2C start). Those writes happen before the first
    // framebuffer line: they must contribute to line 0's start state,
    // not become mid-line segments at their horizontal position (which
    // split the first display line into border-black and colour-0 spans).
    let mut state = blank_state();
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [
        // Mid-line positions on the line above the visible area.
        beam_event(0x2B, 0x0052, 0x008E, 0x2C81),
        beam_event(0x2B, 0x0056, 0x0100, 0x1200),
        beam_event(0x2B, 0x0060, 0x0180, 0x0ABC),
        // A write on the first visible line keeps its position.
        beam_event(0x2C, 0x0052, 0x0102, 0x0004),
    ];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    for segment in &control_segments[0] {
        if segment.control.bplcon1 == 0x0004 {
            // The on-line event lands at its beam position.
            assert_eq!(segment.x, beam_to_framebuffer_x_unclamped(0x0052) as usize);
        } else {
            assert_eq!(segment.x, 0, "pre-visible control change must fold to x=0");
        }
    }
    assert!(control_segments[0]
        .iter()
        .any(|segment| segment.control.diwstrt == 0x2C81));
    assert_eq!(palette_segments[0][0].x, 0);
}

#[test]
fn same_line_ddfstrt_extension_does_not_fetch_already_missed_words() {
    let mut state = RenderState {
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x1000,
        ddfstrt: 0x0050,
        ddfstop: 0x0050,
        ..blank_state()
    };
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [beam_event(0x50, 0x0040, 0x0092, 0x0038)];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let line = (0x50 - 0x2C) as usize;
    assert_eq!(
        line_fetch_hpos_for_word(base_controls[line], &control_segments[line], 0),
        None
    );
    assert_eq!(
        line_fetch_hpos_for_word(base_controls[line], &control_segments[line], 1),
        Some(0x0040)
    );
    assert_eq!(
        line_fetch_hpos_for_word(base_controls[line], &control_segments[line], 3),
        Some(0x0050)
    );
}

#[test]
fn same_line_ddfstrt_shrink_preserves_already_scheduled_words() {
    let mut state = RenderState {
        agnus_revision: AgnusRevision::Ecs8372Rev4,
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x1000,
        ddfstrt: 0x0038,
        ddfstop: 0x0050,
        ..blank_state()
    };
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [beam_event(0x50, 0x0040, 0x0092, 0x0050)];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let line = (0x50 - 0x2C) as usize;
    assert_eq!(
        line_fetch_hpos_for_word(base_controls[line], &control_segments[line], 0),
        Some(0x0038)
    );
    assert_eq!(
        line_fetch_hpos_for_word(base_controls[line], &control_segments[line], 1),
        None
    );
}

#[test]
fn display_line_fetch_plan_matches_per_word_scan_across_beam_timed_control() {
    let mut state = RenderState {
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x3000,
        bplcon1: 0,
        ddfstrt: 0x0038,
        ddfstop: 0x0058,
        ..blank_state()
    };
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [
        beam_event(0x50, 0x003C, 0x0102, 0x0012),
        beam_event(0x50, 0x0040, 0x0092, 0x0050),
        beam_event(0x50, 0x0048, 0x0100, 0x7800),
        beam_event(0x50, 0x0050, 0x0092, 0x0038),
        beam_event(0x50, 0x0058, 0x0094, 0x0060),
    ];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let line = (0x50 - 0x2C) as usize;
    let words_per_row = line_words_per_row(base_controls[line], &control_segments[line]);
    let dma_planes = line_max_dma_planes(base_controls[line], &control_segments[line]);
    let plans = line_fetch_plans_for_line(
        base_controls[line],
        &control_segments[line],
        words_per_row,
        dma_planes,
    );

    for (word_idx, actual) in plans.iter().enumerate() {
        let expected = line_fetch_plan_for_word(
            base_controls[line],
            &control_segments[line],
            word_idx,
            dma_planes,
        );
        assert_eq!(
            actual.word_fetch_hpos, expected.word_fetch_hpos,
            "word {word_idx}"
        );
        assert_eq!(
            actual.iter().collect::<Vec<_>>(),
            expected.iter().collect::<Vec<_>>(),
            "word {word_idx}"
        );
    }
}

#[test]
fn manual_bpl1dat_snapshots_dma_updated_bpldat_latches() {
    let mut state = RenderState {
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x3000,
        ddfstrt: 0x0038,
        ddfstop: 0x0038,
        ..blank_state()
    };
    state.bpldat[1] = 0x4000;
    let base_control = ControlState::from_render_state(&state);
    let base_controls = [base_control; FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let hpos = 0x0040;
    let mut segments = [ManualBplSegment {
        line: 0,
        hpos,
        x: beam_to_framebuffer_x_unclamped(hpos),
        planes: [0; 8],
        palette: state.palette,
    }];
    let mut captured_rows = vec![None; FB_HEIGHT];
    let mut planes: [Vec<u16>; 8] = std::array::from_fn(|_| vec![0]);
    planes[1][0] = 0x8000;
    captured_rows[0] = Some(CapturedBitplaneRow {
        nplanes: 3,
        words_per_row: 1,
        fetch_origin_cck: None,
        planes,
    });
    let events = [beam_event(PAL_VISIBLE_LINE0 as u32, hpos, 0x0110, 0x0000)];

    seed_manual_bpl_segments_from_latches(
        &mut segments,
        state.bpldat,
        &events,
        &base_controls,
        &control_segments,
        &captured_rows,
        PAL_VISIBLE_LINE0,
    );

    assert_eq!(segments[0].planes[0], 0x0000);
    assert_eq!(segments[0].planes[1], 0x8000);
}

#[test]
fn manual_bpl1dat_before_dma_output_stops_at_dma_shifter_load() {
    let mut palette = Palette::new();
    palette.write_ocs(1, 0x0F00);
    let control = ControlState {
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x1000,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | STANDARD_DIW_HSTART as u16,
        diwstop: (((PAL_VISIBLE_LINE0 + 1) as u16) << 8) | STANDARD_DIW_HSTOP as u16,
        ddfstrt: 0x0050,
        ddfstop: 0x00D0,
        ..ControlState::default()
    };
    let base_palettes = [palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [control; FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let dma_output_x = bitplane_dma_output_start_x(
        control,
        &[],
        control.display_window_x().0,
        control.words_per_row(native_frame_width_for_control(control)),
        control.dma_planes(),
    )
    .unwrap();
    let mut fb = vec![rgb12_to_rgba8(0x000F); FB_PIXELS];
    let mut playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0u16;
    let mut planes = [0u16; 8];
    planes[0] = 0xFFFF;
    let segments = [ManualBplSegment {
        line: 0,
        hpos: 0,
        x: dma_output_x as i32 - 8,
        planes,
        palette,
    }];
    let mut dma_output_start_x_by_line = vec![None; FB_HEIGHT];
    dma_output_start_x_by_line[0] = Some(dma_output_x);

    render_manual_bpl_segments_with_visible_line0(
        &segments,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut clxdat,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &dma_output_start_x_by_line,
        PAL_VISIBLE_LINE0,
        0.0,
        0,
    );

    assert_eq!(fb[dma_output_x - 2], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[dma_output_x], rgb12_to_rgba8(0x000F));
}

#[test]
fn manual_bpl1dat_inside_diw_stops_at_next_dma_bpl1dat_load() {
    let mut palette = Palette::new();
    palette.write_ocs(1, 0x0F00);
    let control = ControlState {
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x1000,
        diwstrt: ((PAL_VISIBLE_LINE0 as u16) << 8) | STANDARD_DIW_HSTART as u16,
        diwstop: (((PAL_VISIBLE_LINE0 + 1) as u16) << 8) | STANDARD_DIW_HSTOP as u16,
        ddfstrt: 0x0050,
        ddfstop: 0x00D0,
        ..ControlState::default()
    };
    let base_palettes = [palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_controls = [control; FB_HEIGHT];
    let control_segments = vec![Vec::new(); FB_HEIGHT];
    let dma_output_x = bitplane_dma_output_start_x(
        control,
        &[],
        control.display_window_x().0,
        control.words_per_row(native_frame_width_for_control(control)),
        control.dma_planes(),
    )
    .unwrap();
    let next_dma_bpl1dat_x =
        bitplane_fetch_framebuffer_x(bitplane_fetch_hpos_for_plane(control, 0, 0));
    let mut fb = vec![rgb12_to_rgba8(0x000F); FB_PIXELS];
    let mut playfield_mask = vec![0u8; FB_PIXELS];
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    let mut clxdat = 0u16;
    let mut planes = [0u16; 8];
    planes[0] = 0xFFFF;
    let hpos = 0x004A;
    let segments = [ManualBplSegment {
        line: 0,
        hpos,
        x: beam_to_framebuffer_x_unclamped(hpos),
        planes,
        palette,
    }];
    assert!(segments[0].x as usize > dma_output_x);
    assert!(segments[0].x < next_dma_bpl1dat_x as i32);
    let mut dma_output_start_x_by_line = vec![None; FB_HEIGHT];
    dma_output_start_x_by_line[0] = Some(dma_output_x);

    render_manual_bpl_segments_with_visible_line0(
        &segments,
        &mut fb,
        &mut playfield_mask,
        &mut collision_pixels,
        &mut clxdat,
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &dma_output_start_x_by_line,
        PAL_VISIBLE_LINE0,
        0.0,
        0,
    );

    assert_eq!(fb[next_dma_bpl1dat_x - 2], rgb12_to_rgba8(0x0F00));
    assert_eq!(fb[next_dma_bpl1dat_x], rgb12_to_rgba8(0x000F));
}

#[test]
fn modulo_events_update_later_bitplane_row_advance() {
    let mut state = RenderState {
        bpl1mod: 0,
        ..blank_state()
    };
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [beam_event(
        0x50,
        COPPER_WAIT_HPOS_FB0 as u32,
        0x0108,
        0x0004,
    )];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let line = (0x50 - 0x2C) as usize;
    let before = base_controls[line - 1];
    let after = line_control_at_x(&base_controls, &control_segments, line, 0);
    assert_eq!(before.bpl1mod, 0);
    assert_eq!(after.bpl1mod, 4);

    let mut ptrs = [0x0100, 0, 0, 0, 0, 0, 0, 0];
    advance_bitplane_ptrs_for_rows(&mut ptrs, 1, 1, 1, &before, 0, 0x001F_FFFF);
    assert_eq!(ptrs[0], 0x0102);
    advance_bitplane_ptrs_for_rows(&mut ptrs, 1, 1, 1, &after, 0, 0x001F_FFFF);
    assert_eq!(ptrs[0], 0x0108);
}

#[test]
fn dmacon_events_update_later_bitplane_dma_control() {
    let mut state = RenderState {
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x1000,
        ..blank_state()
    };
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [beam_event(
        0x50,
        COPPER_WAIT_HPOS_FB0 as u32,
        0x0096,
        DMACON_BPLEN,
    )];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let line = (0x50 - 0x2C) as usize;
    assert!(base_controls[line - 1].bitplane_dma_enabled());
    assert!(!control_segments[line][0].control.bitplane_dma_enabled());
    assert!(!base_controls[line + 1].bitplane_dma_enabled());
}

#[test]
fn clxcon_events_update_later_collision_control() {
    let mut state = RenderState {
        clxcon: 0,
        ..blank_state()
    };
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [beam_event(
        0x50,
        COPPER_WAIT_HPOS_FB0 as u32,
        0x0098,
        1 << 12,
    )];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let line = (0x50 - 0x2C) as usize;
    assert_eq!(base_controls[line - 1].clxcon, 0);
    assert_eq!(control_segments[line][0].control.clxcon, 1 << 12);
    assert_eq!(base_controls[line + 1].clxcon, 1 << 12);
}

#[test]
fn sprite_collisions_use_beam_timed_clxcon_control() {
    let mut state = blank_state();
    state.dmacon = DMACON_DMAEN | DMACON_SPREN;
    state.clxcon = 0;
    state.palette.write_ocs(17, 0x0F00);
    let ram = vec![0; 64];
    let base_palettes = [state.palette; FB_HEIGHT];
    let palette_segments = vec![Vec::new(); FB_HEIGHT];
    let base_control = ControlState::from_render_state(&state);
    let base_controls = [base_control; FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut active_control = base_control;
    active_control.clxcon = 1 << 12;
    control_segments[0].push(ControlSegment {
        x: 0,
        control: active_control,
    });
    let mut playfield_mask = vec![0u8; FB_PIXELS];
    playfield_mask[0] = 0x02;
    playfield_mask[1] = 0x02;
    let mut collision_pixels = vec![CollisionPixel::default(); FB_PIXELS];
    collision_pixels[0] = CollisionPixel::new(false, true, false, true);
    collision_pixels[1] = collision_pixels[0];
    let mut fb = vec![rgb12_to_rgba8(0); FB_PIXELS];
    let captured = [CapturedSpriteLine {
        sprite: 1,
        hstart: SPRITE_HSTART_FB0,
        hsub_70ns: false,
        beam_y: PAL_VISIBLE_LINE0,
        data: 0x8000,
        datb: 0,
        attached: false,
        data_ext: [0; 3],
        datb_ext: [0; 3],
        width_words: 1,
    }];

    let clxdat = render_sprites(
        &state,
        &ram,
        &mut fb,
        SpriteClip {
            x_start: 0,
            x_stop: FB_WIDTH,
            y_start: 0,
            y_stop: FB_HEIGHT,
        },
        &base_palettes,
        &palette_segments,
        &base_controls,
        &control_segments,
        &playfield_mask,
        &mut collision_pixels,
        [false; 8],
        &captured,
        true,
    );

    assert_eq!(clxdat & (1 << 5), 1 << 5);
}

#[test]
fn displayed_row_uses_control_at_display_start_not_line_end() {
    let mut state = blank_state();
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [
        beam_event(0x50, (COPPER_WAIT_HPOS_FB0 + 10) as u32, 0x0102, 0x0011),
        beam_event(0x50, (COPPER_WAIT_HPOS_FB0 + 200) as u32, 0x0102, 0x0022),
    ];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let line = (0x50 - 0x2C) as usize;
    assert_eq!(
        line_control_at_x(&base_controls, &control_segments, line, 64).bplcon1,
        0x0011
    );
    assert_eq!(base_controls[line + 1].bplcon1, 0x0022);
}

#[test]
fn unchanged_control_writes_do_not_create_render_segments() {
    let mut state = blank_state();
    state.bplcon1 = 0x0011;
    let mut base_palettes = [state.palette; FB_HEIGHT];
    let mut palette_segments = vec![Vec::new(); FB_HEIGHT];
    let mut base_controls = [ControlState::from_render_state(&state); FB_HEIGHT];
    let mut control_segments = vec![Vec::new(); FB_HEIGHT];
    let mut manual_bpl_segments = Vec::new();
    let events = [
        beam_event(0x50, COPPER_WAIT_HPOS_FB0 as u32, 0x0102, 0x0011),
        beam_event(0x50, (COPPER_WAIT_HPOS_FB0 + 4) as u32, 0x0102, 0x0011),
        beam_event(0x50, (COPPER_WAIT_HPOS_FB0 + 8) as u32, 0x0102, 0x0022),
    ];

    apply_render_events(
        &mut state,
        &events,
        &mut base_palettes,
        &mut palette_segments,
        &mut base_controls,
        &mut control_segments,
        &mut manual_bpl_segments,
    );

    let line = (0x50 - 0x2C) as usize;
    assert_eq!(control_segments[line].len(), 1);
    assert_eq!(control_segments[line][0].x, 32);
    assert_eq!(control_segments[line][0].control.bplcon1, 0x0022);
}

#[test]
fn fetch_origin_below_hard_start_stays_linear_in_ddfstrt() {
    // A run armed from a DDFSTRT below the hardwired start window ($18)
    // anchors its fetch grid at the raw comparator position (vAmigaTS
    // Agnus/DDF oldhwstop3/4 A500 photos: the DDFSTRT=$10 rows sit exactly
    // ($38-$10)*2 lo-res pixels left of the standard picture, with the
    // early words running through the left border). The placement shift
    // must stay linear below $18 instead of clamping to the hard start.
    let mk = |strt: u16| RenderState {
        ddfstrt: strt,
        ddfstop: 0x00D0,
        diwstrt: 0x2C81,
        diwstop: 0x2CC1,
        ..blank_state()
    };
    assert_eq!(mk(0x38).fetch_origin_native_shift(false, 2), 0);
    // Early-DDF placement is linear (diw1-calibrated) ...
    assert_eq!(mk(0x30).fetch_origin_native_shift(false, 2), 16);
    // ... and keeps the same line below the hard start: 5 words of the
    // $10-anchored row lie left of the standard window edge.
    assert_eq!(mk(0x10).fetch_origin_native_shift(false, 2), 80);
    assert_eq!(mk(0x10).native_x_offset(false, 2), 80);
    assert_eq!(mk(0x10).fetch_start_native_x(false, 2), 0);
}

/// Differential regression oracle for the fast interior run: randomized
/// planes, scroll, palette, collision enables, window edges, priorities,
/// and mid-line control/palette segments, rendered by the production
/// (fast-run) line renderer and the scalar per-pixel oracle, must agree
/// byte for byte in every output the line renderer produces. HAM and
/// dual-playfield cases ride along to prove the fallback stays engaged
/// where the fast path must not.
#[test]
fn fast_playfield_interior_matches_scalar_oracle() {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut rand = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    const BPLCON0_POOL: [u16; 10] = [
        0x1000, 0x2000, 0x3000, 0x4000, 0x5000, 0x6000, // 1-6 planes, lores
        0x9000, 0xC000, // 1 and 4 planes, hires
        0x6400, // dual playfield, 6 planes
        0x5800, // HAM6: the fallback must stay engaged and identical
    ];
    const DIW_POOL: [(u16, u16); 3] = [
        (0x2C81, 0x2CC1), // standard PAL window
        (0x2C91, 0x2CB1), // inset window
        (0x2CA1, 0x2CA9), // very narrow window
    ];
    const BPLCON2_POOL: [u16; 4] = [0x0000, 0x0024, 0x0040, 0x0067];

    for case in 0..400u32 {
        let r = rand();
        let bplcon0 = BPLCON0_POOL[(r as usize) % BPLCON0_POOL.len()];
        let (diwstrt, diwstop) = DIW_POOL[((r >> 8) as usize) % DIW_POOL.len()];
        let control = ControlState {
            dmacon: DMACON_DMAEN | DMACON_BPLEN,
            bplcon0,
            bplcon1: ((r >> 24) & 0xFF) as u16,
            bplcon2: BPLCON2_POOL[((r >> 32) as usize) % BPLCON2_POOL.len()],
            clxcon: (r >> 16) as u16,
            clxcon2: (r >> 40) as u16,
            diwstrt,
            diwstop,
            ddfstrt: 0x0038,
            ddfstop: 0x00D0,
            ..ControlState::default()
        };
        let (x0, x1) = control.display_window_x();
        let x1 = x1.min(FB_WIDTH);

        let words = 10 + ((r >> 48) as usize % 12);
        let fetched_pixels = words * 16;
        let nplanes = control.nplanes().max(1);
        let planes: Vec<Vec<u16>> = (0..nplanes)
            .map(|_| (0..words).map(|_| rand() as u16).collect())
            .collect();
        let mut prepared = Vec::new();
        prepare_planar_row_pixels(&planes, fetched_pixels, &mut prepared);

        // Mid-line writes: a scroll change and a palette write make the
        // renderer split the line into several runs, exercising the
        // interior bounds against segment edges.
        let mut control_segments = Vec::new();
        if r & 0x1000_0000_0000_0000 != 0 {
            let mut changed = control;
            changed.bplcon1 = ((r >> 52) & 0xFF) as u16;
            control_segments.push(ControlSegment {
                x: x0 + 40 + (r as usize % 96),
                control: changed,
            });
        }
        let mut palette_segments = Vec::new();
        if r & 0x2000_0000_0000_0000 != 0 {
            palette_segments.push(PaletteSegment {
                x: x0 + 24 + ((r >> 6) as usize % 128),
                entry: ((r >> 12) & 0x1F) as u8,
                loct: false,
                value: (r >> 44) as u16 & 0x0FFF,
            });
        }

        let mut palette = Palette::new();
        for entry in 0..32 {
            palette.write_ocs(entry, (rand() & 0x0FFF) as u16);
        }
        let suppress = r & 0x4000_0000_0000_0000 != 0;
        let bpl_output_start_x = x0 + [0usize, 6, 34][((r >> 58) as usize) % 3];
        let carried_open_ext_fb = [0usize, 8, 48][((r >> 56) as usize) % 3];

        let plan = DenisePlannedPlayfieldLine::with_prepared_pixels(
            0,
            x0,
            x1,
            &planes,
            &prepared,
            fetched_pixels,
        );

        let render = |fast: bool| {
            let mut fb = vec![0u32; FB_PIXELS];
            let mut pf_mask = vec![0u8; FB_PIXELS];
            let mut collisions = vec![CollisionPixel::default(); FB_PIXELS];
            let mut clxdat = 0u16;
            let f = if fast {
                render_planned_playfield_line
            } else {
                render_planned_playfield_line_scalar
            };
            f(
                &plan,
                &mut fb,
                &mut pf_mask,
                &mut collisions,
                &mut CollisionLookup::new(),
                &mut IndexedOutputCache::default(),
                &mut clxdat,
                palette,
                &palette_segments,
                0,
                control,
                &control_segments,
                0,
                control.bplcon1,
                control.bplcon0,
                suppress,
                bpl_output_start_x,
                carried_open_ext_fb,
                &h_row_for(control),
                PAL_VISIBLE_LINE0,
                0.0,
                0,
            );
            (fb, pf_mask, collisions, clxdat)
        };

        let (fast_fb, fast_pf, fast_col, fast_clx) = render(true);
        let (oracle_fb, oracle_pf, oracle_col, oracle_clx) = render(false);

        assert_eq!(fast_clx, oracle_clx, "case {case}: clxdat diverged");
        assert_eq!(fast_fb, oracle_fb, "case {case}: framebuffer diverged");
        assert_eq!(fast_pf, oracle_pf, "case {case}: playfield mask diverged");
        assert!(
            fast_col
                .iter()
                .map(|c| c.0)
                .eq(oracle_col.iter().map(|c| c.0)),
            "case {case}: collision pixels diverged"
        );
    }
}

/// The oracle above would pass vacuously if the eligibility check always
/// fell back to the scalar loop; a plain 4-plane lores screen must
/// actually produce a fast interior covering most of its window.
#[test]
fn fast_playfield_interior_engages_for_standard_screens() {
    let control = ControlState {
        dmacon: DMACON_DMAEN | DMACON_BPLEN,
        bplcon0: 0x4000,
        diwstrt: 0x2C81,
        diwstop: 0x2CC1,
        ddfstrt: 0x0038,
        ddfstop: 0x00D0,
        ..ControlState::default()
    };
    let (x0, x1) = control.display_window_x();
    let words = 20;
    let fetched_pixels = words * 16;
    let planes: Vec<Vec<u16>> = (0..4).map(|_| vec![0xA55A; words]).collect();
    let mut prepared = Vec::new();
    prepare_planar_row_pixels(&planes, fetched_pixels, &mut prepared);
    let plan = DenisePlannedPlayfieldLine::with_prepared_pixels(
        0,
        x0,
        x1.min(FB_WIDTH),
        &planes,
        &prepared,
        fetched_pixels,
    );

    let pixel_repeat = control.framebuffer_pixel_repeat();
    let diw_h_start = control.diw_h_start();
    let delays = std::array::from_fn(|plane| control.sample_delay_for_plane(plane));
    let interior = fast_playfield_run_interior(
        &plan,
        x0,
        plan.x_stop,
        x0,
        pixel_repeat,
        control.native_samples_per_framebuffer_pixel(),
        control.fetch_start_native_x(diw_h_start, pixel_repeat),
        control.native_x_offset(diw_h_start, pixel_repeat),
        0,
        &delays,
        4,
        false,
        false,
        1,
        true,
        false,
        true,
        true,
        control,
    );
    let (fast_lo, fast_hi, _f0, mask) = interior.expect("standard screen must qualify");
    assert_eq!(mask, 0x0F);
    assert!(
        fast_hi - fast_lo >= (plan.x_stop - x0) / 2,
        "interior {fast_lo}..{fast_hi} should cover most of {x0}..{}",
        plan.x_stop
    );
}
