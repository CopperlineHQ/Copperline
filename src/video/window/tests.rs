// SPDX-License-Identifier: GPL-3.0-or-later

//! Unit tests for the window/presentation layer: split out of
//! `window.rs` for size, they are the same `window::tests` module
//! and keep full access to the parent's private items via `super::`.

use super::ui::{Panel, UiControl};
use super::{
    bar_layout, center_present_frame_for_visible_start, center_present_frame_horizontally,
    control_at, copperline_icon_image, copperline_logo_image, copy_present_frame,
    copy_window_present_frame, draw_status_bar, fdd_track_counter_rect, fdd_track_digit_rect,
    host_shortcut_modifier_pressed, host_to_amiga_rawkey, joystick_mode_uses_keyboard,
    joystick_toggle_rect, keyboard_joystick_key_for, led_row_rect, mask_present_frame_to_tv,
    paint_test_screen, parse_amiga_key, pause_button_rect, power_button_rect, present_height,
    presentation_h_shift_for, presentation_source_y_offset, raw_device_qualifier_family_held,
    raw_device_qualifier_rawkey, rawkey_is_held, rawkey_transition_is_duplicate,
    reboot_button_rect, repeated_main_key_should_drop, rgba, shot_button_rect,
    should_render_emulated_frame, standard_window_top_row, status_with_latched_fdd_track,
    take_integral_mouse_delta, texture_height, texture_width, tv_aperture_source_row,
    tv_source_h_bounds, volume_percent_from_pos, volume_slider_track_rect, BarControl, DriveBar,
    JoystickInputMode, KeyboardJoystickHeld, KeyboardJoystickKey, MediaBar, StatusBarView,
    ToolPanelKind, AMIGA_RAWKEY_LEFT_ALT, AMIGA_RAWKEY_LEFT_SHIFT, AMIGA_RAWKEY_RIGHT_ALT,
    AMIGA_RAWKEY_RIGHT_SHIFT, BUTTON_GLYPH, BUTTON_GLYPH_DISABLED, CD_BODY, CD_LED_OFF, CD_LED_ON,
    DISK_BODY, DISK_BODY_SHADOW, DISK_LABEL, FDD_LED_OFF, FDD_LED_ON, HDD_LED_OFF, HDD_LED_ON,
    POWER_GLYPH_OFF, POWER_GLYPH_ON, POWER_LED_OFF, POWER_LED_ON, STANDARD_PAL_VISIBLE_LINES,
    STANDARD_PAL_VISIBLE_START_VPOS, STATUS_BG, TRACK_SEGMENT_OFF, TRACK_SEGMENT_ON,
    TV_PAL_LIVE_PAD_X, TV_PAL_PRESENT_HEIGHT, TV_PAL_PRESENT_SOURCE_X, TV_PAL_PRESENT_SOURCE_Y,
    TV_PAL_PRESENT_WIDTH, VOLUME_FILL, VOLUME_GLYPH_X,
};
use crate::audio::{AudioSink, NullSink};
use crate::bus::{FrontPanelStatus, RenderRegisterSnapshot};
use crate::config::{Overscan, WarpSpeed};
use crate::video::{FB_HEIGHT, FB_PIXELS, FB_WIDTH};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use winit::event::{ElementState, RawKeyEvent};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

/// A typical session: DF0 connected with a disk in, no CD drive.
fn single_drive_media() -> MediaBar {
    let mut drives = [DriveBar::default(); 4];
    drives[0] = DriveBar {
        connected: true,
        inserted: true,
        multi: false,
    };
    MediaBar { drives, cd: None }
}

fn media(connected: usize, cd: Option<bool>) -> MediaBar {
    let mut drives = [DriveBar::default(); 4];
    for drive in drives.iter_mut().take(connected) {
        *drive = DriveBar {
            connected: true,
            inserted: true,
            multi: false,
        };
    }
    MediaBar { drives, cd }
}

fn view(status: FrontPanelStatus, powered_on: bool, paused: bool) -> StatusBarView {
    StatusBarView {
        status,
        powered_on,
        paused,
        media: single_drive_media(),
        joystick_input_mode: JoystickInputMode::Gamepad,
        hover: None,
    }
}

#[test]
fn host_mapping_includes_amiga_modifiers() {
    assert_eq!(host_to_amiga_rawkey(KeyCode::ControlLeft), Some(0x63));
    assert_eq!(host_to_amiga_rawkey(KeyCode::AltLeft), Some(0x64));
    assert_eq!(host_to_amiga_rawkey(KeyCode::AltRight), Some(0x65));
    assert_eq!(host_to_amiga_rawkey(KeyCode::SuperLeft), Some(0x66));
    assert_eq!(host_to_amiga_rawkey(KeyCode::SuperRight), Some(0x67));
    // The Amiga has no right Ctrl, so host ControlRight doubles as a
    // Right Amiga ($67) alias for keyboards without a right Super key.
    assert_eq!(host_to_amiga_rawkey(KeyCode::ControlRight), Some(0x67));
}

#[test]
fn host_repeat_filter_accepts_unheld_amiga_qualifier_press() {
    let mut held = [false; 128];

    assert!(!repeated_main_key_should_drop(
        &held,
        KeyCode::ShiftRight,
        ElementState::Pressed,
        true,
        false
    ));

    held[AMIGA_RAWKEY_RIGHT_SHIFT as usize] = true;
    assert!(repeated_main_key_should_drop(
        &held,
        KeyCode::ShiftRight,
        ElementState::Pressed,
        true,
        false
    ));

    assert!(repeated_main_key_should_drop(
        &held,
        KeyCode::F12,
        ElementState::Pressed,
        true,
        false
    ));
    assert!(!repeated_main_key_should_drop(
        &held,
        KeyCode::ShiftRight,
        ElementState::Pressed,
        false,
        false
    ));
    assert!(!repeated_main_key_should_drop(
        &held,
        KeyCode::ArrowRight,
        ElementState::Pressed,
        true,
        true
    ));
}

#[test]
fn raw_device_qualifier_filter_is_limited_to_amiga_modifier_lines() {
    assert_eq!(
        raw_device_qualifier_rawkey(KeyCode::ShiftLeft),
        Some(AMIGA_RAWKEY_LEFT_SHIFT)
    );
    assert_eq!(
        raw_device_qualifier_rawkey(KeyCode::ShiftRight),
        Some(AMIGA_RAWKEY_RIGHT_SHIFT)
    );
    assert_eq!(
        raw_device_qualifier_rawkey(KeyCode::AltLeft),
        Some(AMIGA_RAWKEY_LEFT_ALT)
    );
    assert_eq!(
        raw_device_qualifier_rawkey(KeyCode::AltRight),
        Some(AMIGA_RAWKEY_RIGHT_ALT)
    );
    assert_eq!(raw_device_qualifier_rawkey(KeyCode::KeyS), None);
    assert_eq!(raw_device_qualifier_rawkey(KeyCode::ArrowRight), None);
}

#[test]
fn raw_device_qualifier_family_reports_physical_side_state() {
    let mut held = [false; 128];
    assert!(!raw_device_qualifier_family_held(
        &held,
        AMIGA_RAWKEY_LEFT_ALT,
        AMIGA_RAWKEY_RIGHT_ALT
    ));

    held[AMIGA_RAWKEY_LEFT_ALT as usize] = true;
    assert!(raw_device_qualifier_family_held(
        &held,
        AMIGA_RAWKEY_LEFT_ALT,
        AMIGA_RAWKEY_RIGHT_ALT
    ));

    held[AMIGA_RAWKEY_LEFT_ALT as usize] = false;
    held[AMIGA_RAWKEY_RIGHT_ALT as usize] = true;
    assert!(raw_device_qualifier_family_held(
        &held,
        AMIGA_RAWKEY_LEFT_ALT,
        AMIGA_RAWKEY_RIGHT_ALT
    ));
}

#[test]
fn amiga_qualifier_transitions_ignore_duplicate_host_events() {
    let mut held = [false; 128];

    assert!(rawkey_transition_is_duplicate(
        &held,
        AMIGA_RAWKEY_LEFT_SHIFT,
        false
    ));
    assert!(!rawkey_transition_is_duplicate(
        &held,
        AMIGA_RAWKEY_LEFT_SHIFT,
        true
    ));

    held[AMIGA_RAWKEY_LEFT_SHIFT as usize] = true;
    assert!(rawkey_transition_is_duplicate(
        &held,
        AMIGA_RAWKEY_LEFT_SHIFT,
        true
    ));
    assert!(!rawkey_transition_is_duplicate(
        &held,
        AMIGA_RAWKEY_LEFT_SHIFT,
        false
    ));
}

#[test]
fn aggregate_modifier_release_clears_held_amiga_qualifiers() {
    let mut app = test_app();
    for rawkey in [
        AMIGA_RAWKEY_LEFT_SHIFT,
        AMIGA_RAWKEY_RIGHT_SHIFT,
        AMIGA_RAWKEY_LEFT_ALT,
        AMIGA_RAWKEY_RIGHT_ALT,
    ] {
        app.handle_amiga_key_event(rawkey, true);
        assert!(rawkey_is_held(&app.held_rawkeys, rawkey));
    }

    app.update_host_modifiers(ModifiersState::SHIFT);
    assert!(rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_LEFT_SHIFT));
    assert!(rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_RIGHT_SHIFT));
    assert!(!rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_LEFT_ALT));
    assert!(!rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_RIGHT_ALT));

    app.update_host_modifiers(ModifiersState::empty());
    assert!(!rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_LEFT_SHIFT));
    assert!(!rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_RIGHT_SHIFT));
}

#[test]
fn raw_device_alt_hold_blocks_altgr_aggregate_cleanup() {
    let mut app = test_app();
    app.main_window_focused = true;

    app.handle_raw_device_key_event(RawKeyEvent {
        physical_key: PhysicalKey::Code(KeyCode::AltLeft),
        state: ElementState::Pressed,
    });
    app.update_host_modifiers(ModifiersState::ALT);
    assert!(rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_LEFT_ALT));

    app.update_host_modifiers(ModifiersState::empty());
    assert!(rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_LEFT_ALT));

    app.handle_raw_device_key_event(RawKeyEvent {
        physical_key: PhysicalKey::Code(KeyCode::AltRight),
        state: ElementState::Pressed,
    });
    assert!(rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_LEFT_ALT));
    assert!(rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_RIGHT_ALT));

    app.handle_raw_device_key_event(RawKeyEvent {
        physical_key: PhysicalKey::Code(KeyCode::AltLeft),
        state: ElementState::Released,
    });
    assert!(!rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_LEFT_ALT));
    assert!(rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_RIGHT_ALT));
}

#[test]
fn raw_device_release_clears_one_side_while_aggregate_modifier_remains() {
    let mut app = test_app();
    app.main_window_focused = true;

    app.handle_raw_device_key_event(RawKeyEvent {
        physical_key: PhysicalKey::Code(KeyCode::ShiftLeft),
        state: ElementState::Pressed,
    });
    app.handle_raw_device_key_event(RawKeyEvent {
        physical_key: PhysicalKey::Code(KeyCode::ShiftRight),
        state: ElementState::Pressed,
    });
    assert!(rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_LEFT_SHIFT));
    assert!(rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_RIGHT_SHIFT));

    app.handle_raw_device_key_event(RawKeyEvent {
        physical_key: PhysicalKey::Code(KeyCode::ShiftLeft),
        state: ElementState::Released,
    });
    app.update_host_modifiers(ModifiersState::SHIFT);
    assert!(!rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_LEFT_SHIFT));
    assert!(rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_RIGHT_SHIFT));

    app.handle_raw_device_key_event(RawKeyEvent {
        physical_key: PhysicalKey::Code(KeyCode::ShiftRight),
        state: ElementState::Released,
    });
    assert!(!rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_RIGHT_SHIFT));
}

#[test]
fn raw_device_alt_right_respects_keyboard_joystick_ownership() {
    let mut app = test_app();
    app.main_window_focused = true;
    app.set_joystick_input_mode(JoystickInputMode::Keyboard);

    app.handle_raw_device_key_event(RawKeyEvent {
        physical_key: PhysicalKey::Code(KeyCode::AltRight),
        state: ElementState::Pressed,
    });
    assert!(app.keyboard_joy_held.fire_right_alt);
    assert!(!rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_RIGHT_ALT));

    app.handle_raw_device_key_event(RawKeyEvent {
        physical_key: PhysicalKey::Code(KeyCode::AltRight),
        state: ElementState::Released,
    });
    assert!(!app.keyboard_joy_held.fire_right_alt);
    assert!(!rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_RIGHT_ALT));
}

#[test]
fn keyboard_joystick_mapping_matches_fsuae_controls() {
    assert_eq!(
        keyboard_joystick_key_for(KeyCode::ArrowUp),
        Some(KeyboardJoystickKey::Up)
    );
    assert_eq!(
        keyboard_joystick_key_for(KeyCode::ArrowDown),
        Some(KeyboardJoystickKey::Down)
    );
    assert_eq!(
        keyboard_joystick_key_for(KeyCode::ArrowLeft),
        Some(KeyboardJoystickKey::Left)
    );
    assert_eq!(
        keyboard_joystick_key_for(KeyCode::ArrowRight),
        Some(KeyboardJoystickKey::Right)
    );
    assert_eq!(
        keyboard_joystick_key_for(KeyCode::ControlRight),
        Some(KeyboardJoystickKey::FireRightCtrl)
    );
    assert_eq!(
        keyboard_joystick_key_for(KeyCode::AltRight),
        Some(KeyboardJoystickKey::FireRightAlt)
    );
    assert_eq!(
        keyboard_joystick_key_for(KeyCode::KeyC),
        Some(KeyboardJoystickKey::Red)
    );
    assert_eq!(
        keyboard_joystick_key_for(KeyCode::KeyX),
        Some(KeyboardJoystickKey::Blue)
    );
    assert_eq!(
        keyboard_joystick_key_for(KeyCode::KeyD),
        Some(KeyboardJoystickKey::Green)
    );
    assert_eq!(
        keyboard_joystick_key_for(KeyCode::KeyS),
        Some(KeyboardJoystickKey::Yellow)
    );
    assert_eq!(
        keyboard_joystick_key_for(KeyCode::Enter),
        Some(KeyboardJoystickKey::Play)
    );
    assert_eq!(
        keyboard_joystick_key_for(KeyCode::KeyZ),
        Some(KeyboardJoystickKey::Rewind)
    );
    assert_eq!(
        keyboard_joystick_key_for(KeyCode::KeyA),
        Some(KeyboardJoystickKey::Forward)
    );
    assert_eq!(keyboard_joystick_key_for(KeyCode::ControlLeft), None);
}

