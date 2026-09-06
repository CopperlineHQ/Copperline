// SPDX-License-Identifier: GPL-3.0-or-later

//! Netplay owns every machine input; the surrounding window still presents it.

use super::*;

impl App {
    pub fn attach_netplay(&mut self, session: crate::netplay::Session) {
        self.netplay = Some(session);
        self.show_osd("Netplay: waiting for peer".to_string());
    }

    pub(super) fn pump_netplay_input(&mut self) {
        let pad = self.gamepad.poll();
        let port = self.netplay.as_ref().unwrap().player();
        if pad.is_none() && self.auto_joy_engaged[port] {
            self.apply_auto_joy_state(port);
            return;
        }
        let mut state = pad.map_or_else(|| self.keyboard_joystick_state(0), |p| p.joystick);
        if (!self.netplay_keyboard_controller && pad.is_none()) || !self.main_window_focused {
            state = Default::default();
        }
        state.fire &=
            crate::config::autofire_asserted(self.autofire_hz, self.emu.bus().emulated_seconds());
        self.netplay_input.buttons = [
            state.up,
            state.down,
            state.left,
            state.right,
            state.fire,
            state.button2,
            state.play,
            state.rwd,
            state.ffw,
            state.green,
            state.yellow,
        ]
        .into_iter()
        .enumerate()
        .fold(0, |bits, (bit, on)| bits | (u16::from(on) << bit));
    }

    pub(super) fn step_netplay(&mut self) -> Result<bool> {
        let session = self.netplay.as_mut().unwrap();
        let before = session.status();
        let connected = before.connected;
        let stepped = session.step(&mut self.emu, self.netplay_input, true)?;
        let after = session.status();
        if after.rollbacks != before.rollbacks {
            self.reset_render_pipeline();
        }
        if !connected && after.connected {
            self.show_osd("Netplay connected: arrows + right Ctrl, or gamepad".to_string());
        }
        if !stepped {
            self.emu.reanchor_realtime_clock();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Ok(stepped)
    }

    /// Scheduled captures wait for actual input so PNGs never preserve a
    /// prediction that the following network packet would correct.
    pub(super) fn confirm_netplay_capture(&mut self) -> Result<()> {
        let now = self.emu.bus().emulated_seconds();
        let due = self
            .auto_shot
            .first()
            .is_some_and(|(at, _)| now >= f64::from(*at))
            || self
                .frame_dump
                .as_ref()
                .is_some_and(|dump| now >= f64::from(dump.start_secs));
        if !due || self.netplay.is_none() {
            return Ok(());
        }
        loop {
            let session = self.netplay.as_mut().unwrap();
            let before = session.status().rollbacks;
            session.step(&mut self.emu, self.netplay_input, false)?;
            let status = session.status();
            if status.rollbacks != before {
                self.reset_render_pipeline();
            }
            if status.frame == status.confirmed_frame {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Consume machine-changing window events before normal shortcuts, menus,
    /// drag-and-drop, mouse input, or focus-loss key release can reach the Bus.
    pub(super) fn netplay_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        event: &WindowEvent,
    ) -> bool {
        if self.netplay.is_none() {
            return false;
        }
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state,
                        repeat,
                        ..
                    },
                ..
            } => {
                if *repeat {
                    return true;
                }
                let pressed = *state == ElementState::Pressed;
                if host_shortcut_modifier_pressed(self.modifiers) && pressed {
                    match code {
                        KeyCode::KeyQ => event_loop.exit(),
                        KeyCode::KeyF => self.toggle_fullscreen(),
                        _ => {}
                    }
                    return true;
                }
                if *code == KeyCode::F12 {
                    if pressed {
                        self.netplay_keyboard_controller = !self.netplay_keyboard_controller;
                        self.keyboard_joy_held = Default::default();
                        self.netplay_input = Default::default();
                        self.show_osd(
                            if self.netplay_keyboard_controller {
                                "Netplay: keyboard controls the joystick (F12 to type)"
                            } else {
                                "Netplay: Amiga keyboard typing (F12 for joystick)"
                            }
                            .to_string(),
                        );
                    }
                    return true;
                }
                if self.netplay_keyboard_controller
                    && matches!(self.keymap.lookup(*code), Some((0, _)))
                {
                    self.keyboard_joy_held[0].set(*code, pressed);
                } else if let Some(rawkey) = host_to_amiga_rawkey(*code) {
                    self.netplay_input.set_key(rawkey, pressed);
                }
                true
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
                true
            }
            WindowEvent::Focused(focused) => {
                self.main_window_focused = *focused;
                if !focused {
                    self.netplay_input = Default::default();
                    self.keyboard_joy_held = Default::default();
                }
                true
            }
            WindowEvent::CloseRequested
            | WindowEvent::Resized(_)
            | WindowEvent::ScaleFactorChanged { .. }
            | WindowEvent::RedrawRequested
            | WindowEvent::Occluded(_) => false,
            _ => true,
        }
    }
}
