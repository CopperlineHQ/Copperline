// SPDX-License-Identifier: GPL-3.0-or-later

//! Unit tests for the window/presentation layer: split out of
//! `window.rs` for size, they are the same `window::tests` module
//! and keep full access to the parent's private items via `super::`.

use super::ui::{Panel, UiControl};
use super::{
    bar_layout, center_present_frame_for_visible_start, center_present_frame_horizontally,
    control_at, copperline_icon_image, copperline_logo_image, copy_present_frame,
    copy_window_present_frame, draw_status_bar, fdd_track_counter_rect, fdd_track_digit_rect,
    host_shortcut_modifier_pressed, host_to_amiga_rawkey, joystick_toggle_rect,
    keyboard_joystick_key_for, led_row_rect, mask_present_frame_to_tv, paint_test_screen,
    parse_amiga_key, pause_button_rect, power_button_rect, present_height,
    presentation_h_shift_for, presentation_source_y_offset, raw_device_qualifier_family_held,
    raw_device_qualifier_rawkey, rawkey_is_held, rawkey_transition_is_duplicate,
    reboot_button_rect, repeated_main_key_should_drop, rgba, short_status_error,
    shorten_status_paths, shot_button_rect, should_render_emulated_frame, standard_window_top_row,
    status_with_latched_fdd_track, take_integral_mouse_delta, texture_height, texture_width,
    tv_aperture_source_row, tv_source_h_bounds, volume_percent_from_pos, volume_slider_track_rect,
    BarControl, DriveBar, JoystickInputMode, KeyboardJoystickHeld, KeyboardJoystickKey, MediaBar,
    StatusBarView, ToolPanelKind, AMIGA_RAWKEY_LEFT_ALT, AMIGA_RAWKEY_LEFT_SHIFT,
    AMIGA_RAWKEY_RIGHT_ALT, AMIGA_RAWKEY_RIGHT_SHIFT, BUTTON_GLYPH, BUTTON_GLYPH_DISABLED, CD_BODY,
    CD_LED_OFF, CD_LED_ON, DISK_BODY, DISK_BODY_SHADOW, DISK_LABEL, FDD_LED_OFF, FDD_LED_ON,
    HDD_LED_OFF, HDD_LED_ON, POWER_GLYPH_OFF, POWER_GLYPH_ON, POWER_LED_OFF, POWER_LED_ON,
    STANDARD_PAL_VISIBLE_LINES, STANDARD_PAL_VISIBLE_START_VPOS, STATUS_BG, TRACK_SEGMENT_OFF,
    TRACK_SEGMENT_ON, TV_PAL_LIVE_PAD_X, TV_PAL_PRESENT_HEIGHT, TV_PAL_PRESENT_SOURCE_X,
    TV_PAL_PRESENT_SOURCE_Y, TV_PAL_PRESENT_WIDTH, VOLUME_FILL, VOLUME_GLYPH_X,
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
        control_connected: false,
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
    assert!(app.keyboard_joy_held[0].fire_right_alt);
    assert!(!rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_RIGHT_ALT));

    app.handle_raw_device_key_event(RawKeyEvent {
        physical_key: PhysicalKey::Code(KeyCode::AltRight),
        state: ElementState::Released,
    });
    assert!(!app.keyboard_joy_held[0].fire_right_alt);
    assert!(!rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_RIGHT_ALT));
}

#[test]
fn keyboard_joystick_mapping_matches_fsuae_controls() {
    use KeyboardJoystickKey as K;
    // Mapping 0: the FS-UAE-compatible cursor-key layout.
    for (code, key) in [
        (KeyCode::ArrowUp, K::Up),
        (KeyCode::ArrowDown, K::Down),
        (KeyCode::ArrowLeft, K::Left),
        (KeyCode::ArrowRight, K::Right),
        (KeyCode::ControlRight, K::FireRightCtrl),
        (KeyCode::AltRight, K::FireRightAlt),
        (KeyCode::ControlLeft, K::FireLeftCtrl),
        (KeyCode::AltLeft, K::BlueLeftAlt),
        (KeyCode::KeyC, K::Red),
        (KeyCode::KeyX, K::Blue),
        (KeyCode::KeyD, K::Green),
        (KeyCode::KeyS, K::Yellow),
        (KeyCode::Enter, K::Play),
        (KeyCode::KeyZ, K::Rewind),
        (KeyCode::KeyA, K::Forward),
    ] {
        assert_eq!(keyboard_joystick_key_for(code), Some((0, key)), "{code:?}");
    }
    // Mapping 1: the numpad layout for the second controller, collision
    // free against mapping 0's letters.
    for (code, key) in [
        (KeyCode::Numpad8, K::Up),
        (KeyCode::Numpad2, K::Down),
        (KeyCode::Numpad4, K::Left),
        (KeyCode::Numpad6, K::Right),
        (KeyCode::Numpad0, K::Red),
        (KeyCode::NumpadDecimal, K::Blue),
        (KeyCode::NumpadEnter, K::Play),
    ] {
        assert_eq!(keyboard_joystick_key_for(code), Some((1, key)), "{code:?}");
    }
    assert_eq!(keyboard_joystick_key_for(KeyCode::Space), None);
}

#[test]
fn keyboard_joystick_fire_aliases_release_independently() {
    let mut held = KeyboardJoystickHeld::default();
    held.set(KeyboardJoystickKey::FireRightCtrl, true);
    held.set(KeyboardJoystickKey::Red, true);
    held.set(KeyboardJoystickKey::FireLeftCtrl, true);
    assert!(held.joystick_state().fire);

    held.set(KeyboardJoystickKey::FireRightCtrl, false);
    assert!(held.joystick_state().fire);

    held.set(KeyboardJoystickKey::Red, false);
    assert!(held.joystick_state().fire);

    held.set(KeyboardJoystickKey::FireLeftCtrl, false);
    assert!(!held.joystick_state().fire);
}

#[test]
fn keyboard_joystick_second_button_aliases_release_independently() {
    let mut held = KeyboardJoystickHeld::default();
    held.set(KeyboardJoystickKey::Blue, true);
    held.set(KeyboardJoystickKey::BlueLeftAlt, true);
    assert!(held.joystick_state().button2);

    held.set(KeyboardJoystickKey::Blue, false);
    assert!(held.joystick_state().button2);

    held.set(KeyboardJoystickKey::BlueLeftAlt, false);
    assert!(!held.joystick_state().button2);
}

#[test]
fn joystick_input_mode_toggles_between_two_explicit_modes() {
    // The toggle flips directly between the two modes; there is no hidden
    // auto-detect state.
    assert_eq!(
        JoystickInputMode::Gamepad.next(),
        JoystickInputMode::Keyboard
    );
    assert_eq!(
        JoystickInputMode::Keyboard.next(),
        JoystickInputMode::Gamepad
    );
}