#[test]
fn keyboard_joystick_fire_aliases_release_independently() {
    let mut held = KeyboardJoystickHeld::default();
    held.set(KeyboardJoystickKey::FireRightCtrl, true);
    held.set(KeyboardJoystickKey::Red, true);
    assert!(held.joystick_state().fire);

    held.set(KeyboardJoystickKey::FireRightCtrl, false);
    assert!(held.joystick_state().fire);

    held.set(KeyboardJoystickKey::Red, false);
    assert!(!held.joystick_state().fire);
}

#[test]
fn joystick_input_mode_toggles_between_two_explicit_modes() {
    // The toggle flips directly between the two modes; there is no hidden
    // auto-detect state, so the keyboard mapping is engaged exactly when
    // (and only when) the mode is Keyboard.
    assert_eq!(
        JoystickInputMode::Gamepad.next(),
        JoystickInputMode::Keyboard
    );
    assert_eq!(
        JoystickInputMode::Keyboard.next(),
        JoystickInputMode::Gamepad
    );

    assert!(joystick_mode_uses_keyboard(JoystickInputMode::Keyboard));
    assert!(!joystick_mode_uses_keyboard(JoystickInputMode::Gamepad));
}

#[test]
fn joystick_toggle_clears_worst_case_media() {
    // The toggle sits at a fixed x just left of the volume glyph. The
    // widest media layout (four floppies plus a CD) must not reach it, and
    // it must stay left of the volume control's hit area.
    let toggle = joystick_toggle_rect();
    let layout = bar_layout(&media(4, Some(true)));
    let media_right = layout
        .cd_eject
        .into_iter()
        .chain(layout.drive_eject.into_iter().flatten())
        .map(|r| r.x + r.w)
        .max()
        .unwrap();
    assert!(
        media_right <= toggle.x,
        "media right edge {media_right} overlaps joystick toggle at {}",
        toggle.x
    );
    assert!(toggle.x + toggle.w <= VOLUME_GLYPH_X);
}

#[test]
fn joystick_toggle_is_hit_tested_and_draws_each_mode() {
    let layout = bar_layout(&single_drive_media());
    let toggle = joystick_toggle_rect();
    let center = (
        (toggle.x + toggle.w / 2) as i32,
        (toggle.y + toggle.h / 2) as i32,
    );
    assert_eq!(control_at(center, &layout), Some(BarControl::Joystick));

    // Each mode lights the green glyph somewhere in the button (gamepad
    // body vs. keyboard keys), so the two states are visually distinct.
    let scale = 1;
    for mode in [JoystickInputMode::Gamepad, JoystickInputMode::Keyboard] {
        let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
        let mut v = view(FrontPanelStatus::default(), true, false);
        v.joystick_input_mode = mode;
        draw_status_bar(&mut frame, &v, scale);
        let lit = (toggle.y..toggle.y + toggle.h).any(|y| {
            (toggle.x..toggle.x + toggle.w)
                .any(|x| pixel(&frame, x, y, scale) == BUTTON_GLYPH.to_le_bytes())
        });
        assert!(lit, "joystick toggle drew no glyph for {mode:?}");
    }
}

#[test]
fn host_shortcut_modifier_uses_platform_convention() {
    #[cfg(target_os = "macos")]
    {
        assert!(host_shortcut_modifier_pressed(ModifiersState::SUPER));
        assert!(!host_shortcut_modifier_pressed(ModifiersState::ALT));
    }

    #[cfg(not(target_os = "macos"))]
    {
        assert!(host_shortcut_modifier_pressed(ModifiersState::ALT));
        assert!(!host_shortcut_modifier_pressed(ModifiersState::SUPER));
    }
}

#[test]
fn named_key_parser_accepts_modifiers_and_raw_codes() {
    assert_eq!(parse_amiga_key("ctrl"), Some(0x63));
    assert_eq!(parse_amiga_key("left-alt"), Some(0x64));
    assert_eq!(parse_amiga_key("ralt"), Some(0x65));
    assert_eq!(parse_amiga_key("lami"), Some(0x66));
    assert_eq!(parse_amiga_key("right-amiga"), Some(0x67));
    assert_eq!(parse_amiga_key("0x04"), Some(0x04));
    assert_eq!(parse_amiga_key("$04"), Some(0x04));
    assert_eq!(parse_amiga_key("unknown-key"), None);
}

#[test]
fn renderer_runs_once_per_emulated_frame() {
    assert!(should_render_emulated_frame(None, 0));
    assert!(!should_render_emulated_frame(Some(12), 12));
    assert!(should_render_emulated_frame(Some(12), 13));
}

#[test]
fn warp_burst_decouples_emulation_from_the_vsync_present() {
    let mut app = test_app();
    // test_app builds an unpaced (warp) emulator. Default warp level is
    // Max: retire many frames per presented frame, bounded by a wall-clock
    // budget so the loop still presents at vsync.
    app.warp_speed = WarpSpeed::Max;
    let (cap, budget) = app.warp_burst_plan(false);
    assert!(cap > 1, "warp Max must skip output frames, got cap {cap}");
    assert!(budget.is_some(), "Max bounds the burst by wall-clock time");

    // A fixed level retires exactly its multiplier in frames, with no time
    // bound -- predictable speed = level x refresh rate.
    app.warp_speed = WarpSpeed::X4;
    assert_eq!(app.warp_burst_plan(false), (4, None));

    // Headless capture renders every frame, so the burst must not engage
    // even though the core is unpaced.
    assert_eq!(app.warp_burst_plan(true), (1, None));

    // Real-time pacing presents one frame per loop regardless of level.
    app.emu.set_paced(true);
    assert_eq!(app.warp_burst_plan(false), (1, None));
}

#[test]
fn cycle_warp_speed_walks_the_levels() {
    let mut app = test_app();
    app.warp_speed = WarpSpeed::X8;
    app.cycle_warp_speed();
    assert_eq!(app.warp_speed, WarpSpeed::X16);
    app.cycle_warp_speed();
    assert_eq!(app.warp_speed, WarpSpeed::Max);
    app.cycle_warp_speed();
    assert_eq!(app.warp_speed, WarpSpeed::X2);
}

#[test]
fn mouse_delta_integrator_keeps_fractional_remainder() {
    let mut delta = 0.75;
    assert_eq!(take_integral_mouse_delta(&mut delta), 0);
    assert_eq!(delta, 0.75);

    delta += 0.5;
    assert_eq!(take_integral_mouse_delta(&mut delta), 1);
    assert_eq!(delta, 0.25);

    delta -= 1.5;
    assert_eq!(take_integral_mouse_delta(&mut delta), -1);
    assert_eq!(delta, -0.25);
}

#[test]
fn status_bar_draws_hdd_led_only_on_ide_machines() {
    let scale = 1;
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    // Row 2 of 3 is where the HDD LED sits on an IDE machine.
    let hdd = led_row_rect(2, 3);

    // No IDE port: the HDD row stays status-bar background.
    draw_status_bar(
        &mut frame,
        &view(FrontPanelStatus::default(), true, false),
        scale,
    );
    assert_eq!(
        pixel(&frame, hdd.x + hdd.w / 2, hdd.y + hdd.h / 2, scale),
        STATUS_BG.to_le_bytes()
    );

    draw_status_bar(
        &mut frame,
        &view(
            FrontPanelStatus {
                hdd_led: Some(false),
                ..FrontPanelStatus::default()
            },
            true,
            false,
        ),
        scale,
    );
    assert_eq!(
        pixel(&frame, hdd.x + hdd.w / 2, hdd.y + hdd.h / 2, scale),
        HDD_LED_OFF.to_le_bytes()
    );

    draw_status_bar(
        &mut frame,
        &view(
            FrontPanelStatus {
                hdd_led: Some(true),
                ..FrontPanelStatus::default()
            },
            true,
            false,
        ),
        scale,
    );
    assert_eq!(
        pixel(&frame, hdd.x + hdd.w / 2, hdd.y + hdd.h / 2, scale),
        HDD_LED_ON.to_le_bytes()
    );
}

#[test]
fn status_bar_draws_power_and_fdd_led_states() {
    let scale = 1;
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    draw_status_bar(
        &mut frame,
        &view(
            FrontPanelStatus {
                power_led_on: true,
                fdd_led_on: false,
                fdd_track: Some(5),
                hdd_led: None,
                cd_led: None,
                output_volume_percent: 100,
            },
            true,
            false,
        ),
        scale,
    );

    let power = led_row_rect(0, 2);
    let fdd = led_row_rect(1, 2);
    assert_eq!(
        pixel(&frame, power.x + power.w / 2, power.y + power.h / 2, scale),
        POWER_LED_ON.to_le_bytes()
    );
    assert_eq!(
        pixel(&frame, fdd.x + fdd.w / 2, fdd.y + fdd.h / 2, scale),
        FDD_LED_OFF.to_le_bytes()
    );
    let hundreds = fdd_track_digit_rect(0);
    let ones = fdd_track_digit_rect(2);
    assert_eq!(
        pixel(
            &frame,
            hundreds.x + hundreds.w / 2,
            hundreds.y + hundreds.h / 2,
            scale
        ),
        TRACK_SEGMENT_OFF.to_le_bytes()
    );
    assert_eq!(
        pixel(&frame, ones.x + ones.w / 2, ones.y + ones.h / 2, scale),
        TRACK_SEGMENT_ON.to_le_bytes()
    );

    draw_status_bar(
        &mut frame,
        &view(
            FrontPanelStatus {
                power_led_on: false,
                fdd_led_on: true,
                fdd_track: Some(42),
                hdd_led: None,
                cd_led: None,
                output_volume_percent: 100,
            },
            true,
            false,
        ),
        scale,
    );
    assert_eq!(
        pixel(&frame, fdd.x + fdd.w / 2, fdd.y + fdd.h / 2, scale),
        FDD_LED_ON.to_le_bytes()
    );
}

#[test]
fn status_bar_extinguishes_power_led_when_host_power_is_off() {
    let scale = 1;
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    draw_status_bar(
        &mut frame,
        &view(
            FrontPanelStatus {
                power_led_on: true,
                fdd_led_on: false,
                fdd_track: Some(0),
                hdd_led: None,
                cd_led: None,
                output_volume_percent: 100,
            },
            false,
            false,
        ),
        scale,
    );

    let power = led_row_rect(0, 2);
    assert_eq!(
        pixel(&frame, power.x + power.w / 2, power.y + power.h / 2, scale),
        POWER_LED_OFF.to_le_bytes()
    );
}

#[test]
fn status_bar_stacks_fdd_led_under_power_led() {
    let power = led_row_rect(0, 2);
    let fdd = led_row_rect(1, 2);
    let track = fdd_track_counter_rect();
    let layout = bar_layout(&single_drive_media());

    assert_eq!(fdd.x, power.x);
    assert_eq!(fdd.w, power.w);
    assert!(fdd.y >= power.y + power.h);
    assert!(track.x >= power.x + power.w);
    assert!(layout.drive_load[0].unwrap().x >= track.x + track.w);
}

#[test]
fn status_bar_power_button_glyph_tracks_power_state() {
    let scale = 1;
    let button = power_button_rect();
    // A pixel squarely on the glyph's vertical bar (button centre
    // column, a few rows above centre), where line coverage is full.
    let gx = button.x + button.w / 2;
    let gy = button.y + button.h / 2 - 3;

    for (powered_on, expected) in [(true, POWER_GLYPH_ON), (false, POWER_GLYPH_OFF)] {
        let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
        draw_status_bar(
            &mut frame,
            &view(
                FrontPanelStatus {
                    power_led_on: powered_on,
                    fdd_led_on: false,
                    fdd_track: Some(0),
                    hdd_led: None,
                    cd_led: None,
                    output_volume_percent: 100,
                },
                powered_on,
                false,
            ),
            scale,
        );
        assert_eq!(pixel(&frame, gx, gy, scale), expected.to_le_bytes());
    }
}

#[test]
fn test_screen_paints_colour_bars_over_a_grey_wedge() {
    use crate::video::{FB_HEIGHT, FB_PIXELS, FB_WIDTH};
    let mut fb = vec![0u32; FB_PIXELS];
    paint_test_screen(&mut fb);

    // Top region: leftmost bar is grey, rightmost is blue.
    assert_eq!(fb[0], rgba(192, 192, 192));
    assert_eq!(fb[FB_WIDTH - 1], rgba(0, 0, 192));

    // Bottom region: grey wedge runs from black at the left to white
    // at the right.
    let bottom = (FB_HEIGHT - 1) * FB_WIDTH;
    assert_eq!(fb[bottom], rgba(0, 0, 0));
    assert_eq!(fb[bottom + FB_WIDTH - 1], rgba(255, 255, 255));
}

#[test]
fn embedded_brand_assets_decode_with_transparent_edges() {
    let logo = copperline_logo_image().expect("embedded logo PNG");
    assert_eq!((logo.width, logo.height), (620, 128));
    assert_eq!(logo.rgba[3], 0);
    assert!(logo.rgba.chunks_exact(4).any(|px| px[3] == 0xFF));

    let icon = copperline_icon_image().expect("embedded icon PNG");
    assert_eq!((icon.width, icon.height), (256, 256));
    assert_eq!(icon.rgba[3], 0);
    assert!(icon.rgba.chunks_exact(4).any(|px| px[3] == 0xFF));
}

