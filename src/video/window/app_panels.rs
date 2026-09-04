// SPDX-License-Identifier: GPL-3.0-or-later

//! Tool windows and overlay panels: keyboard strip, MT-32 and Coppersynth fascias.

use super::*;

impl App {
    pub(super) fn handle_tool_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        kind: ToolPanelKind,
        event: WindowEvent,
    ) {
        // Whichever tool window the host last gave the keyboard to is
        // the one in front, and so the one a "close this" means. Opening
        // one focuses it, so it starts out true of the newest.
        if matches!(
            event,
            WindowEvent::Focused(true) | WindowEvent::KeyboardInput { .. }
        ) {
            self.tool_window_front = Some(kind);
        }
        match event {
            WindowEvent::CloseRequested => self.close_tool_panel(kind),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state,
                        physical_key: PhysicalKey::Code(code),
                        repeat,
                        text,
                        ..
                    },
                ..
            } => {
                if state != ElementState::Pressed
                    || (repeat && !self.ui_key_accepts_repeat(Some(kind), code))
                {
                    return;
                }
                if code == KeyCode::KeyQ && host_shortcut_modifier_pressed(self.modifiers) {
                    event_loop.exit();
                } else if kind == ToolPanelKind::Console
                    && self.console_handle_text_input(code, text.as_deref())
                {
                    // Paste or layout-aware typed text; editing and command
                    // keys fall through to the keycode handler below.
                } else if !self.ui_handle_tool_key(kind, code) {
                    self.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.update_host_modifiers(modifiers.state());
            }
            WindowEvent::CursorMoved { position, .. } => {
                let previous = self.tool_window(kind).and_then(|tool| tool.cursor_pos);
                let pos = self.tool_window(kind).and_then(|tool| {
                    cursor_texture_position(&tool.pixels, position, tool.texture_scale)
                });
                if let Some(tool) = self.tool_window_mut(kind) {
                    tool.cursor_pos = pos;
                }
                if kind == ToolPanelKind::FrameAnalyzer && self.analyzer_dragging {
                    if let Some(pos) = pos {
                        self.activate_analyzer_pick_at(kind, pos);
                    }
                }
                if self.tool_hover_changed(kind, previous, pos) {
                    self.request_redraw();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                let previous = self.tool_window(kind).and_then(|tool| tool.cursor_pos);
                if let Some(tool) = self.tool_window_mut(kind) {
                    tool.cursor_pos = None;
                }
                if kind == ToolPanelKind::FrameAnalyzer {
                    self.analyzer_dragging = false;
                }
                if self.tool_hover_changed(kind, previous, None) {
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button != MouseButton::Left {
                    return;
                }
                if state != ElementState::Pressed {
                    if kind == ToolPanelKind::FrameAnalyzer {
                        self.analyzer_dragging = false;
                    }
                    return;
                }
                if kind == ToolPanelKind::FrameAnalyzer {
                    self.analyzer_dragging = false;
                }
                let control = self
                    .tool_window(kind)
                    .and_then(|tool| tool.cursor_pos)
                    .and_then(|pos| self.tool_panel_control_at(kind, pos));
                if let Some(control) = control {
                    if kind == ToolPanelKind::FrameAnalyzer {
                        self.analyzer_dragging = matches!(control, UiControl::AnalyzerPick { .. });
                    }
                    self.activate_tool_control(kind, control);
                    self.ensure_tool_windows_for_open_panels(event_loop);
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // Same stale-texture hazard as the main window (see the main
                // window's ScaleFactorChanged handler): rebuild the tool
                // window's texture for the new scale so its own hit-testing
                // stays aligned after a DPI change or monitor move.
                if let Some(tool) = self.tool_window_mut(kind) {
                    resync_render_scale(&mut tool.pixels, &mut tool.texture_scale, scale_factor);
                }
                self.request_redraw();
            }
            WindowEvent::Resized(size) => {
                self.apply_tool_surface_size(kind, size);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let rows = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y as i32,
                    MouseScrollDelta::PixelDelta(pos) => -(pos.y / 12.0) as i32,
                };
                // Scroll the Memory tab's hex/bitmap view or the console's
                // scrollback: one display row per wheel notch, a chunk for
                // pixel-precise trackpads.
                if kind == ToolPanelKind::Debugger {
                    if self
                        .debugger_panel
                        .as_ref()
                        .is_some_and(|panel| panel.tab == ui::DebugTab::IoMap)
                    {
                        self.debugger_iomap_move(rows);
                    } else {
                        self.debugger_mem_scroll(rows);
                    }
                } else if kind == ToolPanelKind::Console {
                    if let Some(panel) = self.console_panel.as_mut() {
                        panel.scroll = panel
                            .scroll
                            .saturating_add_signed(-(rows as isize))
                            .min(ui::CONSOLE_SCROLLBACK_LINES);
                        self.request_redraw();
                    }
                }
            }
            WindowEvent::RedrawRequested => self.draw_tool_window(kind),
            _ => {}
        }
    }

    pub(super) fn draw_tool_window(&mut self, kind: ToolPanelKind) {
        let Some(panel) = self.tool_panel_for_kind(kind) else {
            *self.tool_window_slot(kind) = None;
            return;
        };
        self.resync_tool_surface_size(kind);
        if kind == ToolPanelKind::FrameAnalyzer {
            self.ensure_analyzer_underlay();
        }
        let ui_data = self.build_tool_panel_view_data(kind);
        let hover = self
            .tool_window(kind)
            .and_then(|tool| tool.cursor_pos)
            .and_then(|pos| ui::panel_control_at(&panel, pos));
        if let Some(tool) = self.tool_window_mut(kind) {
            if tool.minimized {
                return;
            }
            let frame = tool.pixels.frame_mut();
            frame.fill(0);
            ui::draw_panel_layer(frame, tool.texture_scale, &panel, hover, ui_data.as_ref());
            if let Err(e) = tool.pixels.render() {
                error!("tool pixels.render: {e}");
            }
        }
    }

    /// Show or hide the MT-32's panel, resizing the presentation to match.
    ///
    /// The panel takes height from the canvas, and the draw helpers size
    /// themselves from the flag, so the two must move together: a taller
    /// canvas over an unchanged buffer indexes past the end of it. Every
    /// route in goes through here.
    #[cfg(feature = "mt32")]
    pub(super) fn set_mt32_panel_shown(&mut self, shown: bool) {
        if shown == crate::video::mt32_panel_shown() {
            return;
        }
        // Decide before the flag flips whether the window still matches the
        // canvas, so a manual resize survives, and note the height it is
        // being measured against.
        let was_canvas_sized = self.window_is_canvas_sized();
        let canvas_before = window_present_height();
        crate::video::set_mt32_panel_shown(shown);
        if self.resync_canvas_height() {
            self.follow_canvas_change(was_canvas_sized, canvas_before);
        } else {
            crate::video::set_mt32_panel_shown(!shown);
            let _ = self.resync_canvas_height();
        }
        self.request_redraw();
    }

    /// Show or hide Coppersynth's panel, resizing the presentation
    /// exactly as the MT-32's does.
    #[cfg(feature = "coppersynth")]
    pub(super) fn set_csynth_panel_shown(&mut self, shown: bool) {
        if shown == crate::video::csynth_panel_shown() {
            return;
        }
        let was_canvas_sized = self.window_is_canvas_sized();
        let canvas_before = window_present_height();
        crate::video::set_csynth_panel_shown(shown);
        if self.resync_canvas_height() {
            self.follow_canvas_change(was_canvas_sized, canvas_before);
        } else {
            crate::video::set_csynth_panel_shown(!shown);
            let _ = self.resync_canvas_height();
        }
        self.request_redraw();
    }

    /// Show or hide the on-screen keyboard, resizing the presentation to
    /// match. Like the MT-32's panel, the strip takes height from the
    /// canvas and the draw helpers size themselves from the flag, so the
    /// two have to move together. Every route in goes through here.
    pub(super) fn set_keyboard_panel_shown(&mut self, shown: bool) {
        if shown == crate::video::keyboard_panel_shown() {
            return;
        }
        if !shown {
            // The strip is going away with keys still down on it; hand
            // them back before it does, or the guest is left holding them.
            self.release_keyboard_panel_holds();
        }
        // Decide before the flag flips whether the window still matches the
        // canvas, so a manual resize survives, and note the height it is
        // being measured against.
        let was_canvas_sized = self.window_is_canvas_sized();
        let canvas_before = window_present_height();
        crate::video::set_keyboard_panel_shown(shown);
        if self.resync_canvas_height() {
            self.follow_canvas_change(was_canvas_sized, canvas_before);
        } else {
            crate::video::set_keyboard_panel_shown(!shown);
            let _ = self.resync_canvas_height();
        }
        self.request_redraw();
    }

    pub(super) fn toggle_keyboard_panel(&mut self) {
        let shown = !crate::video::keyboard_panel_shown();
        self.set_keyboard_panel_shown(shown);
        self.show_osd(if shown {
            "On-screen keyboard shown"
        } else {
            "On-screen keyboard hidden"
        });
    }

    /// A click on the on-screen keyboard. The strip works out what it
    /// means; what comes back is rawkey transitions for the machine.
    pub(super) fn press_keyboard_panel_control(&mut self, control: kbdpanel::KbdControl) {
        let outcome = self.kbd_panel.press(control, Instant::now());
        let close = outcome.close;
        self.apply_keyboard_panel_outcome(outcome);
        if close {
            self.set_keyboard_panel_shown(false);
        }
        self.request_redraw();
    }

    /// The mouse button lifted. True when it was holding a keycap, in
    /// which case this click was the keyboard's and nobody else's.
    pub(super) fn release_keyboard_panel_key(&mut self) -> bool {
        if !self.kbd_panel.holding_key() {
            return false;
        }
        let outcome = self.kbd_panel.release(Instant::now());
        self.apply_keyboard_panel_outcome(outcome);
        self.request_redraw();
        true
    }

    /// Hand the strip's key transitions to the machine. They go through
    /// the same door as a host keystroke -- recorded by `--record-input`
    /// and noted for replay exactly as a real one is -- but as their own
    /// source, so a cap and a host key can hold the same rawkey without
    /// either cutting the other short (see [`KeySource`]).
    pub(super) fn apply_keyboard_panel_outcome(&mut self, outcome: kbdpanel::KbdOutcome) {
        for (rawkey, pressed) in outcome.keys {
            self.handle_amiga_key_event_from(KeySource::Panel, rawkey, pressed);
        }
    }

    /// Let go of everything the on-screen keyboard is holding, through the
    /// aggregate path, so the machine hears the releases and the drawn
    /// latches match what it believes.
    ///
    /// Called wherever the machine or its keyboard is about to be replaced
    /// or restarted: a latch is a host-side affordance and has no business
    /// outliving the machine it was latched against.
    pub(super) fn release_keyboard_panel_holds(&mut self) {
        let outcome = self.kbd_panel.release_all();
        self.apply_keyboard_panel_outcome(outcome);
    }

    /// What the strip looks like this frame, with the Caps Lock lamp read
    /// off the keyboard MCU rather than mirrored from the clicks: a
    /// save-state load moves that lamp with no key pressed.
    pub(super) fn keyboard_panel_view(&mut self) -> kbdpanel::KbdPanelView {
        let caps_lit = self.emu.bus().keyboard.caps_lock_led();
        let hover = self
            .cursor_pos
            .zip(kbdpanel::shown_panel_rect(keyboard_panel_top()))
            .and_then(|(pos, panel)| kbdpanel::control_at(panel, pos));
        self.kbd_panel.view(caps_lit, hover)
    }

    /// The synth the panel is driving, when one is fitted and switched on.
    #[cfg(feature = "mt32")]
    pub(super) fn mt32_synth_mut(&mut self) -> Option<&mut crate::mt32::Mt32Synth> {
        Some(
            self.emu
                .bus_mut()
                .midi_serial_mut()?
                .mt32_mut()?
                .synth_mut(),
        )
    }

    /// A press on the MT-32's front panel. The panel decides what it means;
    /// anything it cannot reach itself comes back as an action.
    #[cfg(feature = "mt32")]
    pub(super) fn press_mt32_control(
        &mut self,
        control: mt32panel::Mt32Control,
        left: bool,
        pos: (i32, i32),
    ) {
        let Some(rect) = mt32panel::shown_panel_rect(present_height()) else {
            return;
        };
        // A unit that is switched off still takes note of what is held on
        // it, so the panel is told which it is.
        let powered = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .is_some_and(|sink| sink.mt32().is_some());
        let mut panel = std::mem::take(&mut self.mt32_panel);
        let action = panel.press(control, left, pos, rect, powered, self.mt32_synth_mut());
        self.mt32_panel = panel;
        self.apply_mt32_action(action);
        self.serve_mt32_demo();
        self.request_redraw();
    }

    /// Carry out what the panel asked for.
    #[cfg(feature = "mt32")]
    pub(super) fn apply_mt32_action(&mut self, action: mt32panel::PanelAction) {
        use mt32panel::PanelAction;
        match action {
            PanelAction::None => {}
            PanelAction::Say(text) => self.show_osd(text),
            PanelAction::Power(_) => self.toggle_mt32_power(),
            PanelAction::Recycle => {
                if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
                    // Off and on again is what a reset amounts to here: the
                    // engine comes back at its power-on defaults.
                    sink.set_mt32_power(false);
                    sink.set_mt32_power(true);
                }
                self.emu.bus_mut().paula.rearm_synth_audio();
                self.mt32_panel.reset();
                self.show_osd("MT-32: all reset");
            }
        }
    }

    /// The power switch.
    #[cfg(feature = "mt32")]
    pub(super) fn toggle_mt32_power(&mut self) {
        let on = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .is_some_and(|sink| sink.mt32().is_some());
        if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
            sink.set_mt32_power(!on);
        }
        // A fresh synth has to be asked for audio again, and the panel
        // starts over: a unit just switched on is at its defaults, showing
        // its greeting.
        self.emu.bus_mut().paula.rearm_synth_audio();
        self.mt32_panel.reset();
        self.tell_panel_the_rom_version();
        self.serve_mt32_demo();
        let came_up = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .is_some_and(|sink| sink.mt32().is_some());
        if !on && !came_up {
            // Asked to switch on and it did not: say why rather than
            // claiming it is running.
            self.report_mt32_fault();
            return;
        }
        self.show_osd(if on {
            "MT-32: power off"
        } else {
            "MT-32: power on"
        });
    }

    /// Follow the pointer while a button is held on the dial.
    #[cfg(feature = "mt32")]
    pub(super) fn drag_mt32_dial(&mut self, pos: (i32, i32)) {
        let Some(rect) = mt32panel::shown_panel_rect(present_height()) else {
            return;
        };
        let mut panel = std::mem::take(&mut self.mt32_panel);
        panel.drag_dial(pos, rect, self.mt32_synth_mut());
        self.mt32_panel = panel;
        self.request_redraw();
    }

    /// Step the dial on while a button is held still on it.
    #[cfg(feature = "mt32")]
    pub(super) fn repeat_mt32_dial(&mut self) {
        if !self.mt32_panel.dial_held() {
            return;
        }
        let mut panel = std::mem::take(&mut self.mt32_panel);
        let moved = panel.repeat_dial(self.mt32_synth_mut());
        self.mt32_panel = panel;
        if moved {
            self.request_redraw();
        }
    }

    /// Say why the MT-32 is not there, if it was asked for and could not be
    /// fitted. Said once: the fault is taken, not borrowed.
    #[cfg(feature = "mt32")]
    pub(super) fn report_mt32_fault(&mut self) {
        let fault = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .and_then(crate::midi::MidiSerialSink::take_mt32_fault);
        if let Some(fault) = fault {
            self.warn_osd(format!("MT-32: {fault}"));
        }
    }

    /// A press on Coppersynth's panel. The pointer side resolves it
    /// to a semantic press; the engine's own panel decides what it means.
    #[cfg(feature = "coppersynth")]
    pub(super) fn press_csynth_control(
        &mut self,
        control: csynthpanel::CsynthControl,
        left: bool,
        pos: (i32, i32),
    ) {
        let Some(rect) = csynthpanel::shown_panel_rect(csynth_panel_top()) else {
            return;
        };
        let powered = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .is_some_and(|sink| sink.csynth().is_some());
        if control == csynthpanel::CsynthControl::Dial {
            // The knob turns whether or not the unit is on -- it is a
            // pot -- but with the engine gone there is nothing to hear.
            let volume = self.csynth_volume;
            if let Some(v) = self.csynth_panel.grab_dial(left, pos, rect, volume) {
                self.set_csynth_volume(v);
            }
            self.request_redraw();
            return;
        }
        // While an edit or confirm screen owns the glass, latching
        // gestures stand back -- a right-click means nothing there, and
        // the flashing lamps keep their one meaning.
        if !left
            && self
                .emu
                .bus_mut()
                .midi_serial_mut()
                .and_then(crate::midi::MidiSerialSink::csynth_mut)
                .is_some_and(|synth| synth.panel_in_edit())
        {
            return;
        }
        let press = self.csynth_panel.press(control, left, powered);
        self.apply_csynth_press(press);
        self.request_redraw();
    }

    /// Carry out what a press resolved to.
    #[cfg(feature = "coppersynth")]
    pub(super) fn apply_csynth_press(&mut self, press: csynthpanel::CsynthPress) {
        use csynthpanel::CsynthPress;
        match press {
            CsynthPress::None => {}
            CsynthPress::Button(button) => {
                let request = self
                    .emu
                    .bus_mut()
                    .midi_serial_mut()
                    .and_then(crate::midi::MidiSerialSink::csynth_mut)
                    .and_then(|synth| synth.panel_button(button));
                match request {
                    Some(crate::csynth::PanelRequest::Mt32Mode(mode)) => {
                        // The engine already switched; mirror the choice
                        // into the session's options so a power cycle
                        // keeps it.
                        let value = match mode {
                            crate::csynth::Mt32Mode::On => "on",
                            crate::csynth::Mt32Mode::Off => "off",
                            crate::csynth::Mt32Mode::Auto => "auto",
                        };
                        if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
                            sink.set_csynth_mt32_mode(value);
                        }
                    }
                    Some(crate::csynth::PanelRequest::ResetSoundfont) => {
                        // Confirmed at the fascia. The swap rebuilds the
                        // whole unit around the built-in bank, so the
                        // panel that asked -- holding the Initializing...
                        // screen -- is gone with it; the fresh panel
                        // opens on the same hold, serves its second, and
                        // boots.
                        if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
                            sink.reset_csynth_soundfont();
                        }
                        if let Some(synth) = self
                            .emu
                            .bus_mut()
                            .midi_serial_mut()
                            .and_then(crate::midi::MidiSerialSink::csynth_mut)
                        {
                            synth.panel_begin_initializing();
                        }
                    }
                    None => {}
                }
            }
            CsynthPress::PowerOn(held) => {
                self.set_csynth_powered(true);
                let came_up = if let Some(synth) = self
                    .emu
                    .bus_mut()
                    .midi_serial_mut()
                    .and_then(crate::midi::MidiSerialSink::csynth_mut)
                {
                    // What was held on the fascia through the power-on
                    // reaches the unit the way it reads its own buttons
                    // -- the factory questions included.
                    synth.panel_power_on_held(&held);
                    true
                } else {
                    false
                };
                if came_up {
                    self.show_osd("Coppersynth: power on");
                } else {
                    // Asked to switch on and it did not: say why.
                    self.report_csynth();
                }
            }
            CsynthPress::PowerOff => {
                self.set_csynth_powered(false);
                self.show_osd("Coppersynth: power off");
            }
            CsynthPress::Load => self.load_csynth_soundfont(),
        }
    }

    /// The soundfont picker, reached from the fascia's LOAD button and
    /// the menu alike: pick a file, refit the synth around it.
    #[cfg(feature = "coppersynth")]
    pub(super) fn load_csynth_soundfont(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Choose a SoundFont")
            .add_filter("SoundFonts", &["sf2", "SF2", "zip", "ZIP"])
            .pick_file();
        let Some(path) = picked else {
            return;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
            sink.set_csynth_soundfont(path);
        }
        self.emu.bus_mut().paula.rearm_synth_audio();
        let fitted = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .is_some_and(|sink| sink.csynth().is_some());
        if fitted {
            self.show_osd(format!("Coppersynth: {name}"));
        } else {
            // The file would not load; the fault says why.
            self.report_csynth();
        }
    }

    /// Ask for one half of the MT-32's firmware and fit it. The choice
    /// outlives the session: stopping and starting emulation keeps it, so
    /// ROMs picked once stay picked.
    #[cfg(feature = "mt32")]
    pub(super) fn load_mt32_rom(&mut self, control: bool) {
        let title = if control {
            "Choose an MT-32 control ROM"
        } else {
            "Choose an MT-32 PCM ROM"
        };
        let picked = rfd::FileDialog::new()
            .set_title(title)
            .add_filter("ROM images", &["rom", "ROM", "bin", "BIN"])
            .pick_file();
        let Some(path) = picked else {
            return;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if control {
            crate::mt32::set_control_rom_override(path.clone());
        } else {
            crate::mt32::set_pcm_rom_override(path.clone());
        }
        let mut selected = false;
        if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
            if control {
                sink.set_mt32_control_rom(path);
            } else {
                sink.set_mt32_pcm_rom(path);
            }
            selected = sink.mt32_selected();
            if selected {
                // Refit around the new image: power-cycling brings the
                // unit up on it, greeting and all.
                sink.set_mt32_power(false);
                sink.set_mt32_power(true);
            }
        }
        if selected {
            self.emu.bus_mut().paula.rearm_synth_audio();
            self.sync_mt32_panel();
            let fitted = self
                .emu
                .bus_mut()
                .midi_serial_mut()
                .is_some_and(|sink| sink.mt32().is_some());
            if fitted {
                self.show_osd(format!("MT-32: {name}"));
            } else {
                // Half a pair, or a file the engine rejected.
                self.report_mt32_fault();
            }
        } else {
            self.show_osd(format!("MT-32: {name}"));
        }
    }

    /// The switch itself: drop or refit the engine, and ask Paula for
    /// synth audio again when it changes hands.
    #[cfg(feature = "coppersynth")]
    pub(super) fn set_csynth_powered(&mut self, on: bool) {
        if let Some(sink) = self.emu.bus_mut().midi_serial_mut() {
            sink.set_csynth_power(on);
        }
        self.emu.bus_mut().paula.rearm_synth_audio();
    }

    /// The VOLUME knob's value, applied and remembered: the fascia keeps
    /// the knob's position even while the unit is switched off.
    #[cfg(feature = "coppersynth")]
    pub(super) fn set_csynth_volume(&mut self, volume: f32) {
        self.csynth_volume = volume;
        if let Some(synth) = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .and_then(crate::midi::MidiSerialSink::csynth_mut)
        {
            synth.panel_volume(volume);
        }
    }

    /// Follow the pointer while a button is held on the knob.
    #[cfg(feature = "coppersynth")]
    pub(super) fn drag_csynth_dial(&mut self, pos: (i32, i32)) {
        let Some(rect) = csynthpanel::shown_panel_rect(csynth_panel_top()) else {
            return;
        };
        if let Some(v) = self.csynth_panel.drag_dial(pos, rect) {
            self.set_csynth_volume(v);
            self.request_redraw();
        }
    }

    /// The glass earns its redraws: while the panel is up, its screen
    /// is composed each frame -- strings and bar masks, no pixels --
    /// and a redraw is asked for only when something visible changed,
    /// at most twenty times a second. The audio owns the machine; the
    /// LCD takes what is left. Hidden, the whole check is one flag
    /// read.
    #[cfg(feature = "coppersynth")]
    pub(super) fn animate_csynth_panel(&mut self) {
        if !crate::video::csynth_panel_shown() {
            return;
        }
        let now_ms = self.csynth_panel_epoch.elapsed().as_millis() as u64;
        let blink_on = (now_ms / 300).is_multiple_of(2);
        let Some(sink) = self.emu.bus_mut().midi_serial_mut() else {
            return;
        };
        if !sink.csynth_selected() {
            return;
        }
        let key = match sink.csynth_mut() {
            Some(synth) => {
                let screen = synth.panel_screen(now_ms);
                // The monitor's blinking MUTE lamp is on-screen state,
                // so its phase must break the redraw cache.
                let blinking = screen.mute_blink;
                (Some(screen), true, blinking && blink_on)
            }
            None => (None, false, false),
        };
        if self.csynth_panel_drawn.as_ref() == Some(&key) {
            return;
        }
        // A changed glass redraws when the cap allows; until then the
        // change stays pending, so nothing is ever lost.
        if self.csynth_panel_redraw_at.elapsed() < std::time::Duration::from_millis(50) {
            return;
        }
        self.csynth_panel_redraw_at = std::time::Instant::now();
        self.csynth_panel_drawn = Some(key);
        self.request_redraw();
    }

    /// Repeat a held arrow button, gathering speed.
    #[cfg(feature = "coppersynth")]
    pub(super) fn repeat_csynth_buttons(&mut self) {
        if let Some(button) = self.csynth_panel.repeat_button() {
            self.apply_csynth_press(csynthpanel::CsynthPress::Button(button));
            self.request_redraw();
        }
    }

    /// Step the knob on while a button rests on it.
    #[cfg(feature = "coppersynth")]
    pub(super) fn repeat_csynth_dial(&mut self) {
        if !self.csynth_panel.dial_held() {
            return;
        }
        if let Some(v) = self.csynth_panel.repeat_dial(self.csynth_volume) {
            self.set_csynth_volume(v);
            self.request_redraw();
        }
    }

    /// What Coppersynth's panel should show, when it is the output.
    #[cfg(feature = "coppersynth")]
    pub(super) fn csynth_panel_view(&mut self) -> Option<csynthpanel::CsynthPanelView> {
        let now_ms = self.csynth_panel_epoch.elapsed().as_millis() as u64;
        let hover = self
            .cursor_pos
            .zip(csynthpanel::shown_panel_rect(csynth_panel_top()))
            .and_then(|(pos, panel)| csynthpanel::hover_at(panel, pos));
        let down = self.csynth_panel.down();
        let stored_volume = self.csynth_volume;
        let sink = self.emu.bus_mut().midi_serial_mut()?;
        if !sink.csynth_selected() {
            return None;
        }
        let powered = sink.csynth().is_some();
        // Switched off, the fascia is still there: dark glass, and the
        // knob standing where the hand left it.
        let (screen, volume) = match sink.csynth_mut() {
            Some(synth) => (synth.panel_screen(now_ms), synth.panel_volume_value()),
            None => (csynthpanel::dark_screen(), stored_volume),
        };
        if powered {
            self.csynth_volume = volume;
        }
        Some(csynthpanel::CsynthPanelView {
            screen,
            powered,
            blink_on: (now_ms / 300).is_multiple_of(2),
            volume,
            down,
            hover,
        })
    }

    /// Surface a fault -- the synth could not be fitted -- on the OSD,
    /// and drain any display lines to the debug log; the front panel's
    /// glass is where those are really shown.
    #[cfg(feature = "coppersynth")]
    pub(super) fn report_csynth(&mut self) {
        let Some(sink) = self.emu.bus_mut().midi_serial_mut() else {
            return;
        };
        let fault = sink.take_csynth_fault();
        // Display lines are drained but no longer shown: the front panel's
        // LCD is where they belong, and it is on its way. (Surfacing them
        // on the OSD as an option may return; the tap in the sink stays.)
        for line in sink.take_csynth_display() {
            log::debug!("coppersynth display: {line}");
        }
        if let Some(fault) = fault {
            self.warn_osd(format!("Coppersynth: {fault}"));
        }
    }

    /// Bring the panel into line with what is on the port.
    ///
    /// Unplugging the MT-32 takes the panel down with it -- a fascia with no
    /// instrument behind it is just a blank strip -- and plugging one in
    /// starts its panel from scratch, so the engine's power-up greeting is
    /// what shows.
    #[cfg(feature = "mt32")]
    pub(super) fn sync_mt32_panel(&mut self) {
        // Selected, not powered: switching the unit off leaves its panel
        // where it is, which is the whole point of having a switch.
        let selected = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .is_some_and(|sink| sink.mt32_selected());
        self.mt32_panel.reset();
        self.tell_panel_the_rom_version();
        if !selected {
            self.set_mt32_panel_shown(false);
        }
    }

    /// The same for the Coppersynth fascia: deselecting the synth takes
    /// its panel down rather than leaving a blank strip.
    #[cfg(feature = "coppersynth")]
    pub(super) fn sync_csynth_panel(&mut self) {
        let selected = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .is_some_and(|sink| sink.csynth_selected());
        if !selected {
            self.set_csynth_panel_shown(false);
        }
    }

    /// Start whichever of the ROM's own songs the panel is asking for, and
    /// tell it what that one is called.
    #[cfg(feature = "mt32")]
    pub(super) fn serve_mt32_demo(&mut self) {
        // A song that has run out hands on to the next, which is what makes
        // it a chain.
        if self.mt32_panel.chain_ran_out(
            self.emu
                .bus_mut()
                .midi_serial_mut()
                .is_some_and(|sink| sink.mt32_demo_playing()),
        ) {
            self.request_redraw();
        }
        let Some(want) = self.mt32_panel.demo_want() else {
            return;
        };
        let track = match want {
            mt32panel::DemoWant::Play(track) => Some(track),
            mt32panel::DemoWant::Stop => None,
        };
        let title = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .map(|sink| match track {
                Some(track) => sink
                    .play_mt32_demo(track)
                    // Only the later ROMs carry them; the earlier units had
                    // no demonstration to play.
                    .unwrap_or_else(|| "Needs v2.0x ROM".to_string()),
                None => {
                    sink.stop_mt32_demo();
                    String::new()
                }
            })
            .unwrap_or_default();
        self.mt32_panel.set_track_title(title);
    }

    /// Hand the panel what the control ROM calls itself, for its version
    /// screen. The engine keeps its copy of the image to itself, so this
    /// comes from the sink, which read it off disk when the pair was
    /// configured.
    #[cfg(feature = "mt32")]
    pub(super) fn tell_panel_the_rom_version(&mut self) {
        if let Some(version) = self
            .emu
            .bus_mut()
            .midi_serial_mut()
            .and_then(|sink| sink.mt32_version().map(str::to_string))
        {
            self.mt32_panel.set_version(version);
        }
    }

    /// What the MT-32's panel should show, when one is attached.
    #[cfg(feature = "mt32")]
    pub(super) fn mt32_panel_view(&mut self) -> Option<mt32panel::Mt32PanelView> {
        // A song that runs out hands on to the next, which only happens
        // while frames are being rendered -- so it is looked at here, as
        // the panel is drawn, rather than only when something is clicked.
        self.serve_mt32_demo();
        let sink = self.emu.bus_mut().midi_serial_mut()?;
        if !sink.mt32_selected() {
            return None;
        }
        // Switched off, the fascia is still there: dark display, no lamp.
        let (lcd, led) = sink
            .mt32_mut()
            .map_or_else(|| (String::new(), false), |mt32| mt32.synth_mut().display());
        let powered = sink.mt32().is_some();
        let hover = self
            .cursor_pos
            .zip(mt32panel::shown_panel_rect(present_height()))
            .and_then(|(pos, panel)| mt32panel::hover_at(panel, pos));
        Some(self.mt32_panel.view(lcd, led, powered, hover))
    }

    pub(super) fn activate_tool_control(&mut self, kind: ToolPanelKind, control: UiControl) {
        match (kind, control) {
            (ToolPanelKind::Debugger, UiControl::PanelClose) => self.close_tool_panel(kind),
            (ToolPanelKind::FrameAnalyzer, UiControl::PanelClose)
            | (ToolPanelKind::Console, UiControl::PanelClose) => self.close_tool_panel(kind),
            (ToolPanelKind::Console, UiControl::PanelBody) => {}
            (ToolPanelKind::Debugger, UiControl::PanelBody)
            | (ToolPanelKind::FrameAnalyzer, UiControl::PanelBody) => {}
            (ToolPanelKind::Debugger, UiControl::DebugTab(tab)) => {
                if let Some(panel) = self.debugger_panel.as_mut() {
                    panel.tab = tab;
                }
            }
            (ToolPanelKind::Debugger, UiControl::DebugRun) => self.debugger_toggle_run(),
            (ToolPanelKind::Debugger, UiControl::DebugStep) => self.debugger_step(),
            (ToolPanelKind::Debugger, UiControl::DebugStepOver) => self.debugger_step_over(),
            (ToolPanelKind::Debugger, UiControl::DebugStepOut) => self.debugger_step_out(),
            (ToolPanelKind::Debugger, UiControl::DebugStepFrame) => self.debugger_step_frame(),
            (ToolPanelKind::Debugger, UiControl::DebugRunTo) => self.debugger_run_to(),
            (ToolPanelKind::Debugger, UiControl::DebugRunLine) => self.debugger_run_to_line_end(),
            (ToolPanelKind::Debugger, UiControl::DebugReverseStep) => self.debugger_reverse_step(),
            (ToolPanelKind::Debugger, UiControl::DebugReverseFrame) => {
                self.debugger_reverse_frame()
            }
            (ToolPanelKind::Debugger, UiControl::DebugReverseRun) => {
                self.debugger_reverse_continue()
            }
            (ToolPanelKind::Debugger, UiControl::DebugMemPrev) => self.debugger_mem_page(-1),
            (ToolPanelKind::Debugger, UiControl::DebugMemNext) => self.debugger_mem_page(1),
            (ToolPanelKind::Debugger, UiControl::DebugPoke) => self.debugger_poke(),
            (ToolPanelKind::Debugger, UiControl::DebugEntry) => {
                if let Some(panel) = self.debugger_panel.as_mut() {
                    panel.entry_active = true;
                }
            }
            (ToolPanelKind::Debugger, UiControl::DebugBreakToggle) => {
                self.debugger_toggle_breakpoint()
            }
            (ToolPanelKind::Debugger, UiControl::DebugWatchToggle) => {
                self.debugger_toggle_watchpoint()
            }
            (ToolPanelKind::Debugger, UiControl::DebugRegToggle) => {
                self.debugger_toggle_reg_watch()
            }
            (ToolPanelKind::Debugger, UiControl::DebugBeamToggle) => {
                self.debugger_toggle_beam_trap()
            }
            (ToolPanelKind::Debugger, UiControl::DebugCatchToggle) => self.debugger_toggle_catch(),
            (ToolPanelKind::Debugger, UiControl::DebugCopperBreakToggle) => {
                self.debugger_toggle_copper_break()
            }
            (ToolPanelKind::Debugger, UiControl::DebugCopperStep) => self.debugger_step_copper(),
            (ToolPanelKind::Debugger, UiControl::DebugMemFind) => self.debugger_mem_find(),
            (ToolPanelKind::Debugger, UiControl::DebugMemSave) => self.debugger_mem_save_region(),
            (ToolPanelKind::Debugger, UiControl::DebugMemWriter) => self.debugger_mem_writer(),
            (ToolPanelKind::Debugger, UiControl::DebugMemBits) => self.debugger_mem_toggle_bits(),
            (ToolPanelKind::Debugger, UiControl::DebugPlaneToggle(plane)) => {
                self.debugger_toggle_plane(plane)
            }
            (ToolPanelKind::Debugger, UiControl::DebugSpriteToggle(sprite)) => {
                self.debugger_toggle_sprite(sprite)
            }
            (ToolPanelKind::Debugger, UiControl::DebugBreaksClear) => {
                self.emu.machine.ui_breaks_clear();
                self.last_debug_stop = None;
                self.show_osd("Cleared all breakpoints and watchpoints");
            }
            (ToolPanelKind::Debugger, UiControl::DebugWaveArm) => self.debugger_wave_arm(),
            (ToolPanelKind::Debugger, UiControl::DebugWaveStop) => self.debugger_wave_stop(),
            (ToolPanelKind::Debugger, UiControl::DebugAudioMute(idx)) => {
                let (label, muted) = if idx < 4 {
                    let paula = &mut self.emu.bus_mut().paula;
                    paula.toggle_channel_muted(idx);
                    (format!("AUD{idx}"), paula.channel_muted(idx))
                } else {
                    // Rows past the channels are the line-mixed sources, in
                    // the same order `audio_extra_kinds` builds the tab's
                    // rows. A click on a slot no fitted source occupies is
                    // dead space, not a hidden toggle.
                    let Some(kind) = Self::audio_extra_kinds(self.emu.bus())
                        .get(idx - 4)
                        .copied()
                    else {
                        return;
                    };
                    let paula = &mut self.emu.bus_mut().paula;
                    match kind {
                        ui::AudioExtraKind::Cd => {
                            paula.toggle_cd_muted();
                            ("CD audio".to_string(), paula.cd_muted())
                        }
                        ui::AudioExtraKind::Synth => {
                            paula.toggle_synth_muted();
                            ("MIDI synth".to_string(), paula.synth_muted())
                        }
                        ui::AudioExtraKind::Toccata => {
                            paula.toggle_toccata_muted();
                            ("Toccata".to_string(), paula.toccata_muted())
                        }
                        ui::AudioExtraKind::Mhi => {
                            paula.toggle_mhi_muted();
                            ("MHI".to_string(), paula.mhi_muted())
                        }
                    }
                };
                self.show_osd(format!(
                    "{label} {}",
                    if muted { "muted" } else { "unmuted" }
                ));
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerRun) => {
                self.frame_analyzer_toggle_run()
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerFrame) => {
                self.frame_analyzer_step_frame()
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerPick { x, y, scanline }) => {
                self.frame_analyzer_select(x, y, scanline)
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerUnderlay) => {
                self.frame_analyzer_toggle_underlay()
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerScrub) => {
                self.frame_analyzer_toggle_scrub()
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerCpuWait) => {
                self.frame_analyzer_toggle_cpu_wait()
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerRunTo) => {
                self.frame_analyzer_run_to_slot()
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerTab(tab)) => {
                self.frame_analyzer_set_tab(tab)
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerHeatPreset(index)) => {
                self.frame_analyzer_heat_preset(index)
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerHeatPick { x, y }) => {
                self.frame_analyzer_heat_pick(x, y)
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerResourceRow(index)) => {
                self.frame_analyzer_select_resource(index)
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerResourceSave) => {
                self.frame_analyzer_save_resource()
            }
            (ToolPanelKind::FrameAnalyzer, UiControl::AnalyzerBlitRow(index)) => {
                self.frame_analyzer_select_blit(index)
            }
            _ => {}
        }
        self.request_redraw();
    }

    pub(super) fn tool_window_title(kind: ToolPanelKind) -> &'static str {
        match kind {
            ToolPanelKind::Debugger => "Copperline Debugger",
            ToolPanelKind::FrameAnalyzer => "Copperline Frame Analyzer",
            ToolPanelKind::Console => "Copperline Console",
        }
    }

    pub(super) fn tool_panel_is_open(&self, kind: ToolPanelKind) -> bool {
        match kind {
            ToolPanelKind::Debugger => self.debugger_panel.is_some(),
            ToolPanelKind::FrameAnalyzer => self.frame_analyzer_panel.is_some(),
            ToolPanelKind::Console => self.console_panel.is_some(),
        }
    }

    pub(super) fn ensure_tool_windows_for_open_panels(&mut self, event_loop: &ActiveEventLoop) {
        for kind in ToolPanelKind::ALL {
            self.ensure_tool_window_for_kind(event_loop, kind, true);
        }
    }

    /// Frame-loop variant of ensure_tool_windows_for_open_panels: still
    /// creates/destroys windows to match the open panels every call, but
    /// paces the repaint of existing windows to TOOL_REDRAW_INTERVAL.
    pub(super) fn refresh_tool_windows_paced(&mut self, event_loop: &ActiveEventLoop) {
        let due = self.last_tool_redraw.elapsed() >= TOOL_REDRAW_INTERVAL;
        if due {
            self.last_tool_redraw = Instant::now();
        }
        for kind in ToolPanelKind::ALL {
            self.ensure_tool_window_for_kind(event_loop, kind, due);
        }
    }

    pub(super) fn ensure_tool_window_for_kind(
        &mut self,
        event_loop: &ActiveEventLoop,
        kind: ToolPanelKind,
        redraw: bool,
    ) {
        if !self.tool_panel_is_open(kind) {
            *self.tool_window_slot(kind) = None;
            return;
        }
        let title = Self::tool_window_title(kind);
        if let Some(tool) = self.tool_window(kind) {
            tool.window.set_title(title);
            if redraw && !tool.minimized {
                tool.window.request_redraw();
            }
            return;
        }

        let size = LogicalSize::new(FB_WIDTH as f64, window_present_height() as f64);
        let attrs = WindowAttributes::default()
            .with_title(title)
            .with_window_icon(copperline_window_icon())
            .with_inner_size(size)
            .with_min_inner_size(LogicalSize::new(
                FB_WIDTH as f64 / 2.0,
                window_present_height() as f64 / 2.0,
            ));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                warn!("create tool window failed: {e}");
                return;
            }
        };
        let texture_scale = texture_scale_for_window(&window);
        // No vsync for tool windows: pixels.render() runs on the emulation
        // thread, which already paces against the emulator window's vsynced
        // present. A second vsync gate per frame can push the loop past its
        // frame budget and underrun the audio ring.
        //
        // A tool window shows panel text, not the emulated picture, so it
        // always takes the aspect-preserving fit -- integer scaling is a
        // setting for the machine's display.
        let pixels = match build_pixels_for_window(window.clone(), texture_scale, false) {
            Ok(p) => p,
            Err(e) => {
                warn!("tool window pixels init failed: {e}");
                return;
            }
        };
        info!(
            "tool window ready: {title} (texture {}x{})",
            texture_width(texture_scale),
            texture_height(texture_scale)
        );
        // Paint it now rather than waiting for something to happen: a
        // tool window opened and left alone showed an unpainted surface
        // until the next mouse move or key press asked for a frame.
        window.request_redraw();
        // Newly opened is newly in front, until another is touched.
        self.tool_window_front = Some(kind);
        let inner = window.inner_size();
        *self.tool_window_slot(kind) = Some(ToolWindow {
            window,
            pixels,
            texture_scale,
            cursor_pos: None,
            minimized: false,
            surface_size: (inner.width.max(1), inner.height.max(1)),
        });
        self.request_redraw();
    }

    /// The open tool panel a "close this" means: the one in front, which
    /// is whichever was last given the keyboard. Where that is not known
    /// -- none has been touched since it opened -- the last in order, so
    /// a stack of them still comes down one at a time.
    pub(super) fn topmost_tool_panel(&self) -> Option<ToolPanelKind> {
        self.tool_window_front
            .filter(|&kind| self.tool_panel_is_open(kind))
            .or_else(|| {
                ToolPanelKind::ALL
                    .into_iter()
                    .rev()
                    .find(|&kind| self.tool_panel_is_open(kind))
            })
    }

    pub(super) fn close_tool_panel(&mut self, kind: ToolPanelKind) {
        match kind {
            ToolPanelKind::Debugger => {
                if self.debugger_panel.is_some() {
                    self.paused = self.paused_before_debugger;
                    self.last_debug_stop = None;
                    self.sync_live_audio_suspension();
                }
                self.debugger_panel = None;
                self.debugger_tool_window = None;
            }
            ToolPanelKind::Console => {
                if self.console_panel.is_some() {
                    self.paused = self.paused_before_console;
                    self.sync_live_audio_suspension();
                }
                self.console_panel = None;
                self.console_tool_window = None;
            }
            ToolPanelKind::FrameAnalyzer => {
                if self.frame_analyzer_panel.is_some() {
                    self.paused = self.paused_before_analyzer;
                    // A running profile capture shares the analyzer arming
                    // (the heat map's ownership pattern): closing the pane
                    // hands the arming to the capture instead of wiping a
                    // recording mid-profile.
                    #[cfg(feature = "control")]
                    if self.emu.profile_active() {
                        self.emu.profile_adopt_frame_analyzer();
                    } else {
                        self.emu.bus_mut().set_frame_analyzer_enabled(false);
                    }
                    #[cfg(not(feature = "control"))]
                    self.emu.bus_mut().set_frame_analyzer_enabled(false);
                    self.sync_live_audio_suspension();
                }
                self.analyzer_dragging = false;
                self.frame_analyzer_panel = None;
                self.frame_analyzer_tool_window = None;
                // Release the heat map only if this pane armed it. A map
                // armed over the control protocol belongs to that session
                // and keeps recording after the pane closes.
                if self.heatmap_armed_by_panel {
                    self.emu.bus_mut().set_heat_map(None);
                    self.heatmap_armed_by_panel = false;
                }
                // Release the underlay buffers (frame render + up-to-2MiB
                // chip RAM snapshot) while the analyzer is closed.
                self.analyzer_underlay_fb = std::rc::Rc::new(Vec::new());
                self.analyzer_underlay_rows = 0;
                self.analyzer_underlay_frame = None;
                self.analyzer_underlay_input = None;
            }
        }
        if self.debugger_panel.is_none() && self.console_panel.is_none() {
            self.emu.machine.ui_set_pc_history_enabled(false);
        }
        // Hand the mouse back if this was the last panel holding it. With
        // two panels open, closing one leaves the other still wanting the
        // cursor, and the check inside declines until that one goes too.
        self.restore_mouse_capture_after_ui();
        // In auto mode the grab is owed to the machine regardless of
        // whether this panel is the one that borrowed it.
        self.apply_auto_mouse_capture();
        self.request_redraw();
    }

    /// Persist a completed calibration session and close the panel.
    pub(super) fn save_calibration(&mut self) {
        let Some(Panel::Calibration(session)) = self.ui.panel.as_ref() else {
            return;
        };
        match self.gamepad.save_calibration(session) {
            Ok(()) => {
                // By the same door every other panel leaves: putting the
                // panel down by hand left the marker standing on a
                // control that had gone with it.
                self.close_panel();
                self.show_osd("Gamepad calibration saved");
            }
            Err(e) => {
                warn!("gamepad calibration save failed: {e:#}");
                self.show_osd("Calibration save failed (see log)");
            }
        }
    }
}