#[test]
fn host_routing_assigns_sources_by_device_and_mode() {
    use super::HostRouting;
    use crate::bus::PortDevice;
    fn routing(
        mouse: Option<usize>,
        gamepad: Option<usize>,
        keyboard: Option<usize>,
        keyboard2: Option<usize>,
    ) -> HostRouting {
        HostRouting {
            mouse,
            gamepad,
            keyboard,
            keyboard2,
        }
    }
    let mut app = test_app();
    let set = |app: &mut super::App, p0: PortDevice, p1: PortDevice| {
        app.emu.bus_mut().input.set_port_device(0, p0);
        app.emu.bus_mut().input.set_port_device(1, p1);
    };

    // Stock wiring (mouse + joystick): the mode picks the single source
    // for port 2 (index 1); the cursor-key mapping owns its keys exactly
    // when some port routes to it.
    set(&mut app, PortDevice::Mouse, PortDevice::Joystick);
    app.joystick_input_mode = JoystickInputMode::Gamepad;
    assert_eq!(app.host_routing(), routing(Some(0), Some(1), None, None));
    assert!(!app.keyboard_mapping_active(0));
    app.joystick_input_mode = JoystickInputMode::Keyboard;
    assert_eq!(app.host_routing(), routing(Some(0), None, Some(1), None));
    assert!(app.keyboard_mapping_active(0));

    // Swapped wiring: the sources follow the devices, wherever they are.
    set(&mut app, PortDevice::Cd32Pad, PortDevice::Mouse);
    app.joystick_input_mode = JoystickInputMode::Gamepad;
    assert_eq!(app.host_routing(), routing(Some(1), Some(0), None, None));
    assert_eq!(app.mouse_port(), Some(1));

    // Two joysticks (two-player): the gamepad -- backed by the numpad
    // mapping -- and the cursor-key mapping drive one each; the mode
    // picks which pair gets the lower-numbered port.
    set(&mut app, PortDevice::Joystick, PortDevice::Joystick);
    assert_eq!(app.host_routing(), routing(None, Some(0), Some(1), Some(0)));
    assert!(app.keyboard_mapping_active(1));
    app.joystick_input_mode = JoystickInputMode::Keyboard;
    assert_eq!(app.host_routing(), routing(None, Some(1), Some(0), Some(1)));
    assert_eq!(app.mouse_port(), None, "no mouse port in a two-stick setup");

    // Two mice: the host mouse takes port 1; the second mouse is
    // keyboard-driven in Keyboard mode and undriven in Gamepad mode (a
    // pad cannot be a pointer, and the keyboard keeps passing through).
    set(&mut app, PortDevice::Mouse, PortDevice::Mouse);
    assert_eq!(app.host_routing(), routing(Some(0), None, Some(1), None));
    app.joystick_input_mode = JoystickInputMode::Gamepad;
    assert_eq!(app.host_routing(), routing(Some(0), None, None, None));

    // No host-drivable port besides the mouse: neither joystick source
    // engages and the keyboard passes through to the Amiga.
    set(&mut app, PortDevice::Mouse, PortDevice::Analogue);
    app.joystick_input_mode = JoystickInputMode::Keyboard;
    assert_eq!(app.host_routing(), routing(Some(0), None, None, None));
    assert!(!app.keyboard_mapping_active(0));
}

#[test]
fn numpad_mapping_stands_in_for_the_missing_gamepad() {
    use crate::bus::PortDevice;
    let mut app = test_app();
    app.emu
        .bus_mut()
        .input
        .set_port_device(0, PortDevice::Joystick);
    app.emu
        .bus_mut()
        .input
        .set_port_device(1, PortDevice::Joystick);
    app.joystick_input_mode = JoystickInputMode::Gamepad;

    // No physical pad in the test rig: the numpad mapping drives the
    // gamepad port (port 1) while the cursor-key mapping drives port 2,
    // so both players work from one keyboard.
    app.keyboard_joy_held[1].up = true;
    app.keyboard_joy_held[0].down = true;
    app.keyboard_joy_held[0].fire_right_ctrl = true;
    app.pump_joystick_input();
    assert!(app.emu.bus().input.ports[0].up, "numpad drives port 1");
    assert!(!app.emu.bus().input.ports[0].down);
    assert!(
        app.emu.bus().input.ports[1].down,
        "cursor keys drive port 2"
    );
    assert!(app.emu.bus().input.ports[1].fire);
}

#[test]
fn keyboard_mouse_drives_a_second_mouse_port() {
    use crate::bus::PortDevice;
    let mut app = test_app();
    app.emu
        .bus_mut()
        .input
        .set_port_device(0, PortDevice::Mouse);
    app.emu
        .bus_mut()
        .input
        .set_port_device(1, PortDevice::Mouse);
    app.joystick_input_mode = JoystickInputMode::Keyboard;

    // The cursor-key mapping drives the second mouse: held directions
    // become steady pointer motion, fire the left button, X the right.
    app.keyboard_joy_held[0].right = true;
    app.keyboard_joy_held[0].fire_right_ctrl = true;
    app.keyboard_joy_held[0].blue = true;
    app.pump_joystick_input();
    assert_eq!(
        app.emu.bus().input.ports[1].counter_x,
        super::KEYBOARD_MOUSE_COUNTS_PER_QUANTUM as u8
    );
    assert!(app.emu.bus().input.ports[1].fire, "fire keys = left button");
    assert!(app.emu.bus().input.ports[1].button2, "X = right button");
    assert_eq!(
        app.emu.bus().input.device(1),
        PortDevice::Mouse,
        "stays a mouse"
    );
    // The host-mouse port is untouched.
    assert_eq!(app.emu.bus().input.ports[0].counter_x, 0);
    assert!(!app.emu.bus().input.ports[0].fire);

    // Releasing the keys releases the buttons on the next pump.
    app.keyboard_joy_held[0] = KeyboardJoystickHeld::default();
    app.pump_joystick_input();
    assert!(!app.emu.bus().input.ports[1].fire);
    assert!(!app.emu.bus().input.ports[1].button2);
}

#[test]
fn hot_plug_drops_scripted_joy_ownership_so_the_new_device_sticks() {
    use crate::bus::PortDevice;
    let mut app = test_app();
    // A --joy-after event has fired and released: the scripted state owns
    // the port until something changes.
    app.auto_joy_engaged[1] = true;
    app.auto_joy_held[1] = super::AutoJoyHeld::default();

    // Hot-plugging a mouse must drop that ownership; otherwise the next
    // input pump would re-assert the scripted state and set_joystick
    // would flip the device straight back to Joystick.
    app.hot_plug_port_device(1, PortDevice::Mouse);
    assert!(!app.auto_joy_engaged[1]);
    app.pump_joystick_input();
    assert_eq!(app.emu.bus().input.device(1), PortDevice::Mouse);
}

#[test]
fn mouse_capture_is_refused_with_no_mouse_on_either_port() {
    use crate::bus::PortDevice;
    let mut app = test_app();
    app.emu
        .bus_mut()
        .input
        .set_port_device(0, PortDevice::Joystick);
    app.emu
        .bus_mut()
        .input
        .set_port_device(1, PortDevice::Joystick);
    assert_eq!(app.mouse_port(), None);
    app.toggle_mouse_capture();
    assert!(!app.mouse_captured, "nothing to capture for");
}

#[test]
fn cycle_port_device_hot_plugs_and_releases_held_lines() {
    use crate::bus::PortDevice;
    let mut app = test_app();
    app.emu
        .bus_mut()
        .input
        .set_port_device(1, PortDevice::Joystick);
    app.emu
        .bus_mut()
        .input
        .set_joystick(1, true, false, false, false, true, false);

    // Joystick -> Cd32Pad -> Analogue -> None -> Mouse, releasing the
    // held fire/direction lines at the first swap.
    app.cycle_port_device(1);
    assert_eq!(app.emu.bus().input.device(1), PortDevice::Cd32Pad);
    assert!(!app.emu.bus().input.ports[1].fire, "hot-plug released fire");
    assert!(!app.emu.bus().input.ports[1].up);
    app.cycle_port_device(1);
    assert_eq!(app.emu.bus().input.device(1), PortDevice::Analogue);
    app.cycle_port_device(1);
    assert_eq!(app.emu.bus().input.device(1), PortDevice::None);
    app.cycle_port_device(1);
    assert_eq!(app.emu.bus().input.device(1), PortDevice::Mouse);
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
        control_connected: false,
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

    copy_present_frame(&src, OUT_HEIGHT, FB_WIDTH, &mut frame, scale);

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
fn present_frame_copy_passes_35ns_canvas_through_on_matching_hidpi_texture() {
    // A double-width (35 ns) canvas whose row equals the HiDPI texture row
    // copies 1:1: adjacent SHRES pixels stay distinct on the glass.
    let scale = 2;
    let src_width = FB_WIDTH * 2;
    let rows = 400usize;
    let mut src = vec![0u32; src_width * rows];
    src[0] = 0x1122_3344;
    src[1] = 0x5566_7788;
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];

    copy_present_frame(&src, rows, src_width, &mut frame, scale);

    assert_eq!(pixel(&frame, 0, 0, 1), src[0].to_le_bytes());
    assert_eq!(pixel(&frame, 1, 0, 1), src[1].to_le_bytes());
}