#[test]
fn test_screen_blits_copperline_logo_over_colour_bars() {
    let mut fb = vec![0u32; FB_PIXELS];
    paint_test_screen(&mut fb);

    let logo = copperline_logo_image().expect("embedded logo PNG");
    let (idx, px) = logo
        .rgba
        .chunks_exact(4)
        .enumerate()
        .find(|(_, px)| px[3] == 0xFF)
        .expect("opaque logo pixel");
    let x = FB_WIDTH.saturating_sub(logo.width) / 2 + idx % logo.width;
    let y = (FB_HEIGHT * 4 / 5).saturating_sub(logo.height) / 2 + idx / logo.width;

    assert_eq!(
        fb[y * FB_WIDTH + x],
        rgba(px[0] as u32, px[1] as u32, px[2] as u32)
    );
}

#[test]
fn power_button_sits_left_of_reboot_without_overlap() {
    let power = power_button_rect();
    let reboot = reboot_button_rect();
    let volume = volume_slider_track_rect();
    assert!(power.x + power.w <= reboot.x);
    assert!(power.x >= volume.x + volume.w);
    assert_eq!(power.y, reboot.y);
    assert_eq!(power.h, reboot.h);
}

#[test]
fn pause_button_sits_left_of_power_without_overlap() {
    let pause = pause_button_rect();
    let power = power_button_rect();
    let volume = volume_slider_track_rect();
    assert!(pause.x + pause.w <= power.x);
    assert!(pause.x >= volume.x + volume.w);
    assert_eq!(pause.y, power.y);
    assert_eq!(pause.h, power.h);
}

#[test]
fn status_bar_pause_button_glyph_tracks_pause_state() {
    let scale = 1;
    let button = pause_button_rect();
    // Centre column, a few rows above centre: on the play triangle's
    // body when paused and between the pause bars when running.
    let cx = button.x + button.w / 2;
    let cy = button.y + button.h / 2;

    // Paused: a play triangle fills the centre column.
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    draw_status_bar(
        &mut frame,
        &view(
            FrontPanelStatus {
                power_led_on: true,
                fdd_led_on: false,
                fdd_track: Some(0),
                hdd_led: None,
                cd_led: None,
                output_volume_percent: 100,
            },
            true,
            true,
        ),
        scale,
    );
    assert_eq!(pixel(&frame, cx, cy, scale), BUTTON_GLYPH.to_le_bytes());

    // Running: the gap between the two pause bars leaves the centre
    // column on the button face.
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    draw_status_bar(
        &mut frame,
        &view(
            FrontPanelStatus {
                power_led_on: true,
                fdd_led_on: false,
                fdd_track: Some(0),
                hdd_led: None,
                cd_led: None,
                output_volume_percent: 100,
            },
            true,
            false,
        ),
        scale,
    );
    assert_ne!(pixel(&frame, cx, cy, scale), BUTTON_GLYPH.to_le_bytes());
}

#[test]
fn status_bar_draws_disk_image_button_next_to_track_counter() {
    let scale = 1;
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    draw_status_bar(
        &mut frame,
        &view(
            FrontPanelStatus {
                power_led_on: true,
                fdd_led_on: true,
                fdd_track: Some(5),
                hdd_led: None,
                cd_led: None,
                output_volume_percent: 50,
            },
            true,
            false,
        ),
        scale,
    );

    let layout = bar_layout(&single_drive_media());
    let button = layout.drive_load[0].unwrap();
    let track = fdd_track_counter_rect();
    assert!(button.x >= track.x + track.w);
    assert_eq!(
        pixel(&frame, button.x + 5, button.y + 11, scale),
        DISK_BODY.to_le_bytes()
    );
    assert_eq!(
        pixel(&frame, button.x + button.w / 2, button.y + 15, scale),
        DISK_LABEL.to_le_bytes()
    );
    // The drive number 0 is written on the disk label (top-left of
    // the 3x5 digit).
    assert_eq!(
        pixel(&frame, button.x + 12, button.y + 12, scale),
        DISK_BODY_SHADOW.to_le_bytes()
    );
}

#[test]
fn status_bar_draws_swap_and_eject_buttons_with_enable_states() {
    let scale = 1;
    let mut bar = single_drive_media();
    let layout = bar_layout(&bar);
    let swap = layout.drive_swap[0].unwrap();
    let eject = layout.drive_eject[0].unwrap();
    assert!(swap.x >= layout.drive_load[0].unwrap().x + 22);
    assert!(eject.x >= swap.x + swap.w);

    // Single disk, inserted: swap is dim, eject is live.
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    let mut v = view(FrontPanelStatus::default(), true, false);
    draw_status_bar(&mut frame, &v, scale);
    assert_eq!(
        pixel(&frame, swap.x + 5, swap.y + 8, scale),
        BUTTON_GLYPH_DISABLED.to_le_bytes()
    );
    assert_eq!(
        pixel(&frame, eject.x + 5, eject.y + 15, scale),
        BUTTON_GLYPH.to_le_bytes()
    );

    // Playlist queued, no disk in: swap is live, eject is dim.
    bar.drives[0].multi = true;
    bar.drives[0].inserted = false;
    v.media = bar;
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    draw_status_bar(&mut frame, &v, scale);
    assert_eq!(
        pixel(&frame, swap.x + 5, swap.y + 8, scale),
        BUTTON_GLYPH.to_le_bytes()
    );
    assert_eq!(
        pixel(&frame, eject.x + 5, eject.y + 15, scale),
        BUTTON_GLYPH_DISABLED.to_le_bytes()
    );
}

#[test]
fn status_bar_draws_cd_buttons_only_on_cd_machines() {
    assert!(bar_layout(&media(1, None)).cd_load.is_none());
    assert!(bar_layout(&media(1, None)).cd_eject.is_none());

    let bar = media(1, Some(true));
    let layout = bar_layout(&bar);
    let cd_load = layout.cd_load.unwrap();
    let cd_eject = layout.cd_eject.unwrap();
    assert!(cd_load.x >= layout.drive_eject[0].unwrap().x + 16);
    assert!(cd_eject.x >= cd_load.x + cd_load.w);

    let scale = 1;
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    let v = StatusBarView {
        status: FrontPanelStatus::default(),
        powered_on: true,
        paused: false,
        media: bar,
        joystick_input_mode: JoystickInputMode::Gamepad,
        hover: None,
    };
    draw_status_bar(&mut frame, &v, scale);
    // The disc body below the hub.
    assert_eq!(
        pixel(&frame, cd_load.x + 11, cd_load.y + 17, scale),
        CD_BODY.to_le_bytes()
    );
    assert_eq!(
        pixel(&frame, cd_eject.x + 5, cd_eject.y + 15, scale),
        BUTTON_GLYPH.to_le_bytes()
    );
}

#[test]
fn status_bar_draws_cd_led_on_cd_machines() {
    let scale = 1;

    // CDTV/CD32 without IDE: rows are PWR, FDD, CD at the classic
    // three-row spacing.
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    let mut v = view(
        FrontPanelStatus {
            cd_led: Some(true),
            ..FrontPanelStatus::default()
        },
        true,
        false,
    );
    v.media = media(1, Some(true));
    draw_status_bar(&mut frame, &v, scale);
    let cd = led_row_rect(2, 3);
    assert_eq!(
        pixel(&frame, cd.x + cd.w / 2, cd.y + cd.h / 2, scale),
        CD_LED_ON.to_le_bytes()
    );

    // With IDE as well, all four rows pack tighter and the CD LED is
    // the last row, still inside the bar.
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    let mut v = view(
        FrontPanelStatus {
            hdd_led: Some(false),
            cd_led: Some(false),
            ..FrontPanelStatus::default()
        },
        true,
        false,
    );
    v.media = media(1, Some(true));
    draw_status_bar(&mut frame, &v, scale);
    let cd = led_row_rect(3, 4);
    assert!(cd.y + cd.h <= present_height() + super::STATUS_BAR_HEIGHT);
    assert_eq!(
        pixel(&frame, cd.x + cd.w / 2, cd.y + cd.h / 2, scale),
        CD_LED_OFF.to_le_bytes()
    );

    // No CD drive: the CD row is absent and that area stays bar
    // background.
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    draw_status_bar(
        &mut frame,
        &view(FrontPanelStatus::default(), true, false),
        scale,
    );
    let cd = led_row_rect(2, 3);
    assert_eq!(
        pixel(&frame, cd.x + cd.w / 2, cd.y + cd.h / 2, scale),
        STATUS_BG.to_le_bytes()
    );
}

#[test]
fn bar_layout_stacks_three_or_more_drives_two_up() {
    // One or two drives sit in a single full-height row.
    let flat = bar_layout(&media(2, Some(true)));
    let df0 = flat.drive_load[0].unwrap();
    let df1 = flat.drive_load[1].unwrap();
    assert_eq!(df0.y, df1.y);
    assert_eq!(df0.h, super::STATUS_CONTROL_H);

    // Three or four drives stack in a two-column grid: DF2 sits
    // below DF0 in shorter buttons.
    let stacked = bar_layout(&media(4, Some(true)));
    let df0 = stacked.drive_load[0].unwrap();
    let df1 = stacked.drive_load[1].unwrap();
    let df2 = stacked.drive_load[2].unwrap();
    let df3 = stacked.drive_load[3].unwrap();
    assert_eq!(df0.y, df1.y);
    assert_eq!(df2.y, df3.y);
    assert_eq!(df0.x, df2.x);
    assert_eq!(df1.x, df3.x);
    assert!(df2.y >= df0.y + df0.h);
    assert!(df0.h < super::STATUS_CONTROL_H);
    // The grid clears the track counter on the left and the volume
    // control on the right, CD cluster included.
    assert!(df0.x >= fdd_track_counter_rect().x + fdd_track_counter_rect().w);
    let cd_eject = stacked.cd_eject.unwrap();
    assert!(cd_eject.x + cd_eject.w <= VOLUME_GLYPH_X);
    // Stacked buttons stay inside the status bar.
    assert!(df2.y + df2.h <= present_height() + super::STATUS_BAR_HEIGHT);
}

#[test]
fn control_at_maps_media_and_screenshot_buttons() {
    let layout = bar_layout(&media(2, Some(false)));
    let centre = |r: super::Rect| ((r.x + r.w / 2) as i32, (r.y + r.h / 2) as i32);

    assert_eq!(
        control_at(centre(layout.drive_load[0].unwrap()), &layout),
        Some(BarControl::DriveLoad(0))
    );
    assert_eq!(
        control_at(centre(layout.drive_swap[1].unwrap()), &layout),
        Some(BarControl::DriveSwap(1))
    );
    assert_eq!(
        control_at(centre(layout.drive_eject[1].unwrap()), &layout),
        Some(BarControl::DriveEject(1))
    );
    assert_eq!(
        control_at(centre(layout.cd_load.unwrap()), &layout),
        Some(BarControl::CdLoad)
    );
    assert_eq!(
        control_at(centre(layout.cd_eject.unwrap()), &layout),
        Some(BarControl::CdEject)
    );
    assert_eq!(
        control_at(centre(shot_button_rect()), &layout),
        Some(BarControl::Screenshot)
    );
    assert_eq!(
        control_at(centre(pause_button_rect()), &layout),
        Some(BarControl::Pause)
    );
    // No drive 2 connected: the space right of its would-be cluster
    // is empty bar.
    assert_eq!(control_at((2, 2), &layout), None);
}

#[test]
fn status_bar_draws_volume_control_and_maps_pointer_position() {
    let scale = 1;
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    draw_status_bar(
        &mut frame,
        &view(
            FrontPanelStatus {
                power_led_on: true,
                fdd_led_on: true,
                fdd_track: Some(5),
                hdd_led: None,
                cd_led: None,
                output_volume_percent: 50,
            },
            true,
            false,
        ),
        scale,
    );

    let track = volume_slider_track_rect();
    assert_eq!(volume_percent_from_pos((track.x as i32, track.y as i32)), 0);
    assert_eq!(
        volume_percent_from_pos(((track.x + track.w - 1) as i32, track.y as i32)),
        100
    );
    assert_eq!(
        pixel(&frame, track.x + track.w / 4, track.y + track.h / 2, scale),
        VOLUME_FILL.to_le_bytes()
    );
}

#[test]
fn status_bar_latches_fdd_track_when_no_drive_is_selected() {
    let mut last_fdd_track = None;
    let status = status_with_latched_fdd_track(
        FrontPanelStatus {
            power_led_on: true,
            fdd_led_on: true,
            fdd_track: Some(42),
            hdd_led: None,
            cd_led: None,
            output_volume_percent: 100,
        },
        &mut last_fdd_track,
    );
    assert_eq!(status.fdd_track, Some(42));
    assert_eq!(last_fdd_track, Some(42));

    let status = status_with_latched_fdd_track(
        FrontPanelStatus {
            power_led_on: true,
            fdd_led_on: false,
            fdd_track: None,
            hdd_led: None,
            cd_led: None,
            output_volume_percent: 100,
        },
        &mut last_fdd_track,
    );
    assert_eq!(status.fdd_track, Some(42));
}

#[test]
fn status_bar_draws_at_hidpi_texture_scale() {
    let scale = 2;
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];

    draw_status_bar(
        &mut frame,
        &view(
            FrontPanelStatus {
                power_led_on: true,
                fdd_led_on: true,
                fdd_track: Some(159),
                hdd_led: None,
                cd_led: None,
                output_volume_percent: 100,
            },
            true,
            true,
        ),
        scale,
    );

    let power = led_row_rect(0, 2);
    assert_eq!(
        pixel(
            &frame,
            (power.x + power.w / 2) * scale,
            (power.y + power.h / 2) * scale,
            scale
        ),
        POWER_LED_ON.to_le_bytes()
    );
    let ones = fdd_track_digit_rect(2);
    assert_eq!(
        pixel(
            &frame,
            (ones.x + ones.w / 2) * scale,
            (ones.y + ones.h / 2) * scale,
            scale
        ),
        TRACK_SEGMENT_ON.to_le_bytes()
    );
}

