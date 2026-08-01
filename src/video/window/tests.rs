// SPDX-License-Identifier: GPL-3.0-or-later

//! Unit tests for the window/presentation layer: split out of
//! `window.rs` for size, they are the same `window::tests` module
//! and keep full access to the parent's private items via `super::`.

use super::ui::{AnalyzerTab, Panel, UiControl};
use super::{
    bar_layout, center_present_frame_for_visible_start, center_present_frame_horizontally,
    control_at, copperline_icon_image, copperline_logo_image, copy_present_frame,
    copy_window_present_frame, cursor_position_in_texture, draw_status_bar, fdd_track_counter_rect,
    fdd_track_digit_rect, host_shortcut_modifier_pressed, host_to_amiga_rawkey,
    joystick_toggle_rect, led_row_rect, mask_present_frame_to_tv, paint_test_screen,
    parse_amiga_key, pause_button_rect, power_button_rect, present_height,
    presentation_pixels_equal, presentation_source_y_offset, raw_device_qualifier_family_held,
    raw_device_qualifier_rawkey, rawkey_is_held, rawkey_transition_is_duplicate,
    reboot_button_rect, repeated_main_key_should_drop, rgba, short_status_error,
    shorten_status_paths, shot_button_rect, should_render_emulated_frame, standard_window_top_row,
    status_with_latched_fdd_track, take_integral_mouse_delta, texture_height, texture_width,
    tint_display_rows, tint_lut, tint_rows_in_place, tv_aperture_source_row, tv_source_h_bounds,
    volume_percent_from_pos, volume_slider_track_rect, BarControl, DriveBar, JoystickInputMode,
    MediaBar, PresentationLatch, StatusBarView, ToolPanelKind, AMIGA_RAWKEY_LEFT_ALT,
    AMIGA_RAWKEY_LEFT_SHIFT, AMIGA_RAWKEY_RIGHT_ALT, AMIGA_RAWKEY_RIGHT_SHIFT, BUTTON_GLYPH,
    BUTTON_GLYPH_DISABLED, CD_BODY, CD_LED_OFF, CD_LED_ON, DISK_BODY, DISK_BODY_SHADOW, DISK_LABEL,
    FDD_LED_OFF, FDD_LED_ON, HDD_LED_OFF, HDD_LED_ON, POWER_GLYPH_OFF, POWER_GLYPH_ON,
    POWER_LED_BRIGHT, POWER_LED_DIM, POWER_LED_OFF, STANDARD_PAL_VISIBLE_LINES,
    STANDARD_PAL_VISIBLE_START_VPOS, STATUS_BG, TRACK_SEGMENT_OFF, TRACK_SEGMENT_ON,
    TV_CAPTURED_SOURCE_X, TV_LIVE_PAD_X, TV_PAL_PRESENT_HEIGHT, TV_PRESENT_SOURCE_X,
    TV_PRESENT_SOURCE_Y, TV_PRESENT_WIDTH, VOLUME_FILL, VOLUME_GLYPH_X,
};
use crate::audio::{AudioSink, NullSink};
use crate::bus::{FrontPanelStatus, RenderRegisterSnapshot};
use crate::config::{Overscan, Tint, WarpSpeed};
use crate::heatmap;
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
        bridged: false,
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
            bridged: false,
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
fn ui_needs_continuous_redraw_only_when_running_and_active() {
    assert!(!super::ui_needs_continuous_redraw(false, true));
    assert!(super::ui_needs_continuous_redraw(true, true));
    assert!(!super::ui_needs_continuous_redraw(true, false));
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
    assert!(app.keyboard_joy_held[0].is_set(KeyCode::AltRight));
    assert!(!rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_RIGHT_ALT));

    app.handle_raw_device_key_event(RawKeyEvent {
        physical_key: PhysicalKey::Code(KeyCode::AltRight),
        state: ElementState::Released,
    });
    assert!(!app.keyboard_joy_held[0].is_set(KeyCode::AltRight));
    assert!(!rawkey_is_held(&app.held_rawkeys, AMIGA_RAWKEY_RIGHT_ALT));
}

#[test]
fn keyboard_joystick_mapping_matches_fsuae_controls() {
    use crate::keymap::JoyControl as C;
    let map = crate::keymap::KeyMap::default();
    // Mapping 0: the FS-UAE-compatible cursor-key layout.
    for (code, control) in [
        (KeyCode::ArrowUp, C::Up),
        (KeyCode::ArrowDown, C::Down),
        (KeyCode::ArrowLeft, C::Left),
        (KeyCode::ArrowRight, C::Right),
        (KeyCode::ControlRight, C::Fire),
        (KeyCode::AltRight, C::Fire),
        (KeyCode::ControlLeft, C::Fire),
        (KeyCode::AltLeft, C::Button2),
        (KeyCode::KeyC, C::Fire),
        (KeyCode::KeyX, C::Button2),
        (KeyCode::KeyD, C::Green),
        (KeyCode::KeyS, C::Yellow),
        (KeyCode::Enter, C::Play),
        (KeyCode::KeyZ, C::Rewind),
        (KeyCode::KeyA, C::Forward),
    ] {
        assert_eq!(map.lookup(code), Some((0, control)), "{code:?}");
    }
    // Mapping 1: the numpad layout for the second controller, collision
    // free against mapping 0's letters.
    for (code, control) in [
        (KeyCode::Numpad8, C::Up),
        (KeyCode::Numpad2, C::Down),
        (KeyCode::Numpad4, C::Left),
        (KeyCode::Numpad6, C::Right),
        (KeyCode::Numpad0, C::Fire),
        (KeyCode::NumpadDecimal, C::Button2),
        (KeyCode::NumpadEnter, C::Play),
    ] {
        assert_eq!(map.lookup(code), Some((1, control)), "{code:?}");
    }
    assert_eq!(map.lookup(KeyCode::Space), None);
}

#[test]
fn keyboard_joystick_fire_aliases_release_independently() {
    let map = crate::keymap::KeyMap::default();
    let mapping = map.mapping(0);
    let mut held = crate::keymap::HeldKeys::default();
    held.set(KeyCode::ControlRight, true);
    held.set(KeyCode::KeyC, true);
    held.set(KeyCode::ControlLeft, true);
    assert!(mapping.joystick_state(&held).fire);

    held.set(KeyCode::ControlRight, false);
    assert!(mapping.joystick_state(&held).fire);

    held.set(KeyCode::KeyC, false);
    assert!(mapping.joystick_state(&held).fire);

    held.set(KeyCode::ControlLeft, false);
    assert!(!mapping.joystick_state(&held).fire);
}