#[test]
fn present_frame_copy_downmaps_35ns_canvas_on_single_scale_texture() {
    // The same canvas on a non-HiDPI texture maps nearest: each texture
    // pixel samples one of its pair.
    let scale = 1;
    let src_width = FB_WIDTH * 2;
    let rows = 400usize;
    let mut src = vec![0u32; src_width * rows];
    src[0] = 0x1122_3344;
    src[2] = 0x5566_7788;
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];

    copy_present_frame(&src, rows, src_width, &mut frame, scale);

    assert_eq!(pixel(&frame, 0, 0, 1), src[0].to_le_bytes());
    assert_eq!(pixel(&frame, 1, 0, 1), src[2].to_le_bytes());
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
    copy_window_present_frame(
        &src,
        OUT_HEIGHT,
        FB_WIDTH,
        &mut frame,
        scale,
        Overscan::Tv,
        true,
    );

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
    let dst_fb_right = TV_PAL_LIVE_PAD_X + (FB_WIDTH - 1 - TV_PAL_PRESENT_SOURCE_X);
    assert_eq!(
        pixel(&frame, dst_fb_right, 0, scale),
        right_edge.to_le_bytes()
    );
}

#[test]
fn tv_window_copy_black_pads_aperture_past_framebuffer() {
    use crate::video::deinterlace::{OUT_HEIGHT, OUT_PIXELS};
    // The aperture reaches a few columns past the framebuffer's right edge.
    // A display fetching into the deepest right overscan fills the
    // framebuffer's last column; the uncaptured aperture columns are bezel
    // and must stay black instead of replicating that edge column into
    // horizontal streaks (Gen-X logo slide-in).
    let black = rgba(0, 0, 0).to_le_bytes();
    for scale in 1..=3 {
        let mut src = vec![0u32; OUT_PIXELS];
        let row_y = TV_PAL_PRESENT_SOURCE_Y;
        let edge = 0xDDEE_FF00u32;
        src[row_y * FB_WIDTH + FB_WIDTH - 1] = edge;

        let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
        copy_window_present_frame(
            &src,
            OUT_HEIGHT,
            FB_WIDTH,
            &mut frame,
            scale,
            Overscan::Tv,
            true,
        );

        let dst_fb_right = TV_PAL_LIVE_PAD_X + (FB_WIDTH - 1 - TV_PAL_PRESENT_SOURCE_X);
        assert_eq!(
            pixel(&frame, dst_fb_right * scale, 0, scale),
            edge.to_le_bytes(),
            "scale {scale}: framebuffer edge column should stay visible"
        );
        for x in (dst_fb_right + 1) * scale..FB_WIDTH * scale {
            assert_eq!(
                pixel(&frame, x, 0, scale),
                black,
                "scale {scale}: aperture past the framebuffer must be black at {x}"
            );
        }
    }
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
    copy_window_present_frame(
        &src,
        OUT_HEIGHT,
        FB_WIDTH,
        &mut frame,
        scale,
        Overscan::Tv,
        false,
    );

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

    // A standard display shifted left 8px for centring: the bezel
    // moves with it, so the window's left edge is not clipped. (The shift
    // must stay below the source-left bound, 14px with the hardware
    // window edge at 62, for the unmasked strip to remain in-frame.)
    let std_top = standard_window_top_row(STANDARD_PAL_VISIBLE_START_VPOS);
    mask_present_frame_to_tv(&mut fb, 8, std_top);

    let (source_left, source_right) = tv_source_h_bounds();
    let left = source_left - 8;
    let right = source_right - 8;
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
    let mut app = test_app_with_audio(Box::new(NullSink));
    // The stock wiring the config layer applies on a real machine: mouse
    // in port 1, joystick in port 2.
    app.emu
        .bus_mut()
        .input
        .set_port_device(1, crate::bus::PortDevice::Joystick);
    app
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
        mb_ram: Vec::new(),
        accel_ram: Vec::new(),
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
    let emu = Emulator::new(
        bus,
        CpuModel::M68000,
        false,
        Default::default(),
        PacingBudget::Cycles,
        2,
        false,
    )
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
        true,
        crate::sampler::SamplerRequest::default(),
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

/// Every tool-window kind has to appear in `ToolPanelKind::ALL`, because
/// that is what `request_redraw` iterates. The panels that pause the machine
/// (debugger, console) stop the frame loop, and the frame loop is the only
/// other thing that repaints tool windows -- so a kind missing from `ALL` is
/// a window frozen on whatever it last drew. That was issue #236: the console
/// opened blank and stayed blank until a click happened to expose it, and
/// typed characters did not show up until something else forced a repaint.
#[test]
fn every_tool_panel_kind_is_in_the_redraw_pass() {
    for kind in [
        ToolPanelKind::Debugger,
        ToolPanelKind::FrameAnalyzer,
        ToolPanelKind::Console,
    ] {
        // Exhaustive on purpose: a new kind stops compiling here, which is
        // the prompt to add it to the list above and to ALL.
        match kind {
            ToolPanelKind::Debugger | ToolPanelKind::FrameAnalyzer | ToolPanelKind::Console => {}
        }
        assert!(
            ToolPanelKind::ALL.contains(&kind),
            "{kind:?} is not in ToolPanelKind::ALL, so its window never repaints"
        );
    }
}

/// The console pauses the machine, which is what makes the redraw pass the
/// only thing that can repaint its window.
#[test]
fn opening_the_console_pauses_the_machine() {
    let mut app = test_app();
    assert!(!app.paused);
    app.open_console();
    assert!(app.paused);
    app.close_tool_panel(ToolPanelKind::Console);
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
            super::ui::DebugTab::IoMap => {
                // The register grid names DMACON with a live value and
                // the selection pane decodes it.
                assert!(
                    view.lines.iter().any(|l| l.text.contains("DMACON")),
                    "IO map missing DMACON"
                );
                assert!(view
                    .lines
                    .iter()
                    .any(|l| l.highlight && l.text.starts_with("$096")));
            }
            super::ui::DebugTab::Break => {
                assert!(view.lines.iter().any(|l| l.text == "Breakpoints:"));
                assert!(view.lines.iter().any(|l| l.text == "  (none)"));
            }
            super::ui::DebugTab::Waveform => {
                assert!(view.lines.iter().any(|l| l.text == "No waveform capture."));
                assert!(view
                    .lines
                    .iter()
                    .any(|l| l.text.starts_with("Trigger:  NOW")));
            }
        }
    }
}

#[test]
fn waveform_tab_buttons_arm_and_stop_through_dispatch() {
    let mut app = test_app();
    app.open_debugger();
    let path = std::env::temp_dir().join(format!("copperline-wave-tab-{}.vcd", std::process::id()));
    let _ = std::fs::remove_file(&path);
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.tab = super::ui::DebugTab::Waveform;
        panel.entry = format!("{} 200CCK", path.display());
    }
    app.activate_ui_control(super::ui::UiControl::DebugWaveArm);
    let status = app.emu.machine.ui_wave_status().expect("armed capture");
    assert_eq!(status.state, "capturing");
    assert_eq!(status.path, path);
    app.activate_ui_control(super::ui::UiControl::DebugWaveStop);
    assert!(app.emu.machine.ui_wave_status().is_none());
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("$enddefinitions $end"));
    let _ = std::fs::remove_file(&path);
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
    assert!(app.ui_handle_key(KeyCode::Escape, None));
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
        assert!(app.ui_handle_key(key, None));
    }
    assert!(app.ui_handle_key(KeyCode::Enter, None));
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

    assert!(app.ui_handle_key(KeyCode::ArrowRight, None));
    assert!(app.ui_handle_key(KeyCode::ArrowDown, None));
    match app.frame_analyzer_panel.as_ref() {
        Some(panel) => {
            assert_eq!(panel.selected_hpos, start_hpos + 1);
            assert_eq!(panel.selected_vpos, start_vpos + 1);
        }
        _ => panic!("frame analyzer panel should be open"),
    }

    assert!(app.ui_handle_key(KeyCode::ArrowLeft, None));
    assert!(app.ui_handle_key(KeyCode::ArrowUp, None));
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
    assert!(app.ui_handle_key(KeyCode::ArrowLeft, None));
    assert!(app.ui_handle_key(KeyCode::ArrowUp, None));
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
    assert!(app.ui_handle_key(KeyCode::ArrowRight, None));
    assert!(app.ui_handle_key(KeyCode::ArrowDown, None));
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
    assert!(app.ui_handle_key(KeyCode::KeyU, None));
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