#[test]
fn present_frame_copy_scales_texture_rows_at_hidpi() {
    use crate::video::deinterlace::{OUT_HEIGHT, OUT_PIXELS};
    let scale = 2;
    let mut src = vec![0u32; OUT_PIXELS];
    src[0] = 0x1122_3344;
    src[1] = 0x5566_7788;
    src[(OUT_HEIGHT - 1) * FB_WIDTH] = 0xAABB_CCDD;
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];

    copy_present_frame(&src, OUT_HEIGHT, &mut frame, scale);

    // The top output row samples the top source row exactly (the
    // centre-aligned position clamps at the edge), and horizontal
    // duplication carries each source pixel across the HiDPI pair.
    assert_eq!(pixel(&frame, 0, 0, scale), src[0].to_le_bytes());
    assert_eq!(pixel(&frame, 1, 0, scale), src[0].to_le_bytes());
    assert_eq!(pixel(&frame, 2, 0, scale), src[1].to_le_bytes());
    // The bottom output row resolves to the last woven source line.
    assert_eq!(
        pixel(&frame, 0, present_height() * scale - 1, scale),
        src[(OUT_HEIGHT - 1) * FB_WIDTH].to_le_bytes()
    );
}

#[test]
fn tv_window_copy_centres_reference_aperture_in_live_texture() {
    use crate::video::deinterlace::{OUT_HEIGHT, OUT_PIXELS};
    let scale = 1;
    let mut src = vec![0u32; OUT_PIXELS];
    let row_y = TV_PAL_PRESENT_SOURCE_Y;
    let standard_left = crate::video::bitplane::STANDARD_VISIBLE_X0;
    let standard_right = standard_left + 320 * 2 - 1;
    let left_marker = 0x1122_3344u32;
    let right_marker = 0x5566_7788u32;
    let left_edge = 0x99AA_BBCCu32;
    let right_edge = 0xDDEE_FF00u32;

    src[row_y * FB_WIDTH + TV_PAL_PRESENT_SOURCE_X] = left_edge;
    src[row_y * FB_WIDTH + FB_WIDTH - 1] = right_edge;
    src[row_y * FB_WIDTH + standard_left] = left_marker;
    src[row_y * FB_WIDTH + standard_right] = right_marker;

    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    copy_window_present_frame(&src, OUT_HEIGHT, &mut frame, scale, Overscan::Tv, true);

    let dst_standard_left = TV_PAL_LIVE_PAD_X + (standard_left - TV_PAL_PRESENT_SOURCE_X);
    let dst_standard_right = dst_standard_left + 320 * 2 - 1;
    assert_eq!(dst_standard_left, FB_WIDTH - 1 - dst_standard_right);
    assert_eq!(
        pixel(&frame, dst_standard_left, 0, scale),
        left_marker.to_le_bytes()
    );
    assert_eq!(
        pixel(&frame, dst_standard_right, 0, scale),
        right_marker.to_le_bytes()
    );
    assert_eq!(pixel(&frame, 0, 0, scale), left_edge.to_le_bytes());
    assert_eq!(
        pixel(&frame, FB_WIDTH - 1, 0, scale),
        right_edge.to_le_bytes()
    );
}

#[test]
fn tv_window_copy_preserves_true_overscan_fetches() {
    use crate::video::deinterlace::{OUT_HEIGHT, OUT_PIXELS};
    let scale = 1;
    let mut src = vec![0u32; OUT_PIXELS];
    let left_overscan = 0x1122_3344u32;
    let standard_crop_edge = 0x5566_7788u32;

    src[0] = left_overscan;
    src[TV_PAL_PRESENT_SOURCE_X] = standard_crop_edge;

    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    copy_window_present_frame(&src, OUT_HEIGHT, &mut frame, scale, Overscan::Tv, false);

    assert_eq!(pixel(&frame, 0, 0, scale), left_overscan.to_le_bytes());
    assert_ne!(pixel(&frame, 0, 0, scale), standard_crop_edge.to_le_bytes());
}

#[test]
fn square_pixel_canvas_maps_woven_rows_one_to_one() {
    use crate::video::deinterlace::OUT_HEIGHT;
    use crate::video::PRESENT_HEIGHT_SQUARE;
    // The square-pixel canvas is exactly the woven field: every
    // scanline is one output row, so a standard 320x256 PAL display
    // occupies precisely 640x512 output pixels.
    assert_eq!(PRESENT_HEIGHT_SQUARE, OUT_HEIGHT);
    for y in 0..OUT_HEIGHT {
        assert_eq!(
            crate::screenshot::scaled_source_row(y, OUT_HEIGHT, PRESENT_HEIGHT_SQUARE),
            y
        );
    }
}

#[test]
fn tv_aperture_row_mapping_pads_square_bezel_and_covers_43() {
    use crate::video::{PRESENT_HEIGHT_SQUARE, PRESENT_HEIGHT_TV};
    // 4:3 canvas: no bezel rows; the 540 aperture rows map onto all
    // 537 output rows exactly as before the square-pixel option.
    assert_eq!(tv_aperture_source_row(0, PRESENT_HEIGHT_TV, 1), Some(0));
    assert_eq!(
        tv_aperture_source_row(PRESENT_HEIGHT_TV - 1, PRESENT_HEIGHT_TV, 1),
        Some(TV_PAL_PRESENT_HEIGHT - 1)
    );
    // Square canvas: black bezel bands centre the aperture and its
    // rows map 1:1.
    let pad = (PRESENT_HEIGHT_SQUARE - TV_PAL_PRESENT_HEIGHT) / 2;
    assert_eq!(tv_aperture_source_row(0, PRESENT_HEIGHT_SQUARE, 1), None);
    assert_eq!(
        tv_aperture_source_row(pad - 1, PRESENT_HEIGHT_SQUARE, 1),
        None
    );
    assert_eq!(
        tv_aperture_source_row(pad, PRESENT_HEIGHT_SQUARE, 1),
        Some(0)
    );
    assert_eq!(
        tv_aperture_source_row(pad + TV_PAL_PRESENT_HEIGHT - 1, PRESENT_HEIGHT_SQUARE, 1),
        Some(TV_PAL_PRESENT_HEIGHT - 1)
    );
    assert_eq!(
        tv_aperture_source_row(pad + TV_PAL_PRESENT_HEIGHT, PRESENT_HEIGHT_SQUARE, 1),
        None
    );
    // HiDPI: bezel and 1:1 mapping scale with the texture factor.
    assert_eq!(
        tv_aperture_source_row(2 * pad - 1, PRESENT_HEIGHT_SQUARE, 2),
        None
    );
    assert_eq!(
        tv_aperture_source_row(2 * pad, PRESENT_HEIGHT_SQUARE, 2),
        Some(0)
    );
    assert_eq!(
        tv_aperture_source_row(2 * pad + 1, PRESENT_HEIGHT_SQUARE, 2),
        Some(0)
    );
    assert_eq!(
        tv_aperture_source_row(2 * pad + 2, PRESENT_HEIGHT_SQUARE, 2),
        Some(1)
    );
}

#[test]
fn present_row_selection_covers_every_source_line_at_hidpi() {
    use crate::video::deinterlace::OUT_HEIGHT;
    // The live window writes a HiDPI texture before the OS compositor
    // scales it. At that output size, every woven source row should be
    // represented by one or more whole texture rows without mixing
    // neighbouring Amiga scanlines.
    let out_rows = present_height() * 2;
    let mut hits = vec![0usize; OUT_HEIGHT];
    let mut prev = 0usize;
    for y in 0..out_rows {
        let src_y = crate::screenshot::scaled_source_row(y, OUT_HEIGHT, out_rows);
        assert!(src_y < OUT_HEIGHT);
        assert!(src_y >= prev);
        hits[src_y] += 1;
        prev = src_y;
    }
    for (y, count) in hits.iter().enumerate() {
        assert!(
            *count > 0,
            "source row {y} dropped from the presentation entirely"
        );
        assert!(
            *count <= 2,
            "source row {y} has unexpectedly thick presentation coverage: {count}"
        );
    }
}

#[test]
fn standard_pal_frames_get_vertical_presentation_margin() {
    let standard_offset = presentation_source_y_offset(STANDARD_PAL_VISIBLE_START_VPOS);

    assert!(standard_offset > 0);
    assert_eq!(
        presentation_source_y_offset(STANDARD_PAL_VISIBLE_START_VPOS - standard_offset as u32),
        0
    );
}

#[test]
fn horizontal_centering_shifts_left_and_blacks_the_right() {
    let mut fb = vec![rgba(0, 0, 0); FB_PIXELS];
    let marker = rgba(0x12, 0x34, 0x56);
    // A marker 30px in on the first row, and one at the right edge.
    fb[30] = marker;
    fb[FB_WIDTH - 1] = rgba(0x99, 0x88, 0x77);

    center_present_frame_horizontally(&mut fb, 26);

    // Content moved left by 26: x=30 -> x=4.
    assert_eq!(fb[4], marker);
    assert_eq!(fb[30], rgba(0, 0, 0));
    // The right 26 columns are blacked out.
    for x in (FB_WIDTH - 26)..FB_WIDTH {
        assert_eq!(fb[x], rgba(0, 0, 0));
    }
}

#[test]
fn horizontal_centering_is_a_noop_for_zero_shift() {
    let mut fb = vec![rgba(0, 0, 0); FB_PIXELS];
    let marker = rgba(1, 2, 3);
    fb[100] = marker;
    center_present_frame_horizontally(&mut fb, 0);
    assert_eq!(fb[100], marker);
}

#[test]
fn tv_presentation_keeps_standard_hires_framebuffer_origin() {
    let snapshot = RenderRegisterSnapshot {
        bplcon0: 0x0200,
        diwstrt: 0x0581,
        diwstop: 0x40C1,
        ddfstrt: 0x003C,
        ddfstop: 0x00D0,
        ..RenderRegisterSnapshot::default()
    };

    assert_eq!(presentation_h_shift_for(&snapshot, Overscan::Tv), 0);
}

#[test]
fn tv_pal_crop_centres_standard_display_in_aperture() {
    let standard_left = crate::video::bitplane::STANDARD_VISIBLE_X0;
    let standard_right = standard_left + 640;

    assert_eq!(TV_PAL_PRESENT_WIDTH, 692);
    assert_eq!(TV_PAL_PRESENT_HEIGHT, 540);
    assert_eq!(standard_left - TV_PAL_PRESENT_SOURCE_X, 26);
    assert_eq!(
        TV_PAL_PRESENT_WIDTH - (standard_right - TV_PAL_PRESENT_SOURCE_X),
        26
    );
    assert_eq!(TV_PAL_PRESENT_SOURCE_Y, 18);
}

#[test]
fn standard_pal_frame_centering_preserves_horizontal_margin() {
    let offset = presentation_source_y_offset(STANDARD_PAL_VISIBLE_START_VPOS);
    let mut fb = vec![rgba(0, 0, 0); FB_PIXELS];
    let marker = rgba(0x12, 0x34, 0x56);
    fb[32] = marker;

    center_present_frame_for_visible_start(&mut fb, STANDARD_PAL_VISIBLE_START_VPOS);

    assert_eq!(fb[0], rgba(0, 0, 0));
    assert_eq!(fb[(offset * FB_WIDTH) + 31], rgba(0, 0, 0));
    assert_eq!(fb[(offset * FB_WIDTH) + 32], marker);
}

#[test]
fn tv_overscan_mask_blacks_margins_and_keeps_the_tv_window() {
    let marker = rgba(0x12, 0x34, 0x56);
    let mut fb = vec![marker; FB_PIXELS];

    // A standard screen after vertical presentation: window top at
    // framebuffer row 14 (the standard centring offset), with the TV
    // aperture anchored to the emulated framebuffer.
    let std_top = standard_window_top_row(STANDARD_PAL_VISIBLE_START_VPOS);
    let shift = 0;
    mask_present_frame_to_tv(&mut fb, shift, std_top);

    let (source_left, source_right) = tv_source_h_bounds();
    let left = source_left - shift;
    let right = source_right - shift;
    let mid_row = std_top + 100;
    assert_eq!(right, FB_WIDTH);
    assert_eq!(fb[mid_row * FB_WIDTH + left - 1], rgba(0, 0, 0));
    assert_eq!(fb[mid_row * FB_WIDTH + left], marker);
    assert_eq!(fb[mid_row * FB_WIDTH + right - 1], marker);
    assert_eq!(fb[mid_row * FB_WIDTH + FB_WIDTH - 1], marker);
    // The deep left overscan margin stays hidden.
    assert_eq!(fb[mid_row * FB_WIDTH], rgba(0, 0, 0));
    // Vertical border rows remain visible; the TV mask only hides the
    // deep horizontal margins.
    assert_eq!(fb[(std_top - 1) * FB_WIDTH + left - 1], rgba(0, 0, 0));
    assert_eq!(fb[(std_top - 1) * FB_WIDTH + left], marker);
    let bottom = std_top + STANDARD_PAL_VISIBLE_LINES - 1;
    assert_eq!(fb[bottom * FB_WIDTH + left - 1], rgba(0, 0, 0));
    assert_eq!(fb[bottom * FB_WIDTH + left], marker);
}

#[test]
fn tv_mask_preserves_visible_left_overscan_margin_without_shifting() {
    let std_top = standard_window_top_row(STANDARD_PAL_VISIBLE_START_VPOS);
    let mid_row = std_top + 100;
    let (source_left, _) = tv_source_h_bounds();
    let marker = rgba(0x12, 0x34, 0x56);
    let hidden_marker = rgba(0x98, 0x76, 0x54);
    let mut fb = vec![rgba(0, 0, 0); FB_PIXELS];

    fb[mid_row * FB_WIDTH + source_left - 1] = hidden_marker;
    fb[mid_row * FB_WIDTH + source_left] = marker;

    mask_present_frame_to_tv(&mut fb, 0, std_top);

    assert_eq!(fb[mid_row * FB_WIDTH + source_left - 1], rgba(0, 0, 0));
    assert_eq!(fb[mid_row * FB_WIDTH + source_left], marker);
}