#[test]
fn keyboard_joystick_second_button_aliases_release_independently() {
    let map = crate::keymap::KeyMap::default();
    let mapping = map.mapping(0);
    let mut held = crate::keymap::HeldKeys::default();
    held.set(KeyCode::KeyX, true);
    held.set(KeyCode::AltLeft, true);
    assert!(mapping.joystick_state(&held).button2);

    held.set(KeyCode::KeyX, false);
    assert!(mapping.joystick_state(&held).button2);

    held.set(KeyCode::AltLeft, false);
    assert!(!mapping.joystick_state(&held).button2);
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
    app.keyboard_joy_held[1].set(KeyCode::Numpad8, true);
    app.keyboard_joy_held[0].set(KeyCode::ArrowDown, true);
    app.keyboard_joy_held[0].set(KeyCode::ControlRight, true);
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
fn autofire_pulses_a_held_fire_button_and_leaves_the_rest_alone() {
    use crate::bus::PortDevice;
    let mut app = test_app();
    app.emu
        .bus_mut()
        .input
        .set_port_device(1, PortDevice::Joystick);
    app.joystick_input_mode = JoystickInputMode::Keyboard;
    app.keyboard_joy_held[0].set(KeyCode::ControlRight, true);
    app.keyboard_joy_held[0].set(KeyCode::ArrowRight, true);
    app.keyboard_joy_held[0].set(KeyCode::KeyX, true);

    // Off: a held fire button is simply held.
    app.autofire_hz = 0;
    app.pump_joystick_input();
    assert!(app.emu.bus().input.ports[1].fire);

    // On: fire alternates as emulated time passes, while the direction and
    // the second button stay steady -- only fire is gated. At 16 Hz a half
    // period is under two PAL frames, so a handful of frames covers several.
    app.autofire_hz = 16;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..10 {
        app.pump_joystick_input();
        seen.insert(app.emu.bus().input.ports[1].fire);
        assert!(app.emu.bus().input.ports[1].right, "direction is not gated");
        assert!(
            app.emu.bus().input.ports[1].button2,
            "the second button is not gated"
        );
        app.emu.step_frame().expect("frame");
    }
    assert_eq!(
        seen.len(),
        2,
        "autofire should both assert and release the fire line"
    );

    // Releasing fire releases it regardless of the autofire phase.
    app.keyboard_joy_held[0].set(KeyCode::ControlRight, false);
    for _ in 0..6 {
        app.pump_joystick_input();
        assert!(!app.emu.bus().input.ports[1].fire);
        app.emu.step_frame().expect("frame");
    }
}

#[test]
fn autofire_does_not_gate_scripted_joystick_input() {
    use crate::bus::PortDevice;
    // A --joy-after / control-protocol run must replay the events it was
    // given, verbatim: autofire is a live-input convenience only.
    let mut app = test_app();
    app.emu
        .bus_mut()
        .input
        .set_port_device(1, PortDevice::Joystick);
    app.autofire_hz = 16;
    app.auto_joy_held[1].set(super::JoyButtonKind::Red, true);
    app.auto_joy_engaged[1] = true;
    for _ in 0..12 {
        app.apply_auto_joy_state(1);
        assert!(app.emu.bus().input.ports[1].fire);
        app.emu.step_frame().expect("frame");
    }
}

#[test]
fn input_mapping_panel_rebinds_only_on_save() {
    // Save writes the per-user keymap.toml, which on a developer's machine is
    // a real file they may have customised; put it back afterwards.
    let path = crate::keymap::keymap_path_for_test();
    let saved = path.as_ref().and_then(|p| std::fs::read(p).ok());

    let mut app = test_app();
    app.open_input_mapping();

    let fire_index = crate::keymap::CONTROLS
        .iter()
        .position(|c| *c == crate::keymap::JoyControl::Fire)
        .unwrap();

    // Arming a row and pressing a key binds it in the panel's working copy
    // only; the live map is untouched until Save.
    app.activate_ui_control(UiControl::RemapBind(fire_index));
    assert!(
        app.ui_handle_key(KeyCode::KeyQ, None),
        "capture eats the key"
    );
    assert_eq!(
        app.keymap.lookup(KeyCode::KeyQ),
        None,
        "the live map must not change before Save"
    );

    app.activate_ui_control(UiControl::RemapSave);
    assert_eq!(
        app.keymap.lookup(KeyCode::KeyQ),
        Some((0, crate::keymap::JoyControl::Fire))
    );
    assert!(app.ui.panel.is_none(), "Save closes the panel");

    // The new binding drives the port.
    app.emu
        .bus_mut()
        .input
        .set_port_device(1, crate::bus::PortDevice::Joystick);
    app.joystick_input_mode = JoystickInputMode::Keyboard;
    assert!(app.handle_keyboard_joystick_key(KeyCode::KeyQ, true));
    assert!(app.emu.bus().input.ports[1].fire);

    // Restore the defaults so the edited map does not leak into the rest of
    // the suite, then put the developer's own file back byte for byte.
    app.open_input_mapping();
    app.activate_ui_control(UiControl::RemapDefaults);
    app.activate_ui_control(UiControl::RemapSave);
    assert_eq!(app.keymap, crate::keymap::KeyMap::default());
    if let Some(path) = path {
        match saved {
            Some(bytes) => std::fs::write(&path, bytes).unwrap(),
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

#[test]
fn input_mapping_panel_discards_edits_when_closed() {
    let mut app = test_app();
    let before = app.keymap.clone();
    app.open_input_mapping();
    app.activate_ui_control(UiControl::RemapBind(0));
    app.ui_handle_key(KeyCode::KeyP, None);
    // Escape while a row is armed cancels the binding, not the panel.
    app.activate_ui_control(UiControl::RemapBind(0));
    assert!(app.ui_handle_key(KeyCode::Escape, None));
    assert!(app.ui.panel.is_some(), "Escape cancelled the capture only");

    app.activate_ui_control(UiControl::PanelClose);
    assert!(app.ui.panel.is_none());
    assert_eq!(app.keymap, before, "closing discards the edits");
}

#[test]
fn input_mapping_panel_clears_and_switches_mappings() {
    let mut app = test_app();
    app.open_input_mapping();
    let Some(Panel::InputMap(panel)) = app.ui.panel.as_ref() else {
        panic!("panel open");
    };
    assert_eq!(panel.mapping, 0);

    app.activate_ui_control(UiControl::RemapClear(0));
    let Some(Panel::InputMap(panel)) = app.ui.panel.as_ref() else {
        panic!("panel open");
    };
    assert_eq!(
        panel
            .map
            .mapping(0)
            .binding_text(crate::keymap::CONTROLS[0]),
        "-"
    );

    // Switching mapping leaves the other one's edits alone.
    app.activate_ui_control(UiControl::RemapSet(1));
    let Some(Panel::InputMap(panel)) = app.ui.panel.as_ref() else {
        panic!("panel open");
    };
    assert_eq!(panel.mapping, 1);
    assert!(panel.capturing.is_none(), "switching disarms the capture");
    assert_ne!(
        panel
            .map
            .mapping(1)
            .binding_text(crate::keymap::CONTROLS[0]),
        "-"
    );
}

#[test]
fn menu_scroll_controls_move_the_list_without_closing_the_menu() {
    let mut app = test_app();
    app.activate_bar_control(super::BarControl::Menu);
    assert!(app.ui.menu_open);
    assert_eq!(app.ui.menu_scroll, 0, "each open starts at the top");

    // The real menu fits, so there is nothing to scroll -- but the controls
    // must be inert rather than wrong, and must never close the menu.
    app.activate_ui_control(UiControl::MenuScrollDown);
    assert_eq!(app.ui.menu_scroll, 0);
    app.activate_ui_control(UiControl::MenuScrollUp);
    assert_eq!(app.ui.menu_scroll, 0);
    assert!(app.ui.menu_open, "scrolling never dismisses the menu");

    // A scroll position left over from a previous open is cleared, so the
    // list never reappears part-way down.
    app.ui.menu_scroll = 3;
    app.activate_bar_control(super::BarControl::Menu); // close
    app.activate_bar_control(super::BarControl::Menu); // open again
    assert_eq!(app.ui.menu_scroll, 0);

    // Choosing an item still closes the menu.
    app.activate_ui_control(UiControl::MenuItem(super::ui::MenuItem::Autofire));
    assert!(!app.ui.menu_open);
}

#[test]
fn autofire_menu_item_cycles_the_rate() {
    let mut app = test_app();
    assert_eq!(app.autofire_hz, 0);
    app.activate_ui_control(UiControl::MenuItem(super::ui::MenuItem::Autofire));
    assert_eq!(app.autofire_hz, crate::config::AUTOFIRE_RATES[1]);
    for _ in 0..crate::config::AUTOFIRE_RATES.len() - 1 {
        app.activate_ui_control(UiControl::MenuItem(super::ui::MenuItem::Autofire));
    }
    assert_eq!(app.autofire_hz, 0, "the cycle returns to off");
}

#[test]
fn crt_shader_menu_item_cycles_the_presets() {
    use crate::config::ShaderKind;
    let mut app = test_app();
    assert_eq!(app.crt_shader_kind, ShaderKind::None);
    for expected in [
        ShaderKind::Scanlines,
        ShaderKind::Mask,
        ShaderKind::Crt,
        // No user shader is configured, so the cycle skips Custom and
        // returns to off rather than selecting a shader it cannot load.
        ShaderKind::None,
    ] {
        app.activate_ui_control(UiControl::MenuItem(super::ui::MenuItem::CrtShader));
        assert_eq!(app.crt_shader_kind, expected);
    }
}

#[test]
fn a_broken_custom_shader_never_becomes_the_selected_preset() {
    use crate::config::ShaderKind;
    let mut app = test_app();
    // Configured but unloadable: the fixture has no window, so there is no
    // device to compile it against.
    app.custom_shader_path = Some(std::path::PathBuf::from("/nonexistent/nope.wgsl"));
    for _ in 0..3 {
        app.activate_ui_control(UiControl::MenuItem(super::ui::MenuItem::CrtShader));
    }
    assert_eq!(app.crt_shader_kind, ShaderKind::Crt);
    // Custom is offered next, fails to load, and the cycle falls through to
    // off rather than selecting a preset that would draw nothing.
    app.activate_ui_control(UiControl::MenuItem(super::ui::MenuItem::CrtShader));
    assert_eq!(app.crt_shader_kind, ShaderKind::None);
}

/// The pass draws one scanline per emulated field line the present copy
/// actually shows, which is not `present_rows / 2` in the default TV
/// presentation: that path crops to the fixed 540-row aperture (270 lines)
/// and stretches it over the whole 537-row rect.
#[test]
fn crt_scanline_count_follows_what_the_present_copy_shows() {
    use super::crt_scanline_count;
    use crate::video::{PRESENT_HEIGHT_SQUARE, PRESENT_HEIGHT_TV};
    let woven = crate::video::deinterlace::OUT_HEIGHT;

    // TV aspect + TV aperture (the out-of-the-box config): 540 aperture rows
    // = 270 lines, filling the rect.
    assert_eq!(
        crt_scanline_count(woven, PRESENT_HEIGHT_TV, Some(TV_PAL_PRESENT_HEIGHT)),
        (TV_PAL_PRESENT_HEIGHT / 2) as f32
    );
    // Full overscan takes the plain copy, which shows every woven row.
    assert_eq!(
        crt_scanline_count(woven, PRESENT_HEIGHT_TV, None),
        (woven / 2) as f32
    );
    // The square canvas is taller than the aperture and pads it with bezel
    // rows, so the same 270 lines cover only 540 of 570 rect rows: the pitch
    // across the whole viewport scales up to match.
    assert_eq!(
        crt_scanline_count(woven, PRESENT_HEIGHT_SQUARE, Some(TV_PAL_PRESENT_HEIGHT)),
        270.0 * PRESENT_HEIGHT_SQUARE as f32 / TV_PAL_PRESENT_HEIGHT as f32
    );
    assert_eq!(
        crt_scanline_count(woven, PRESENT_HEIGHT_SQUARE, Some(TV_PAL_PRESENT_HEIGHT)),
        285.0
    );
    assert_eq!(
        crt_scanline_count(woven, PRESENT_HEIGHT_SQUARE, None),
        (woven / 2) as f32
    );
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
    app.keyboard_joy_held[0].set(KeyCode::ArrowRight, true);
    app.keyboard_joy_held[0].set(KeyCode::ControlRight, true);
    app.keyboard_joy_held[0].set(KeyCode::KeyX, true);
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
    app.keyboard_joy_held[0] = crate::keymap::HeldKeys::default();
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

/// Uncaptured host motion used to reach the port as raw texture-space
/// pixels, bypassing the sensitivity scale that captured motion goes
/// through -- so the setting silently did nothing until the display was
/// clicked, and the mouse changed feel the moment it was grabbed.
#[test]
fn uncaptured_cursor_motion_honours_the_sensitivity_setting() {
    // The counters are 8-bit quadrature; read the port the host mouse
    // drives before and after to get the applied motion.
    fn drag_x(app: &mut super::App, from: i32, to: i32) -> u8 {
        let port = app.mouse_port().expect("fixture has a mouse in port 1");
        // The first sample only anchors the baseline; the second is the
        // one that produces a delta.
        app.track_uncaptured_cursor_motion(Some((from, 64)));
        let before = app.emu.bus().input.ports[port].counter_x;
        app.track_uncaptured_cursor_motion(Some((to, 64)));
        app.emu.bus().input.ports[port]
            .counter_x
            .wrapping_sub(before)
    }

    // Default sensitivity is the neutral midpoint, where the factor is
    // exactly 1.0: the historical 1:1 tracking is unchanged.
    let mut app = test_app();
    assert_eq!(app.mouse_sensitivity, 50);
    assert_eq!(drag_x(&mut app, 100, 110), 10);

    // Turned up, the same host travel moves the pointer further.
    let mut app = test_app();
    app.set_mouse_sensitivity(100);
    assert_eq!(drag_x(&mut app, 100, 110), 40);

    // Turned down, less -- and the fractional remainder is carried, not
    // dropped, so slow motion still accumulates instead of vanishing.
    let mut app = test_app();
    app.set_mouse_sensitivity(0);
    assert_eq!(drag_x(&mut app, 100, 110), 2);
    assert!(
        app.mouse_delta_remainder.0 > 0.0,
        "the 0.5-count remainder is kept for the next motion"
    );
}

/// A tool window borrows the host cursor while it is open. It used to
/// release the capture outright and never give it back, so every trip to
/// the debugger left the machine uncaptured for good -- worst in
/// fullscreen, where there is no desktop to reach for anyway.
#[test]
fn a_tool_panel_hands_the_mouse_capture_back_when_it_closes() {
    let mut app = test_app();
    // The windowless fixture cannot take a real grab -- set_mouse_captured
    // needs a window to call set_cursor_grab on -- so stage the captured
    // state the shortcut would have found. What is under test is the
    // bookkeeping that decides whether to re-grab, not winit's grab.
    app.mouse_captured = true;
    app.main_window_focused = true;

    app.open_debugger();
    assert!(
        app.capture_suspended_by_ui,
        "the panel noted that it borrowed a live capture"
    );

    app.close_tool_panel(ToolPanelKind::Debugger);
    assert!(
        !app.capture_suspended_by_ui,
        "and handed it back on the way out"
    );
}

#[test]
fn the_capture_stays_suspended_while_another_panel_wants_the_cursor() {
    let mut app = test_app();
    app.mouse_captured = true;
    app.main_window_focused = true;

    app.open_debugger();
    app.open_frame_analyzer();
    app.close_tool_panel(ToolPanelKind::Debugger);
    assert!(
        app.capture_suspended_by_ui,
        "the analyzer still needs the cursor"
    );

    app.close_tool_panel(ToolPanelKind::FrameAnalyzer);
    assert!(
        !app.capture_suspended_by_ui,
        "the last panel out returns the capture"
    );
}

/// The restore is owed only to a capture the UI actually took: a session
/// that was never captured must not find the cursor grabbed because it
/// happened to open the debugger.
#[test]
fn closing_a_panel_does_not_grab_a_mouse_that_was_never_captured() {
    let mut app = test_app();
    assert!(!app.mouse_captured);

    app.open_debugger();
    assert!(!app.capture_suspended_by_ui, "nothing was borrowed");

    app.close_tool_panel(ToolPanelKind::Debugger);
    assert!(!app.mouse_captured, "so nothing is handed back");
}

/// Closing a tool window hands the focus back to the main window, but the
/// order of that against the close is the window manager's business. A grab
/// attempted while the focus is still elsewhere fails, and discharging the
/// loan on a failed grab would lose the capture for good -- the exact bug
/// this mechanism exists to prevent. The loan stays outstanding until a
/// focused moment can repay it.
#[test]
fn a_capture_loan_outlives_a_panel_that_closed_while_unfocused() {
    let mut app = test_app();
    app.mouse_captured = true;
    app.main_window_focused = true;
    app.open_debugger();
    assert!(app.capture_suspended_by_ui);

    // The tool window still holds the focus as the panel goes away.
    app.main_window_focused = false;
    app.close_tool_panel(ToolPanelKind::Debugger);
    assert!(
        app.capture_suspended_by_ui,
        "the loan survives a close that could not repay it"
    );

    // The Focused(true) that follows is what actually repays it.
    app.main_window_focused = true;
    app.restore_mouse_capture_after_ui();
    assert!(
        !app.capture_suspended_by_ui,
        "and is discharged once the window can take the grab"
    );
}

/// A loan no later event could repay is void rather than outstanding: with
/// no mouse left on a port there is nothing to capture for.
#[test]
fn a_capture_loan_is_dropped_when_no_port_holds_a_mouse() {
    use crate::bus::PortDevice;
    let mut app = test_app();
    app.mouse_captured = true;
    app.main_window_focused = true;
    app.open_debugger();
    assert!(app.capture_suspended_by_ui);

    app.emu
        .bus_mut()
        .input
        .set_port_device(0, PortDevice::Joystick);
    assert_eq!(app.mouse_port(), None);

    app.close_tool_panel(ToolPanelKind::Debugger);
    assert!(
        !app.capture_suspended_by_ui,
        "nothing to drive, so the loan is void rather than held forever"
    );
}

/// An explicit Cmd/Alt+G settles the question: the operator asked for the
/// capture to be off, and a panel closing later must not overrule that.
#[test]
fn an_explicit_toggle_clears_a_pending_ui_suspension() {
    let mut app = test_app();
    app.mouse_captured = true;
    app.main_window_focused = true;
    app.open_console();
    assert!(app.capture_suspended_by_ui);

    // The console does not count as modal UI, so the shortcut still
    // reaches the toggle while it is open.
    app.mouse_captured = false;
    app.toggle_mouse_capture();
    assert!(
        !app.capture_suspended_by_ui,
        "the operator's own toggle wins over the borrowed capture"
    );

    app.close_tool_panel(ToolPanelKind::Console);
    assert!(!app.mouse_captured, "closing the panel does not re-grab");
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
fn canvas_sized_check_tolerates_rounding_but_not_a_resize() {
    use super::{logical_size_is_canvas, FB_WIDTH};
    let canvas_h = 600;
    // Exact and sub-pixel-off both count as canvas-sized.
    assert!(logical_size_is_canvas(
        FB_WIDTH as f64,
        canvas_h as f64,
        canvas_h
    ));
    assert!(logical_size_is_canvas(
        FB_WIDTH as f64 + 1.0,
        canvas_h as f64 - 1.0,
        canvas_h
    ));
    // A real resize in either dimension does not.
    assert!(!logical_size_is_canvas(
        FB_WIDTH as f64 + 40.0,
        canvas_h as f64,
        canvas_h
    ));
    assert!(!logical_size_is_canvas(
        FB_WIDTH as f64,
        canvas_h as f64 + 40.0,
        canvas_h
    ));
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
                power_led_bright: true,
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
        POWER_LED_BRIGHT.to_le_bytes()
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
                power_led_bright: false,
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
fn power_led_is_bright_dim_or_off_by_the_led_line() {
    let scale = 1;
    let power = led_row_rect(0, 2);
    let center = |frame: &[u8]| pixel(frame, power.x + power.w / 2, power.y + power.h / 2, scale);
    let render = |led_engaged: bool, powered: bool| {
        let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
        draw_status_bar(
            &mut frame,
            &view(
                FrontPanelStatus {
                    power_led_bright: led_engaged,
                    fdd_led_on: false,
                    fdd_track: Some(0),
                    hdd_led: None,
                    cd_led: None,
                    output_volume_percent: 100,
                },
                powered,
                false,
            ),
            scale,
        );
        center(&frame)
    };
    // Powered: full brightness while the guest holds /LED engaged, dimmed
    // -- still lit -- once it releases it, as on A500 rev 6+ boards.
    assert_eq!(render(true, true), POWER_LED_BRIGHT.to_le_bytes());
    assert_eq!(render(false, true), POWER_LED_DIM.to_le_bytes());
    // Unpowered: dark regardless of the /LED line.
    assert_eq!(render(true, false), POWER_LED_OFF.to_le_bytes());
}

#[test]
fn status_bar_extinguishes_power_led_when_host_power_is_off() {
    let scale = 1;
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    draw_status_bar(
        &mut frame,
        &view(
            FrontPanelStatus {
                power_led_bright: true,
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
                    power_led_bright: powered_on,
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
                power_led_bright: true,
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
                power_led_bright: true,
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
                power_led_bright: true,
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
fn status_bar_greys_swap_and_eject_on_a_bridged_drive() {
    // A real drive's disk is loaded and ejected by hand, so the buttons stay
    // drawn -- the drive is still visibly there, and numbered -- but dim,
    // because there is nothing here for them to do.
    let scale = 1;
    let mut bar = single_drive_media();
    bar.drives[0].multi = true;
    bar.drives[0].inserted = true;
    bar.drives[0].bridged = true;
    let layout = bar_layout(&bar);
    let swap = layout.drive_swap[0].expect("a bridged drive still shows its buttons");
    let eject = layout.drive_eject[0].expect("a bridged drive still shows its buttons");
    assert!(
        layout.drive_load[0].is_some(),
        "the disk icon stays, so the drive reads as present"
    );

    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    let mut v = view(FrontPanelStatus::default(), true, false);
    v.media = bar;
    draw_status_bar(&mut frame, &v, scale);
    assert_eq!(
        pixel(&frame, swap.x + 5, swap.y + 8, scale),
        BUTTON_GLYPH_DISABLED.to_le_bytes(),
        "swap is dim on a bridged drive even with a playlist"
    );
    assert_eq!(
        pixel(&frame, eject.x + 5, eject.y + 15, scale),
        BUTTON_GLYPH_DISABLED.to_le_bytes(),
        "eject is dim on a bridged drive even with a disk in"
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
                power_led_bright: true,
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
            power_led_bright: true,
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
            power_led_bright: true,
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
                power_led_bright: true,
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
        POWER_LED_BRIGHT.to_le_bytes()
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
fn exact_presentation_repeat_ignores_unused_capacity_but_not_pixels_or_geometry() {
    let current = vec![1, 2, 3, 4, 0xAAAA, 0xBBBB];
    let mut next = vec![1, 2, 3, 4, 0xCCCC, 0xDDDD];
    assert!(presentation_pixels_equal(&current, 2, 2, &next, 2, 2));

    next[3] ^= 1;
    assert!(!presentation_pixels_equal(&current, 2, 2, &next, 2, 2));
    assert!(!presentation_pixels_equal(&current, 2, 2, &current, 1, 4));
    assert!(!presentation_pixels_equal(&current, 2, 2, &current, 2, 3));
    assert!(!presentation_pixels_equal(
        &current[..3],
        2,
        2,
        &next[..3],
        2,
        2
    ));
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
fn tint_off_builds_no_lut() {
    assert!(tint_lut(Tint::None).is_none());
}

#[test]
fn bw_tint_lut_is_the_identity_on_grey() {
    let lut = tint_lut(Tint::Bw).unwrap();
    for level in 0..=255u32 {
        assert_eq!(lut[level as usize], rgba(level, level, level));
    }
}

/// Every tint is a phosphor-style ramp: black stays black, the top end is
/// bright, alpha stays opaque, and no channel dips along the way (beyond
/// the one-step wobble the CSS chain's inter-stage clamping can produce).
#[test]
fn tint_luts_ramp_from_black_to_bright() {
    for tint in [Tint::Bw, Tint::Green, Tint::Amber, Tint::Sepia] {
        let lut = tint_lut(tint).unwrap();
        assert_eq!(lut[0], rgba(0, 0, 0), "{tint:?} must map black to black");
        let mut prev = [0u8; 4];
        for (level, px) in lut.iter().enumerate() {
            let cur = px.to_le_bytes();
            assert_eq!(cur[3], 0xFF, "{tint:?} level {level} must stay opaque");
            for c in 0..3 {
                assert!(
                    cur[c].saturating_add(2) >= prev[c],
                    "{tint:?} channel {c} dips at level {level}: {} -> {}",
                    prev[c],
                    cur[c]
                );
            }
            prev = cur;
        }
        let top = lut[255].to_le_bytes();
        assert!(
            top[..3].iter().any(|&v| v >= 200),
            "{tint:?} top of the ramp is dim: {top:?}"
        );
    }
}

/// Each tint's mid-grey lands on its nominal hue: green phosphor is
/// green-dominant, amber and sepia run warm (r > g > b).
#[test]
fn tint_luts_favour_their_phosphor_colour() {
    let mid = |tint| tint_lut(tint).unwrap()[128].to_le_bytes();

    let [r, g, b, _] = mid(Tint::Green);
    assert!(
        g > r.saturating_add(50) && g > b.saturating_add(50),
        "green mid: {r} {g} {b}"
    );

    let [r, g, b, _] = mid(Tint::Amber);
    assert!(
        r > g.saturating_add(50) && g > b.saturating_add(50),
        "amber mid: {r} {g} {b}"
    );

    let [r, g, b, _] = mid(Tint::Sepia);
    assert!(r > g && g > b, "sepia mid: {r} {g} {b}");
}

/// The per-pixel step collapses a colour to its luma and takes the ramp
/// entry: a pure-green pixel and the grey of its luma tint identically,
/// and the alpha byte passes through.
#[test]
fn tint_rows_collapse_colour_to_luma() {
    let lut = tint_lut(Tint::Green).unwrap();
    let luma_of_green = 182u8; // (183 * 255) >> 8: the green weight on a full channel
    let mut px = [
        rgba(0, 255, 0).to_le_bytes(),
        rgba(
            u32::from(luma_of_green),
            u32::from(luma_of_green),
            u32::from(luma_of_green),
        )
        .to_le_bytes(),
    ]
    .concat();
    tint_rows_in_place(&mut px, &lut);
    let expected = lut[usize::from(luma_of_green)].to_le_bytes();
    assert_eq!(&px[0..4], &expected);
    assert_eq!(&px[4..8], &expected);
}

/// The in-place pass covers exactly the display region of the composited
/// frame: the status-bar rows below it keep their pixels.
#[test]
fn tint_display_rows_leave_the_status_bar_alone() {
    let scale = 1;
    let lut = tint_lut(Tint::Bw).unwrap();
    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    let saturated = rgba(255, 0, 0).to_le_bytes();
    for px in frame.chunks_exact_mut(4) {
        px.copy_from_slice(&saturated);
    }
    tint_display_rows(&mut frame, scale, &lut);
    let display_bytes = present_height() * scale * texture_width(scale) * 4;
    let grey_red = lut[usize::from(53u8)].to_le_bytes(); // (54 * 255) >> 8
    for px in frame[..display_bytes].chunks_exact(4) {
        assert_eq!(px, grey_red, "display pixel left untinted");
    }
    for px in frame[display_bytes..].chunks_exact(4) {
        assert_eq!(px, saturated, "status-bar pixel was tinted");
    }
}

#[test]
fn tv_window_copy_centres_captured_aperture_in_live_texture() {
    use crate::video::deinterlace::{OUT_HEIGHT, OUT_PIXELS};
    let scale = 1;
    let mut src = vec![0u32; OUT_PIXELS];
    let row_y = TV_PRESENT_SOURCE_Y;
    let standard_left = crate::video::bitplane::STANDARD_VISIBLE_X0;
    let standard_right = standard_left + 320 * 2 - 1;
    let left_marker = 0x1122_3344u32;
    let right_marker = 0x5566_7788u32;
    let left_edge = 0x99AA_BBCCu32;
    let right_edge = 0xDDEE_FF00u32;

    src[row_y * FB_WIDTH + TV_CAPTURED_SOURCE_X] = left_edge;
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
        Some(TV_PAL_PRESENT_HEIGHT),
    );

    // The standard window and the visible raster are both exactly centred:
    // the captured aperture is symmetric around the standard window and
    // ends on the framebuffer's edges, so the crop's first and last
    // columns land mirror-imaged around the texture centre.
    let dst_standard_left = TV_LIVE_PAD_X + (standard_left - TV_CAPTURED_SOURCE_X);
    let dst_standard_right = dst_standard_left + 320 * 2 - 1;
    assert_eq!(dst_standard_left, FB_WIDTH - 1 - dst_standard_right);
    let dst_crop_left = TV_LIVE_PAD_X;
    let dst_crop_right = TV_LIVE_PAD_X + (FB_WIDTH - 1 - TV_CAPTURED_SOURCE_X);
    assert_eq!(dst_crop_left, FB_WIDTH - 1 - dst_crop_right);
    assert_eq!(
        pixel(&frame, dst_standard_left, 0, scale),
        left_marker.to_le_bytes()
    );
    assert_eq!(
        pixel(&frame, dst_standard_right, 0, scale),
        right_marker.to_le_bytes()
    );
    assert_eq!(
        pixel(&frame, dst_crop_left, 0, scale),
        left_edge.to_le_bytes()
    );
    assert_eq!(
        pixel(&frame, dst_crop_right, 0, scale),
        right_edge.to_le_bytes()
    );
}

#[test]
fn tv_window_copy_black_pads_never_replicate_edge_columns() {
    use crate::video::deinterlace::{OUT_HEIGHT, OUT_PIXELS};
    // The pads beside the captured aperture are off-capture bezel. A
    // display fetching or parking sprites in the deepest overscan fills
    // the crop's edge columns; the pads must stay black instead of
    // replicating those columns into horizontal streaks (Gen-X logo
    // slide-in).
    let black = rgba(0, 0, 0).to_le_bytes();
    for scale in 1..=3 {
        let mut src = vec![0u32; OUT_PIXELS];
        let row_y = TV_PRESENT_SOURCE_Y;
        let left_edge = 0x99AA_BBCCu32;
        let right_edge = 0xDDEE_FF00u32;
        src[row_y * FB_WIDTH + TV_CAPTURED_SOURCE_X] = left_edge;
        src[row_y * FB_WIDTH + FB_WIDTH - 1] = right_edge;

        let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
        copy_window_present_frame(
            &src,
            OUT_HEIGHT,
            FB_WIDTH,
            &mut frame,
            scale,
            Overscan::Tv,
            Some(TV_PAL_PRESENT_HEIGHT),
        );

        let dst_crop_left = TV_LIVE_PAD_X;
        let dst_crop_right = TV_LIVE_PAD_X + (FB_WIDTH - 1 - TV_CAPTURED_SOURCE_X);
        assert_eq!(
            pixel(&frame, dst_crop_left * scale, 0, scale),
            left_edge.to_le_bytes(),
            "scale {scale}: crop's first column should stay visible"
        );
        assert_eq!(
            pixel(&frame, dst_crop_right * scale, 0, scale),
            right_edge.to_le_bytes(),
            "scale {scale}: framebuffer edge column should stay visible"
        );
        for x in 0..dst_crop_left * scale {
            assert_eq!(
                pixel(&frame, x, 0, scale),
                black,
                "scale {scale}: left pad must be black at {x}"
            );
        }
        for x in (dst_crop_right + 1) * scale..FB_WIDTH * scale {
            assert_eq!(
                pixel(&frame, x, 0, scale),
                black,
                "scale {scale}: right pad must be black at {x}"
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
    src[TV_CAPTURED_SOURCE_X] = standard_crop_edge;

    let mut frame = vec![0u8; texture_width(scale) * texture_height(scale) * 4];
    copy_window_present_frame(
        &src,
        OUT_HEIGHT,
        FB_WIDTH,
        &mut frame,
        scale,
        Overscan::Tv,
        None,
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
    assert_eq!(
        tv_aperture_source_row(0, PRESENT_HEIGHT_TV, 1, TV_PAL_PRESENT_HEIGHT),
        Some(0)
    );
    assert_eq!(
        tv_aperture_source_row(
            PRESENT_HEIGHT_TV - 1,
            PRESENT_HEIGHT_TV,
            1,
            TV_PAL_PRESENT_HEIGHT
        ),
        Some(TV_PAL_PRESENT_HEIGHT - 1)
    );
    // Square canvas: black bezel bands centre the aperture and its
    // rows map 1:1.
    let pad = (PRESENT_HEIGHT_SQUARE - TV_PAL_PRESENT_HEIGHT) / 2;
    assert_eq!(
        tv_aperture_source_row(0, PRESENT_HEIGHT_SQUARE, 1, TV_PAL_PRESENT_HEIGHT),
        None
    );
    assert_eq!(
        tv_aperture_source_row(pad - 1, PRESENT_HEIGHT_SQUARE, 1, TV_PAL_PRESENT_HEIGHT),
        None
    );
    assert_eq!(
        tv_aperture_source_row(pad, PRESENT_HEIGHT_SQUARE, 1, TV_PAL_PRESENT_HEIGHT),
        Some(0)
    );
    assert_eq!(
        tv_aperture_source_row(
            pad + TV_PAL_PRESENT_HEIGHT - 1,
            PRESENT_HEIGHT_SQUARE,
            1,
            TV_PAL_PRESENT_HEIGHT
        ),
        Some(TV_PAL_PRESENT_HEIGHT - 1)
    );
    assert_eq!(
        tv_aperture_source_row(
            pad + TV_PAL_PRESENT_HEIGHT,
            PRESENT_HEIGHT_SQUARE,
            1,
            TV_PAL_PRESENT_HEIGHT
        ),
        None
    );
    // HiDPI: bezel and 1:1 mapping scale with the texture factor.
    assert_eq!(
        tv_aperture_source_row(2 * pad - 1, PRESENT_HEIGHT_SQUARE, 2, TV_PAL_PRESENT_HEIGHT),
        None
    );
    assert_eq!(
        tv_aperture_source_row(2 * pad, PRESENT_HEIGHT_SQUARE, 2, TV_PAL_PRESENT_HEIGHT),
        Some(0)
    );
    assert_eq!(
        tv_aperture_source_row(2 * pad + 1, PRESENT_HEIGHT_SQUARE, 2, TV_PAL_PRESENT_HEIGHT),
        Some(0)
    );
    assert_eq!(
        tv_aperture_source_row(2 * pad + 2, PRESENT_HEIGHT_SQUARE, 2, TV_PAL_PRESENT_HEIGHT),
        Some(1)
    );
}

#[test]
fn ntsc_tv_aperture_row_mapping_stretches_43_and_pads_square() {
    use crate::video::present_common::TV_NTSC_PRESENT_HEIGHT;
    use crate::video::{PRESENT_HEIGHT_SQUARE, PRESENT_HEIGHT_TV};
    // The 4:3 canvas is the glass, which a 60 Hz scan's aperture fills
    // like a 50 Hz one: its 428 rows stretch onto all output rows, with
    // no bezel bands (the lines of a 200-line display are taller on the
    // same screen).
    assert_eq!(
        tv_aperture_source_row(0, PRESENT_HEIGHT_TV, 1, TV_NTSC_PRESENT_HEIGHT),
        Some(0)
    );
    assert_eq!(
        tv_aperture_source_row(
            PRESENT_HEIGHT_TV - 1,
            PRESENT_HEIGHT_TV,
            1,
            TV_NTSC_PRESENT_HEIGHT
        ),
        Some(TV_NTSC_PRESENT_HEIGHT - 1)
    );
    // The square-pixel canvas maps woven rows 1:1, so the shorter 60 Hz
    // aperture is centred between black bezel bands.
    let pad = (PRESENT_HEIGHT_SQUARE - TV_NTSC_PRESENT_HEIGHT) / 2;
    assert_eq!(
        tv_aperture_source_row(pad - 1, PRESENT_HEIGHT_SQUARE, 1, TV_NTSC_PRESENT_HEIGHT),
        None
    );
    assert_eq!(
        tv_aperture_source_row(pad, PRESENT_HEIGHT_SQUARE, 1, TV_NTSC_PRESENT_HEIGHT),
        Some(0)
    );
    assert_eq!(
        tv_aperture_source_row(
            pad + TV_NTSC_PRESENT_HEIGHT - 1,
            PRESENT_HEIGHT_SQUARE,
            1,
            TV_NTSC_PRESENT_HEIGHT
        ),
        Some(TV_NTSC_PRESENT_HEIGHT - 1)
    );
    assert_eq!(
        tv_aperture_source_row(
            pad + TV_NTSC_PRESENT_HEIGHT,
            PRESENT_HEIGHT_SQUARE,
            1,
            TV_NTSC_PRESENT_HEIGHT
        ),
        None
    );
    // The CRT pass follows the same geometry: 214 beam lines fill the 4:3
    // rect, and on the square canvas the bezel padding scales the pitch
    // back to the uniform woven-row spacing.
    use super::crt_scanline_count;
    assert_eq!(
        crt_scanline_count(
            crate::video::deinterlace::OUT_HEIGHT,
            PRESENT_HEIGHT_TV,
            Some(TV_NTSC_PRESENT_HEIGHT)
        ),
        (TV_NTSC_PRESENT_HEIGHT / 2) as f32
    );
    assert_eq!(
        crt_scanline_count(
            crate::video::deinterlace::OUT_HEIGHT,
            PRESENT_HEIGHT_SQUARE,
            Some(TV_NTSC_PRESENT_HEIGHT)
        ),
        (TV_NTSC_PRESENT_HEIGHT / 2) as f32 * PRESENT_HEIGHT_SQUARE as f32
            / TV_NTSC_PRESENT_HEIGHT as f32
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

    assert_eq!(
        PresentationLatch::default().presentation_h_shift(&snapshot, Overscan::Tv),
        0
    );
}

#[test]
fn tv_pal_crop_centres_standard_display_in_aperture() {
    let standard_left = crate::video::bitplane::STANDARD_VISIBLE_X0;
    let standard_right = standard_left + 640;

    assert_eq!(TV_PRESENT_WIDTH, 692);
    assert_eq!(TV_PAL_PRESENT_HEIGHT, 540);
    assert_eq!(standard_left - TV_PRESENT_SOURCE_X, 26);
    assert_eq!(
        TV_PRESENT_WIDTH - (standard_right - TV_PRESENT_SOURCE_X),
        26
    );
    assert_eq!(TV_PRESENT_SOURCE_Y, 18);
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
    test_app_with_audio_and_cpu(audio, crate::config::CpuModel::M68000)
}

/// The same fixture on a chosen CPU model. Fast-RAM banks past the 24-bit
/// space are fitted by the caller through `bus_mut().mem`, since only a
/// 32-bit model reaches them.
fn test_app_with_audio_and_cpu(
    audio: Box<dyn AudioSink>,
    cpu: crate::config::CpuModel,
) -> super::App {
    test_app_with_audio_cpu_and_program(audio, cpu, &[])
}

/// The same fixture running a guest program: `program` (big-endian 68000
/// words) is laid down at the reset PC, replacing the head of the NOP
/// sled. An empty program leaves the plain sled.
fn test_app_with_audio_cpu_and_program(
    audio: Box<dyn AudioSink>,
    cpu: crate::config::CpuModel,
    program: &[u16],
) -> super::App {
    use crate::chipset::paula::Paula;
    use crate::config::PacingBudget;
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
    for (idx, word) in program.iter().enumerate() {
        let at = 8 + idx * 2;
        rom[at..at + 2].copy_from_slice(&word.to_be_bytes());
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
        cpu,
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
        Vec::new(),
        None,
        std::array::from_fn(|_| Vec::new()),
        [true; 4],
        crate::config::Overscan::Full,
        true,
        0.0,
        crate::config::ShaderMode::None,
        1.0,
        false,
        crate::config::Tint::None,
        false,
        false,
        crate::config::WarpSpeed::Max,
        crate::config::JoystickInputMode::Gamepad,
        50,
        crate::config::MouseCapture::Click,
        vec!["Machine: test".to_string()],
        crate::config::RawConfig::default(),
        true,
        crate::sampler::SamplerRequest::default(),
    )
}

// ---------------------------------------------------------------------
// A test machine that draws a hardware pointer.
//
// `crate::pointer` observes the guest's pointer the way a person looking
// at the screen does: it reads where the hardware painted sprite 0. So
// anything that servos the pointer can only be tested against a machine
// that really runs a sprite-0 pointer, which is what the fixture below
// builds out of the plain NOP-sled test machine.
//
// Two pieces of chip behaviour dictate its shape:
//
//   * Agnus never reloads SPRxPT. A field's sprite DMA walks the channel
//     list from wherever the pointer was left, so after one field it
//     sits past the list's null control word and the channel stays dark
//     (`begin_new_beam_frame` carries that DMA frontier across the field
//     boundary deliberately; see src/bus/frame_capture.rs). Software
//     reloads SPRxPT once per field, essentially always from the Copper
//     list, and so does this fixture. That Copper rewrite is the
//     mechanism, not a workaround for one.
//
//   * A sprite's position is not a register the CPU can poke once:
//     SPRxPOS/SPRxCTL are re-fetched by DMA from chip RAM every field.
//     Moving the pointer means rewriting those control words, which the
//     guest program does in vertical blank. VERTB is asserted at the
//     frame wrap (vpos 0) and the channel's control-word DMA slot is
//     ~25 lines later (PAL_SPRITE_DMA_FIRST_ACTIVE_VPOS = $19), so the
//     new position takes effect in the same field it was written.

/// Chip-RAM address of the fixture's Copper list.
const POINTER_COPPER_LIST: u32 = 0x0000_2000;
/// Chip-RAM address of sprite 0's DMA list: control words, then one
/// DATA/DATB pair per line, then the null control word.
const POINTER_SPRITE_LIST: u32 = 0x0000_3000;
/// Lines of sprite data in that list, and so the pointer's height.
const POINTER_SPRITE_LINES: u16 = 8;

/// Where the pointer starts, in the sprite comparators' own units:
/// HSTART counts lo-res pixels (two columns of the presented canvas) and
/// VSTART counts scan lines.
const POINTER_START_HSTART: u16 = 0x0080;
const POINTER_START_VSTART: u16 = 0x0060;

/// Travel limits the guest program clamps to.
///
/// HSTART is nine bits (H8-H1 in SPRxPOS, H0 in SPRxCTL bit 0), so it
/// has to stay under $200; this narrower range also keeps the painted
/// origin inside the 716-column presented canvas at both ends.
const POINTER_MIN_HSTART: u16 = 0x0070;
const POINTER_MAX_HSTART: u16 = 0x0170;
/// VSTART stays below the bottom of the scanned field and above the
/// unprogrammed display window's first line (vpos $2C), so every fetched
/// line lands in the field's live sprite-DMA pass; VSTART + the sprite's
/// height stays under 256, which keeps VSTART8/VSTOP8 (SPRxCTL bits 2-1)
/// clear so the control words the program builds are complete.
const POINTER_MIN_VSTART: u16 = 0x0040;
const POINTER_MAX_VSTART: u16 = 0x00D0;

/// Fields between injecting quadrature counts and reading the resulting
/// motion back. The guest samples the counters in vertical blank and the
/// observation is of the last *completed* field's sprite DMA, so counts
/// that land part-way through a field are consumed by the next field's
/// blank and show up in the field after that.
const POINTER_RESPONSE_FIELDS: u64 = 2;

/// Fields the fixture needs before the pointer is observable: the frame
/// geometry and the channel's vertical comparators settle over the first
/// few fields, and the guest has to reach its first vertical blank.
const POINTER_SETTLE_FIELDS: u64 = 8;

/// Addresses inside a machine built by [`pointer_machine`].
struct PointerFixture {
    /// PC of the per-field update loop's first instruction (the INTREQR
    /// poll). A breakpoint here fires once every frame.
    mainloop_pc: u32,
    /// Chip-RAM address of sprite 0's DMA control words.
    sprite_list: u32,
}

/// Resolve a placeholder 8-bit branch at word index `slot` to word index
/// `target`. The 68000 measures the displacement from the word after the
/// opcode.
fn patch_short_branch(code: &mut [u16], slot: usize, target: usize) {
    let disp = (target as i32 - slot as i32 - 1) * 2;
    let disp = i8::try_from(disp).expect("short branch out of range");
    code[slot] |= u16::from(disp as u8);
}

/// The fixture's guest program, hand-assembled into 68000 words: it runs
/// a sprite-0 pointer steered by port 1's quadrature counters. Returns
/// the words and the word index of the per-field update loop.
fn pointer_program() -> (Vec<u16>, usize) {
    let mut code: Vec<u16> = Vec::new();

    // Hand the low address space back to chip RAM, so the program's
    // control-word stores reach the memory Agnus fetches from. CIA-A
    // PRA bit 0 is /OVL and bit 1 the power LED.
    code.extend_from_slice(&[0x13FC, 0x0003, 0x00BF, 0xE201]); // move.b #$03,$BFE201 (DDRA)
    code.extend_from_slice(&[0x13FC, 0x0002, 0x00BF, 0xE001]); // move.b #$02,$BFE001 (PRA)

    code.extend_from_slice(&[0x4BF9, 0x00DF, 0xF000]); // lea $DFF000,a5

    // Point the Copper at the list that reloads SPR0PT, start it there
    // once, then enable Copper and sprite DMA. Every later field
    // restarts the Copper from COP1LC on its own.
    let list_high = (POINTER_COPPER_LIST >> 16) as u16;
    let list_low = POINTER_COPPER_LIST as u16;
    code.extend_from_slice(&[0x3B7C, list_high, 0x0080]); // move.w #high,COP1LCH(a5)
    code.extend_from_slice(&[0x3B7C, list_low, 0x0082]); // move.w #low,COP1LCL(a5)
    code.extend_from_slice(&[0x3B7C, 0x0000, 0x0088]); // move.w #0,COPJMP1(a5)
    code.extend_from_slice(&[0x3B7C, 0x82A0, 0x0096]); // move.w #SET|DMAEN|COPEN|SPREN,DMACON(a5)

    // d2/d3 hold the pointer position in comparator units, d4 the
    // previous JOY0DAT sample.
    code.extend_from_slice(&[0x343C, POINTER_START_HSTART]); // move.w #hstart,d2
    code.extend_from_slice(&[0x363C, POINTER_START_VSTART]); // move.w #vstart,d3
    code.extend_from_slice(&[0x382D, 0x000A]); // move.w JOY0DAT(a5),d4

    let mainloop = code.len();
    // Wait for VERTB (INTREQ bit 5) and clear it. Writing INTREQ with
    // bit 15 clear clears the named bits.
    code.extend_from_slice(&[0x302D, 0x001E]); // move.w INTREQR(a5),d0
    code.extend_from_slice(&[0x0800, 0x0005]); // btst #5,d0
    let wait_branch = code.len();
    code.push(0x6700); // beq.s mainloop
    patch_short_branch(&mut code, wait_branch, mainloop);
    code.extend_from_slice(&[0x3B7C, 0x0020, 0x009C]); // move.w #$0020,INTREQ(a5)

    // The mouse is a quadrature encoder: JOY0DAT's low byte is the X
    // counter and its high byte the Y counter, and motion is the signed
    // 8-bit difference from the last sample. One count moves the pointer
    // one comparator unit, so the guest applies no acceleration of its
    // own.
    code.extend_from_slice(&[0x302D, 0x000A]); // move.w JOY0DAT(a5),d0
    code.push(0x3200); // move.w d0,d1
    code.push(0x9004); // sub.b d4,d0
    code.push(0x4880); // ext.w d0        (dx)
    code.push(0x3A01); // move.w d1,d5
    code.push(0xE04D); // lsr.w #8,d5     (Y counter now)
    code.push(0x3C04); // move.w d4,d6
    code.push(0xE04E); // lsr.w #8,d6     (Y counter last field)
    code.push(0x9A06); // sub.b d6,d5
    code.push(0x4885); // ext.w d5        (dy)
    code.push(0x3801); // move.w d1,d4    (this field becomes the baseline)
    code.push(0xD440); // add.w d0,d2
    code.push(0xD645); // add.w d5,d3

    // Clamp to the travel limits above.
    code.extend_from_slice(&[0x0C42, POINTER_MIN_HSTART]); // cmp.w #min,d2
    let clamp = code.len();
    code.push(0x6C00); // bge.s +
    code.extend_from_slice(&[0x343C, POINTER_MIN_HSTART]); // move.w #min,d2
    let after = code.len();
    patch_short_branch(&mut code, clamp, after);
    code.extend_from_slice(&[0x0C42, POINTER_MAX_HSTART]); // cmp.w #max,d2
    let clamp = code.len();
    code.push(0x6F00); // ble.s +
    code.extend_from_slice(&[0x343C, POINTER_MAX_HSTART]); // move.w #max,d2
    let after = code.len();
    patch_short_branch(&mut code, clamp, after);
    code.extend_from_slice(&[0x0C43, POINTER_MIN_VSTART]); // cmp.w #min,d3
    let clamp = code.len();
    code.push(0x6C00); // bge.s +
    code.extend_from_slice(&[0x363C, POINTER_MIN_VSTART]); // move.w #min,d3
    let after = code.len();
    patch_short_branch(&mut code, clamp, after);
    code.extend_from_slice(&[0x0C43, POINTER_MAX_VSTART]); // cmp.w #max,d3
    let clamp = code.len();
    code.push(0x6F00); // ble.s +
    code.extend_from_slice(&[0x363C, POINTER_MAX_VSTART]); // move.w #max,d3
    let after = code.len();
    patch_short_branch(&mut code, clamp, after);

    // Rebuild sprite 0's control words in chip RAM. SPRxPOS carries
    // VSTART in bits 15-8 and HSTART bits 8-1 in bits 7-0; SPRxCTL
    // carries VSTOP in bits 15-8 and HSTART bit 0 in bit 0.
    let pos_high = (POINTER_SPRITE_LIST >> 16) as u16;
    let pos_low = POINTER_SPRITE_LIST as u16;
    let ctl_high = ((POINTER_SPRITE_LIST + 2) >> 16) as u16;
    let ctl_low = (POINTER_SPRITE_LIST + 2) as u16;
    code.push(0x3003); // move.w d3,d0
    code.push(0xE148); // lsl.w #8,d0
    code.push(0x3202); // move.w d2,d1
    code.push(0xE249); // lsr.w #1,d1
    code.extend_from_slice(&[0x0241, 0x00FF]); // and.w #$00FF,d1
    code.push(0x8041); // or.w d1,d0
    code.extend_from_slice(&[0x33C0, pos_high, pos_low]); // move.w d0,SPR0POS
    code.push(0x3003); // move.w d3,d0
    code.push(0x5040); // addq.w #8,d0   (VSTOP = VSTART + height)
    code.push(0xE148); // lsl.w #8,d0
    code.push(0x3202); // move.w d2,d1
    code.extend_from_slice(&[0x0241, 0x0001]); // and.w #$0001,d1
    code.push(0x8041); // or.w d1,d0
    code.extend_from_slice(&[0x33C0, ctl_high, ctl_low]); // move.w d0,SPR0CTL

    // bra.w back to the vertical-blank wait. The word displacement
    // keeps the loop free to grow past a short branch's reach.
    let back = code.len();
    code.push(0x6000);
    let disp = (mainloop as i32 - back as i32 - 1) * 2;
    let disp = i16::try_from(disp).expect("loop branch out of range");
    code.push(disp as u16);

    (code, mainloop)
}

/// Write big-endian words into chip RAM directly. Pokes are unaffected
/// by the CPU's boot-time ROM overlay, and Agnus always fetches from
/// chip RAM, so this seeds the DMA lists before the guest runs.
fn poke_chip_words(chip_ram: &mut [u8], addr: u32, words: &[u16]) {
    for (idx, word) in words.iter().enumerate() {
        let at = addr as usize + idx * 2;
        chip_ram[at..at + 2].copy_from_slice(&word.to_be_bytes());
    }
}

/// A test machine running a sprite-0 pointer: a Copper list that reloads
/// SPR0PT every field, sprite 0's DMA list in chip RAM, and a guest
/// program that moves the pointer by the mouse counters in vertical
/// blank. Port 1 is a mouse and port 2 a joystick, as on the stock
/// fixture.
fn pointer_machine() -> (super::App, PointerFixture) {
    let (program, mainloop) = pointer_program();
    let mut app = test_app_with_audio_cpu_and_program(
        Box::new(NullSink),
        crate::config::CpuModel::M68000,
        &program,
    );
    app.emu
        .bus_mut()
        .input
        .set_port_device(1, crate::bus::PortDevice::Joystick);

    // The Copper list: reload SPR0PT for this field, then park. Without
    // it the channel would fetch from wherever the previous field's DMA
    // left the pointer.
    let copper_list = [
        0x0120, // MOVE SPR0PTH
        (POINTER_SPRITE_LIST >> 16) as u16,
        0x0122, // MOVE SPR0PTL
        POINTER_SPRITE_LIST as u16,
        0xFFFF, // WAIT for a beam position that never comes: end of list
        0xFFFE,
    ];
    let mut sprite_list = vec![
        (POINTER_START_VSTART << 8) | ((POINTER_START_HSTART >> 1) & 0x00FF),
        ((POINTER_START_VSTART + POINTER_SPRITE_LINES) << 8) | (POINTER_START_HSTART & 1),
    ];
    for _ in 0..POINTER_SPRITE_LINES {
        sprite_list.push(0xFFFF); // SPRxDATA: a solid run of colour 1
        sprite_list.push(0x0000); // SPRxDATB
    }
    // The null control word an armed channel fetches on its VSTOP line:
    // VSTART and VSTOP of 0 leave it disabled for the rest of the field.
    sprite_list.push(0x0000);
    sprite_list.push(0x0000);

    let chip_ram = &mut app.emu.bus_mut().mem.chip_ram;
    poke_chip_words(chip_ram, POINTER_COPPER_LIST, &copper_list);
    poke_chip_words(chip_ram, POINTER_SPRITE_LIST, &sprite_list);

    let fixture = PointerFixture {
        mainloop_pc: crate::memory::ROM_BASE as u32 + 8 + 2 * mainloop as u32,
        sprite_list: POINTER_SPRITE_LIST,
    };
    (app, fixture)
}

/// Advance the machine by `fields` complete emulated fields.
/// `Emulator::step_frame` spends a fixed instruction budget rather than
/// running to a beam wrap, so a guest whose instructions are cheaper than
/// the pacing average covers less than a field per call. Anything timed
/// against the beam counts fields off Agnus instead.
fn step_fields(app: &mut super::App, fields: u64) {
    let target = app.emu.bus().emulated_frames() + fields;
    while app.emu.bus().emulated_frames() < target {
        app.emu.step_frame().expect("the pointer machine runs");
    }
}

/// Run the fixture until the hardware is painting sprite 0, and report
/// where, in the presented-pixel coordinates the servo works in.
fn settle_pointer_machine(app: &mut super::App) -> (i32, i32) {
    step_fields(app, POINTER_SETTLE_FIELDS);
    crate::pointer::pointer_position(app.emu.bus())
        .expect("the fixture's Copper list re-arms sprite 0 every field")
}

/// Sprite 0's origin on the last completed field, as the servo reads it.
fn pointer_origin(app: &super::App) -> (i32, i32) {
    crate::pointer::pointer_position(app.emu.bus()).expect("sprite 0 is being drawn")
}

#[test]
fn sprite_pointer_fixture_redraws_each_frame_via_copper_pointer_rewrite() {
    let (mut app, fixture) = pointer_machine();
    let origin = settle_pointer_machine(&mut app);

    // Agnus does not reload SPRxPT at vertical blank, so a channel whose
    // pointer is not rewritten walks off the end of its list after one
    // field and goes dark. Thirty further fields is far past that: what
    // keeps the pointer alive is the Copper list's per-field reload.
    step_fields(&mut app, 30);
    assert_eq!(
        pointer_origin(&app),
        origin,
        "an idle mouse leaves the pointer where it was"
    );

    // Ten quadrature counts right and six down. A count is one
    // comparator unit here: HSTART counts lo-res pixels, which are two
    // columns of the presented canvas, and VSTART counts scan lines.
    app.emu.bus_mut().input.add_mouse_delta(0, 10, 6);
    step_fields(&mut app, POINTER_RESPONSE_FIELDS);
    assert_eq!(
        pointer_origin(&app),
        (origin.0 + 20, origin.1 + 6),
        "one count is one lo-res pixel across and one line down"
    );

    // And it moved by rewriting the DMA control words, which is the only
    // way a sprite's position can change: SPRxPOS/SPRxCTL are re-fetched
    // from chip RAM every field.
    let hstart = POINTER_START_HSTART + 10;
    let vstart = POINTER_START_VSTART + 6;
    let chip_ram = &app.emu.bus().mem.chip_ram;
    let at = fixture.sprite_list as usize;
    let pos = u16::from_be_bytes([chip_ram[at], chip_ram[at + 1]]);
    let ctl = u16::from_be_bytes([chip_ram[at + 2], chip_ram[at + 3]]);
    assert_eq!(pos, (vstart << 8) | ((hstart >> 1) & 0x00FF), "SPR0POS");
    assert_eq!(
        ctl,
        ((vstart + POINTER_SPRITE_LINES) << 8) | (hstart & 1),
        "SPR0CTL"
    );

    // The loop address the fixture advertises really is the per-field
    // update loop, so a breakpoint planted there stops the machine.
    app.emu
        .machine
        .ui_set_breakpoint(fixture.mainloop_pc, None, 0);
    app.emu.step_frame().expect("the pointer machine runs");
    assert!(
        app.emu.machine.ui_debug_stop_pending(),
        "the guest reaches mainloop_pc every field"
    );
}

#[test]
fn scripted_mouse_deltas_defer_to_a_steering_pointer_servo() {
    let (mut app, _fixture) = pointer_machine();
    let (x0, y0) = settle_pointer_machine(&mut app);

    // A --mouse-to-after target and a --mouse-after delta both due now.
    // The servo measures the pointer's response to its own counts to
    // learn the guest's acceleration, so the delta has to be held back
    // until it finishes. The target is far enough out that the servo's
    // per-field count limit makes it several corrections' work, so the
    // deferral is observed over several fields rather than one.
    let target = (x0 + 400, y0 + 24);
    app.arm_scripted_pointer_target(0.0, target.0, target.1, 0);
    app.auto_mouse.push((0.0, 20, 0, 0));

    let mut corrections = 0;
    loop {
        // The servo has to see what its previous delta did before
        // choosing the next, so the scheduled-event pass runs at the
        // fixture's response cadence.
        step_fields(&mut app, POINTER_RESPONSE_FIELDS);
        app.fire_scheduled_events();
        if !app.scripted_pointer_target_active() {
            break;
        }
        assert!(
            !app.auto_mouse.is_empty(),
            "a scripted delta must stay queued while the servo is steering"
        );
        corrections += 1;
        assert!(
            corrections < crate::pointer::DEFAULT_MAX_FRAMES,
            "the servo should finish inside its own frame budget"
        );
    }
    assert!(
        corrections > 1,
        "the deferral should have been exercised over several corrections"
    );

    // The servo arrived rather than being knocked off course, and the
    // pass it finished on released the deltas again.
    let arrived = pointer_origin(&app);
    assert!(
        (arrived.0 - target.0).abs() <= crate::pointer::DEFAULT_TOLERANCE
            && (arrived.1 - target.1).abs() <= crate::pointer::DEFAULT_TOLERANCE,
        "servo stopped at {arrived:?}, wanted {target:?}"
    );
    assert!(
        app.auto_mouse.is_empty(),
        "the delta is released on the field the servo finishes"
    );

    // The held-back delta then lands intact: 20 counts is 20 lo-res
    // pixels, 40 columns of the presented canvas.
    step_fields(&mut app, POINTER_RESPONSE_FIELDS);
    assert_eq!(
        pointer_origin(&app),
        (arrived.0 + 40, arrived.1),
        "the deferred delta is applied, not dropped"
    );
}

/// The CCP `input.mouse_to` servo steps frames itself, and `step_frame`
/// reports success even when it ended early on a breakpoint. Without the
/// check under test the servo would keep stepping and swallow the stop.
#[cfg(feature = "control")]
#[test]
fn pointer_servo_yields_to_a_pending_debugger_stop() {
    let (mut app, fixture) = pointer_machine();
    let (x0, y0) = settle_pointer_machine(&mut app);
    app.emu
        .machine
        .ui_set_breakpoint(fixture.mainloop_pc, None, 0);

    let err = crate::control::exec::mouse_to(
        &mut app.emu,
        0,
        (x0 + 200, y0),
        crate::pointer::DEFAULT_TOLERANCE,
        crate::pointer::DEFAULT_MAX_FRAMES,
        |emu, action| {
            if let crate::control::session::InputAction::MouseMove { port, dx, dy } = action {
                emu.bus_mut().input.add_mouse_delta(port as usize, dx, dy);
            }
        },
    )
    .expect_err("the breakpoint must interrupt the servo");
    assert!(
        err.message
            .contains("debugger stop interrupted the pointer servo"),
        "{}",
        err.message
    );

    // The stop was left pending, so the window's normal path still
    // surfaces it: yielding must not consume the hit.
    assert!(
        app.surface_debug_stop(),
        "the interrupted servo must leave the stop for the normal path"
    );
    assert!(app.paused, "surfacing a debugger stop pauses the machine");
}

#[test]
fn mouse_sensitivity_factor_hits_the_anchors_and_is_monotonic() {
    use super::mouse_sensitivity_factor as f;
    // 0 quarter speed, 50 exactly 1:1 (today's speed), 100 quadruple.
    assert!((f(0) - 0.25).abs() < 1e-9);
    assert!((f(50) - 1.0).abs() < 1e-9);
    assert!((f(100) - 4.0).abs() < 1e-9);
    assert!(f(25) < f(50) && f(50) < f(75));
    // Out-of-range clamps to 100.
    assert!((f(200) - 4.0).abs() < 1e-9);
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
fn quick_save_slots_round_trip_and_report_empty_slots() {
    // The slot directory comes from the per-user config location; skip on a
    // host that has none (no HOME/APPDATA/XDG_CONFIG_HOME).
    let Some(path) = crate::savestate::slot_path(7) else {
        return;
    };
    let restore = std::fs::read(&path).ok();
    let _ = std::fs::remove_file(&path);

    let mut app = test_app();
    // An unwritten slot reports rather than failing the load.
    app.quick_load_state(7, None);
    assert_eq!(app.save_slot, 7, "addressing a slot selects it");
    assert_eq!(app.emu.bus().emulated_frames(), 0, "nothing was restored");

    for _ in 0..6 {
        app.emu.step_frame().expect("frame");
    }
    let saved_frame = app.emu.bus().emulated_frames();
    let saved_pc = app.emu.machine.pc();
    app.quick_save_state(7);
    assert!(path.exists(), "slot 7 written to {}", path.display());

    // Run on, then load the slot back: the machine returns to the saved point.
    for _ in 0..6 {
        app.emu.step_frame().expect("frame");
    }
    assert!(app.emu.bus().emulated_frames() > saved_frame);
    app.quick_load_state(7, None);
    assert_eq!(app.emu.bus().emulated_frames(), saved_frame);
    assert_eq!(app.emu.machine.pc(), saved_pc);

    let _ = std::fs::remove_file(&path);
    if let Some(bytes) = restore {
        let _ = std::fs::write(&path, bytes);
    }
}

#[test]
fn save_slot_selection_wraps_through_every_slot() {
    let mut app = test_app();
    assert_eq!(app.save_slot, 1);
    let mut seen = Vec::new();
    for _ in 0..crate::savestate::SLOT_COUNT {
        app.cycle_save_slot();
        seen.push(app.save_slot);
    }
    assert_eq!(seen.last(), Some(&1), "wraps back to the first slot");
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        (1..=crate::savestate::SLOT_COUNT).collect::<Vec<_>>(),
        "every slot is reachable from the menu"
    );
    // Every slot the hotkeys address has a distinct file.
    let paths: Vec<_> = (1..=crate::savestate::SLOT_COUNT)
        .map(crate::savestate::slot_path)
        .collect();
    if paths[0].is_some() {
        let mut unique = paths.clone();
        unique.dedup();
        assert_eq!(unique.len(), paths.len());
    }
    assert!(crate::savestate::slot_path(0).is_none());
    assert!(crate::savestate::slot_path(crate::savestate::SLOT_COUNT + 1).is_none());
}

#[test]
fn number_row_keys_map_onto_the_ten_slots_in_printed_order() {
    use winit::keyboard::KeyCode;
    assert_eq!(super::save_slot_for_key(KeyCode::Digit1), Some(1));
    assert_eq!(super::save_slot_for_key(KeyCode::Digit9), Some(9));
    assert_eq!(
        super::save_slot_for_key(KeyCode::Digit0),
        Some(crate::savestate::SLOT_COUNT),
        "0 sits after 9 on the row, so it is the tenth slot"
    );
    assert_eq!(super::save_slot_for_key(KeyCode::KeyA), None);
    // The numpad keeps driving the emulated machine (it is the port-2
    // joystick mapping), so it must not be swallowed as a slot shortcut.
    assert_eq!(super::save_slot_for_key(KeyCode::Numpad0), None);
}

#[test]
fn rewind_toggle_arms_the_ring_and_steps_back_a_capture_point() {
    let mut app = test_app();
    // Off by default: the hotkey reports rather than silently doing nothing.
    assert!(!app.rewind_armed);
    assert!(!app.emu.time_travel_enabled());
    app.rewind_one_step();
    assert_eq!(app.emu.retired_instructions(), 0, "nothing to rewind to");

    // Two frames per capture keeps the test short.
    app.rewind_interval_frames = 2;
    app.toggle_rewind();
    assert!(app.rewind_armed && app.emu.time_travel_enabled());

    for _ in 0..12 {
        app.emu.step_frame().expect("frame");
    }
    let pos_before = app.emu.retired_instructions();
    let frame_before = app.emu.bus().emulated_frames();
    assert!(pos_before > 0);

    app.rewind_one_step();
    assert!(
        app.emu.retired_instructions() < pos_before,
        "rewind moves the position backward"
    );
    assert!(
        app.emu.bus().emulated_frames() < frame_before,
        "rewind moves emulated time backward"
    );
    // A rewind step lands exactly on a capture point, so no replay is needed.
    assert_eq!(
        app.emu.retired_instructions(),
        app.emu
            .time_travel_ring()
            .and_then(|r| r.newest_pos())
            .expect("a retained anchor"),
    );

    // Repeated steps keep walking back until history runs out, and running
    // off the end leaves the machine parked on the oldest anchor rather than
    // failing.
    for _ in 0..20 {
        app.rewind_one_step();
    }
    let oldest = app
        .emu
        .time_travel_ring()
        .and_then(|r| r.oldest_pos())
        .expect("the arming anchor");
    assert_eq!(app.emu.retired_instructions(), oldest);
    assert_eq!(oldest, 0, "arming takes an anchor immediately, at power-on");
}

#[test]
fn rewinding_then_running_forward_recaptures_and_can_rewind_again() {
    // The regression: a snapshot taken on the abandoned timeline must not
    // survive a rewind, and the capture interval must be re-baselined so the
    // re-run frames are captured again instead of being suppressed.
    let mut app = test_app();
    app.rewind_interval_frames = 2;
    app.toggle_rewind();
    for _ in 0..12 {
        app.emu.step_frame().expect("frame");
    }
    let snaps_before = app.emu.time_travel_ring().map(|r| r.len()).unwrap();
    assert!(snaps_before > 2);

    app.rewind_one_step();
    app.rewind_one_step();
    let pos_rewound = app.emu.retired_instructions();
    let ring = app.emu.time_travel_ring().unwrap();
    assert!(ring.len() < snaps_before, "the abandoned future is dropped");
    assert_eq!(
        ring.newest_pos(),
        Some(pos_rewound),
        "no snapshot survives past the rewound position"
    );

    // Run forward again: captures resume immediately rather than waiting out
    // an interval measured against the abandoned timeline's frame counter.
    for _ in 0..4 {
        app.emu.step_frame().expect("frame");
    }
    let ring = app.emu.time_travel_ring().unwrap();
    assert!(
        ring.newest_pos().is_some_and(|p| p > pos_rewound),
        "the re-run interval was captured"
    );

    // And rewind still works from the re-run timeline.
    let pos_now = app.emu.retired_instructions();
    app.rewind_one_step();
    assert!(app.emu.retired_instructions() < pos_now);
}

#[test]
fn turning_rewind_off_releases_the_ring_unless_the_debugger_needs_it() {
    let mut app = test_app();
    app.toggle_rewind();
    assert!(app.emu.time_travel_enabled());
    app.toggle_rewind();
    assert!(
        !app.emu.time_travel_enabled(),
        "the snapshot memory is the cost; turning it off must release it"
    );

    // With the debugger open the ring stays armed for its reverse controls.
    app.toggle_rewind();
    app.open_debugger();
    app.toggle_rewind();
    assert!(!app.rewind_armed);
    assert!(
        app.emu.time_travel_enabled(),
        "the debugger's reverse controls still need the ring"
    );
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
fn console_watch_refuses_a_pc_qualifier_on_a_dma_class() {
    let mut app = test_app();
    app.open_console();
    // A DMA engine's access has no instruction behind it, so this pair
    // could only ever install a watch that never fires.
    let out = console_run(&mut app, "WATCH 20000 SPR3 PC=F80010");
    assert!(
        out.iter()
            .any(|l| l.contains("only qualifies CPU accesses")),
        "{out:?}"
    );
    assert!(app.emu.machine.ui_breaks().watches.is_empty());
    // A repeated qualifier is a typo, not a last-one-wins override.
    for cmd in ["WATCH 20010 CPU BLITTER", "WATCH 20010 PC=F80010 PC=F80020"] {
        let out = console_run(&mut app, cmd);
        assert!(
            out.iter().any(|l| l.contains("more than once")),
            "{cmd}: {out:?}"
        );
    }
    assert!(app.emu.machine.ui_breaks().watches.is_empty());
    // Either qualifier alone, and the CPU pairing, are accepted.
    for cmd in [
        "WATCH 20000 SPR3",
        "WATCH 20002 PC=F80010",
        "WATCH 20004 CPU PC=F80010",
    ] {
        let out = console_run(&mut app, cmd);
        assert!(out.iter().any(|l| l.contains("set")), "{cmd}: {out:?}");
    }
    assert_eq!(app.emu.machine.ui_breaks().watches.len(), 3);
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
fn console_execbase_dumps_the_scheduler_state() {
    let mut app = test_app();
    app.open_console();
    plant_exec_world(&mut app);
    {
        let ram = &mut app.emu.bus_mut().mem.chip_ram;
        let put32 = |ram: &mut [u8], addr: usize, v: u32| {
            ram[addr..addr + 4].copy_from_slice(&v.to_be_bytes());
        };
        let put16 = |ram: &mut [u8], addr: usize, v: u16| {
            ram[addr..addr + 2].copy_from_slice(&v.to_be_bytes());
        };
        let base = 0x1000usize;
        put32(ram, base + 0x118, 4242); // IdleCount
        put32(ram, base + 0x11C, 999); // DispCount
        put16(ram, base + 0x120, 4); // Quantum
        put16(ram, base + 0x124, 0x4000); // SysFlags: TQE
        ram[base + 0x126] = 0xFF; // IDNestCnt -1
        ram[base + 0x127] = 0x00; // TDNestCnt 0: one Forbid()
        put16(ram, base + 0x128, 0x0007); // AttnFlags: 68010/20/30
        put32(ram, base + 0x202, 0x8100_0009); // LastAlert
    }
    let out = console_run(&mut app, "EXECBASE");
    assert!(out[0].contains("ExecBase $001000"), "{out:?}");
    assert!(
        out.iter()
            .any(|l| l.contains("IdleCount 4242") && l.contains("DispCount 999")),
        "{out:?}"
    );
    assert!(
        out.iter().any(|l| l.contains("SysFlags $4000 (TQE)")),
        "{out:?}"
    );
    assert!(
        out.iter()
            .any(|l| l.contains("IDNestCnt -1 (interrupts enabled)")),
        "{out:?}"
    );
    assert!(
        out.iter()
            .any(|l| l.contains("TDNestCnt 0 (Forbid()den, nesting 1)")),
        "{out:?}"
    );
    assert!(
        out.iter()
            .any(|l| l.contains("AttnFlags $0007 (68010 68020 68030)")),
        "{out:?}"
    );
    assert!(
        out.iter()
            .any(|l| l.contains("ThisTask $002000  boot.task")),
        "{out:?}"
    );
    // The stored alert code comes back decoded, not just in hex.
    assert!(
        out.iter()
            .any(|l| l.starts_with("alert  $81000009") && l.contains("DEADEND")),
        "{out:?}"
    );

    // Exec's own "nothing has alerted" sentinel is -1, which reads as
    // such; a zeroed field is neither that sentinel nor a decodable
    // code, so it is reported raw.
    for (stored, expected) in [
        (0xFFFF_FFFFu32, "alert  none since reset"),
        (0, "alert  LastAlert $00000000"),
    ] {
        app.emu.bus_mut().mem.chip_ram[0x1202..0x1206].copy_from_slice(&stored.to_be_bytes());
        let out = console_run(&mut app, "EXECBASE");
        assert!(out.iter().any(|l| l == expected), "{stored:08X}: {out:?}");
    }

    // Before the OS is up the command says so rather than printing junk.
    app.emu.bus_mut().mem.chip_ram[4..8].copy_from_slice(&0u32.to_be_bytes());
    let out = console_run(&mut app, "EXEC");
    assert!(out[0].starts_with("!no ExecBase"), "{out:?}");
}

#[test]
fn console_task_dumps_one_task_by_default_address_or_name() {
    let mut app = test_app();
    app.open_console();
    plant_exec_world(&mut app);
    // ThisTask ($2000) is a CLI process with a one-hunk command loaded.
    {
        let ram = &mut app.emu.bus_mut().mem.chip_ram;
        let put32 = |ram: &mut [u8], addr: usize, v: u32| {
            ram[addr..addr + 4].copy_from_slice(&v.to_be_bytes());
        };
        ram[0x2000 + 8] = 13; // NT_PROCESS
        ram[0x2000 + 14] = 0x40; // tc_Flags: SWITCH
        put32(ram, 0x2000 + 0x12, 0x0000_FFFF); // tc_SigAlloc
        put32(ram, 0x2000 + 0x16, 0x0000_1000); // tc_SigWait
        put32(ram, 0x2000 + 0x36, 0x0000_7F00); // tc_SPReg
        put32(ram, 0x2000 + 0x3A, 0x0000_7000); // tc_SPLower
        put32(ram, 0x2000 + 0x3E, 0x0000_8000); // tc_SPUpper
        put32(ram, 0x2000 + 0x84, 4096); // pr_StackSize
        put32(ram, 0x2000 + 0x8C, 3); // pr_TaskNum
        put32(ram, 0x2000 + 0xAC, 0x4000 >> 2); // pr_CLI
        put32(ram, 0x4000 + 0x10, 0x4100 >> 2); // cli_CommandName
        ram[0x4100] = 11;
        ram[0x4101..0x410C].copy_from_slice(b"dh0:c/hello");
        put32(ram, 0x4000 + 0x3C, 0x8000 >> 2); // cli_Module
        put32(ram, 0x8000 - 4, 0x100);
        put32(ram, 0x8000, 0);
    }
    // Park the CPU's A7 in the task's stack: the running task's live
    // stack pointer is what the dump must measure against.
    app.emu.machine.debug_set_register(15, 0x0000_7C00);

    let out = console_run(&mut app, "TASK");
    assert!(
        out[0].contains("task $002000  boot.task  (process)")
            && out[0].contains("pri 10")
            && out[0].contains("state run"),
        "{out:?}"
    );
    assert!(out.iter().any(|l| l.contains("$40 (SWITCH)")), "{out:?}");
    assert!(
        out.iter()
            .any(|l| l.contains("alloc $0000FFFF") && l.contains("wait $00001000")),
        "{out:?}"
    );
    assert!(
        out.iter()
            .any(|l| l.contains("$007000-$008000 (4096 bytes)")
                && l.contains("sp $007C00 (live A7), 1024 used")),
        "{out:?}"
    );
    assert!(
        out.iter().any(|l| l.contains("CLI 3")
            && l.contains("\"hello\"")
            && l.contains("StackSize 4096")),
        "{out:?}"
    );
    assert!(
        out.iter().any(|l| l.contains("hunk 0  $008004..$0080FC")),
        "{out:?}"
    );

    // By name (case-insensitive) and by explicit address, and a
    // non-process task stops after the exec half.
    let by_name = console_run(&mut app, "TASK HELP");
    assert!(by_name[0].contains("task $002100  helper"), "{by_name:?}");
    assert!(
        !by_name.iter().any(|l| l.contains("proc ")),
        "a plain task has no process half: {by_name:?}"
    );
    // tc_SPReg is what a task exec has switched away from is measured by.
    assert!(by_name.iter().any(|l| l.contains("SPReg")), "{by_name:?}");
    let by_addr = console_run(&mut app, "TASK $2100");
    assert_eq!(by_addr[0], by_name[0]);
    let missing = console_run(&mut app, "TASK nosuchtask");
    assert!(missing[0].starts_with("!no task matches"), "{missing:?}");

    // An ambiguous name lists the candidates instead of guessing.
    {
        let ram = &mut app.emu.bus_mut().mem.chip_ram;
        ram[0x3000..0x3007].copy_from_slice(b"helper2");
        ram[0x3007] = 0;
    }
    let both = console_run(&mut app, "TASK helper");
    assert!(both[0].contains("matches several tasks"), "{both:?}");
    assert_eq!(both.len(), 4, "{both:?}");
}

#[test]
fn console_memlist_summarizes_exec_memory() {
    let mut app = test_app();
    app.open_console();
    plant_exec_world(&mut app);
    {
        let ram = &mut app.emu.bus_mut().mem.chip_ram;
        let put32 = |ram: &mut [u8], addr: usize, v: u32| {
            ram[addr..addr + 4].copy_from_slice(&v.to_be_bytes());
        };
        let put16 = |ram: &mut [u8], addr: usize, v: u16| {
            ram[addr..addr + 2].copy_from_slice(&v.to_be_bytes());
        };
        // One MemHeader at $5000 covering chip RAM, two free chunks.
        put32(ram, 0x1000 + 0x142, 0x5000); // MemList lh_Head
        put32(ram, 0x5000, 0x1000 + 0x142 + 4); // succ -> lh_Tail
        put32(ram, 0x1000 + 0x142 + 4, 0);
        put32(ram, 0x5000 + 10, 0x5100); // ln_Name
        ram[0x5100..0x5104].copy_from_slice(b"chip");
        ram[0x5104] = 0;
        ram[0x5000 + 9] = (-10i8) as u8; // ln_Pri
        put16(ram, 0x5000 + 0x0E, 0x0003); // PUBLIC | CHIP
        put32(ram, 0x5000 + 0x10, 0x6000); // mh_First
        put32(ram, 0x5000 + 0x14, 0x0000_0400); // mh_Lower
        put32(ram, 0x5000 + 0x18, 0x0008_0000); // mh_Upper
        put32(ram, 0x5000 + 0x1C, 0x0004_0000); // mh_Free
        put32(ram, 0x6000, 0x7000); // mc_Next
        put32(ram, 0x6004, 0x0001_0000); // mc_Bytes
        put32(ram, 0x7000, 0);
        put32(ram, 0x7004, 0x0003_0000);
    }
    let out = console_run(&mut app, "MEMLIST");
    assert!(
        out[0].contains("$000400-$080000")
            && out[0].contains("pri  -10")
            && out[0].ends_with("chip"),
        "{out:?}"
    );
    assert!(
        out[1].contains("PUBLIC CHIP")
            && out[1].contains("largest 196608")
            && out[1].contains("chunks 2"),
        "{out:?}"
    );
    assert!(
        out.last().unwrap().contains("free of") && out.last().unwrap().contains("1 region"),
        "{out:?}"
    );
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

    // A START is taken through the machine's address bus, so on this
    // 24-bit fixture $2050000 sweeps from $050000 and reports the hit
    // above it. Unmasked it would name no region at all and restart from
    // the bottom of the map, reporting the earlier copy instead.
    console_run(&mut app, "POKE 40000 BEEF");
    let out = console_run(&mut app, "FIND BEEF 2050000");
    assert!(out[0].contains("found at $060000"), "{out:?}");
    let out = console_run(&mut app, "FIND BEEF 0");
    assert!(out[0].contains("found at $040000"), "{out:?}");

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

/// Find sweeps the decoded memory map, so on a 32-bit CPU it reaches the
/// RAM banks past the 24-bit space -- here the CPU-slot accelerator bank at
/// $08000000. A search of a fixed 16 MiB span could never see them.
#[test]
fn memory_tab_find_reaches_ram_above_the_24_bit_space() {
    let mut app = test_app_with_audio_and_cpu(Box::new(NullSink), crate::config::CpuModel::M68030);
    app.emu.bus_mut().mem.fit_accel_ram(1024 * 1024);
    app.open_debugger();
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.tab = super::ui::DebugTab::Memory;
        panel.mem_addr = 0;
    }
    app.emu.bus_mut().mem.accel_ram[0x4_0000..0x4_0004].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.entry = "DEADBEEF".to_string();
    }
    app.activate_ui_control(UiControl::DebugMemFind);
    let panel = app.debugger_panel.as_ref().unwrap();
    assert_eq!(
        panel.mem_last_find,
        Some(crate::memory::ACCEL_RAM_BASE as u32 + 0x4_0000)
    );
    assert_eq!(
        panel.mem_addr,
        crate::memory::ACCEL_RAM_BASE as u32 + 0x4_0000
    );
}

/// A full motherboard bank ends at $08000000, exactly where the CPU-slot
/// bank begins, so the two abut in the decoded map. The per-span reads run
/// one pattern short of a chunk past their span end precisely so a match
/// straddling that seam is still found, anchored in the bank it starts in.
#[test]
fn memory_tab_find_spans_two_abutting_ram_banks() {
    let mut app = test_app_with_audio_and_cpu(Box::new(NullSink), crate::config::CpuModel::M68030);
    {
        let mem = &mut app.emu.bus_mut().mem;
        mem.fit_mb_ram(4 * 1024 * 1024);
        mem.fit_accel_ram(1024 * 1024);
        assert_eq!(mem.mb_ram_base(), 0x07C0_0000);
        // DE AD in the last two bytes of the motherboard bank, BE EF in
        // the first two of the CPU-slot bank.
        let top = mem.mb_ram.len();
        mem.mb_ram[top - 2..].copy_from_slice(&[0xDE, 0xAD]);
        mem.accel_ram[..2].copy_from_slice(&[0xBE, 0xEF]);
    }
    app.open_debugger();
    if let Some(panel) = app.debugger_panel.as_mut() {
        panel.tab = super::ui::DebugTab::Memory;
        panel.mem_addr = 0;
        panel.entry = "DEADBEEF".to_string();
    }
    app.activate_ui_control(UiControl::DebugMemFind);
    assert_eq!(
        app.debugger_panel.as_ref().unwrap().mem_last_find,
        Some(0x07FF_FFFE)
    );
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
    fn a_control_heatmap_request_takes_the_map_over_from_the_pane() {
        use crate::video::ui::{AnalyzerTab, UiControl};

        let (mut app, cmd_tx, reply_rx) = attached_app();
        app.open_frame_analyzer();
        app.activate_ui_control(UiControl::AnalyzerTab(AnalyzerTab::Memory));
        assert!(
            app.heatmap_armed_by_panel,
            "entering the tab on an unarmed machine arms the map"
        );

        // A memory.heatmap request re-windows the map: the last window
        // request wins, and the map's lifecycle goes with it, so the pane
        // no longer releases the map when it closes.
        push(
            &cmd_tx,
            1,
            "memory.heatmap",
            json!({"enabled": true, "base": 1048576, "span": 1048576}),
        );
        app.drain_control();
        assert_eq!(reply(&reply_rx)["result"]["armed"], true);
        assert!(
            !app.heatmap_armed_by_panel,
            "the protocol owns the map after any memory.heatmap request"
        );
        app.close_tool_panel(super::super::ToolPanelKind::FrameAnalyzer);
        match app.emu.bus().heat_map() {
            Some(map) => assert_eq!(map.base(), 1048576),
            None => panic!("a protocol-owned map keeps recording after the pane closes"),
        }

        // Once the protocol disarms it, a preset click is an arming of an
        // unarmed map, and that arming is the pane's own to release.
        push(&cmd_tx, 2, "memory.heatmap", json!({"enabled": false}));
        app.drain_control();
        assert_eq!(reply(&reply_rx)["id"], 2);
        assert!(app.emu.bus().heat_map().is_none());
        // Entering the tab arms the map again (the pane's arming); the
        // protocol disarms once more, leaving the Memory tab open on an
        // unarmed map, which is the state the preset row exists to
        // recover from.
        app.open_frame_analyzer();
        app.activate_ui_control(UiControl::AnalyzerTab(AnalyzerTab::Memory));
        push(&cmd_tx, 3, "memory.heatmap", json!({"enabled": false}));
        app.drain_control();
        assert_eq!(reply(&reply_rx)["id"], 3);
        assert!(app.emu.bus().heat_map().is_none());
        assert!(!app.heatmap_armed_by_panel);
        let chip = super::heat_preset_index(&app, "Chip");
        app.activate_ui_control(UiControl::AnalyzerHeatPreset(chip));
        assert!(
            app.heatmap_armed_by_panel,
            "arming an unarmed map from the preset row is the pane's arming"
        );
        assert_eq!(
            app.emu.bus().heat_map().map(|map| map.base()),
            Some(crate::memory::CHIP_RAM_BASE as u32)
        );
        app.close_tool_panel(super::super::ToolPanelKind::FrameAnalyzer);
        assert!(
            app.emu.bus().heat_map().is_none(),
            "the pane releases the arming it made"
        );
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
    fn a_scripted_pointer_target_gives_up_when_no_sprite_pointer_exists() {
        let (mut app, _cmd_tx, _reply_rx) = attached_app();
        app.arm_scripted_pointer_target(0.0, 300, 120, 0);
        // The NOP sled draws no sprites, so the first poll has nothing to
        // observe. It must clear the servo rather than steering blind or
        // retrying every frame for the rest of the run.
        app.emu.step_frame().unwrap();
        app.fire_scheduled_events();
        assert!(
            !app.scripted_pointer_target_active(),
            "a pointerless guest must not leave a servo armed"
        );
    }

    #[test]
    fn run_until_stable_frames_completes_in_the_burst_check() {
        let (mut app, cmd_tx, reply_rx) = attached_app();
        push(&cmd_tx, 1, "run_until", json!({"stable_frames": 2}));
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
        assert!(completed, "the NOP sled's still display should settle");
        assert!(app.paused);
        let stop = reply(&reply_rx);
        assert_eq!(stop["result"]["reason"], "target");
        assert!(
            stop["result"]["detail"]
                .as_str()
                .unwrap()
                .contains("stable for 2"),
            "{}",
            stop["result"]["detail"]
        );
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
    fn scheduled_tap_survives_windowed_connection_turnover() {
        let (mut app, cmd_tx, reply_rx) = attached_app();
        cmd_tx.send(CtlMsg::Connected).unwrap();
        app.drain_control();
        push(
            &cmd_tx,
            1,
            "input.key",
            json!({"rawkey": 0x12, "action": "tap", "hold_ms": 80}),
        );
        app.drain_control();
        assert_eq!(reply(&reply_rx)["result"]["scheduled"], 1);
        assert!(app.emu.bus().keyboard.is_held(0x12));

        cmd_tx.send(CtlMsg::Disconnected).unwrap();
        app.drain_control();
        cmd_tx.send(CtlMsg::Connected).unwrap();
        app.drain_control();

        for _ in 0..6 {
            app.emu.step_frame().unwrap();
            app.drain_control();
        }
        assert!(
            !app.emu.bus().keyboard.is_held(0x12),
            "the replacement connection must deliver the deferred release"
        );
    }

    #[test]
    fn windowed_reset_clears_future_input() {
        let (mut app, cmd_tx, reply_rx) = attached_app();
        push(
            &cmd_tx,
            1,
            "input.key",
            json!({"rawkey": 0x23, "action": "press", "at_seconds": 0.04}),
        );
        app.drain_control();
        assert_eq!(reply(&reply_rx)["result"]["scheduled"], 1);
        push(&cmd_tx, 2, "machine.reset", json!({"kind": "warm"}));
        app.drain_control();
        assert_eq!(reply(&reply_rx)["id"], 2);

        for _ in 0..6 {
            app.emu.step_frame().unwrap();
            app.drain_control();
        }
        assert!(
            !app.emu.bus().keyboard.is_held(0x23),
            "reset must discard input aimed at the old timeline"
        );
    }

    #[test]
    fn windowed_state_load_clears_future_input() {
        let (mut app, cmd_tx, reply_rx) = attached_app();
        let path = std::env::temp_dir().join(format!(
            "copperline-control-scheduled-{}.clstate",
            std::process::id()
        ));
        push(
            &cmd_tx,
            1,
            "state.save",
            json!({"path": path.display().to_string()}),
        );
        app.drain_control();
        assert_eq!(reply(&reply_rx)["id"], 1);
        push(
            &cmd_tx,
            2,
            "input.key",
            json!({"rawkey": 0x24, "action": "press", "at_seconds": 0.04}),
        );
        app.drain_control();
        assert_eq!(reply(&reply_rx)["result"]["scheduled"], 1);
        push(
            &cmd_tx,
            3,
            "state.load",
            json!({"path": path.display().to_string()}),
        );
        app.drain_control();
        assert_eq!(reply(&reply_rx)["id"], 3);

        for _ in 0..6 {
            app.emu.step_frame().unwrap();
            app.drain_control();
        }
        std::fs::remove_file(path).ok();
        assert!(
            !app.emu.bus().keyboard.is_held(0x24),
            "state load must discard input aimed at the old timeline"
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

// ---------------------------------------------------------------------------
// Frame Analyzer, Memory tab: the memory heat map
// ---------------------------------------------------------------------------

/// The Memory tab's view of the live map, as the pane would draw it.
fn built_heat_view(app: &super::App) -> super::ui::AnalyzerHeatView {
    let panel = app
        .frame_analyzer_panel
        .clone()
        .expect("the analyzer pane is open");
    app.build_frame_analyzer_view(&panel)
        .heat
        .expect("the Memory tab has an armed map to show")
}

/// Cells one named toucher holds in a built census.
fn heat_census_cells(view: &super::ui::AnalyzerHeatView, name: &str) -> usize {
    view.census
        .iter()
        .find(|row| row.name == name)
        .map(|row| row.cells)
        .expect("every toucher keeps a census row")
}

/// The census as (toucher, cells) pairs, for assertion messages.
fn census_summary(view: &super::ui::AnalyzerHeatView) -> Vec<(&'static str, usize)> {
    view.census
        .iter()
        .map(|row| (row.name, row.cells))
        .collect()
}

/// Index of the preset button with this label.
fn heat_preset_index(app: &super::App, label: &str) -> u8 {
    let presets = &app
        .frame_analyzer_panel
        .as_ref()
        .expect("the analyzer pane is open")
        .heat_presets;
    presets
        .iter()
        .position(|preset| preset.label == label)
        .unwrap_or_else(|| panic!("no {label} preset in {:?}", preset_labels(app))) as u8
}

fn preset_labels(app: &super::App) -> Vec<String> {
    app.frame_analyzer_panel
        .as_ref()
        .map(|panel| {
            panel
                .heat_presets
                .iter()
                .map(|preset| preset.label.clone())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn frame_analyzer_memory_tab_arms_the_heat_map_over_chip_ram() {
    let (mut app, _fixture) = pointer_machine();
    app.open_frame_analyzer();
    assert!(
        app.emu.bus().heat_map().is_none(),
        "opening the pane alone arms nothing"
    );

    app.activate_ui_control(UiControl::AnalyzerTab(AnalyzerTab::Memory));
    let chip_len = app.emu.bus().mem.chip_ram.len() as u32;
    match app.emu.bus().heat_map() {
        Some(map) => {
            assert_eq!(map.base(), crate::memory::CHIP_RAM_BASE as u32);
            assert_eq!(map.span(), heatmap::rounded_span(chip_len));
        }
        None => panic!("entering the Memory tab arms the map"),
    }
    assert!(app.heatmap_armed_by_panel, "the pane owns this arming");

    // Two fields of recording. The fixture's Copper list reloads SPR0PT
    // every field (Copper instruction fetches from $2000) and sprite 0's
    // DMA walks its control words and data (sprite fetches from $3000), so
    // two engines show in two parts of the same bank.
    //
    // The guest's own stores to the sprite control words land in a cell the
    // channel's DMA re-reads later in the same field, and a cell records
    // only its *last* toucher, so those CPU writes are not what the window
    // ends the field showing. The CPU's other traffic -- instruction fetch
    // and the custom registers -- is outside chip RAM entirely.
    step_fields(&mut app, 2);
    let view = built_heat_view(&app);
    assert_eq!(view.base, crate::memory::CHIP_RAM_BASE as u32);
    assert_eq!(
        view.bytes_per_cell,
        heatmap::rounded_span(chip_len) / heatmap::CELLS as u32
    );
    assert!(
        heat_census_cells(&view, "copper") > 0,
        "the Copper list's fetches: {:?}",
        census_summary(&view)
    );
    assert!(
        heat_census_cells(&view, "sprite") > 0,
        "sprite 0's DMA list: {:?}",
        census_summary(&view)
    );
    // Every toucher keeps a row, so the column reads as the legend too.
    assert_eq!(view.census.len(), 8);
    for row in &view.census {
        assert_eq!(row.bytes, row.cells as u64 * u64::from(view.bytes_per_cell));
    }
    assert!(
        view.image.iter().any(|pixel| *pixel != 0xFF00_0000),
        "recorded cells paint their toucher's colour"
    );
}

#[test]
fn heat_presets_name_every_fitted_ram_bank() {
    // A machine with one of everything: chip, slow/ranger, Ramsey
    // motherboard, CPU-slot accelerator, and two autoconfigured Zorro III
    // RAM boards. The presets come from that decoded bank map, so the two
    // banks above the 24-bit space and the boards wherever autoconfig put
    // them are all reachable, which a fixed address list could not do.
    let (mut app, _fixture) = pointer_machine();
    {
        let mem = &mut app.emu.bus_mut().mem;
        mem.slow_ram = vec![0u8; 512 * 1024];
        mem.fit_mb_ram(4 * 1024 * 1024);
        mem.fit_accel_ram(8 * 1024 * 1024);
        for base in [0x1000_0000u32, 0x2000_0000] {
            mem.zorro
                .add_board_configured_at(crate::zorro::BoardSpec::z3_ram(16 * 1024 * 1024), base)
                .expect("Zorro III RAM board");
        }
    }
    app.open_frame_analyzer();
    app.activate_ui_control(UiControl::AnalyzerTab(AnalyzerTab::Memory));
    // Two boards of the same kind are told apart by their base address.
    assert_eq!(
        preset_labels(&app),
        vec![
            "Chip",
            "Slow",
            "MB",
            "CPU",
            "Z3 $10000000",
            "Z3 $20000000",
            "24-bit"
        ]
    );

    // A bank preset arms exactly that bank, wherever it sits.
    let index = heat_preset_index(&app, "Z3 $20000000");
    app.activate_ui_control(UiControl::AnalyzerHeatPreset(index));
    match app.emu.bus().heat_map() {
        Some(map) => {
            assert_eq!(map.base(), 0x2000_0000);
            assert_eq!(map.span(), heatmap::rounded_span(16 * 1024 * 1024));
        }
        None => panic!("a preset click arms the map"),
    }
}

#[test]
fn frame_analyzer_heat_preset_moves_the_window_onto_a_cold_map() {
    let (mut app, _fixture) = pointer_machine();
    app.open_frame_analyzer();
    app.activate_ui_control(UiControl::AnalyzerTab(AnalyzerTab::Memory));
    step_fields(&mut app, 2);

    let index = heat_preset_index(&app, "24-bit");
    app.activate_ui_control(UiControl::AnalyzerHeatPreset(index));
    let frame = app.emu.bus().emulated_frames();
    let (base, span, cold) = match app.emu.bus().heat_map() {
        Some(map) => (map.base(), map.span(), map.census(frame).is_empty()),
        None => panic!("a preset re-windows the map, it does not disarm it"),
    };
    assert_eq!(base, 0);
    assert_eq!(span, heatmap::rounded_span(heatmap::DEFAULT_SPAN));
    assert!(cold, "moving the window starts a cold map");

    // The wider window reaches what the chip-RAM one cannot: the guest runs
    // from the Kickstart window, so its instruction fetches (which the map
    // counts as CPU reads -- seeing where the CPU is executing is half of
    // what it is for) land in the 24-bit space but never in chip RAM.
    step_fields(&mut app, 2);
    let view = built_heat_view(&app);
    assert!(
        heat_census_cells(&view, "cpu-read") > 0,
        "the guest's instruction fetches: {:?}",
        census_summary(&view)
    );
    assert!(
        heat_census_cells(&view, "copper") > 0 && heat_census_cells(&view, "sprite") > 0,
        "chip-bus DMA is still recorded: {:?}",
        census_summary(&view)
    );

    // A click that lands after the preset list was rebuilt shorter is
    // ignored rather than re-windowing to something arbitrary.
    app.activate_ui_control(UiControl::AnalyzerHeatPreset(u8::MAX));
    match app.emu.bus().heat_map() {
        Some(map) => assert_eq!((map.base(), map.span()), (base, span)),
        None => panic!("an out-of-range preset leaves the map alone"),
    }
}

#[test]
fn closing_the_analyzer_releases_only_a_heat_map_it_armed() {
    let (mut app, _fixture) = pointer_machine();
    app.open_frame_analyzer();
    app.activate_ui_control(UiControl::AnalyzerTab(AnalyzerTab::Memory));
    assert!(app.emu.bus().heat_map().is_some());
    app.close_tool_panel(ToolPanelKind::FrameAnalyzer);
    assert!(
        app.emu.bus().heat_map().is_none(),
        "the pane releases the map it armed"
    );
    assert!(!app.heatmap_armed_by_panel);

    // A map armed before the pane touched it (the control protocol's
    // memory.heatmap) is not the pane's to release.
    app.emu
        .bus_mut()
        .set_heat_map(Some((0, heatmap::DEFAULT_SPAN)));
    app.open_frame_analyzer();
    app.activate_ui_control(UiControl::AnalyzerTab(AnalyzerTab::Memory));
    assert!(
        !app.heatmap_armed_by_panel,
        "an already-armed map keeps its owner"
    );
    // A preset click re-windows the protocol's map (the last window
    // request wins) but does not steal its lifecycle: only arming an
    // unarmed map makes the pane the owner.
    let chip = heat_preset_index(&app, "Chip");
    app.activate_ui_control(UiControl::AnalyzerHeatPreset(chip));
    assert!(
        !app.heatmap_armed_by_panel,
        "re-windowing an armed map is not an arming"
    );
    app.close_tool_panel(ToolPanelKind::FrameAnalyzer);
    let chip_len = app.emu.bus().mem.chip_ram.len() as u32;
    match app.emu.bus().heat_map() {
        Some(map) => {
            assert_eq!(map.base(), crate::memory::CHIP_RAM_BASE as u32);
            assert_eq!(map.span(), heatmap::rounded_span(chip_len));
        }
        None => panic!("a map the pane did not arm keeps recording after it closes"),
    }
}

#[test]
fn frame_analyzer_heat_pick_names_the_copper_lists_toucher() {
    let (mut app, _fixture) = pointer_machine();
    app.open_frame_analyzer();
    app.activate_ui_control(UiControl::AnalyzerTab(AnalyzerTab::Memory));
    step_fields(&mut app, 2);

    // The cell covering the fixture's Copper list. The Copper fetches its
    // instructions from there every field, so that cell's last toucher is
    // the Copper.
    let bytes_per_cell = app
        .emu
        .bus()
        .heat_map()
        .expect("the Memory tab armed the map")
        .bytes_per_cell();
    let cell = (POINTER_COPPER_LIST / bytes_per_cell) as usize;
    app.activate_ui_control(UiControl::AnalyzerHeatPick {
        x: (cell % heatmap::GRID) as u8,
        y: (cell / heatmap::GRID) as u8,
    });
    assert_eq!(
        app.frame_analyzer_panel
            .as_ref()
            .and_then(|panel| panel.heat_selected),
        Some(cell)
    );

    let view = built_heat_view(&app);
    let selected = view.selected.expect("the pinned cell is read out");
    assert_eq!(selected.cell, cell);
    assert_eq!(selected.toucher, Some("copper"));
    assert_eq!(selected.colour, heatmap::Toucher::Copper.colour());
    match selected.age_frames {
        Some(age) => assert!(age < heatmap::DECAY_FRAMES, "a live cell, aged {age}"),
        None => panic!("a touched cell carries an age"),
    }

    // A cell nothing has touched still reads out, with no toucher and no
    // age, so the pane can say so rather than showing a stale record.
    // Halfway up the bank is untouched: the fixture's DMA lists sit at the
    // bottom of chip RAM and its stack at the very top.
    let cold = heatmap::CELLS / 2;
    app.activate_ui_control(UiControl::AnalyzerHeatPick {
        x: (cold % heatmap::GRID) as u8,
        y: (cold / heatmap::GRID) as u8,
    });
    let selected = built_heat_view(&app)
        .selected
        .expect("a pinned cell always reads out");
    assert_eq!(selected.cell, cold);
    assert_eq!(selected.toucher, None);
    assert_eq!(selected.age_frames, None);
    assert_eq!(selected.colour, 0);
}

#[test]
fn frame_analyzer_m_key_toggles_between_the_beam_and_memory_tabs() {
    let (mut app, _fixture) = pointer_machine();
    app.open_frame_analyzer();
    let tab = |app: &super::App| app.frame_analyzer_panel.as_ref().map(|panel| panel.tab);
    assert_eq!(tab(&app), Some(AnalyzerTab::Beam));

    assert!(app.ui_handle_key(KeyCode::KeyM, None));
    assert_eq!(tab(&app), Some(AnalyzerTab::Memory));
    assert!(
        app.emu.bus().heat_map().is_some(),
        "arriving on the Memory tab arms the map"
    );

    assert!(app.ui_handle_key(KeyCode::KeyM, None));
    assert_eq!(tab(&app), Some(AnalyzerTab::Beam));
}

#[test]
fn frame_analyzer_cursor_keys_move_the_pinned_cell_on_the_memory_tab() {
    let (mut app, _fixture) = pointer_machine();
    app.open_frame_analyzer();
    app.frame_analyzer_step_frame();
    let beam_slot = |app: &super::App| {
        app.frame_analyzer_panel
            .as_ref()
            .map(|panel| (panel.selected_hpos, panel.selected_vpos))
    };
    let pinned = |app: &super::App| {
        app.frame_analyzer_panel
            .as_ref()
            .and_then(|panel| panel.heat_selected)
    };
    let slot_before = beam_slot(&app);
    app.activate_ui_control(UiControl::AnalyzerTab(AnalyzerTab::Memory));

    // With nothing pinned the first arrow starts from the centre cell, and
    // the beam selection the Beam tab owns is left where it was.
    let centre = heatmap::CELLS / 2 + heatmap::GRID / 2;
    assert!(app.ui_handle_key(KeyCode::ArrowRight, None));
    assert_eq!(pinned(&app), Some(centre + 1));
    assert!(app.ui_handle_key(KeyCode::ArrowDown, None));
    assert_eq!(pinned(&app), Some(centre + 1 + heatmap::GRID));
    assert_eq!(beam_slot(&app), slot_before);

    // The grid's edges clamp: the selection never wraps into the next row.
    app.activate_ui_control(UiControl::AnalyzerHeatPick { x: 0, y: 0 });
    assert!(app.ui_handle_key(KeyCode::ArrowLeft, None));
    assert!(app.ui_handle_key(KeyCode::ArrowUp, None));
    assert_eq!(pinned(&app), Some(0));
    let last = (heatmap::GRID - 1) as u8;
    app.activate_ui_control(UiControl::AnalyzerHeatPick { x: last, y: last });
    assert!(app.ui_handle_key(KeyCode::ArrowRight, None));
    assert!(app.ui_handle_key(KeyCode::ArrowDown, None));
    assert_eq!(pinned(&app), Some(heatmap::CELLS - 1));
}

#[test]
fn switching_analyzer_tabs_keeps_the_heat_map_recording() {
    let (mut app, _fixture) = pointer_machine();
    app.open_frame_analyzer();
    app.activate_ui_control(UiControl::AnalyzerTab(AnalyzerTab::Memory));
    step_fields(&mut app, 2);
    let before = built_heat_view(&app);
    let recorded: usize = before.census.iter().map(|row| row.cells).sum();
    assert!(recorded > 0, "the fixture's DMA lands in the chip window");

    // Leaving and re-entering the tab must not re-arm (and so wipe) the
    // map: the recording is the point of it.
    app.activate_ui_control(UiControl::AnalyzerTab(AnalyzerTab::Beam));
    app.activate_ui_control(UiControl::AnalyzerTab(AnalyzerTab::Memory));
    let after = built_heat_view(&app);
    assert_eq!(after.base, before.base);
    assert_eq!(after.span, before.span);
    assert_eq!(
        after.census.iter().map(|row| row.cells).sum::<usize>(),
        recorded
    );
}

/// At a 150% fractional host scale the supersampled texture (2x logical) is
/// larger than the surface (1.5x logical): 1432x1162 over 1074x872 for the
/// default TV canvas, for which the Fill scaler computes a clip rect of
/// (0, 0, 1074, 871). pixels' own `window_pos_to_pixel` re-centres through
/// `min(texture, surface) / 2` and shifted every hit 72 logical pixels
/// up-left in that state -- the whole 44-pixel status bar mapped into the
/// display region, so bar clicks took the mouse capture instead.
#[test]
fn cursor_mapping_reaches_status_bar_at_fractional_scale() {
    let clip = (0, 0, 1074, 871);
    let texture = (1432, 1162);
    let texture_scale = 2;

    // Mid-height of the visible status-bar strip (the bottom 44/581 of the
    // window): logical y 559 sits inside the bar's 537..581 band.
    let (x, y) = cursor_position_in_texture((537.0, 839.0), clip, texture).unwrap();
    assert_eq!((x / texture_scale, y / texture_scale), (358, 559));

    // Near the left edge of the bar: the old mapping pushed this off the
    // texture's left edge and returned an error (click swallowed).
    let (x, y) = cursor_position_in_texture((53.7, 839.0), clip, texture).unwrap();
    assert_eq!((x / texture_scale, y / texture_scale), (35, 559));

    // Top-left of the display: also unmapped before.
    let (x, y) = cursor_position_in_texture((10.0, 10.0), clip, texture).unwrap();
    assert_eq!((x / texture_scale, y / texture_scale), (6, 6));

    // The sub-pixel letterbox row under the picture stays outside.
    assert_eq!(
        cursor_position_in_texture((537.0, 871.9), clip, texture),
        None
    );
}

/// A texture that fits inside the surface (a 125% host scale rounds to a 1x
/// texture) is the case pixels' helper handled correctly; the clip-rect
/// mapping must reproduce it. 716x581 texture in an 895x726 surface: Fill
/// scales by 726/581 and clips to (0, 0, 894, 726).
#[test]
fn cursor_mapping_unchanged_when_texture_fits_surface() {
    let clip = (0, 0, 894, 726);
    let texture = (716, 581);
    assert_eq!(
        cursor_position_in_texture((447.5, 363.0), clip, texture),
        Some((358, 290))
    );
    // Mid-height of the visible status-bar strip maps into the bar band.
    assert_eq!(
        cursor_position_in_texture((447.5, 698.5), clip, texture),
        Some((358, 558))
    );
}

/// A manually resized wide window pillarboxes the picture; clicks in the
/// side bars are outside the presentation and stay unmapped, while clicks
/// on the picture land as if the bars were not there. 1432x1162 texture in
/// a 2000x600 surface: Fill clips to (630, 0, 739, 600).
#[test]
fn cursor_mapping_rejects_pillarbox_clicks() {
    let clip = (630, 0, 739, 600);
    let texture = (1432, 1162);
    assert_eq!(
        cursor_position_in_texture((300.0, 300.0), clip, texture),
        None
    );
    let (x, _) = cursor_position_in_texture((1000.0, 300.0), clip, texture).unwrap();
    assert_eq!(x, 716);
    // A zero-area clip (minimized surface) maps nothing.
    assert_eq!(
        cursor_position_in_texture((0.0, 0.0), (0, 0, 0, 0), texture),
        None
    );
}
