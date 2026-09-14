// SPDX-License-Identifier: GPL-3.0-or-later

//! Browser ownership of the shared rollback timeline; JavaScript owns WebRTC.

use super::*;
use anyhow::Context as _;
use copperline::netplay::{spectate, Connection, PacketQueue, Settings, Spectator, SwapRecord};

/// A spectator this far behind the host replays unpaced; a catch-up call
/// runs at most this many frames or milliseconds to keep the page responsive.
const CATCHUP_START: u64 = 25;
const CATCHUP_FRAMES: u32 = 64;
const CATCHUP_MS: f64 = 12.0;
/// Bytes one feed call may return; a disk change is returned whole.
const FEED_TAKE_MIN: f64 = 1024.0;
const FEED_TAKE_MAX: f64 = 16.0 * 1024.0 * 1024.0;

pub(super) struct DiskSwap {
    stop: u64,
    disk: Option<(usize, Vec<u8>, bool)>,
    applied: bool,
}

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
                "Netplay needs a fresh machine; create a new WebEmu and load ROM and disks before starting",
            ));
        }
        let settings = Settings {
            player: usize::from(integer(player, 1, 2, "player")? - 1),
            session: copperline::netplay::parse_session_id(code).map_err(js_err)?,
            input_delay: integer(delay, 0, 6, "input delay")?,
            rollback_frames: integer(window, 1, 12, "rollback window")?,
        };
        let device = match controller {
            "mouse" => PortDevice::Mouse,
            "joystick" => PortDevice::Joystick,
            "cd32" => PortDevice::Cd32Pad,
            _ => {
                return Err(JsValue::from_str(
                    "Netplay controller must be mouse, joystick or cd32",
                ))
            }
        };
        self.start_netplay_inner(settings, device).map_err(js_err)
    }

    /// [protocol version, maximum packet bytes, header bytes, input record bytes].
    pub fn netplay_packet_layout() -> Vec<u32> {
        use copperline::netplay::{INPUT_RECORD, MAX_PACKET, PACKET_HEADER, PROTOCOL_VERSION};
        vec![
            u32::from(PROTOCOL_VERSION),
            MAX_PACKET as u32,
            PACKET_HEADER as u32,
            INPUT_RECORD as u32,
        ]
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
        self.mouse_remainder = (0.0, 0.0);
    }

    /// Freeze at the current frame while the reliable channel negotiates a
    /// common boundary. Input packets continue to reconcile and acknowledge.
    pub fn netplay_hold(&mut self) -> Result<f64, JsValue> {
        let peer = self
            .netplay
            .as_ref()
            .ok_or_else(|| JsValue::from_str("No netplay session"))?;
        if !peer.status().connected || self.netplay_swap.is_some() {
            return Err(JsValue::from_str(
                "A connected, idle netplay session is required",
            ));
        }
        let stop = peer.status().frame;
        self.netplay_swap = Some(DiskSwap {
            stop,
            disk: None,
            applied: false,
        });
        self.anchor = None;
        Ok(stop as f64)
    }

    /// Both stopped peers catch up to the greater of their two frame numbers.
    pub fn netplay_stop_at(&mut self, frame: f64) -> Result<(), JsValue> {
        let current = self
            .netplay
            .as_ref()
            .ok_or_else(|| JsValue::from_str("No netplay session"))?
            .status()
            .frame;
        let swap = self
            .netplay_swap
            .as_mut()
            .ok_or_else(|| JsValue::from_str("No disk swap in progress"))?;
        if !frame.is_finite()
            || frame.fract() != 0.0
            || frame < current as f64
            || frame > (current + 32) as f64
            || swap.disk.is_some()
            || swap.applied
        {
            return Err(JsValue::from_str("Invalid disk swap frame"));
        }
        swap.stop = frame as u64;
        self.anchor = None;
        Ok(())
    }

    pub fn netplay_swap_ready(&self) -> bool {
        self.netplay_swap.as_ref().is_some_and(|swap| {
            self.netplay.as_ref().is_some_and(|peer| {
                peer.status().frame == swap.stop && peer.status().ready_to_capture()
            })
        })
    }

    /// Digests before and after the change are compared over the reliable
    /// channel; neither peer resumes on a mismatch.
    pub fn netplay_swap_digest(&self) -> Result<Vec<u8>, JsValue> {
        if !self.netplay_swap_ready() {
            return Err(JsValue::from_str("Disk swap is not at a confirmed frame"));
        }
        self.netplay
            .as_ref()
            .unwrap()
            .confirmed_state_digest(&self.emu)
            .map(|hash| hash.to_vec())
            .map_err(js_err)
    }

    /// Validate an image without touching the live drive. Empty bytes mean eject.
    pub fn netplay_validate_disk(
        &self,
        drive: f64,
        bytes: Vec<u8>,
        writable: bool,
    ) -> Result<(), JsValue> {
        let drive = usize::from(integer(drive, 0, 1, "drive")?);
        self.validate_netplay_disk(drive, bytes, writable)
            .map_err(js_err)
    }

    pub fn netplay_stage_disk(
        &mut self,
        drive: f64,
        bytes: Vec<u8>,
        writable: bool,
    ) -> Result<(), JsValue> {
        let drive = usize::from(integer(drive, 0, 1, "drive")?);
        if !self.netplay_swap_ready()
            || self
                .netplay_swap
                .as_ref()
                .is_none_or(|s| s.applied || s.disk.is_some())
        {
            return Err(JsValue::from_str("Disk swap is not ready for an image"));
        }
        self.validate_netplay_disk(drive, bytes.clone(), writable)
            .map_err(js_err)?;
        self.netplay_swap.as_mut().unwrap().disk = Some((drive, bytes, writable));
        Ok(())
    }

    pub fn netplay_apply_disk(&mut self) -> Result<(), JsValue> {
        if !self.netplay_swap_ready() {
            return Err(JsValue::from_str("Disk swap is not at a confirmed frame"));
        }
        let peer = self.netplay.as_mut().unwrap();
        let swap = self.netplay_swap.as_mut().unwrap();
        let (drive, bytes, writable) = swap
            .disk
            .take()
            .ok_or_else(|| JsValue::from_str("No verified replacement disk"))?;
        // Spectators replay the change at this frame and check the same
        // digests on either side of it.
        let before = peer.confirmed_state_digest(&self.emu).map_err(js_err)?;
        let bytes = std::sync::Arc::new(bytes);
        let floppy = &mut self.emu.bus_mut().floppy;
        if bytes.is_empty() {
            floppy.eject_disk_image(drive)
        } else {
            floppy.insert_memory_disk_image_bytes_with_limit(
                drive,
                bytes.to_vec(),
                format!("netplay-df{drive}").into(),
                !writable,
                16 * 1024 * 1024,
            )
        }
        .map_err(js_err)?;
        let after = peer.confirmed_state_digest(&self.emu).map_err(js_err)?;
        peer.feed_swap(SwapRecord {
            frame: swap.stop,
            drive,
            writable,
            before,
            after,
            bytes,
        })
        .map_err(js_err)?;
        swap.applied = true;
        self.anchor = None;
        Ok(())
    }

    /// Retain the confirmed history so spectators can join, even late. Call
    /// right after `start_netplay`, before the first run.
    pub fn netplay_enable_spectators(&mut self) -> Result<(), JsValue> {
        self.netplay
            .as_mut()
            .ok_or_else(|| JsValue::from_str("No netplay session"))?
            .enable_feed(spectate::WEB_FEED_LIMIT)
            .map_err(js_err)
    }

    /// The initial machine fingerprint every spectator must reproduce.
    pub fn netplay_identity(&self) -> Vec<u8> {
        self.netplay
            .as_ref()
            .map_or_else(Vec::new, |peer| peer.identity().to_vec())
    }

    /// Open a feed cursor for one spectator at frame zero; returns its id.
    /// Fails once the retained history is too large for a late joiner.
    pub fn spectator_feed_open(&mut self) -> Result<u32, JsValue> {
        self.feed_open_inner().map_err(js_err)
    }

    /// Encode what a spectator still needs, up to about `max_bytes` (a disk
    /// change is returned whole). Empty means it has everything so far.
    pub fn spectator_feed_take(&mut self, id: u32, max_bytes: f64) -> Result<Vec<u8>, JsValue> {
        if !max_bytes.is_finite() || !(FEED_TAKE_MIN..=FEED_TAKE_MAX).contains(&max_bytes) {
            return Err(JsValue::from_str("Feed budget must be 1 KiB to 16 MiB"));
        }
        self.feed_take_inner(id, max_bytes as usize).map_err(js_err)
    }

    pub fn spectator_feed_close(&mut self, id: u32) {
        self.feed_cursors.remove(&id);
    }

    /// Confirmed frames retained for spectators.
    pub fn spectator_feed_frames(&self) -> f64 {
        self.netplay
            .as_ref()
            .and_then(|peer| peer.feed())
            .map_or(0.0, |feed| feed.frames() as f64)
    }

    /// Follow a host's game on a fresh machine built from its media, before
    /// any run. The page feeds `spectate_receive` from the host's stream.
    pub fn start_spectating(&mut self, controller: &str) -> Result<(), JsValue> {
        self.require_local_session()?;
        if !self.netplay_eligible || self.emu.bus().emulated_cck() != 0 {
            return Err(JsValue::from_str(
                "Spectating needs a fresh machine; create a new WebEmu and load the host's ROM and disks first",
            ));
        }
        let device = match controller {
            "mouse" => PortDevice::Mouse,
            "joystick" => PortDevice::Joystick,
            "cd32" => PortDevice::Cd32Pad,
            _ => {
                return Err(JsValue::from_str(
                    "Netplay controller must be mouse, joystick or cd32",
                ))
            }
        };
        self.start_spectating_inner(device).map_err(js_err)
    }

    /// The spectator's initial fingerprint, sent to the host for comparison.
    pub fn spectate_identity(&self) -> Vec<u8> {
        self.spectator
            .as_ref()
            .map_or_else(Vec::new, |s| s.identity().to_vec())
    }

    /// Feed bytes from the host, split anywhere.
    pub fn spectate_receive(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.spectate_receive_inner(bytes).map_err(js_err)
    }

    /// [spectating, frame, behind, checked, disk changes applied].
    pub fn spectate_status(&self) -> Vec<f64> {
        self.spectator.as_ref().map_or_else(Vec::new, |s| {
            vec![
                1.0,
                s.executed() as f64,
                s.behind() as f64,
                s.checked() as f64,
                s.swaps_applied() as f64,
            ]
        })
    }

    pub fn netplay_resume(&mut self) -> Result<(), JsValue> {
        if !self.netplay_swap_ready() || !self.netplay_swap.as_ref().is_some_and(|s| s.applied) {
            return Err(JsValue::from_str("Disk swap has not been applied"));
        }
        self.netplay_swap = None;
        self.anchor = None;
        Ok(())
    }
}