#[test]
fn tv_overscan_mask_tracks_the_centering_shift() {
    let marker = rgba(0x12, 0x34, 0x56);
    let mut fb = vec![marker; FB_PIXELS];

    // A standard display shifted left 16px for centring: the bezel
    // moves with it, so the window's left edge is not clipped.
    let std_top = standard_window_top_row(STANDARD_PAL_VISIBLE_START_VPOS);
    mask_present_frame_to_tv(&mut fb, 16, std_top);

    let (source_left, source_right) = tv_source_h_bounds();
    let left = source_left - 16;
    let right = source_right - 16;
    let mid_row = std_top + 100;
    assert_eq!(fb[mid_row * FB_WIDTH + left - 1], rgba(0, 0, 0));
    assert_eq!(fb[mid_row * FB_WIDTH + left], marker);
    assert_eq!(fb[mid_row * FB_WIDTH + right - 1], marker);
    assert_eq!(fb[mid_row * FB_WIDTH + right], rgba(0, 0, 0));
    assert_eq!(fb[mid_row * FB_WIDTH + FB_WIDTH - 1], rgba(0, 0, 0));
}

#[test]
fn tv_overscan_mask_preserves_vertical_border_rows() {
    let marker = rgba(0x12, 0x34, 0x56);
    let mut fb = vec![marker; FB_PIXELS];
    let std_top = standard_window_top_row(STANDARD_PAL_VISIBLE_START_VPOS);
    let shift = 0;
    let (source_left, source_right) = tv_source_h_bounds();
    let left = source_left - shift;
    let right = source_right - shift;
    let bottom = std_top + STANDARD_PAL_VISIBLE_LINES - 1;

    mask_present_frame_to_tv(&mut fb, shift, std_top);

    assert_eq!(right, FB_WIDTH);
    assert_eq!(fb[bottom * FB_WIDTH + left - 1], rgba(0, 0, 0));
    assert_eq!(fb[bottom * FB_WIDTH + left], marker);
    assert_eq!(fb[bottom * FB_WIDTH + right - 1], marker);
}

#[test]
fn tv_overscan_mask_tracks_overscan_visible_starts() {
    // A deep-overscan frame (visible start above the standard window):
    // the centring shift is consumed, the standard window sits lower in
    // the framebuffer, and the TV window follows it down.
    let visible_start = STANDARD_PAL_VISIBLE_START_VPOS - 16;
    let std_top = standard_window_top_row(visible_start);
    assert_eq!(std_top, 16);

    let marker = rgba(0x12, 0x34, 0x56);
    let mut fb = vec![marker; FB_PIXELS];
    mask_present_frame_to_tv(&mut fb, 0, std_top);

    let (left, _) = tv_source_h_bounds();
    assert_eq!(
        fb[std_top.saturating_sub(1) * FB_WIDTH + left - 1],
        rgba(0, 0, 0)
    );
    assert_eq!(fb[std_top.saturating_sub(1) * FB_WIDTH + left], marker);
    let bottom = std_top + STANDARD_PAL_VISIBLE_LINES - 1;
    assert_eq!(fb[bottom * FB_WIDTH + left - 1], rgba(0, 0, 0));
    assert_eq!(fb[bottom * FB_WIDTH + left], marker);
}

#[test]
fn reboot_button_hit_rect_is_bounded_to_status_bar_button() {
    let button = reboot_button_rect();

    assert!(button.contains(((button.x + 1) as i32, (button.y + 1) as i32)));
    assert!(button.contains((
        (button.x + button.w - 1) as i32,
        (button.y + button.h - 1) as i32
    )));
    assert!(!button.contains(((button.x - 1) as i32, button.y as i32)));
    assert!(!button.contains((button.x as i32, (button.y - 1) as i32)));
}

fn pixel(frame: &[u8], x: usize, y: usize, scale: usize) -> [u8; 4] {
    frame[(y * texture_width(scale) + x) * 4..(y * texture_width(scale) + x) * 4 + 4]
        .try_into()
        .unwrap()
}

/// An interactive App around a minimal machine: a NOP-sled ROM with
/// reset vectors pointing into it, no audio, unpaced. Lets the
/// debugger window's actions and view builders run against the real
/// emulator without a host window.
fn test_app() -> super::App {
    test_app_with_audio(Box::new(NullSink))
}

fn test_app_with_audio(audio: Box<dyn AudioSink>) -> super::App {
    use crate::chipset::paula::Paula;
    use crate::config::{CpuModel, PacingBudget};
    use crate::emulator::Emulator;
    use crate::floppy::FloppyController;
    use crate::memory::{Memory, ROM_BASE, ROM_SIZE};
    use crate::serial::StdoutSink;

    let mut rom = vec![0u8; ROM_SIZE];
    let pc = ROM_BASE as u32 + 8;
    rom[0..4].copy_from_slice(&0x0007_FFFEu32.to_be_bytes());
    rom[4..8].copy_from_slice(&pc.to_be_bytes());
    // NOP sled for the rest of the test program.
    for word in rom[8..4096].chunks_exact_mut(2) {
        word.copy_from_slice(&0x4E71u16.to_be_bytes());
    }
    let mem = Memory {
        chip_ram: vec![0u8; 512 * 1024],
        slow_ram: Vec::new(),
        rom,
        overlay: true,
        zorro: crate::zorro::ZorroChain::default(),
        extended_rom: Vec::new(),
        extended_rom_base: 0,
        wcs: Vec::new(),
        wcs_write_protected: false,
    };
    let bus = crate::bus::Bus::new(
        mem,
        Paula::new(Box::new(StdoutSink::new()), audio),
        FloppyController::default(),
    );
    let emu = Emulator::new(bus, CpuModel::M68000, false, PacingBudget::Cycles, 2, false)
        .expect("test emulator");
    super::App::new(
        emu,
        true,
        None,
        None,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
        std::array::from_fn(|_| Vec::new()),
        [true; 4],
        crate::config::Overscan::Full,
        0.0,
        crate::config::WarpSpeed::Max,
        crate::config::JoystickInputMode::Gamepad,
        vec!["Machine: test".to_string()],
        crate::config::RawConfig::default(),
    )
}

#[test]
fn launcher_panel_edits_machine_setup() {
    use crate::config::MachineModel;
    use crate::video::launcher::{LauncherField, LauncherTab};

    let mut app = test_app();
    app.open_launcher();
    assert!(matches!(app.ui.panel, Some(Panel::Launcher(_))));

    // Pick a machine, switch tabs, and flip a toggle through the same
    // control dispatch the mouse uses.
    app.activate_ui_control(UiControl::LauncherModel(MachineModel::A1200));
    app.activate_ui_control(UiControl::LauncherTab(LauncherTab::Cpu));
    app.activate_ui_control(UiControl::LauncherToggle(LauncherField::Fpu));

    let state = match &app.ui.panel {
        Some(Panel::Launcher(state)) => state,
        _ => panic!("launcher closed unexpectedly"),
    };
    assert_eq!(state.setup.model(), Some(MachineModel::A1200));
    assert_eq!(state.tab, LauncherTab::Cpu);
    // The A1200's profile defaults (AGA, EC020, 2M chip) plus the FPU we
    // toggled on are what a save would emit.
    let raw = state.setup.to_raw();
    assert_eq!(raw.machine.profile.as_deref(), Some("A1200"));
    assert_eq!(raw.cpu.fpu, Some(true));
    assert!(state.setup.build_config().is_ok());
}

#[test]
fn launcher_run_keeps_panel_open_on_error() {
    use crate::video::launcher::LauncherField;

    let mut app = test_app();
    app.powered_on = false;
    app.open_launcher();
    // A floppy image that does not exist fails config validation.
    if let Some(Panel::Launcher(state)) = app.ui.panel.as_mut() {
        state
            .setup
            .set_path(LauncherField::Df0Image, PathBuf::from("/no/such/disk.adf"));
    }
    app.launcher_run();
    match &app.ui.panel {
        Some(Panel::Launcher(state)) => assert!(
            state.status.as_ref().is_some_and(|s| s.error),
            "expected an error status to keep the user in the launcher"
        ),
        _ => panic!("launcher should stay open on a validation error"),
    }
    assert!(
        !app.powered_on,
        "a failed Run must not power the machine on"
    );
}

#[test]
fn state_load_closes_launcher_and_powers_restored_machine() {
    let path = std::env::temp_dir().join(format!(
        "copperline-launcher-state-load-{}.clstate",
        std::process::id()
    ));
    let mut app = test_app();
    app.emu.save_state(&path).expect("save test state");

    app.power_off();
    let parked_present = app.present_fb.clone();
    app.open_launcher();
    assert!(matches!(app.ui.panel, Some(Panel::Launcher(_))));

    assert!(app.load_state_from_path(&path));
    assert!(app.ui.panel.is_none(), "state load should dismiss launcher");
    assert!(app.powered_on);
    assert!(!app.cpu_halted);
    assert!(
        app.present_fb == parked_present,
        "load itself should not invent a rendered frame"
    );

    for _ in 0..3 {
        app.emu.step_frame().expect("step restored frame");
        if app.finish_render_for_current_frame() {
            break;
        }
    }
    assert_ne!(
        app.present_fb, parked_present,
        "restored machine should render over the parked test screen"
    );

    let _ = std::fs::remove_file(&path);
}

struct SuspensionSink {
    states: Rc<RefCell<Vec<bool>>>,
}

impl AudioSink for SuspensionSink {
    fn push(&mut self, _left: f32, _right: f32) {}

    fn flush(&mut self) {}

    fn set_live_output_suspended(&mut self, suspended: bool) {
        self.states.borrow_mut().push(suspended);
    }
}

#[test]
fn host_pause_states_suspend_live_audio_output() {
    let states = Rc::new(RefCell::new(Vec::new()));
    let mut app = test_app_with_audio(Box::new(SuspensionSink {
        states: Rc::clone(&states),
    }));

    app.toggle_pause();
    assert_eq!(states.borrow().last(), Some(&true));
    app.toggle_pause();
    assert_eq!(states.borrow().last(), Some(&false));

    app.power_off();
    assert_eq!(states.borrow().last(), Some(&true));
    app.toggle_power();
    assert_eq!(states.borrow().last(), Some(&false));

    app.open_debugger();
    assert_eq!(states.borrow().last(), Some(&true));
    app.debugger_toggle_run();
    assert_eq!(states.borrow().last(), Some(&false));
}

#[test]
fn host_io_audio_suspension_restores_current_run_state() {
    let states = Rc::new(RefCell::new(Vec::new()));
    let mut app = test_app_with_audio(Box::new(SuspensionSink {
        states: Rc::clone(&states),
    }));

    app.suspend_live_audio_for_host_io();
    app.finish_host_io_pause();
    assert_eq!(states.borrow().as_slice(), &[true, false]);

    app.toggle_pause();
    app.suspend_live_audio_for_host_io();
    app.finish_host_io_pause();
    assert_eq!(states.borrow().last(), Some(&true));
}

#[test]
fn restoring_over_placeholder_detected_only_for_the_silent_config_screen() {
    // The exact configuration-screen placeholder: powered off, launcher
    // open, NullSink installed (as build_placeholder_machine produces).
    let mut app = test_app();
    app.power_off();
    app.open_launcher();
    assert!(
        app.restoring_over_placeholder(),
        "powered-off launcher with a null sink is the placeholder"
    );

    // A live (non-null) sink behind the launcher is a real running session
    // re-opening the config screen: its audio must not be torn out.
    let states = Rc::new(RefCell::new(Vec::new()));
    let mut live = test_app_with_audio(Box::new(SuspensionSink {
        states: Rc::clone(&states),
    }));
    live.power_off();
    live.open_launcher();
    assert!(
        !live.restoring_over_placeholder(),
        "a real audio sink behind the launcher is not the placeholder"
    );

    // A null sink but powered on, or with no launcher open, is not the
    // pre-boot placeholder either.
    let mut powered = test_app();
    powered.open_launcher();
    assert!(powered.powered_on);
    assert!(!powered.restoring_over_placeholder());

    let mut no_launcher = test_app();
    no_launcher.power_off();
    assert!(no_launcher.ui.panel.is_none());
    assert!(!no_launcher.restoring_over_placeholder());
}

#[test]
fn state_load_over_running_session_keeps_its_live_audio_sink() {
    // Re-opening the config screen over a running machine and loading a
    // state must not replace the live audio sink (the regression guard for
    // the placeholder-upgrade path). Uses a probe sink so no real audio
    // device is touched.
    let path = std::env::temp_dir().join(format!(
        "copperline-running-state-load-{}.clstate",
        std::process::id()
    ));
    let states = Rc::new(RefCell::new(Vec::new()));
    let mut app = test_app_with_audio(Box::new(SuspensionSink {
        states: Rc::clone(&states),
    }));
    app.emu.save_state(&path).expect("save test state");
    app.open_launcher();
    assert!(app.powered_on);
    assert!(!app.restoring_over_placeholder());

    assert!(app.load_state_from_path(&path));

    // The probe sink is still the installed one: a suspension toggle still
    // reaches it. (A replacement CpalSink would have dropped the probe.)
    states.borrow_mut().clear();
    app.suspend_live_audio_for_host_io();
    app.finish_host_io_pause();
    assert_eq!(
        states.borrow().as_slice(),
        &[true, false],
        "live audio sink should survive a state load over a running session"
    );

    let _ = std::fs::remove_file(&path);
}