#[test]
fn frame_analyzer_scrub_enable_snaps_predisplay_selection_to_frame_end() {
    let mut app = test_app();
    app.open_frame_analyzer();

    // Program a standard PAL display window. The analyzer reads the
    // frame-start register snapshot, so run two frames: the first ends
    // with a pre-write snapshot, the second starts after the writes.
    {
        let bus = app.emu.bus_mut();
        bus.custom_write(0x08E, 2, 0x2C81); // DIWSTRT
        bus.custom_write(0x090, 2, 0x2CC1); // DIWSTOP
    }
    app.frame_analyzer_step_frame();
    app.frame_analyzer_step_frame();
    let (max_vpos, max_hpos) = app
        .emu
        .bus()
        .frame_bus_trace()
        .map(|trace| (trace.rows as u16 - 1, trace.cols as u16 - 1))
        .expect("frame trace armed");

    // A fresh panel's selection sits at the DIW top-left corner, where the
    // CRT has drawn nothing: enabling scrub there would ghost the whole
    // picture, so the selection snaps to the end of the traced frame.
    let panel = app.frame_analyzer_panel.as_ref().unwrap();
    assert_eq!((panel.selected_vpos, panel.selected_hpos), (0x2C, 0x28));
    app.activate_ui_control(UiControl::AnalyzerScrub);
    let panel = app.frame_analyzer_panel.as_ref().unwrap();
    assert!(panel.show_scrub);
    assert_eq!(
        (panel.selected_vpos, panel.selected_hpos),
        (max_vpos, max_hpos)
    );

    // A selection inside the display window is a deliberate scrub point
    // and survives re-enabling scrub.
    app.activate_ui_control(UiControl::AnalyzerScrub);
    if let Some(panel) = app.frame_analyzer_panel.as_mut() {
        panel.selected_vpos = 100;
        panel.selected_hpos = 0x80;
    }
    app.activate_ui_control(UiControl::AnalyzerScrub);
    let panel = app.frame_analyzer_panel.as_ref().unwrap();
    assert!(panel.show_scrub);
    assert_eq!((panel.selected_vpos, panel.selected_hpos), (100, 0x80));
}

#[test]
fn frame_analyzer_scrub_enable_without_display_window_keeps_selection() {
    // No DIWSTRT/DIWSTOP programmed: there is no picture to reveal, so
    // enabling scrub leaves the selection alone.
    let mut app = test_app();
    app.open_frame_analyzer();
    app.frame_analyzer_step_frame();
    app.activate_ui_control(UiControl::AnalyzerScrub);
    let panel = app.frame_analyzer_panel.as_ref().unwrap();
    assert!(panel.show_scrub);
    assert_eq!((panel.selected_vpos, panel.selected_hpos), (0x2C, 0x28));
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

/// Lay a minimal exec world into chip RAM: ExecBase with a valid
/// ChkBase, a scheduled task, one ready task, and one library.
fn plant_exec_world(app: &mut super::App) {
    let bus = app.emu.bus_mut();
    bus.mem.overlay = false;
    let put32 = |ram: &mut [u8], addr: usize, v: u32| {
        ram[addr..addr + 4].copy_from_slice(&v.to_be_bytes());
    };
    let put_str = |ram: &mut [u8], addr: usize, s: &str| {
        ram[addr..addr + s.len()].copy_from_slice(s.as_bytes());
        ram[addr + s.len()] = 0;
    };
    let ram = &mut bus.mem.chip_ram;
    let base = 0x1000usize;
    put32(ram, 4, base as u32);
    put32(ram, base + 0x26, !(base as u32)); // ChkBase complement
                                             // ThisTask -> task at $2000 named "boot.task", state run.
    put32(ram, base + 0x114, 0x2000);
    put32(ram, 0x2000 + 10, 0x3000);
    put_str(ram, 0x3000, "boot.task");
    ram[0x2000 + 9] = 10; // pri
    ram[0x2000 + 15] = 2; // run
                          // TaskReady: one task named "helper" (list head at base+0x196).
    put32(ram, base + 0x196, 0x2100);
    put32(ram, 0x2100, (base + 0x196 + 4) as u32); // succ -> lh_Tail
    put32(ram, base + 0x196 + 4, 0);
    put32(ram, 0x2100 + 10, 0x3100);
    put_str(ram, 0x3100, "helper");
    ram[0x2100 + 15] = 3; // ready
                          // LibList: one library "exec.library" v40.10.
    put32(ram, base + 0x17A, 0x2200);
    put32(ram, 0x2200, (base + 0x17A + 4) as u32);
    put32(ram, base + 0x17A + 4, 0);
    put32(ram, 0x2200 + 10, 0x3200);
    put_str(ram, 0x3200, "exec.library");
    put32(ram, 0x2200 + 20, 0x0028_000A); // v40 r10
}

#[test]
fn console_segments_walks_the_cli_module() {
    let mut app = test_app();
    app.open_console();
    plant_exec_world(&mut app);
    // Make ThisTask ($2000) a CLI process: NT_PROCESS, pr_CLI -> $4000
    // whose cli_Module is a two-hunk seglist at $8000 -> $9000.
    {
        let ram = &mut app.emu.bus_mut().mem.chip_ram;
        let put32 = |ram: &mut [u8], addr: usize, v: u32| {
            ram[addr..addr + 4].copy_from_slice(&v.to_be_bytes());
        };
        ram[0x2000 + 8] = 13; // NT_PROCESS
        put32(ram, 0x2000 + 0xAC, 0x4000 >> 2);
        put32(ram, 0x4000 + 0x3C, 0x8000 >> 2);
        put32(ram, 0x8000 - 4, 0x100);
        put32(ram, 0x8000, 0x9000 >> 2);
        put32(ram, 0x9000 - 4, 0x40);
        put32(ram, 0x9000, 0);
    }
    let out = console_run(&mut app, "SEGMENTS");
    assert!(
        out.iter().any(|l| l.contains("hunk 0: $008004..$0080FC")),
        "{out:?}"
    );
    assert!(
        out.iter().any(|l| l.contains("hunk 1: $009004..$00903C")),
        "{out:?}"
    );
    assert!(
        out.iter()
            .any(|l| l.contains("add-symbol-file") && l.contains("0x8004")),
        "{out:?}"
    );
}

#[test]
fn console_os_introspection_and_task_catch() {
    let mut app = test_app();
    app.open_console();
    plant_exec_world(&mut app);

    let out = console_run(&mut app, "TASKS");
    assert!(
        out.iter()
            .any(|l| l.starts_with('>') && l.contains("boot.task")),
        "{out:?}"
    );
    assert!(
        out.iter()
            .any(|l| l.contains("ready") && l.contains("helper")),
        "{out:?}"
    );
    let out = console_run(&mut app, "LIBS");
    assert!(
        out.iter()
            .any(|l| l.contains("v40.10") && l.contains("exec.library")),
        "{out:?}"
    );
    // An empty exec list reads as empty, not garbage.
    let out = console_run(&mut app, "PORTS");
    assert!(out.iter().any(|l| l.contains("empty")), "{out:?}");

    // Arm the task catch, then reschedule to a matching task: the stop
    // fires with the task's name on the next executed instruction.
    console_run(&mut app, "CATCHTASK HELPER");
    {
        let bus = app.emu.bus_mut();
        let addr = 0x1000 + 0x114;
        bus.mem.chip_ram[addr..addr + 4].copy_from_slice(&0x2100u32.to_be_bytes());
    }
    let out = console_run(&mut app, "S 1");
    assert!(
        out.iter().any(|l| l.contains("Task scheduled: helper")),
        "{out:?}"
    );
    // Clearing disarms it.
    console_run(&mut app, "CATCHTASK");
    assert!(app.emu.machine.ui_breaks().task_catch.is_none());
}

#[test]
fn iomap_tab_navigation_and_jump() {
    let mut app = test_app();
    app.open_debugger();
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.tab = super::ui::DebugTab::IoMap;
    }
    assert_eq!(app.debugger_panel.as_ref().unwrap().iomap_sel, 0x096);
    app.debugger_iomap_move(1);
    assert_eq!(app.debugger_panel.as_ref().unwrap().iomap_sel, 0x098);
    app.debugger_iomap_move(-300); // clamps at the bank start
    assert_eq!(app.debugger_panel.as_ref().unwrap().iomap_sel, 0x000);

    // The $ box jumps by offset or full address.
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.entry = "DFF180".to_string();
        panel.entry_active = true;
    }
    assert!(app.ui_handle_tool_key(ToolPanelKind::Debugger, KeyCode::Enter));
    assert_eq!(app.debugger_panel.as_ref().unwrap().iomap_sel, 0x180);

    let panel = app.debugger_panel.clone().unwrap();
    let view = app.build_debugger_view(&panel);
    assert!(view
        .lines
        .iter()
        .any(|l| l.highlight && l.text.starts_with("$180 COLOR00")));
}

