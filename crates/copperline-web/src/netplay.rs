// SPDX-License-Identifier: GPL-3.0-or-later

//! Browser ownership of the shared rollback timeline; JavaScript owns WebRTC.

use super::*;
use copperline::netplay::{Connection, PacketQueue, Settings};

fn integer(value: f64, min: u8, max: u8, name: &str) -> Result<u8, JsValue> {
    if !value.is_finite()
        || value.fract() != 0.0
        || !(f64::from(min)..=f64::from(max)).contains(&value)
    {
        return Err(JsValue::from_str(&format!(
            "{name} must be an integer from {min} to {max}"
        )));
    }
    Ok(value as u8)
}

#[wasm_bindgen]
impl WebEmu {
    /// Call after loading ROM/disks into a fresh WebEmu, before any run/state load.
    /// Connection codes and data-channel setup are handled by the page.
    pub fn start_netplay(
        &mut self,
        player: f64,
        code: &str,
        delay: f64,
        window: f64,
        controller: &str,
    ) -> Result<(), JsValue> {
        self.require_local_session()?;
        if !self.netplay_eligible || self.emu.bus().emulated_cck() != 0 {
            return Err(JsValue::from_str(
                "Netplay needs a fresh machine; load ROM and disks before starting",
            ));
        }
        let settings = Settings {
            player: usize::from(integer(player, 1, 2, "player")? - 1),
            session: copperline::netplay::parse_session_id(code).map_err(js_err)?,
            input_delay: integer(delay, 0, 6, "input delay")?,
            rollback_frames: integer(window, 1, 12, "rollback window")?,
        };
        let device = match controller {
            "joystick" => PortDevice::Joystick,
            "cd32" => PortDevice::Cd32Pad,
            _ => {
                return Err(JsValue::from_str(
                    "Netplay controller must be joystick or cd32",
                ))
            }
        };
        let mut cfg = self.config.clone();
        cfg.serial.mode = copperline::config::SerialMode::Off;
        copperline::netplay::prepare_config(&mut cfg).map_err(js_err)?;
        self.netplay_volume = self.emu.bus().output_volume_percent();
        self.emu.bus_mut().set_output_volume_percent(100);
        self.emu.bus_mut().rtc.set_seed(Some(946684800), false);
        self.emu.bus_mut().paula.serial = Box::new(copperline::serial::NullSerialSink);
        for port in 0..2 {
            self.emu.bus_mut().input.set_port_device(port, device);
        }
        self.mouse_pending = (0, 0);
        self.mouse_remainder = (0.0, 0.0);
        self.netplay_input = Default::default();
        self.netplay = Some(
            Connection::with_transport(settings, PacketQueue::default(), &mut self.emu, &cfg)
                .map_err(js_err)?,
        );
        self.anchor = None;
        Ok(())
    }

    pub fn netplay_receive(&mut self, packet: &[u8]) -> Result<(), JsValue> {
        self.netplay
            .as_mut()
            .ok_or_else(|| JsValue::from_str("No netplay session"))?
            .transport_mut()
            .push(packet)
            .map_err(js_err)
    }

    /// Empty means there is no outgoing packet. Drain after every run call.
    pub fn netplay_take_packet(&mut self) -> Vec<u8> {
        self.netplay
            .as_mut()
            .and_then(|peer| peer.transport_mut().pop())
            .unwrap_or_default()
    }

    /// [connected, frame, confirmed, acknowledged, rollbacks, replayed, checked].
    /// Counters are exact JavaScript numbers for any practical session duration.
    pub fn netplay_status(&self) -> Vec<f64> {
        self.netplay.as_ref().map_or_else(Vec::new, |peer| {
            let s = peer.status();
            vec![
                u8::from(s.connected) as f64,
                s.frame as f64,
                s.confirmed_frame as f64,
                s.acknowledged_frame as f64,
                s.rollbacks as f64,
                s.replayed_frames as f64,
                s.checked_frame as f64,
            ]
        })
    }

    /// Release this peer's held keys/controller without touching the guest directly.
    pub fn netplay_release_input(&mut self) {
        self.netplay_input = Default::default();
    }
}

impl WebEmu {
    pub(super) fn require_local_session(&self) -> Result<(), JsValue> {
        if self.netplay.is_some() {
            Err(JsValue::from_str(
                "Unavailable during netplay; disconnect first",
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn run_netplay(
        &mut self,
        now_ms: f64,
        max_frames: u32,
        render: bool,
    ) -> Result<u32, JsValue> {
        if !now_ms.is_finite() {
            return Err(JsValue::from_str("Netplay clock must be finite"));
        }
        self.last_run_core_ms = 0.0;
        self.last_run_render_ms = 0.0;
        let started = Instant::now();
        let peer = self.netplay.as_mut().unwrap();
        let before = peer.status();
        peer.step(&mut self.emu, self.netplay_input, false)
            .map_err(js_err)?;
        if !before.connected {
            self.anchor = None;
        }
        let (wall, emulated) = *self
            .anchor
            .get_or_insert((now_ms, self.emu.bus().emulated_seconds()));
        let target = emulated + (now_ms - wall) / 1000.0;
        let mut stepped = 0;
        while self.emu.bus().emulated_seconds() < target && stepped < max_frames.min(8) {
            if !peer
                .step(&mut self.emu, self.netplay_input, true)
                .map_err(js_err)?
            {
                self.anchor = Some((now_ms, self.emu.bus().emulated_seconds()));
                break;
            }
            stepped += 1;
        }
        let corrected = peer.status().rollbacks != before.rollbacks;
        if corrected {
            self.last_rendered_frame = None;
            self.deinterlacer.reset_history();
            self.reset_presentation_latches();
        }
        self.last_run_core_ms = started.elapsed().as_secs_f64() * 1000.0;
        if target - self.emu.bus().emulated_seconds() > MAX_CATCHUP_SECONDS {
            self.anchor = Some((now_ms, self.emu.bus().emulated_seconds()));
        }
        if render && (stepped > 0 || corrected || self.deferred_fields > 0) {
            let render_started = Instant::now();
            self.render_completed_frame_elapsed(
                self.deferred_fields.saturating_add(stepped).max(1),
            );
            self.deferred_fields = 0;
            self.last_run_render_ms = render_started.elapsed().as_secs_f64() * 1000.0;
        } else if !render {
            self.deferred_fields = self
                .deferred_fields
                .saturating_add(stepped)
                .max(u32::from(corrected));
        }
        Ok(stepped)
    }
}