/// End-to-end recording through the app: start, run emulated frames
/// through the same render/capture path as the event loop, stop, and
/// check the resulting AVI carries the frames and matching audio.
/// COPPERLINE_RECORDER_KEEP=1 keeps the file for playback checks.
#[test]
fn recording_captures_emulated_frames_with_audio() {
    let mut app = test_app();
    let path = std::env::temp_dir().join(format!(
        "copperline-app-recording-{}.avi",
        std::process::id()
    ));
    for warmup_step in 0..4 {
        app.emu.step_frame().expect("step frame");
        let rendered = if app.render_worker.is_some() {
            app.finish_render_for_current_frame()
        } else {
            app.render_emulated_frame_if_needed()
        };
        if rendered {
            break;
        }
        assert!(
            warmup_step < 3,
            "fixture should produce an initial renderable frame"
        );
    }

    app.start_recording_to(path.clone());
    assert!(app.recorder.is_some(), "recorder should be active");

    let frames_to_record = 5;
    let mut rendered_frames = 0;
    let mut step_quanta = 0;
    while rendered_frames < frames_to_record {
        app.emu.step_frame().expect("step frame");
        let rendered = if app.render_worker.is_some() {
            app.finish_render_for_current_frame()
        } else {
            app.render_emulated_frame_if_needed()
        };
        app.capture_recorder_output(rendered);
        if rendered {
            rendered_frames += 1;
        }
        step_quanta += 1;
        assert!(
            step_quanta <= frames_to_record * 2,
            "fixture should keep producing renderable frames"
        );
    }
    app.stop_recording();
    assert!(app.recorder.is_none());
    // Stopping again is a no-op, and Paula's tap is off.
    app.stop_recording();
    assert!(app.emu.bus_mut().paula.take_captured_audio().is_empty());

    let data = std::fs::read(&path).expect("recording file exists");
    if crate::envcfg::flag("COPPERLINE_RECORDER_KEEP") {
        eprintln!("kept {}", path.display());
    } else {
        std::fs::remove_file(&path).unwrap();
    }
    assert_eq!(&data[0..4], b"RIFF");
    assert_eq!(&data[8..12], b"AVI ");
    assert_eq!(&data[112..116], b"ZMBV");
    // avih dwTotalFrames at offset 48 (see recorder::build_header).
    let frames = u32::from_le_bytes(data[48..52].try_into().unwrap());
    assert_eq!(frames, frames_to_record);
    // Audio stream length (samples) at offset 264 should cover the
    // same emulated interval: ~882 mixer samples per PAL frame or
    // ~735 per NTSC frame (the fixture machine's standard).
    let audio_len = u32::from_le_bytes(data[264..268].try_into().unwrap());
    let per_frame = audio_len as f64 / frames_to_record as f64;
    assert!(
        (700.0..=920.0).contains(&per_frame),
        "audio samples per frame {per_frame}"
    );
}

#[test]
fn debugger_window_pauses_steps_and_restores_run_state() {
    let mut app = test_app();
    assert!(!app.paused);

    // Opening pauses; the memory view starts at the PC's page.
    app.toggle_debugger();
    assert!(app.paused);
    let pc_before = app.emu.machine.pc();
    match app.debugger_panel.as_ref() {
        Some(panel) => {
            assert_eq!(panel.mem_addr, pc_before & 0x00FF_FFF0);
        }
        _ => panic!("debugger panel should be open"),
    }

    // Step executes exactly one instruction (a 2-byte NOP).
    app.debugger_step();
    assert_eq!(app.emu.machine.pc(), pc_before.wrapping_add(2));

    // Run to a nearby address lands exactly there.
    let target = pc_before.wrapping_add(10);
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.entry = format!("{target:X}");
    }
    app.debugger_run_to();
    assert_eq!(app.emu.machine.pc() & 0x00FF_FFFF, target & 0x00FF_FFFF);

    // Step Frame advances emulated time by one whole frame.
    let frame_before = app.emu.bus().emulated_frames();
    app.debugger_step_frame();
    assert!(app.emu.bus().emulated_frames() > frame_before);

    // Closing restores the pre-debugger (running) state.
    app.toggle_debugger();
    assert!(app.debugger_panel.is_none());
    assert!(!app.paused);

    // Run pressed inside the debugger survives closing it.
    app.toggle_debugger();
    assert!(app.paused);
    app.debugger_toggle_run();
    assert!(!app.paused);
    app.close_tool_panel(ToolPanelKind::Debugger);
    assert!(!app.paused);
}

#[test]
fn debugger_and_frame_analyzer_can_stay_open_together() {
    let mut app = test_app();
    assert!(!app.paused);

    app.open_debugger();
    assert!(app.paused);
    assert!(app.debugger_panel.is_some());
    assert!(app.frame_analyzer_panel.is_none());

    app.open_frame_analyzer();
    assert!(app.paused);
    assert!(app.debugger_panel.is_some());
    assert!(app.frame_analyzer_panel.is_some());

    app.close_tool_panel(ToolPanelKind::FrameAnalyzer);
    assert!(app.paused, "debugger should keep the machine paused");
    assert!(app.debugger_panel.is_some());
    assert!(app.frame_analyzer_panel.is_none());

    app.close_tool_panel(ToolPanelKind::Debugger);
    assert!(!app.paused);
    assert!(app.debugger_panel.is_none());

    let mut app = test_app();
    app.open_frame_analyzer();
    assert!(app.paused);
    app.open_debugger();
    assert!(app.debugger_panel.is_some());
    assert!(app.frame_analyzer_panel.is_some());

    app.close_tool_panel(ToolPanelKind::Debugger);
    assert!(app.paused, "analyzer should keep the machine paused");
    assert!(app.debugger_panel.is_none());
    assert!(app.frame_analyzer_panel.is_some());

    app.close_tool_panel(ToolPanelKind::FrameAnalyzer);
    assert!(!app.paused);
    assert!(app.frame_analyzer_panel.is_none());
}

#[test]
fn debugger_views_reflect_machine_state() {
    let mut app = test_app();
    app.open_debugger();

    let pc = app.emu.machine.pc();
    for tab in super::ui::DEBUG_TABS {
        if let Some(panel) = app.debugger_panel.as_mut() {
            panel.tab = tab;
        }
        let Some(panel) = app.debugger_panel.as_ref() else {
            unreachable!()
        };
        let view = app.build_debugger_view(panel);
        // The Video tab draws a custom layout from its structured view;
        // every other tab renders text lines.
        if tab != super::ui::DebugTab::Video {
            assert!(!view.lines.is_empty());
        }
        match tab {
            super::ui::DebugTab::Cpu => {
                assert!(view.lines[0].text.contains(&format!("PC {pc:08X}")));
                // The disassembly cursor line is highlighted and decodes
                // the NOP sled.
                let cursor = view
                    .lines
                    .iter()
                    .find(|line| line.highlight)
                    .expect("a highlighted PC line");
                assert!(cursor.text.contains("NOP"), "{}", cursor.text);
            }
            super::ui::DebugTab::Chipset => {
                assert!(view.lines.iter().any(|l| l.text.starts_with("DMACON")));
                assert!(view.lines.iter().any(|l| l.text.starts_with("INTENA")));
                assert!(view.lines.iter().any(|l| l.text.contains("COLOR00")));
            }
            super::ui::DebugTab::Copper => {
                // The first content lines are blank (the CBreak/CStep
                // button row); the register header follows.
                assert!(view.lines.iter().any(|l| l.text.contains("COP1LC")));
            }
            super::ui::DebugTab::Audio => {
                // Text is mirrored into `lines` for the fallback/invariant.
                assert!(view.lines[0].text.starts_with("DMACON"));
                assert!(view.lines[0].text.contains("ADKCON"));
                assert!(view.lines.iter().any(|l| l.text.starts_with("AUD0")));
                assert!(view.lines.iter().any(|l| l.text.starts_with("AUD3")));
                // The structured view drives the graphical layout: four
                // Paula channels plus a CD row.
                let audio = view.audio.as_ref().expect("audio scope view");
                assert!(audio.header.starts_with("DMACON"));
                assert_eq!(audio.channels.len(), 4);
                assert!(audio.channels[0].text[0].text.starts_with("AUD0"));
                assert!(audio.cd.text[0].text.contains("CD-DA"));
            }
            super::ui::DebugTab::Memory => {
                // The hex dump shows the NOP sled at the PC's ROM page.
                assert!(view.lines.iter().any(|l| l.text.contains("4E 71")));
            }
            super::ui::DebugTab::Video => {
                let video = view.video.as_ref().expect("video view");
                assert!(video.header.starts_with("BPLCON0"), "{}", video.header);
                assert_eq!(video.sprites.len(), 8);
                // test_app is an OCS machine: the classic 32-entry palette.
                assert_eq!(video.palette.len(), 32);
                assert_eq!(video.plane_mask, 0xFF);
                assert_eq!(video.sprite_mask, 0xFF);
            }
            super::ui::DebugTab::Break => {
                assert!(view.lines.iter().any(|l| l.text == "Breakpoints:"));
                assert!(view.lines.iter().any(|l| l.text == "  (none)"));
            }
        }
    }
}

#[test]
fn audio_tab_mute_buttons_toggle_paula_mutes() {
    let mut app = test_app();
    app.open_debugger();
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.tab = super::ui::DebugTab::Audio;
    }
    // Clicking a channel's mute button toggles that Paula channel's mute
    // through the full dispatch path.
    assert!(!app.emu.bus().paula.channel_muted(1));
    app.activate_ui_control(super::ui::UiControl::DebugAudioMute(1));
    assert!(app.emu.bus().paula.channel_muted(1));
    app.activate_ui_control(super::ui::UiControl::DebugAudioMute(1));
    assert!(!app.emu.bus().paula.channel_muted(1));
    // Index 4 is the CD-DA mute.
    assert!(!app.emu.bus().paula.cd_muted());
    app.activate_ui_control(super::ui::UiControl::DebugAudioMute(4));
    assert!(app.emu.bus().paula.cd_muted());
}

#[test]
fn interactive_breakpoint_pauses_and_reopens_the_debugger() {
    let mut app = test_app();
    app.open_debugger();

    // Toggle a breakpoint a few instructions ahead via the entry box.
    let target = app.emu.machine.pc().wrapping_add(8) & 0x00FF_FFFF;
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.tab = super::ui::DebugTab::Break;
        panel.entry = format!("{target:X}");
    }
    app.activate_ui_control(super::ui::UiControl::DebugBreakToggle);
    assert!(app.emu.machine.ui_breaks().is_breakpoint(target));

    // The Break tab lists it.
    if let Some(panel) = app.debugger_panel.as_ref() {
        let view = app.build_debugger_view(panel);
        assert!(view
            .lines
            .iter()
            .any(|l| l.text.contains(&format!("${target:06X}"))));
    }

    // Close the panel (machine resumes) and run a frame: the hit
    // pauses the machine at the breakpoint, before it executes.
    app.close_tool_panel(ToolPanelKind::Debugger);
    assert!(!app.paused);
    app.emu.step_frame().expect("frame");
    assert!(app.surface_debug_stop());
    assert!(app.paused);
    assert_eq!(app.emu.machine.pc() & 0x00FF_FFFF, target);
    assert!(app.debugger_panel.is_some());
    assert!(app
        .last_debug_stop
        .as_deref()
        .is_some_and(|s| s.contains("Breakpoint")));

    // Resuming does not immediately re-trip the same breakpoint.
    app.debugger_toggle_run();
    assert!(app.last_debug_stop.is_none());
    app.emu.step_frame().expect("frame");
    assert_ne!(app.emu.machine.pc() & 0x00FF_FFFF, target);

    // Toggling the same address again removes the breakpoint.
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.entry = format!("{target:X}");
    }
    app.activate_ui_control(super::ui::UiControl::DebugBreakToggle);
    assert!(!app.emu.machine.ui_breaks().is_breakpoint(target));
}

#[test]
fn opening_the_debugger_arms_reverse_and_step_reconstructs() {
    let mut app = test_app();
    // Opening the debugger auto-arms the reverse snapshot ring.
    app.open_debugger();
    assert!(app.emu.time_travel_enabled());
    if let Some(panel) = app.debugger_panel.as_ref() {
        assert!(
            app.build_debugger_view(panel).reverse_available,
            "reverse controls should be enabled once armed"
        );
    }

    // Advance enough frames that several snapshots accrue.
    for _ in 0..16 {
        app.debugger_step_frame();
    }
    let pos_before = app.emu.retired_instructions();
    let pc_before = app.emu.machine.pc();
    assert!(pos_before > 0, "the NOP sled retired instructions");

    // Reverse Step moves the position strictly backward.
    app.activate_ui_control(super::ui::UiControl::DebugReverseStep);
    let pos_after = app.emu.retired_instructions();
    assert_eq!(pos_after, pos_before - 1, "stepped back exactly one");

    // Replaying forward to the original position reconstructs the PC.
    for _ in 0..16 {
        app.debugger_step_frame();
    }
    assert!(app.emu.retired_instructions() >= pos_before);
    let frame_before = app.emu.bus().emulated_frames();
    let pos_before_frame = app.emu.retired_instructions();
    assert!(frame_before > 0, "frame history should have advanced");

    // Reverse Frame moves to the previous Agnus frame counter value.
    app.activate_ui_control(super::ui::UiControl::DebugReverseFrame);
    assert_eq!(
        app.emu.bus().emulated_frames(),
        frame_before - 1,
        "stepped back exactly one emulated video frame"
    );
    assert!(
        app.emu.retired_instructions() < pos_before_frame,
        "reverse frame should move to an earlier instruction boundary"
    );

    // And reverse-continue with no breakpoints is a no-op (reports, does
    // not move): position is unchanged afterward.
    let pos = app.emu.retired_instructions();
    app.activate_ui_control(super::ui::UiControl::DebugReverseRun);
    assert_eq!(app.emu.retired_instructions(), pos);
    let _ = pc_before;
}