#[test]
fn console_blits_lists_frame_blit_records() {
    let mut app = test_app();
    app.open_console();
    app.open_frame_analyzer(); // arms the frame trace

    // Run a 2x2 D-only blit with blitter DMA enabled, then finish the
    // frame so the trace (with its blit record) becomes current.
    {
        let bus = app.emu.bus_mut();
        bus.mem.overlay = false;
        bus.custom_write(0x096, 2, 0x8240); // DMACON SET DMAEN|BLTEN
        bus.custom_write(0x040, 2, 0x01F0); // BLTCON0: USED, LF=$F0
        bus.custom_write(0x042, 2, 0x0000);
        bus.custom_write(0x044, 2, 0xFFFF);
        bus.custom_write(0x046, 2, 0xFFFF);
        bus.custom_write(0x074, 2, 0xBEEF); // BLTADAT
        bus.custom_write(0x066, 2, 0x0000); // BLTDMOD
        bus.custom_write(0x054, 4, 0x0006_0000); // BLTDPT
        bus.custom_write(0x058, 2, 0x0082); // BLTSIZE: 2 rows x 2 words
    }
    app.frame_analyzer_step_frame();

    let out = console_run(&mut app, "BLITS");
    assert!(out[0].contains("blit(s) in frame"), "{out:?}");
    assert!(
        out.iter()
            .any(|l| l.contains("2x2") && l.contains("con0 01F0")),
        "{out:?}"
    );
    assert!(out.iter().any(|l| l.contains("D $060000")), "{out:?}");
    // The record completed within the frame.
    assert!(
        out.iter()
            .any(|l| l.contains("->") && !l.contains("running")),
        "{out:?}"
    );

    // Selecting a slot inside the blit's beam span annotates it.
    let trace_blit = app
        .emu
        .bus()
        .frame_bus_trace()
        .and_then(|t| t.blits.first().cloned())
        .expect("a recorded blit");
    if let Some(panel) = app.frame_analyzer_panel.as_mut() {
        panel.selected_vpos = trace_blit.start.0;
        panel.selected_hpos = trace_blit.start.1;
    }
    let panel = app.frame_analyzer_panel.clone().unwrap();
    let view = app.build_frame_analyzer_view(&panel);
    let annotated = view.trace.unwrap().selected_blit.expect("blit annotation");
    assert!(annotated.contains("in blit #0"), "{annotated}");
}

#[test]
fn console_hunt_narrows_to_the_changed_word() {
    let mut app = test_app();
    app.open_console();
    app.emu.bus_mut().mem.overlay = false;

    // Plant a "lives counter" and snapshot.
    {
        let ram = &mut app.emu.bus_mut().mem.chip_ram;
        ram[0x60000..0x60002].copy_from_slice(&0x0003u16.to_be_bytes());
    }
    let out = console_run(&mut app, "HUNT START");
    assert!(out[0].contains("hunting 16-bit"), "{out:?}");

    // First filter: everything equal to 3 (the counter plus noise).
    let out = console_run(&mut app, "HUNT EQ 3");
    assert!(out[0].contains("candidate(s) remain"), "{out:?}");

    // "Lose a life", then narrow to values now equal to 2 -- only the
    // counter both was 3 and became 2.
    {
        let ram = &mut app.emu.bus_mut().mem.chip_ram;
        ram[0x60000..0x60002].copy_from_slice(&0x0002u16.to_be_bytes());
    }
    let out = console_run(&mut app, "HUNT EQ 2");
    assert!(out[0].starts_with("1 candidate(s) remain"), "{out:?}");
    let out = console_run(&mut app, "HUNT LIST");
    assert!(out.iter().any(|l| l.contains("$060000 = 0002")), "{out:?}");

    // SAME keeps it (nothing changed since the last filter); DIFF drops it.
    let out = console_run(&mut app, "HUNT SAME");
    assert!(out[0].starts_with("1 candidate"), "{out:?}");
    let out = console_run(&mut app, "HUNT DIFF");
    assert!(out[0].starts_with("0 candidate"), "{out:?}");
    console_run(&mut app, "HUNT OFF");
    assert!(app.hunt.is_none());
}