impl WebEmu {
    pub(super) fn netplay_mouse(&self) -> bool {
        self.netplay.as_ref().is_some_and(|peer| {
            self.emu.bus().input.ports[peer.player()].device == PortDevice::Mouse
        })
    }

    fn validate_netplay_disk(
        &self,
        drive: usize,
        bytes: Vec<u8>,
        writable: bool,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.netplay
                .as_ref()
                .is_some_and(|peer| peer.status().connected),
            "No connected netplay session"
        );
        anyhow::ensure!(
            self.emu.bus().floppy.drive_connected(drive),
            "Floppy drive is not connected"
        );
        anyhow::ensure!(
            bytes.len() <= 16 * 1024 * 1024,
            "Replacement disk exceeds 16 MiB"
        );
        if !bytes.is_empty() {
            // Decode before the two-phase commit, without changing drive state.
            copperline::floppy::FloppyController::default()
                .insert_memory_disk_image_bytes_with_limit(
                    0,
                    bytes,
                    "replacement".into(),
                    !writable,
                    16 * 1024 * 1024,
                )?;
        }
        Ok(())
    }
    fn start_netplay_inner(
        &mut self,
        settings: Settings,
        device: PortDevice,
    ) -> anyhow::Result<()> {
        let mut cfg = self.config.clone();
        cfg.serial.mode = copperline::config::SerialMode::Off;
        copperline::netplay::prepare_config(&mut cfg)?;
        let checkpoint = self.emu.netplay_snapshot()?;
        let volume = self.emu.bus().output_volume_percent();
        let serial = std::mem::replace(
            &mut self.emu.bus_mut().paula.serial,
            Box::new(copperline::serial::NullSerialSink),
        );
        self.emu.bus_mut().set_output_volume_percent(100);
        self.emu
            .bus_mut()
            .rtc
            .set_seed(Some(copperline::netplay::RTC_SEED), false);
        for port in 0..2 {
            self.emu.bus_mut().input.set_port_device(port, device);
        }
        let connection =
            Connection::with_transport(settings, PacketQueue::default(), &mut self.emu, &cfg);
        match connection {
            Ok(connection) => self.netplay = Some(connection),
            Err(error) => {
                let restored = self.emu.netplay_restore(&checkpoint);
                self.emu.bus_mut().paula.serial = serial;
                restored?;
                return Err(error);
            }
        }
        self.netplay_volume = volume;
        self.mouse_pending = (0, 0);
        self.mouse_remainder = (0.0, 0.0);
        self.netplay_input = Default::default();
        self.anchor = None;
        Ok(())
    }

    fn start_spectating_inner(&mut self, device: PortDevice) -> anyhow::Result<()> {
        let mut cfg = self.config.clone();
        cfg.serial.mode = copperline::config::SerialMode::Off;
        copperline::netplay::prepare_config(&mut cfg)?;
        let checkpoint = self.emu.netplay_snapshot()?;
        let volume = self.emu.bus().output_volume_percent();
        let serial = std::mem::replace(
            &mut self.emu.bus_mut().paula.serial,
            Box::new(copperline::serial::NullSerialSink),
        );
        self.emu.bus_mut().set_output_volume_percent(100);
        self.emu
            .bus_mut()
            .rtc
            .set_seed(Some(copperline::netplay::RTC_SEED), false);
        for port in 0..2 {
            self.emu.bus_mut().input.set_port_device(port, device);
        }
        match copperline::netplay::machine_identity(&mut self.emu, &cfg) {
            Ok(identity) => self.spectator = Some(Spectator::new(identity)),
            Err(error) => {
                let restored = self.emu.netplay_restore(&checkpoint);
                self.emu.bus_mut().paula.serial = serial;
                restored?;
                return Err(error);
            }
        }
        self.netplay_volume = volume;
        self.mouse_pending = (0, 0);
        self.mouse_remainder = (0.0, 0.0);
        self.netplay_input = Default::default();
        self.spectate_catchup = false;
        self.anchor = None;
        Ok(())
    }

    fn feed_open_inner(&mut self) -> anyhow::Result<u32> {
        let feed = self
            .netplay
            .as_ref()
            .and_then(|peer| peer.feed())
            .context("Spectators are not enabled for this session")?;
        anyhow::ensure!(
            !feed.full(),
            "The game's history is too large for another spectator to replay"
        );
        let id = self
            .feed_cursors
            .last_key_value()
            .map_or(0, |(id, _)| id + 1);
        self.feed_cursors.insert(id, Default::default());
        Ok(id)
    }

    fn feed_take_inner(&mut self, id: u32, max_bytes: usize) -> anyhow::Result<Vec<u8>> {
        let cursor = *self
            .feed_cursors
            .get(&id)
            .context("Unknown spectator feed")?;
        let feed = self
            .netplay
            .as_ref()
            .and_then(|peer| peer.feed())
            .context("No spectator feed")?;
        let (bytes, next) = feed.encode_from(cursor, max_bytes);
        self.feed_cursors.insert(id, next);
        Ok(bytes)
    }

    fn spectate_receive_inner(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.spectator
            .as_mut()
            .context("No spectator session")?
            .push(bytes)
    }

    /// A player's rollback session or a spectator's replay owns the machine.
    pub(super) fn session_active(&self) -> bool {
        self.netplay.is_some() || self.spectator.is_some()
    }

    pub(super) fn require_local_session(&self) -> Result<(), JsValue> {
        if self.session_active() {
            Err(JsValue::from_str(
                "Unavailable during netplay; free this instance and create a new WebEmu",
            ))
        } else {
            Ok(())
        }
    }

    /// Execute the host's confirmed frames: paced while close behind the
    /// players, in bounded unpaced bursts while replaying a backlog.
    pub(super) fn run_spectate(
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
        let spectator = self.spectator.as_mut().unwrap();
        // A checkpoint due at this frame is compared before a disk change
        // at the same frame changes the machine, as the host did.
        let settled = spectator.verify_frame(&mut self.emu).map_err(js_err)?;
        if let Some(swap) = spectator.due_swap().filter(|_| settled).cloned() {
            spectate::apply_swap(&mut self.emu, &swap).map_err(js_err)?;
            spectator.swap_applied();
            self.anchor = None;
        }
        let mut stepped = 0;
        if spectator.behind() > CATCHUP_START {
            self.spectate_catchup = true;
            self.anchor = None;
            while stepped < CATCHUP_FRAMES
                && started.elapsed().as_secs_f64() * 1000.0 < CATCHUP_MS
                && spectator.run_frame(&mut self.emu).map_err(js_err)?
            {
                stepped += 1;
            }
        } else {
            self.spectate_catchup = false;
            let (wall, emulated) = *self
                .anchor
                .get_or_insert((now_ms, self.emu.bus().emulated_seconds()));
            let target = emulated + (now_ms - wall) / 1000.0;
            while self.emu.bus().emulated_seconds() < target && stepped < max_frames.min(8) {
                if !spectator.run_frame(&mut self.emu).map_err(js_err)? {
                    self.anchor = Some((now_ms, self.emu.bus().emulated_seconds()));
                    break;
                }
                stepped += 1;
            }
            if target - self.emu.bus().emulated_seconds() > MAX_CATCHUP_SECONDS {
                self.anchor = Some((now_ms, self.emu.bus().emulated_seconds()));
            }
        }
        spectator.verify_frame(&mut self.emu).map_err(js_err)?;
        self.last_run_core_ms = started.elapsed().as_secs_f64() * 1000.0;
        if render && (stepped > 0 || self.deferred_fields > 0) {
            let render_started = Instant::now();
            self.render_completed_frame_elapsed(
                self.deferred_fields.saturating_add(stepped).max(1),
            );
            self.deferred_fields = 0;
            self.last_run_render_ms = render_started.elapsed().as_secs_f64() * 1000.0;
        } else if !render {
            self.deferred_fields = self.deferred_fields.saturating_add(stepped);
        }
        Ok(stepped)
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
        peer.step_local(&mut self.emu, &mut self.netplay_input, false)
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
            if self
                .netplay_swap
                .as_ref()
                .is_some_and(|swap| peer.status().frame >= swap.stop)
            {
                self.anchor = Some((now_ms, self.emu.bus().emulated_seconds()));
                break;
            }
            if !peer
                .step_local(&mut self.emu, &mut self.netplay_input, true)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(player: usize) -> Settings {
        Settings {
            player,
            session: [42; 16],
            input_delay: 0,
            rollback_frames: 8,
        }
    }

    #[test]
    fn failed_startup_restores_machine_and_host_state() -> anyhow::Result<()> {
        let mut web = WebEmu::new(Some("A500".into()), Some("PAL".into()), Some(1.0)).unwrap();
        web.insert_floppy(0, vec![0; 901_120], "original.adf")
            .unwrap();
        web.set_volume_percent(37);
        web.serial_set_carrier(true);
        web.mouse_delta(12.5, -3.25);
        web.anchor = Some((123.0, 0.0));
        // This is a runtime rejection after preparation, not argument validation.
        web.emu.enable_time_travel(8, 1);
        let before = web.emu.netplay_snapshot()?;
        let lines = web.emu.bus().paula.serial.control_lines();
        assert!(web
            .start_netplay_inner(settings(0), PortDevice::Cd32Pad)
            .unwrap_err()
            .to_string()
            .contains("reverse history"));
        assert!(
            web.emu.netplay_snapshot()? == before,
            "startup changed the machine"
        );
        assert_eq!(web.emu.bus().paula.serial.control_lines(), lines);
        assert_eq!(web.mouse_pending, (12, -3));
        assert_eq!(web.mouse_remainder, (0.5, -0.25));
        assert_eq!(web.anchor, Some((123.0, 0.0)));
        assert!(web.netplay.is_none());
        web.emu.disable_time_travel();
        web.start_netplay_inner(settings(0), PortDevice::Cd32Pad)?;
        Ok(())
    }

    #[test]
    fn netplay_mouse_routes_both_players_without_touching_the_live_machine() -> anyhow::Result<()> {
        for player in 0..2 {
            let mut web = WebEmu::new(None, None, Some(0.0)).unwrap();
            web.start_netplay_inner(settings(player), PortDevice::Mouse)?;
            let before = web.emu.netplay_snapshot()?;
            web.mouse_delta(12.5, -3.25);
            web.mouse_delta(-1.0, 0.75);
            assert_eq!(web.netplay_input.mouse_pending, (12, -3));
            assert_eq!(web.mouse_remainder, (-0.5, 0.5));
            web.mouse_delta(f64::NAN, f64::INFINITY);
            for (button, bit) in [(0, 0), (1, 2), (2, 1)] {
                web.mouse_button(button, true);
                assert_eq!(web.netplay_input.held.mouse_buttons, 1 << bit);
                web.set_joystick_port2(true, true, true, true, true, true);
                web.set_cd32_buttons_port2(true, true, true, true, true);
                assert_eq!(web.netplay_input.held.mouse_buttons, 1 << bit);
                assert_eq!(web.netplay_input.held.buttons, 0);
                web.mouse_button(button, false);
                assert_eq!(web.netplay_input.held.mouse_buttons, 0);
            }
            web.key_event("ArrowUp", true);
            assert_ne!(web.netplay_input.held.keys, [0; 16]);
            assert!(
                web.emu.netplay_snapshot()? == before,
                "host input bypassed the timeline"
            );
            web.netplay_release_input();
            assert_eq!(web.netplay_input, Default::default());
            assert_eq!(web.mouse_remainder, (0.0, 0.0));
            web.mouse_delta(70_000.0, -90_000.0);
            assert_eq!(web.netplay_input.mouse_pending, (70_000, -90_000));
            web.netplay_release_input();
            assert_eq!(web.netplay_input.mouse_pending, (0, 0));
        }
        Ok(())
    }

    #[test]
    fn spectators_replay_the_host_feed_from_frame_zero_and_take_no_input() -> anyhow::Result<()> {
        // Four machines at once exceed the default test stack in debug builds.
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(spectators_replay_the_host_feed)?
            .join()
            .unwrap()
    }

    fn spectators_replay_the_host_feed() -> anyhow::Result<()> {
        let mut host = WebEmu::new(None, None, Some(1.0)).unwrap();
        let mut guest = WebEmu::new(None, None, Some(1.0)).unwrap();
        host.start_netplay_inner(settings(0), PortDevice::Cd32Pad)?;
        guest.start_netplay_inner(settings(1), PortDevice::Cd32Pad)?;
        host.netplay_enable_spectators().unwrap();
        host.feed_open_inner()?;
        let exchange = |host: &mut WebEmu, guest: &mut WebEmu| -> anyhow::Result<()> {
            loop {
                let to_guest = host.netplay_take_packet();
                let to_host = guest.netplay_take_packet();
                if to_guest.is_empty() && to_host.is_empty() {
                    return Ok(());
                }
                if !to_guest.is_empty() {
                    guest.netplay_receive(&to_guest).unwrap();
                }
                if !to_host.is_empty() {
                    host.netplay_receive(&to_host).unwrap();
                }
            }
        };
        let mut tick = 0u32;
        let play = |host: &mut WebEmu,
                    guest: &mut WebEmu,
                    tick: &mut u32,
                    until: f64|
         -> anyhow::Result<()> {
            while host.netplay_status()[2] < until || guest.netplay_status()[2] < until {
                *tick += 1;
                for (emu, player) in [(&mut *host, 0u32), (&mut *guest, 1u32)] {
                    let frame = emu.netplay_status()[1];
                    let step = u32::from(frame < until);
                    let frame = frame as u32;
                    emu.set_joystick_port(
                        2,
                        frame % 9 < 3,
                        false,
                        false,
                        false,
                        frame % 7 < 3,
                        false,
                    );
                    emu.key_event("Space", (frame + player) % 13 < 4);
                    emu.run_hidden(f64::from(*tick) * 20.0, step).unwrap();
                }
                exchange(host, guest)?;
                anyhow::ensure!(*tick < 100_000, "players did not confirm {until} frames");
            }
            Ok(())
        };
        // A disk change exactly on the 120-frame checkpoint boundary: the
        // host hashes the boundary first, then changes the machine, and a
        // spectator must reproduce that order.
        play(&mut host, &mut guest, &mut tick, 120.0)?;
        for emu in [&mut host, &mut guest] {
            assert_eq!(emu.netplay_hold().unwrap(), 120.0);
            emu.netplay_stop_at(120.0).unwrap();
        }
        while !(host.netplay_swap_ready() && guest.netplay_swap_ready()) {
            tick += 1;
            for emu in [&mut host, &mut guest] {
                emu.run_hidden(f64::from(tick) * 20.0, 0).unwrap();
            }
            exchange(&mut host, &mut guest)?;
            anyhow::ensure!(tick < 100_000, "players did not settle on the boundary");
        }
        for emu in [&mut host, &mut guest] {
            emu.netplay_stage_disk(0.0, vec![3; 901_120], false)
                .unwrap();
            emu.netplay_apply_disk().unwrap();
            emu.netplay_resume().unwrap();
        }
        play(&mut host, &mut guest, &mut tick, 130.0)?;
        assert_eq!(host.spectator_feed_frames(), 130.0);
        assert_eq!(host.disk_name(0).as_deref(), Some("netplay-df0"));

        // A spectator built from the same cold media follows the host's
        // history from frame zero, verifies every checkpoint, and reaches
        // the host's machine state regardless of what it holds locally.
        let mut spectator = WebEmu::new(None, None, Some(1.0)).unwrap();
        spectator.start_spectating_inner(PortDevice::Cd32Pad)?;
        assert!(spectator.session_active());
        assert_eq!(spectator.spectate_identity(), host.netplay_identity());
        assert_eq!(spectator.spectate_status(), vec![1.0, 0.0, 0.0, 0.0, 0.0]);
        let before = spectator.emu.netplay_snapshot()?;
        spectator.set_joystick_port(2, true, true, true, true, true, true);
        spectator.set_cd32_buttons_port(2, true, true, true, true, true);
        spectator.key_event("Space", true);
        spectator.key_raw(0x40, true);
        spectator.mouse_delta(12.0, 5.0);
        spectator.mouse_button(0, true);
        assert_eq!(spectator.netplay_input, Default::default());
        assert_eq!(spectator.emu.netplay_snapshot()?, before);
        let cursor = host.feed_open_inner()?;
        let mut seed = 7u64;
        let mut runs = 0;
        loop {
            let bytes = host.feed_take_inner(cursor, 4096)?;
            let mut offset = 0;
            while offset < bytes.len() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let len = (1 + (seed >> 33) as usize % 700).min(bytes.len() - offset);
                spectator.spectate_receive_inner(&bytes[offset..offset + len])?;
                offset += len;
            }
            // A backlog replays regardless of the clock; the last stretch is
            // paced by it, so the clock ticks like a page's would.
            spectator.run_hidden(f64::from(runs) * 20.0, 8).unwrap();
            runs += 1;
            if bytes.is_empty() && spectator.spectate_status()[2] == 0.0 {
                break;
            }
            anyhow::ensure!(runs < 10_000, "spectator did not drain the feed");
        }
        let status = spectator.spectate_status();
        assert_eq!(status, vec![1.0, 130.0, 0.0, 120.0, 1.0]);
        assert_eq!(spectator.disk_name(0).as_deref(), Some("netplay-df0"));
        assert!(!spectator.spectate_catchup, "level with the host again");
        assert_eq!(
            spectator.emu.netplay_snapshot()?,
            host.emu.netplay_snapshot()?
        );
        host.spectator_feed_close(cursor);
        assert!(host.feed_take_inner(cursor, 4096).is_err());
        assert!(host.feed_take_inner(99, 4096).is_err());
        // Corrupt feed bytes fail closed rather than running a different game.
        let mut broken = WebEmu::new(None, None, Some(1.0)).unwrap();
        broken.start_spectating_inner(PortDevice::Cd32Pad)?;
        let cursor = host.feed_open_inner()?;
        let mut bytes = host.feed_take_inner(cursor, 4096)?;
        bytes[5] ^= 0x80;
        assert!(broken.spectate_receive_inner(&bytes).is_err());
        // The guest keeps no history and admits nobody.
        assert!(guest.feed_open_inner().is_err());
        assert_eq!(guest.spectator_feed_frames(), 0.0);
        Ok(())
    }

    #[test]
    fn netplay_routes_each_controller_button_and_keyboard_and_scales_local_audio(
    ) -> anyhow::Result<()> {
        for player in 0..2 {
            let mut web = WebEmu::new(None, None, Some(0.0)).unwrap();
            web.start_netplay_inner(settings(player), PortDevice::Cd32Pad)?;
            for bit in 0..11 {
                let on = |i| i == bit;
                web.set_joystick_port2(on(0), on(1), on(2), on(3), on(4), on(5));
                web.set_cd32_buttons_port2(on(6), on(7), on(8), on(9), on(10));
                assert_eq!(web.netplay_input.held.buttons, 1 << bit);
                // The secondary page controller cannot overwrite the primary.
                web.set_joystick_port(1, false, false, false, false, false, false);
                web.set_cd32_buttons_port(1, false, false, false, false, false);
                assert_eq!(web.netplay_input.held.buttons, 1 << bit);
            }
            assert!(web.key_event("Space", true));
            web.key_raw(0x20, true);
            assert_eq!(web.netplay_input.held.keys[8], 1);
            assert_eq!(web.netplay_input.held.keys[4], 1);
            web.key_raw(0x20, false);
            assert_eq!(web.netplay_input.held.keys[4], 0);
            web.netplay_release_input();
            assert_eq!(web.netplay_input, Default::default());
            for volume in [0, 35, 100] {
                web.set_volume_percent(volume);
                web.audio.borrow_mut().extend([1.0, -0.5]);
                let gain = f32::from(volume) / 100.0;
                assert_eq!(web.take_audio(), vec![gain, -0.5 * gain]);
                assert_eq!(web.emu.bus().output_volume_percent(), 100);
            }
        }
        Ok(())
    }
}