#[test]
fn interactive_watchpoint_stops_when_the_word_changes() {
    let mut app = test_app();
    // Map chip RAM at $0 so the watched word is real memory.
    app.emu.machine.disable_overlay();
    let addr = 0x0000_1000u32;
    assert!(app.emu.machine.ui_toggle_watch(addr));

    // Unchanged memory: a full frame runs without stopping.
    app.emu.step_frame().expect("frame");
    assert!(!app.surface_debug_stop());

    // Change the watched word (as any non-CPU bus master would); the
    // next executed instruction notices and stops the machine.
    app.emu.bus_mut().mem.chip_ram[addr as usize] = 0xAB;
    app.emu.step_frame().expect("frame");
    assert!(app.surface_debug_stop());
    assert!(app.paused);
    assert!(app
        .last_debug_stop
        .as_deref()
        .is_some_and(|s| s.contains("Watch $001000")));
}

#[test]
fn chipset_register_watch_stops_on_a_cpu_write() {
    let mut app = test_app();
    // Replace part of the NOP sled with MOVE.W #$8020,$DFF096
    // (DMACON), a few instructions ahead of the PC so the already
    // prefetched words are not affected.
    let pc = app.emu.machine.pc();
    let off = (pc as usize & 0x7FFFF) + 8;
    let mov: [u16; 4] = [0x33FC, 0x8020, 0x00DF, 0xF096];
    for (k, word) in mov.iter().enumerate() {
        app.emu.bus_mut().mem.rom[off + k * 2..off + k * 2 + 2]
            .copy_from_slice(&word.to_be_bytes());
    }

    // Watch DMACON via the entry box, accepting the full address form.
    app.open_debugger();
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.tab = super::ui::DebugTab::Break;
        panel.entry = "DFF096".to_string();
    }
    app.activate_ui_control(super::ui::UiControl::DebugRegToggle);
    assert_eq!(app.emu.machine.ui_breaks().reg_watches, [0x096]);
    app.debugger_toggle_run();
    app.close_tool_panel(ToolPanelKind::Debugger);

    app.emu.step_frame().expect("frame");
    assert!(app.surface_debug_stop());
    assert!(app.paused);
    let stop = app.last_debug_stop.as_deref().unwrap();
    assert!(stop.contains("DMACON"), "{stop}");
    assert!(stop.contains("8020"), "{stop}");
    assert!(stop.contains("cpu write"), "{stop}");
}

#[test]
fn modal_panel_swallows_amiga_key_presses() {
    let mut app = test_app();
    app.ui.panel = Some(Panel::About);

    // Escape closes the panel.
    assert!(app.ui_handle_key(KeyCode::Escape));
    assert!(app.ui.panel.is_none());

    // Hex entry: digits accumulate, Enter commits to the memory view.
    app.open_debugger();
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.entry_active = true;
        panel.tab = super::ui::DebugTab::Memory;
    }
    for key in [
        KeyCode::KeyC,
        KeyCode::Digit0,
        KeyCode::Digit0,
        KeyCode::Digit1,
    ] {
        assert!(app.ui_handle_key(key));
    }
    assert!(app.ui_handle_key(KeyCode::Enter));
    match app.debugger_panel.as_ref() {
        Some(panel) => {
            assert_eq!(panel.entry, "C001");
            assert_eq!(panel.mem_addr, 0xC000);
            assert!(!panel.entry_active);
        }
        _ => panic!("debugger panel should be open"),
    }
}

#[test]
fn frame_analyzer_cursor_keys_move_selected_slot() {
    let mut app = test_app();
    app.open_frame_analyzer();
    app.frame_analyzer_step_frame();
    assert!(app.emu.bus().frame_bus_trace().is_some());
    assert!(app.ui_key_accepts_repeat(Some(ToolPanelKind::FrameAnalyzer), KeyCode::ArrowRight));
    assert!(!app.ui_key_accepts_repeat(Some(ToolPanelKind::FrameAnalyzer), KeyCode::KeyR));

    let (start_hpos, start_vpos) = match app.frame_analyzer_panel.as_ref() {
        Some(panel) => (panel.selected_hpos, panel.selected_vpos),
        _ => panic!("frame analyzer panel should be open"),
    };

    assert!(app.ui_handle_key(KeyCode::ArrowRight));
    assert!(app.ui_handle_key(KeyCode::ArrowDown));
    match app.frame_analyzer_panel.as_ref() {
        Some(panel) => {
            assert_eq!(panel.selected_hpos, start_hpos + 1);
            assert_eq!(panel.selected_vpos, start_vpos + 1);
        }
        _ => panic!("frame analyzer panel should be open"),
    }

    assert!(app.ui_handle_key(KeyCode::ArrowLeft));
    assert!(app.ui_handle_key(KeyCode::ArrowUp));
    match app.frame_analyzer_panel.as_ref() {
        Some(panel) => {
            assert_eq!(panel.selected_hpos, start_hpos);
            assert_eq!(panel.selected_vpos, start_vpos);
        }
        _ => panic!("frame analyzer panel should be open"),
    }

    if let Some(panel) = app.frame_analyzer_panel.as_mut() {
        panel.selected_hpos = 0;
        panel.selected_vpos = 0;
    }
    assert!(app.ui_handle_key(KeyCode::ArrowLeft));
    assert!(app.ui_handle_key(KeyCode::ArrowUp));
    match app.frame_analyzer_panel.as_ref() {
        Some(panel) => {
            assert_eq!(panel.selected_hpos, 0);
            assert_eq!(panel.selected_vpos, 0);
        }
        _ => panic!("frame analyzer panel should be open"),
    }

    let (max_hpos, max_vpos) = app
        .emu
        .bus()
        .frame_bus_trace()
        .map(|trace| {
            (
                trace.cols.saturating_sub(1).min(u16::MAX as usize) as u16,
                trace.rows.saturating_sub(1).min(u16::MAX as usize) as u16,
            )
        })
        .unwrap();
    if let Some(panel) = app.frame_analyzer_panel.as_mut() {
        panel.selected_hpos = max_hpos;
        panel.selected_vpos = max_vpos;
    }
    assert!(app.ui_handle_key(KeyCode::ArrowRight));
    assert!(app.ui_handle_key(KeyCode::ArrowDown));
    match app.frame_analyzer_panel.as_ref() {
        Some(panel) => {
            assert_eq!(panel.selected_hpos, max_hpos);
            assert_eq!(panel.selected_vpos, max_vpos);
        }
        _ => panic!("frame analyzer panel should be open"),
    }
}

#[test]
fn frame_analyzer_underlay_toggles_and_renders() {
    let mut app = test_app();
    app.open_frame_analyzer();
    app.frame_analyzer_step_frame();
    assert!(app.emu.bus().frame_bus_trace().is_some());

    // Off by default: no underlay is rendered or attached to the view.
    app.ensure_analyzer_underlay();
    assert_eq!(app.analyzer_underlay_rows, 0);
    let panel = app.frame_analyzer_panel.clone().unwrap();
    assert!(app.build_frame_analyzer_view(&panel).underlay.is_none());

    // The U key ticks the checkbox on.
    assert!(app.ui_handle_key(KeyCode::KeyU));
    assert!(app
        .frame_analyzer_panel
        .as_ref()
        .is_some_and(|panel| panel.show_underlay));

    // With the box ticked a beam-space frame render is captured and
    // handed to the view, sized to the traced frame's scan.
    app.ensure_analyzer_underlay();
    assert!(app.analyzer_underlay_rows > 0);
    let panel = app.frame_analyzer_panel.clone().unwrap();
    let view = app.build_frame_analyzer_view(&panel);
    let underlay = view.underlay.expect("underlay attached to view");
    assert_eq!(underlay.rows, app.analyzer_underlay_rows);
    assert!(underlay.fb.len() >= FB_WIDTH * underlay.rows);

    // The render must not perturb emulated state: peeking the same frame
    // twice leaves the underlay cache keyed to the same traced frame.
    let frame = app.analyzer_underlay_frame;
    app.ensure_analyzer_underlay();
    assert_eq!(app.analyzer_underlay_frame, frame);

    // Toggling off via the control drops it from the view again.
    app.activate_ui_control(UiControl::AnalyzerUnderlay);
    let panel = app.frame_analyzer_panel.clone().unwrap();
    assert!(app.build_frame_analyzer_view(&panel).underlay.is_none());

    // Closing the analyzer releases the underlay buffers.
    app.close_tool_panel(ToolPanelKind::FrameAnalyzer);
    assert_eq!(app.analyzer_underlay_rows, 0);
    assert!(app.analyzer_underlay_input.is_none());
}

/// Type a command into the open console and return the lines it printed.
fn console_run(app: &mut super::App, cmd: &str) -> Vec<String> {
    if let Some(panel) = app.console_panel.as_mut() {
        panel.input = cmd.to_string();
    }
    let before = app
        .console_panel
        .as_ref()
        .map(|panel| panel.output.len() + 1) // +1 skips the echoed command
        .unwrap_or(0);
    app.console_submit();
    app.console_panel
        .as_ref()
        .map(|panel| panel.output.iter().skip(before).cloned().collect())
        .unwrap_or_default()
}

#[test]
fn console_keyboard_path_types_and_executes() {
    let mut app = test_app();
    app.open_console();
    // Type "HELP" through the tool-window key handler and execute it.
    for code in [KeyCode::KeyH, KeyCode::KeyE, KeyCode::KeyL, KeyCode::KeyP] {
        assert!(app.ui_handle_tool_key(ToolPanelKind::Console, code));
    }
    assert_eq!(app.console_panel.as_ref().unwrap().input, "HELP");
    // Backspace edits; retype the P.
    assert!(app.ui_handle_tool_key(ToolPanelKind::Console, KeyCode::Backspace));
    assert_eq!(app.console_panel.as_ref().unwrap().input, "HEL");
    assert!(app.ui_handle_tool_key(ToolPanelKind::Console, KeyCode::KeyP));
    assert!(app.ui_handle_tool_key(ToolPanelKind::Console, KeyCode::Enter));
    let panel = app.console_panel.as_ref().unwrap();
    assert!(panel.input.is_empty());
    assert!(panel.output.iter().any(|l| l.contains("execution:")));
    // Up recalls the command into the prompt.
    assert!(app.ui_handle_tool_key(ToolPanelKind::Console, KeyCode::ArrowUp));
    assert_eq!(app.console_panel.as_ref().unwrap().input, "HELP");
    // Escape (handled a level up) closes the window.
    assert!(app.ui_handle_tool_key(ToolPanelKind::Console, KeyCode::Escape));
    assert!(app.console_panel.is_none());
}

#[test]
fn console_text_insertion_and_multiline_paste() {
    let mut app = test_app();
    app.open_console();

    // Typed/pasted text preserves case and punctuation; the interpreter
    // is case-insensitive.
    app.console_insert_text("b $c01000");
    assert_eq!(app.console_panel.as_ref().unwrap().input, "b $c01000");
    app.console_insert_text("\n");
    assert!(app.emu.machine.ui_breaks().is_breakpoint(0x00C0_1000));
    assert!(app.console_panel.as_ref().unwrap().input.is_empty());

    // A multi-line paste runs each complete line and leaves the trailing
    // fragment in the prompt. Blank lines are ignored.
    app.console_insert_text("btrap 100 40\n\nsetreg d2 77\nm 0");
    assert_eq!(app.emu.bus().ui_beam_traps().len(), 1);
    assert_eq!(app.emu.machine.d(2), 0x77);
    assert_eq!(app.console_panel.as_ref().unwrap().input, "m 0");

    // Control characters never reach the prompt.
    app.console_insert_text("\u{16}\u{7f}");
    assert_eq!(app.console_panel.as_ref().unwrap().input, "m 0");
}

#[test]
fn console_inspection_and_stop_commands() {
    let mut app = test_app();
    app.open_console();
    assert!(app.paused, "opening the console pauses the machine");

    let out = console_run(&mut app, "HELP");
    assert!(out.iter().any(|l| l.contains("execution:")));

    // Stepping advances the PC through the ROM NOP sled.
    let pc0 = app.emu.machine.pc();
    let out = console_run(&mut app, "S 2");
    assert_eq!(app.emu.machine.pc(), pc0 + 4);
    assert!(out.last().unwrap().contains("pc $"), "{out:?}");

    let out = console_run(&mut app, "R");
    assert!(out.iter().any(|l| l.starts_with("D0-D7")));
    let out = console_run(&mut app, "D");
    assert!(out.iter().any(|l| l.contains("NOP")), "{out:?}");
    let out = console_run(&mut app, "M 0 20");
    assert_eq!(out.len(), 2, "{out:?}");
    let out = console_run(&mut app, "COPPER");
    assert!(out[0].contains("COP1LC"), "{out:?}");

    // Every stop kind toggles on, lists, and clears.
    console_run(&mut app, "B C01000");
    console_run(&mut app, "W C09580");
    console_run(&mut app, "RWATCH DMACON");
    console_run(&mut app, "BTRAP 100 40");
    console_run(&mut app, "CATCH TRAP 0");
    console_run(&mut app, "CBREAK C02000");
    let out = console_run(&mut app, "BREAKS");
    assert!(out.iter().any(|l| l.contains("break  $C01000")), "{out:?}");
    assert!(out.iter().any(|l| l.contains("watch  $C09580")), "{out:?}");
    assert!(out.iter().any(|l| l.contains("DMACON")), "{out:?}");
    assert!(out.iter().any(|l| l.contains("btrap  v100 h40")), "{out:?}");
    assert!(out.iter().any(|l| l.contains("TRAP #0")), "{out:?}");
    assert!(out.iter().any(|l| l.contains("cbreak $C02000")), "{out:?}");
    console_run(&mut app, "CLEARBREAKS");
    let out = console_run(&mut app, "BREAKS");
    assert!(out.iter().any(|l| l.contains("no breakpoints")), "{out:?}");

    // Errors come back prefixed for the accent colour.
    let out = console_run(&mut app, "BOGUS");
    assert!(out[0].starts_with('!'), "{out:?}");
}