#[test]
fn console_trace_writes_disassembled_lines() {
    let mut app = test_app();
    app.open_console();
    let path = std::env::temp_dir().join(format!(
        "copperline-console-trace-{}.txt",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

    let out = console_run(&mut app, &format!("TRACE START {}", path.display()));
    assert!(out[0].contains("tracing to"), "{out:?}");
    console_run(&mut app, "S 4");
    let out = console_run(&mut app, "TRACE");
    assert!(out[0].contains("lines so far"), "{out:?}");
    let out = console_run(&mut app, "TRACE STOP");
    assert!(out[0].contains("trace stopped: 4 lines"), "{out:?}");

    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    // Disassembled NOP sled with beam annotations.
    assert!(lines[0].contains("NOP") && lines[0].contains('['), "{text}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn console_wave_arms_captures_and_stops() {
    let mut app = test_app();
    app.open_console();
    let path = std::env::temp_dir().join(format!(
        "copperline-console-wave-{}.vcd",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

    let out = console_run(&mut app, "WAVE");
    assert!(out[0].contains("no waveform capture"), "{out:?}");
    // Arm with order-free arguments: path, immediate trigger, a short
    // window, two signal groups.
    let out = console_run(
        &mut app,
        &format!("WAVE START {} NOW 300CCK BEAM,BUS", path.display()),
    );
    assert!(out[0].contains("waveform armed"), "{out:?}");
    assert!(out[0].contains("beam,bus"), "{out:?}");
    let out = console_run(&mut app, "WAVE");
    assert!(out[0].contains("waveform capturing"), "{out:?}");
    // A malformed trigger is rejected, not treated as a path.
    let out = console_run(&mut app, "WAVE START PC=ZZ");
    assert!(out[0].contains("bad trigger"), "{out:?}");

    // Stepping instructions advances the chipset past the 300 cck window.
    console_run(&mut app, "S 400");
    let out = console_run(&mut app, "WAVE");
    assert!(out[0].contains("waveform done"), "{out:?}");
    let out = console_run(&mut app, "WAVE STOP");
    assert!(out[0].contains("waveform stopped"), "{out:?}");

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("$enddefinitions $end"), "{text}");
    // Only the requested groups are declared.
    assert!(text.contains("$scope module beam $end"));
    assert!(text.contains("$scope module bus $end"));
    assert!(!text.contains("$scope module copper $end"));
    let _ = std::fs::remove_file(&path);

    // A pc= trigger stays armed until the CPU retires the instruction at
    // that address (a few NOPs ahead on the test machine's sled).
    let target = app.emu.machine.pc() + 16;
    let out = console_run(
        &mut app,
        &format!("WAVE START {} PC={target:X} 100CCK", path.display()),
    );
    assert!(out[0].contains("waveform armed"), "{out:?}");
    let out = console_run(&mut app, "WAVE");
    assert!(out[0].contains("waveform armed"), "{out:?}");
    console_run(&mut app, "S 20");
    let out = console_run(&mut app, "WAVE");
    assert!(
        out[0].contains("capturing") || out[0].contains("done"),
        "pc trigger did not fire: {out:?}"
    );
    console_run(&mut app, "WAVE STOP");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn double_fault_halt_surfaces_once() {
    let mut app = test_app();
    app.open_console();
    assert!(!app.surface_debug_stop());
    app.emu.machine.test_force_double_fault();
    // First poll reports and pauses; repeat polls stay quiet.
    assert!(app.surface_debug_stop());
    assert!(app.paused);
    assert!(app
        .last_debug_stop
        .as_deref()
        .is_some_and(|m| m.contains("double fault")));
    assert!(app
        .console_panel
        .as_ref()
        .is_some_and(|panel| panel.output.iter().any(|l| l.contains("double fault"))));
    assert!(!app.surface_debug_stop());
}

#[test]
fn console_catchalert_and_guru_decode() {
    let mut app = test_app();
    app.open_console();
    plant_exec_world(&mut app);

    // CATCHALERT toggles a breakpoint at ExecBase - 108 (Alert's LVO).
    let out = console_run(&mut app, "CATCHALERT");
    assert!(out[0].contains("exec Alert()"), "{out:?}");
    let lvo = 0x1000u32 - 108;
    assert!(app.emu.machine.ui_breaks().is_breakpoint(lvo));
    let out = console_run(&mut app, "CATCHALERT");
    assert!(out[0].contains("removed"), "{out:?}");
    assert!(!app.emu.machine.ui_breaks().is_breakpoint(lvo));

    // GURU decodes an explicit code and defaults to D7.
    let out = console_run(&mut app, "GURU 81000005");
    assert!(out[0].contains("DEADEND exec.library"), "{out:?}");
    console_run(&mut app, "SETREG D7 80000003");
    let out = console_run(&mut app, "GURU");
    assert!(out[0].contains("Address error"), "{out:?}");
}

#[test]
fn console_history_and_stack_walk() {
    let mut app = test_app();
    app.open_console();

    // Stepping with a debug window open records the retired PCs.
    let pc0 = app.emu.machine.pc();
    console_run(&mut app, "S 3");
    let out = console_run(&mut app, "HISTORY 4");
    assert!(
        out.iter()
            .any(|l| l.contains(&format!("{pc0:06X}")) && l.contains("NOP")),
        "{out:?}"
    );
    // The CPU tab mirrors a compact trail.
    app.open_debugger();
    let panel = app.debugger_panel.clone().unwrap();
    let view = app.build_debugger_view(&panel);
    assert!(
        view.lines.iter().any(|l| l.text.starts_with("recent ")),
        "recent-PC line missing"
    );

    // Stack walk: plant a BSR.S and a stack slot holding its return
    // address, then point A7 at it.
    {
        let bus = app.emu.bus_mut();
        bus.mem.overlay = false;
        bus.mem.chip_ram[0x8000..0x8002].copy_from_slice(&0x6104u16.to_be_bytes());
        bus.mem.chip_ram[0x9000..0x9004].copy_from_slice(&0x0000_8002u32.to_be_bytes());
    }
    console_run(&mut app, "SETREG A7 9000");
    let out = console_run(&mut app, "STACK");
    assert!(out[0].starts_with("#0 pc $"), "{out:?}");
    assert!(out.iter().any(|l| l.contains("#1 ret $008002")), "{out:?}");
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
    // Find again continues past the hit. The pattern is CPU-visible again
    // at each Agnus image repeat of the 512 KiB chip RAM (OCS Agnus decodes
    // only A1-A18, so the image recurs every $80000 across the $000000-
    // $1FFFFF chip window), then the search wraps back to the original.
    for expected in [0xE0000, 0x160000, 0x1E0000, 0x60000] {
        app.activate_ui_control(UiControl::DebugMemFind);
        assert_eq!(
            app.debugger_panel.as_ref().unwrap().mem_last_find,
            Some(expected)
        );
    }

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
    assert!(app.ui_handle_key(KeyCode::KeyS, None));
    assert_eq!(app.emu.machine.pc(), pc_before.wrapping_add(2));

    // R toggles run; the explicit choice survives closing the panel.
    assert!(app.paused);
    assert!(app.ui_handle_key(KeyCode::KeyR, None));
    assert!(!app.paused);
    assert!(app.ui_handle_key(KeyCode::KeyR, None));
    assert!(app.paused);

    // On the CPU tab, Enter pins the disassembly origin to the typed
    // address; an empty box follows the PC again.
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.entry_active = true;
        panel.entry = "FC0010".to_string();
    }
    assert!(app.ui_handle_key(KeyCode::Enter, None));
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
    assert!(app.ui_handle_key(KeyCode::Enter, None));
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
    assert!(app.ui_handle_key(KeyCode::KeyS, None));
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

// --- dropped disk images -------------------------------------------------

/// A blank standard ADF written to a unique temp path (floppy inserts read
/// from the filesystem). Callers remove it when done.
fn temp_adf(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("copperline-drop-test-{nanos}-{counter}-{name}"));
    std::fs::write(&path, vec![0u8; crate::floppy::ADF_SIZE]).unwrap();
    path
}

#[test]
fn dropped_media_classifies_by_extension() {
    use super::{classify_dropped_media, DroppedMediaKind};
    let kind = |name: &str| classify_dropped_media(std::path::Path::new(name));
    assert_eq!(kind("game.adf"), DroppedMediaKind::Floppy);
    assert_eq!(kind("game.ADZ"), DroppedMediaKind::Floppy);
    assert_eq!(kind("game.dms"), DroppedMediaKind::Floppy);
    assert_eq!(kind("dump.scp"), DroppedMediaKind::Floppy);
    assert_eq!(kind("game.adf.gz"), DroppedMediaKind::Floppy);
    assert_eq!(kind("game.zip"), DroppedMediaKind::Floppy);
    assert_eq!(kind("mystery"), DroppedMediaKind::Floppy);
    assert_eq!(kind("game.CUE"), DroppedMediaKind::Cd);
    assert_eq!(kind("game.iso"), DroppedMediaKind::Cd);
    assert_eq!(kind("disk.hdf"), DroppedMediaKind::HardDisk);
    assert_eq!(kind("disk.img"), DroppedMediaKind::HardDisk);
    assert_eq!(kind("kick31.rom"), DroppedMediaKind::Rom);
}

#[test]
fn dropped_floppy_with_single_drive_inserts_directly() {
    let mut app = test_app();
    let adf = temp_adf("single.adf");
    app.handle_dropped_files(vec![adf.clone()]);
    assert!(app.emu.bus().floppy.disk_inserted(0));
    assert_eq!(app.disk_playlists[0], vec![adf.clone()]);
    assert!(app.ui.panel.is_none());
    assert!(app.osd.as_ref().unwrap().text.starts_with("DF0:"));
    std::fs::remove_file(&adf).unwrap();
}

#[test]
fn dropped_floppies_with_multiple_drives_open_chooser() {
    let mut app = test_app();
    app.emu
        .bus_mut()
        .floppy
        .set_connected_drives([true, true, false, false]);
    let disks = vec![PathBuf::from("disk1.adf"), PathBuf::from("disk2.adf")];
    app.handle_dropped_files(disks.clone());
    // Nothing inserted yet; the chooser lists exactly the connected drives.
    assert!(!app.emu.bus().floppy.disk_inserted(0));
    match &app.ui.panel {
        Some(Panel::DropChooser(state)) => {
            assert_eq!(state.disks, disks);
            assert_eq!(state.disk_label, "disk1.adf");
            let drives: Vec<usize> = state.drives.iter().map(|e| e.drive).collect();
            assert_eq!(drives, vec![0, 1]);
            assert_eq!(state.drives[0].label, "DF0 (empty)");
        }
        _ => panic!("drop chooser should be open"),
    }
}

#[test]
fn drop_chooser_click_routes_playlist_to_drive() {
    let mut app = test_app();
    app.emu
        .bus_mut()
        .floppy
        .set_connected_drives([true, true, false, false]);
    let disk1 = temp_adf("multi1.adf");
    let disk2 = temp_adf("multi2.adf");
    app.handle_dropped_files(vec![disk1.clone(), disk2.clone()]);
    assert!(matches!(app.ui.panel, Some(Panel::DropChooser(_))));

    app.activate_ui_control(UiControl::DropDrive(1));
    assert!(app.ui.panel.is_none());
    assert!(app.emu.bus().floppy.disk_inserted(1));
    assert!(!app.emu.bus().floppy.disk_inserted(0));
    assert_eq!(app.disk_playlists[1], vec![disk1.clone(), disk2.clone()]);
    assert_eq!(app.disk_playlist_index[1], 0);
    assert!(app.osd.as_ref().unwrap().text.contains("(1/2)"));
    std::fs::remove_file(&disk1).unwrap();
    std::fs::remove_file(&disk2).unwrap();
}

#[test]
fn drop_chooser_escape_cancels_without_insert() {
    let mut app = test_app();
    app.emu
        .bus_mut()
        .floppy
        .set_connected_drives([true, true, false, false]);
    app.handle_dropped_files(vec![PathBuf::from("disk.adf")]);
    assert!(matches!(app.ui.panel, Some(Panel::DropChooser(_))));

    assert!(app.ui_handle_key(KeyCode::Escape, None));
    assert!(app.ui.panel.is_none());
    assert!(!app.emu.bus().floppy.disk_inserted(0));
    assert!(!app.emu.bus().floppy.disk_inserted(1));
}

#[test]
fn drop_chooser_digit_selects_listed_drive() {
    let mut app = test_app();
    // DF0 and DF2 connected: digit 2 must pick the second LISTED drive
    // (DF2), not literal DF1.
    app.emu
        .bus_mut()
        .floppy
        .set_connected_drives([true, false, true, false]);
    let adf = temp_adf("digit.adf");
    app.handle_dropped_files(vec![adf.clone()]);
    assert!(matches!(app.ui.panel, Some(Panel::DropChooser(_))));

    assert!(app.ui_handle_key(KeyCode::Digit2, None));
    assert!(app.ui.panel.is_none());
    assert!(app.emu.bus().floppy.disk_inserted(2));
    std::fs::remove_file(&adf).unwrap();
}

#[test]
fn dropped_hard_disk_shows_notice_only() {
    let mut app = test_app();
    app.handle_dropped_files(vec![PathBuf::from("system.hdf")]);
    assert!(app.ui.panel.is_none());
    assert!(!app.emu.bus().floppy.disk_inserted(0));
    assert!(app.osd.as_ref().unwrap().text.contains("machine screen"));
}

#[test]
fn dropped_cue_without_cd_drive_shows_notice() {
    let mut app = test_app();
    app.handle_dropped_files(vec![PathBuf::from("game.cue")]);
    assert!(app.ui.panel.is_none());
    assert_eq!(
        app.osd.as_ref().map(|osd| osd.text.as_str()),
        Some("No CD drive on this machine")
    );
}

#[test]
fn drop_on_launcher_screen_is_refused() {
    let mut app = test_app();
    app.open_launcher();
    app.handle_dropped_files(vec![PathBuf::from("disk.adf")]);
    // The launcher (and its unsaved state) survives; nothing was inserted.
    assert!(matches!(app.ui.panel, Some(Panel::Launcher(_))));
    assert!(!app.emu.bus().floppy.disk_inserted(0));
    assert!(app.osd.as_ref().unwrap().text.contains("machine screen"));
}

#[test]
fn dropped_files_coalesce_across_events() {
    let mut app = test_app();
    // One DroppedFile event per file lands in the pending list; the batch
    // is then handled as a single action.
    app.pending_dropped_files.push(PathBuf::from("a.adf"));
    app.pending_dropped_files.push(PathBuf::from("b.adf"));
    let files = std::mem::take(&mut app.pending_dropped_files);
    app.handle_dropped_files(files);
    // Single drive connected: both disks become DF0's playlist.
    assert_eq!(app.disk_playlists[0].len(), 2);
}

/// Windowed control-protocol drain tests: a synthetic ControlHandle
/// (channel pair, no sockets) feeds commands straight into the same
/// drain `about_to_wait` runs, against the real App and emulator.
#[cfg(feature = "control")]
mod control_drain {
    use super::test_app;
    use crate::control::exec::parse_method;
    use crate::control::windowed::{ControlHandle, CtlMsg};
    use serde_json::{json, Value};
    use std::sync::mpsc::{Receiver, Sender};

    fn attached_app() -> (super::super::App, Sender<CtlMsg>, Receiver<String>) {
        let mut app = test_app();
        let (handle, cmd_tx, reply_rx) = ControlHandle::test_pair();
        app.attach_control(handle, &crate::control::Config::new(":0".into()));
        (app, cmd_tx, reply_rx)
    }

    fn push(cmd_tx: &Sender<CtlMsg>, id: u64, method: &str, params: Value) {
        let req = parse_method(method, &params).expect("request should parse");
        cmd_tx.send(CtlMsg::Request { id: json!(id), req }).unwrap();
    }

    fn reply(reply_rx: &Receiver<String>) -> Value {
        serde_json::from_str(&reply_rx.try_recv().expect("a reply should be queued"))
            .expect("replies are JSON")
    }

    #[test]
    fn drain_executes_core_ops_and_replies() {
        let (mut app, cmd_tx, reply_rx) = attached_app();
        push(&cmd_tx, 1, "regs.get", Value::Null);
        app.drain_control();
        let msg = reply(&reply_rx);
        assert_eq!(msg["id"], 1);
        assert_eq!(msg["result"]["pc"], app.emu.machine.pc());
    }

    #[test]
    fn continue_completes_on_breakpoint_without_opening_the_debugger() {
        let (mut app, cmd_tx, reply_rx) = attached_app();
        let target = app.emu.machine.pc() + 16; // ahead in the NOP sled
        push(
            &cmd_tx,
            1,
            "break.add",
            json!({"kind": "pc", "addr": target}),
        );
        push(&cmd_tx, 2, "continue", json!({}));
        app.drain_control();
        assert_eq!(reply(&reply_rx)["result"]["id"], 1);
        assert!(!app.paused, "continue unpaused the machine");

        // Mimic the about_to_wait burst: step frames, surface stops.
        let mut stopped = false;
        for _ in 0..3 {
            app.emu.step_frame().unwrap();
            if app.surface_debug_stop() {
                stopped = true;
                break;
            }
        }
        assert!(stopped, "the planted breakpoint should stop the run");
        assert!(app.paused, "a remote stop pauses the machine");
        assert!(
            app.debugger_panel.is_none(),
            "a remote-driven stop must not commandeer the debugger window"
        );
        let stop = reply(&reply_rx);
        assert_eq!(stop["id"], 2);
        assert_eq!(stop["result"]["reason"], "breakpoint");
        assert_eq!(stop["result"]["pc"], target);
    }

    #[test]
    fn run_until_frame_target_completes_in_the_burst_check() {
        let (mut app, cmd_tx, reply_rx) = attached_app();
        let target = app.emu.bus().emulated_frames() + 2;
        push(&cmd_tx, 1, "run_until", json!({"frame": target}));
        app.drain_control();
        assert!(!app.paused);
        let mut completed = false;
        for _ in 0..4 {
            app.emu.step_frame().unwrap();
            if app.surface_debug_stop() {
                break;
            }
            if app.control_run_target_reached() {
                completed = true;
                break;
            }
        }
        assert!(completed, "the frame target should complete the run");
        assert!(app.paused);
        let stop = reply(&reply_rx);
        assert_eq!(stop["result"]["reason"], "target");
        assert!(stop["result"]["frame"].as_u64().unwrap() >= target);
    }

    #[test]
    fn user_pause_completes_a_pending_resume() {
        let (mut app, cmd_tx, reply_rx) = attached_app();
        push(&cmd_tx, 1, "continue", json!({}));
        app.drain_control();
        assert!(!app.paused);
        app.toggle_pause();
        assert!(app.paused);
        let stop = reply(&reply_rx);
        assert_eq!(stop["id"], 1);
        assert_eq!(stop["result"]["reason"], "user_pause");
    }

    #[test]
    fn injected_key_reaches_the_app_recorder() {
        let (mut app, cmd_tx, reply_rx) = attached_app();
        app.input_recorder = Some(crate::inputrec::InputRecorder::new(0.0));
        push(
            &cmd_tx,
            1,
            "input.key",
            json!({"rawkey": 0x45, "action": "press"}),
        );
        app.drain_control();
        let msg = reply(&reply_rx);
        assert!(msg["result"]["applied_at_seconds"].is_number());
        let recorder = app.input_recorder.take().unwrap();
        assert!(
            recorder.events_recorded() > 0,
            "the App recorder journals control-injected input"
        );
    }

    #[test]
    fn connected_arms_time_travel_and_shutdown_requests_exit() {
        let (mut app, cmd_tx, reply_rx) = attached_app();
        assert!(!app.emu.time_travel_enabled());
        cmd_tx.send(CtlMsg::Connected).unwrap();
        app.drain_control();
        assert!(app.emu.time_travel_enabled(), "connect arms the ring");

        cmd_tx.send(CtlMsg::Shutdown { id: json!(9) }).unwrap();
        app.drain_control();
        assert!(app.control_exit_requested());
        let msg = reply(&reply_rx);
        assert_eq!(msg["id"], 9);
    }

    #[test]
    fn step_requires_a_paused_machine_and_advances_it() {
        let (mut app, cmd_tx, reply_rx) = attached_app();
        // test_app starts unpaused: step must refuse.
        push(&cmd_tx, 1, "step", json!({"n": 2}));
        app.drain_control();
        assert_eq!(
            reply(&reply_rx)["error"]["code"],
            crate::control::proto::INVALID_STATE
        );

        app.paused = true;
        let before = app.emu.machine.pc();
        push(&cmd_tx, 2, "step", json!({"n": 2}));
        app.drain_control();
        let stop = reply(&reply_rx);
        assert_eq!(stop["result"]["reason"], "step");
        assert_eq!(stop["result"]["pc"], before + 4, "two NOPs retired");
        assert!(app.paused, "sync steps leave the machine paused");
    }

    #[test]
    fn frame_subscription_streams_from_the_windowed_sampler() {
        let (mut app, cmd_tx, reply_rx) = attached_app();
        push(
            &cmd_tx,
            1,
            "events.subscribe",
            json!({"events": ["frame"], "frame_interval": 1}),
        );
        app.drain_control();
        assert_eq!(reply(&reply_rx)["result"]["active"], json!(["frame"]));

        for _ in 0..3 {
            app.emu.step_frame().unwrap();
            app.control_emit_events();
        }
        let mut notifications = Vec::new();
        while let Ok(line) = reply_rx.try_recv() {
            notifications.push(serde_json::from_str::<Value>(&line).unwrap());
        }
        let notification = notifications
            .last()
            .expect("at least one hardware frame should complete");
        assert_eq!(notification["method"], "event.frame");
        assert_eq!(
            notification["params"]["position"]["frame"],
            app.emu.bus().emulated_frames()
        );
    }
}

#[test]
fn a_missing_rom_reads_as_a_failure_with_a_shortened_path() {
    // A config naming a ROM that is not there must say so, not read like a
    // progress message, and must not run the whole path past the panel. A
    // synthetic NotFound cause keeps this deterministic and off the filesystem.
    let cause = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
    let err = anyhow::Error::new(cause)
        .context("reading ROM /Users/me/Desktop/Amiga/kickstarts/roms/kick31.rom".to_string());

    // The cause is what says it failed, so it must survive; the path is
    // shortened to keep the whole line inside the panel.
    let status = short_status_error(&err);
    assert!(
        status.starts_with("Reading ROM .../roms/kick31.rom:"),
        "{status}"
    );
    assert!(status.contains("no such file"), "{status}");
    assert!(!status.contains("/Users/me/Desktop"), "{status}");
    assert!(status.chars().count() <= 80, "{status}");
}

#[test]
fn status_paths_keep_the_file_name() {
    // Short paths are left alone.
    let short = "unable to read ROM roms/kick.rom";
    assert_eq!(shorten_status_paths(short), short);

    // A long Unix path collapses to its file name.
    let unix = "unable to read ROM /Users/me/Desktop/Amiga/roms/kickstart31.rom";
    let out = shorten_status_paths(unix);
    assert!(out.ends_with("kickstart31.rom"), "{out}");
    assert!(out.contains("..."), "{out}");

    // A long Windows path keeps its separator and file name.
    let win = r"unable to read ROM C:\Users\me\Documents\Amiga\roms\kickstart31.rom";
    let out = shorten_status_paths(win);
    assert!(out.ends_with("kickstart31.rom"), "{out}");
    assert!(out.contains('\\'), "{out}");

    // A path containing spaces is clipped as one span, not split apart into
    // several "..." fragments, and the cause after it survives.
    let spaced = "reading ROM /Users/me/My Amiga Roms/kickstart31.rom: no such file";
    let out = shorten_status_paths(spaced);
    assert!(out.contains("kickstart31.rom"), "{out}");
    assert!(out.ends_with(": no such file"), "{out}");
    assert_eq!(out.matches("...").count(), 1, "{out}");

    // The cause is kept behind the "reading ROM" context (Display's ": " chain).
    let with_cause =
        "unable to read extended ROM /Users/me/Desktop/Amiga/roms/cd32ext.rom: No such file";
    let out = shorten_status_paths(with_cause);
    assert!(out.contains("cd32ext.rom: No such file"), "{out}");
}

// --- windowless capture runs ----------------------------------------------

/// A unique temp path for a windowless-capture output artifact.
fn temp_capture_path(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("copperline-headless-{nanos}-{counter}-{name}"))
}

#[test]
fn windowless_screenshot_run_saves_png_and_exits() {
    let path = temp_capture_path("shot.png");
    let mut app = test_app();
    app.pending_auto_shot = Some((0.04, path.clone()));
    app.run_headless().expect("windowless screenshot run");
    let data = std::fs::read(&path).expect("screenshot file written");
    assert!(
        data.starts_with(b"\x89PNG\r\n\x1a\n"),
        "screenshot should be a PNG, got {} bytes",
        data.len()
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn windowless_frame_dump_run_saves_frames_and_exits() {
    let dir = temp_capture_path("dump");
    std::fs::create_dir_all(&dir).unwrap();
    let mut app = test_app();
    app.pending_frame_dump = Some(super::FrameDumpSpec {
        dir: dir.clone(),
        start_secs: 0.0,
        count: 2,
    });
    app.run_headless().expect("windowless frame dump run");
    assert!(dir.join("frame-000000.png").exists());
    assert!(dir.join("frame-000001.png").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn windowless_run_fires_scheduled_input_and_flushes_recording() {
    let shot = temp_capture_path("input-shot.png");
    let script = temp_capture_path("session.clscript");
    let mut app = test_app();
    app.pending_auto_shot = Some((0.2, shot.clone()));
    app.pending_auto_keys.push(super::KeyPressSpec {
        secs: 0.04,
        rawkey: 0x45,
        hold_ms: 40,
    });
    app.input_recorder = Some(crate::inputrec::InputRecorder::new(0.0));
    app.record_input_path = Some(script.clone());
    app.run_headless()
        .expect("windowless run with scheduled input");
    let text = std::fs::read_to_string(&script).expect("recorded script written");
    assert!(
        text.contains("key-after") && text.contains("0x45"),
        "scheduled key should be recorded: {text}"
    );
    std::fs::remove_file(&shot).ok();
    std::fs::remove_file(&script).ok();
}

#[test]
fn windowless_run_without_captures_errors_instead_of_spinning() {
    let app = test_app();
    assert!(app.run_headless().is_err());
}