#[test]
fn console_modify_search_and_transport_commands() {
    let mut app = test_app();
    app.open_console();

    console_run(&mut app, "SETREG D3 CAFE");
    assert_eq!(app.emu.machine.d(3), 0xCAFE);

    // Drop the boot overlay so chip RAM is CPU-visible, then poke and
    // find the word back.
    app.emu.bus_mut().mem.overlay = false;
    console_run(&mut app, "POKE 60000 BEEF");
    assert_eq!(app.emu.bus().peek_word_any(0x60000), 0xBEEF);
    let out = console_run(&mut app, "FIND BEEF 50000");
    assert!(out[0].contains("found at $060000"), "{out:?}");

    // Run to an exact beam slot; the one-shot trap reports its position.
    let out = console_run(&mut app, "TOSLOT 50 30");
    assert!(
        out.iter().any(|l| l.contains("Beam trap at v50 h30")),
        "{out:?}"
    );

    // run/pause flip the host pause state.
    console_run(&mut app, "RUN");
    assert!(!app.paused);
    console_run(&mut app, "PAUSE");
    assert!(app.paused);

    // History recall, clear, and close.
    if let Some(panel) = app.console_panel.as_mut() {
        panel.history_step(-1);
        assert_eq!(panel.input, "PAUSE");
        panel.history_step(-1);
        assert_eq!(panel.input, "RUN");
        panel.history_step(1);
        assert_eq!(panel.input, "PAUSE");
        panel.history_step(1);
        assert_eq!(panel.input, "");
    }
    console_run(&mut app, "CLEAR");
    assert!(app
        .console_panel
        .as_ref()
        .is_some_and(|panel| panel.output.is_empty()));
    console_run(&mut app, "CLOSE");
    assert!(app.console_panel.is_none());
}

#[test]
fn beam_scrub_rides_the_underlay() {
    let mut app = test_app();
    app.open_frame_analyzer();
    app.frame_analyzer_step_frame();

    // Enabling scrub alone activates the underlay render and flags the
    // view for the up-to-the-beam crop.
    app.activate_ui_control(UiControl::AnalyzerScrub);
    assert!(app
        .frame_analyzer_panel
        .as_ref()
        .is_some_and(|panel| panel.show_scrub && panel.underlay_active()));
    app.ensure_analyzer_underlay();
    assert!(app.analyzer_underlay_rows > 0);
    let panel = app.frame_analyzer_panel.clone().unwrap();
    let view = app.build_frame_analyzer_view(&panel);
    assert!(view.scrub);
    assert!(view.underlay.is_some());

    // Turning the underlay off ends the scrub with it.
    app.activate_ui_control(UiControl::AnalyzerUnderlay);
    app.activate_ui_control(UiControl::AnalyzerUnderlay);
    assert!(app
        .frame_analyzer_panel
        .as_ref()
        .is_some_and(|panel| !panel.show_scrub && !panel.underlay_active()));
}

#[test]
fn beam_trap_gui_toggle_line_step_and_run_to_slot() {
    let mut app = test_app();
    app.open_debugger();

    // Break tab: a decimal "VPOS HPOS" entry toggles a beam trap.
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.tab = super::ui::DebugTab::Break;
        panel.entry = "100 40".to_string();
    }
    app.activate_ui_control(UiControl::DebugBeamToggle);
    assert_eq!(
        app.emu.bus().ui_beam_traps(),
        &[crate::bus::BeamTrap {
            vpos: 100,
            hpos: Some(40),
            once: false,
        }]
    );
    app.activate_ui_control(UiControl::DebugBeamToggle);
    assert!(app.emu.bus().ui_beam_traps().is_empty());

    // Line: run to the start of the next scanline. The stop reason
    // reports the exact beam position of the one-shot trap.
    let vpos_before = app.emu.bus().agnus.vpos;
    let frame_lines = app.emu.bus().agnus.current_frame_lines();
    app.activate_ui_control(UiControl::DebugRunLine);
    let expected = (vpos_before + 1) % frame_lines;
    assert_eq!(
        app.last_debug_stop.as_deref(),
        Some(format!("Beam trap at v{expected} h0").as_str())
    );
    assert!(app.emu.bus().ui_beam_traps().is_empty());

    // Analyzer: To slot runs until the beam reaches the selected slot.
    app.open_frame_analyzer();
    let (target_v, target_h) = {
        let bus = app.emu.bus();
        ((((bus.agnus.vpos + 2) % frame_lines) as u16), 30u16)
    };
    if let Some(panel) = app.frame_analyzer_panel.as_mut() {
        panel.selected_vpos = target_v;
        panel.selected_hpos = target_h;
    }
    app.activate_ui_control(UiControl::AnalyzerRunTo);
    assert_eq!(
        app.last_debug_stop.as_deref(),
        Some(format!("Beam trap at v{target_v} h{target_h}").as_str())
    );
}

#[test]
fn memory_tab_find_scroll_and_bitmap_toggle() {
    let mut app = test_app();
    app.open_debugger();
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.tab = super::ui::DebugTab::Memory;
        panel.mem_addr = 0;
    }
    // Plant a pattern in chip RAM and find it from page zero. The boot
    // overlay would shadow chip RAM with ROM, so drop it like the
    // Kickstart boot path does.
    {
        let bus = app.emu.bus_mut();
        bus.mem.overlay = false;
        bus.mem.chip_ram[0x60000..0x60004].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    }
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.entry = "DEADBEEF".to_string();
    }
    app.activate_ui_control(UiControl::DebugMemFind);
    {
        let panel = app.debugger_panel.as_ref().unwrap();
        assert_eq!(panel.mem_last_find, Some(0x60000));
        assert_eq!(panel.mem_addr, 0x60000);
    }
    // Find again continues past the hit; with a single match the page
    // wraps back around to the same place.
    app.activate_ui_control(UiControl::DebugMemFind);
    assert_eq!(
        app.debugger_panel.as_ref().unwrap().mem_last_find,
        Some(0x60000)
    );

    // Scrolling moves by 16-byte hex rows.
    app.debugger_mem_scroll(2);
    assert_eq!(app.debugger_panel.as_ref().unwrap().mem_addr, 0x60020);

    // Bits toggles the bitmap view; a decimal entry sets the stride.
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.entry = "40".to_string();
    }
    app.activate_ui_control(UiControl::DebugMemBits);
    let panel = app.debugger_panel.clone().unwrap();
    assert!(panel.mem_view_bits);
    assert_eq!(panel.mem_bitmap_stride, 40);
    let view = app.build_debugger_view(&panel);
    let bitmap = view.bitmap.expect("bitmap view in Bits mode");
    assert_eq!(bitmap.stride, 40);
    assert_eq!(bitmap.rows, super::ui::mem_bitmap_rows());
    assert_eq!(bitmap.data.len(), 40 * bitmap.rows);

    // Bitmap-mode scrolling steps by the stride; toggling back restores
    // the hex view (and its 16-byte scroll step).
    let before = app.debugger_panel.as_ref().unwrap().mem_addr;
    app.debugger_mem_scroll(1);
    assert_eq!(app.debugger_panel.as_ref().unwrap().mem_addr, before + 40);
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.entry.clear();
    }
    app.activate_ui_control(UiControl::DebugMemBits);
    assert!(!app.debugger_panel.as_ref().unwrap().mem_view_bits);
}

#[test]
fn video_tab_layer_toggles_flip_bus_masks() {
    let mut app = test_app();
    app.open_debugger();
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.tab = super::ui::DebugTab::Video;
    }
    assert_eq!(app.emu.bus().ui_layer_masks().planes, 0xFF);
    app.activate_ui_control(UiControl::DebugPlaneToggle(0));
    assert_eq!(app.emu.bus().ui_layer_masks().planes, 0xFE);
    app.activate_ui_control(UiControl::DebugSpriteToggle(3));
    assert_eq!(app.emu.bus().ui_layer_masks().sprites, 0xF7);

    // The Video view mirrors the masks for the toggle-row display.
    let panel = app.debugger_panel.clone().unwrap();
    let view = app.build_debugger_view(&panel);
    let video = view.video.expect("video view");
    assert_eq!(video.plane_mask, 0xFE);
    assert_eq!(video.sprite_mask, 0xF7);

    // Toggling back restores everything-visible.
    app.activate_ui_control(UiControl::DebugPlaneToggle(0));
    app.activate_ui_control(UiControl::DebugSpriteToggle(3));
    assert_eq!(
        app.emu.bus().ui_layer_masks(),
        crate::bus::UiLayerMasks::default()
    );
}

#[test]
fn exception_catchpoint_toggle_from_the_break_tab() {
    let mut app = test_app();
    app.open_debugger();
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.tab = super::ui::DebugTab::Break;
        panel.entry = "trap 0".to_string();
    }
    app.activate_ui_control(UiControl::DebugCatchToggle);
    assert_eq!(app.emu.machine.ui_breaks().catches, vec![32]);

    // The Break tab lists it by name.
    let panel = app.debugger_panel.clone().unwrap();
    let view = app.build_debugger_view(&panel);
    assert!(view.lines.iter().any(|l| l.text.contains("TRAP #0")));

    // Toggling again removes it; Clear all also clears catches.
    app.activate_ui_control(UiControl::DebugCatchToggle);
    assert!(app.emu.machine.ui_breaks().catches.is_empty());
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.entry = "irq 3".to_string();
    }
    app.activate_ui_control(UiControl::DebugCatchToggle);
    assert_eq!(app.emu.machine.ui_breaks().catches, vec![27]);
    app.activate_ui_control(UiControl::DebugBreaksClear);
    assert!(app.emu.machine.ui_breaks().catches.is_empty());
}

#[test]
fn copper_breakpoint_toggle_and_copper_step_from_the_gui() {
    let mut app = test_app();
    app.open_debugger();

    // Copper tab: the entry address toggles a Copper breakpoint, and the
    // Break tab lists it.
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.tab = super::ui::DebugTab::Copper;
        panel.entry = "C01000".to_string();
    }
    app.activate_ui_control(UiControl::DebugCopperBreakToggle);
    assert_eq!(app.emu.bus().ui_copper_breaks(), &[0x00C0_1000]);
    app.activate_ui_control(UiControl::DebugCopperBreakToggle);
    assert!(app.emu.bus().ui_copper_breaks().is_empty());

    // CStep with an armed Copper list advances the retired count.
    {
        let bus = app.emu.bus_mut();
        let cop1 = 0x0400usize;
        let words: [u16; 6] = [0x0180, 0x0111, 0x0182, 0x0222, 0xFFFF, 0xFFFE];
        for (idx, word) in words.iter().enumerate() {
            bus.mem.chip_ram[cop1 + idx * 2..cop1 + idx * 2 + 2]
                .copy_from_slice(&word.to_be_bytes());
        }
        bus.agnus.dmacon |= 0x0280; // DMAEN | COPEN
        bus.copper.jump(cop1 as u32);
    }
    let before = app.emu.bus().copper_instructions_retired();
    app.activate_ui_control(UiControl::DebugCopperStep);
    assert!(app.emu.bus().copper_instructions_retired() > before);

    // The Copper tab view lists the register header and the live list.
    let panel = app.debugger_panel.clone().unwrap();
    let view = app.build_debugger_view(&panel);
    assert!(view.lines.iter().any(|l| l.text.contains("COP1LC")));
    assert!(view
        .lines
        .iter()
        .any(|l| l.text.contains("MOVE") || l.text.contains("WAIT") || l.text.contains("SKIP")));
}

#[test]
fn debugger_keys_step_and_pin_disassembly() {
    let mut app = test_app();
    app.open_debugger();

    // S steps one instruction while the entry box is unfocused.
    let pc_before = app.emu.machine.pc();
    assert!(app.ui_handle_key(KeyCode::KeyS));
    assert_eq!(app.emu.machine.pc(), pc_before.wrapping_add(2));

    // R toggles run; the explicit choice survives closing the panel.
    assert!(app.paused);
    assert!(app.ui_handle_key(KeyCode::KeyR));
    assert!(!app.paused);
    assert!(app.ui_handle_key(KeyCode::KeyR));
    assert!(app.paused);

    // On the CPU tab, Enter pins the disassembly origin to the typed
    // address; an empty box follows the PC again.
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.entry_active = true;
        panel.entry = "FC0010".to_string();
    }
    assert!(app.ui_handle_key(KeyCode::Enter));
    match app.debugger_panel.as_ref() {
        Some(panel) => {
            assert_eq!(panel.disasm_addr, Some(0xFC0010));
            let view = app.build_debugger_view(panel);
            // Each disasm line carries a one-char breakpoint marker prefix.
            assert!(view.lines.iter().any(|l| l.text.contains("00FC0010")));
        }
        _ => panic!("debugger panel should be open"),
    }
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.entry_active = true;
        panel.entry.clear();
    }
    assert!(app.ui_handle_key(KeyCode::Enter));
    match app.debugger_panel.as_ref() {
        Some(panel) => assert_eq!(panel.disasm_addr, None),
        _ => panic!("debugger panel should be open"),
    }

    // While the entry box is focused, S types the register-name letter
    // 'S' (for SR/SP) into the box instead of stepping.
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.entry_active = true;
        panel.entry.clear();
    }
    let pc_before = app.emu.machine.pc();
    assert!(app.ui_handle_key(KeyCode::KeyS));
    assert_eq!(app.emu.machine.pc(), pc_before);
    assert_eq!(
        app.debugger_panel.as_ref().map(|p| p.entry.as_str()),
        Some("S")
    );
}

#[test]
fn debugger_poke_writes_memory_and_registers() {
    let mut app = test_app();
    app.open_debugger();
    // Map chip RAM at $0 so the low test address is writable RAM, not the
    // boot ROM overlay.
    app.emu.machine.disable_overlay();

    // Memory tab: "ADDR VALUE" writes a word into chip RAM.
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.tab = super::ui::DebugTab::Memory;
        panel.entry = "2000 BEEF".to_string();
    }
    app.debugger_poke();
    assert_eq!(
        app.emu.machine.debug_read_memory(0x2000, 2),
        vec![0xBE, 0xEF]
    );

    // CPU tab: "REG VALUE" sets a register.
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.tab = super::ui::DebugTab::Cpu;
        panel.entry = "D3 12345678".to_string();
    }
    app.debugger_poke();
    assert_eq!(app.emu.machine.d(3), 0x1234_5678);
}
