// SPDX-License-Identifier: GPL-3.0-or-later

//! Live-input recording to the scripted-input format.
//!
//! While recording, every input event that reaches the emulated machine is
//! logged with its emulated timestamp and written out as a script of
//! `--press-after`-style directives (one per line, without the leading
//! dashes) that `--script FILE` replays deterministically. Combined with a
//! save state, a recording is a complete, shareable reproduction of a
//! manually driven session.
//!
//! Two capture styles feed the recorder:
//!
//! - Direct hooks for events with identity the bus state cannot expose
//!   afterwards: Amiga key transitions (`record_key`, from
//!   `handle_amiga_key_event`) and floppy inserts (`record_disk_insert`).
//! - A once-per-quantum `observe` of the live `InputState`, which diffs
//!   each controller port according to the device plugged into it: mouse
//!   buttons and quadrature counters (wrapped u8 deltas become
//!   `mouse-after` lines), joystick/CD32-pad controls, or analogue pot
//!   positions. Frame-rate granularity (~20 ms PAL) is the same
//!   resolution the replay side schedules at.
//!
//! Port tokens are emitted only when they differ from a directive's
//! default port (`click-after`/`mouse-after` default to port 1,
//! `joy-after`/`pot-after` to port 2), so a session on the default
//! mouse+joystick wiring records byte-identically to the pre-port-aware
//! format. A device change mid-recording (hot-plug) closes that port's
//! open holds; the change itself has no script directive and is not
//! replayed.
//!
//! Press/release pairs are merged into hold directives (`key-after` /
//! `click-after` / `joy-after`) keyed at the press time; controls still
//! held when the recording stops are closed at the final observed time.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::bus::{ControllerPort, InputState, PortDevice};
use crate::chipset::paula::pot_resistance_position;

/// Minimum emitted hold so a press/release pair that lands inside one
/// quantum still asserts the control for at least one emulated frame on
/// replay.
const MIN_HOLD_MS: u32 = 20;

/// Reads one control's held state out of a controller port.
type ControlRead = fn(&ControllerPort) -> bool;

/// The joystick/CD32-pad controls the per-quantum diff watches: the
/// canonical `joy-after` name (accepted by `JoyButtonKind::parse`) and the
/// port line each one reads.
const JOY_CONTROLS: [(&str, ControlRead); 11] = [
    ("up", |p| p.up),
    ("down", |p| p.down),
    ("left", |p| p.left),
    ("right", |p| p.right),
    ("red", |p| p.fire),
    ("blue", |p| p.button2),
    ("green", |p| p.cd32_green),
    ("yellow", |p| p.cd32_yellow),
    ("play", |p| p.cd32_play),
    ("rwd", |p| p.cd32_rwd),
    ("ffw", |p| p.cd32_ffw),
];

/// Mouse buttons by `click-after` name: left = /FIRx, right = POTxY,
/// middle = POTxX.
const MOUSE_BUTTONS: [(&str, ControlRead); 3] = [
    ("left", |p| p.fire),
    ("right", |p| p.button2),
    ("middle", |p| p.button3),
];

/// Trailing port token for a mouse directive (default port 1).
fn mouse_port_suffix(port: usize) -> &'static str {
    if port == 1 {
        " 2"
    } else {
        ""
    }
}

/// Trailing port token for a joystick/pot directive (default port 2).
fn joy_port_suffix(port: usize) -> &'static str {
    if port == 0 {
        " 1"
    } else {
        ""
    }
}

/// Both analogue pot positions of a port, when both axes are connected.
fn pot_positions(p: &ControllerPort) -> Option<(u8, u8)> {
    match (p.pot_x_ohms, p.pot_y_ohms) {
        (Some(x), Some(y)) => Some((pot_resistance_position(x), pot_resistance_position(y))),
        _ => None,
    }
}

/// Default recording file name, timestamped like the screenshot/recorder
/// names.
pub fn auto_filename() -> PathBuf {
    let ts = crate::timestamp::compact_now();
    PathBuf::from(format!("copperline-input-{ts}.clscript"))
}

pub struct InputRecorder {
    /// Emulated time of the most recent `observe`, used to close holds
    /// that are still open when the recording stops.
    last_secs: f64,
    /// Finished directives, keyed by the press/event time for sorting.
    lines: Vec<(f64, String)>,
    /// Open key presses: rawkey -> press time.
    open_keys: HashMap<u8, f64>,
    /// Open mouse-button presses per port, indexed like MOUSE_BUTTONS.
    open_clicks: [[Option<f64>; 3]; 2],
    /// Open joystick/pad control presses per port, indexed like
    /// JOY_CONTROLS.
    open_joys: [[Option<f64>; 11]; 2],
    /// Input state at the previous `observe`, the diff baseline. Controls
    /// already held when recording starts are not recorded.
    prev: Option<InputState>,
    /// Devices seen on the first `observe`, for the script header note.
    first_devices: Option<[PortDevice; 2]>,
}

/// Format an event time truncated (not rounded) to milliseconds. Replay
/// fires an event at the first frame boundary at-or-after its timestamp,
/// so rounding a boundary time UP would push the event one frame late;
/// truncation keeps it at-or-just-before the boundary it was recorded on.
fn fmt_secs(secs: f64) -> String {
    format!("{:.3}", (secs * 1000.0).floor() / 1000.0)
}

fn hold_ms(press_secs: f64, release_secs: f64) -> u32 {
    // Floored for the same boundary-preserving reason as fmt_secs.
    (((release_secs - press_secs) * 1000.0).floor().max(0.0) as u32).max(MIN_HOLD_MS)
}

impl InputRecorder {
    pub fn new(now_secs: f64) -> Self {
        Self {
            last_secs: now_secs,
            lines: Vec::new(),
            open_keys: HashMap::new(),
            open_clicks: [[None; 3]; 2],
            open_joys: [[None; 11]; 2],
            prev: None,
            first_devices: None,
        }
    }

    pub fn events_recorded(&self) -> usize {
        self.lines.len()
            + self.open_keys.len()
            + self.open_clicks.iter().flatten().flatten().count()
            + self.open_joys.iter().flatten().flatten().count()
    }

    /// Record an Amiga key transition that reached the keyboard queue
    /// (call from the single key choke point, after the reset chord has
    /// had its chance to consume the event).
    pub fn record_key(&mut self, rawkey: u8, pressed: bool, secs: f64) {
        if pressed {
            self.open_keys.entry(rawkey).or_insert(secs);
        } else if let Some(press) = self.open_keys.remove(&rawkey) {
            self.lines.push((
                press,
                format!(
                    "key-after {} 0x{rawkey:02X} {}",
                    fmt_secs(press),
                    hold_ms(press, secs)
                ),
            ));
        }
    }

    /// Record a floppy insert that succeeded.
    pub fn record_disk_insert(&mut self, drive_idx: usize, path: &Path, secs: f64) {
        let path = path.display().to_string();
        let path = if path.chars().any(char::is_whitespace) {
            format!("\"{path}\"")
        } else {
            path
        };
        self.lines.push((
            secs,
            format!("insert-disk-after {} df{drive_idx} {path}", fmt_secs(secs)),
        ));
    }

    fn emit_click(&mut self, port: usize, name: &str, press: f64, release: f64) {
        self.lines.push((
            press,
            format!(
                "click-after {} {name} {}{}",
                fmt_secs(press),
                hold_ms(press, release),
                mouse_port_suffix(port)
            ),
        ));
    }

    fn emit_joy(&mut self, port: usize, name: &str, press: f64, release: f64) {
        self.lines.push((
            press,
            format!(
                "joy-after {} {name} {}{}",
                fmt_secs(press),
                hold_ms(press, release),
                joy_port_suffix(port)
            ),
        ));
    }

    /// Close every hold a port's device left open, at `secs`.
    fn close_port_holds(&mut self, port: usize, secs: f64) {
        for idx in 0..MOUSE_BUTTONS.len() {
            if let Some(press) = self.open_clicks[port][idx].take() {
                self.emit_click(port, MOUSE_BUTTONS[idx].0, press, secs);
            }
        }
        for idx in 0..JOY_CONTROLS.len() {
            if let Some(press) = self.open_joys[port][idx].take() {
                self.emit_joy(port, JOY_CONTROLS[idx].0, press, secs);
            }
        }
    }

    /// Diff the live input state against the previous quantum, per port
    /// and according to the device plugged into it.
    pub fn observe(&mut self, input: &InputState, secs: f64) {
        self.last_secs = secs;
        let Some(prev) = self.prev.replace(*input) else {
            self.first_devices = Some([input.ports[0].device, input.ports[1].device]);
            return;
        };

        for port in 0..2 {
            let old = &prev.ports[port];
            let cur = &input.ports[port];
            if old.device != cur.device {
                // Hot-plug: close what the old device held; the device
                // change itself has no script directive.
                self.close_port_holds(port, secs);
            }
            match cur.device {
                PortDevice::Mouse => {
                    let dx = cur.counter_x.wrapping_sub(old.counter_x) as i8;
                    let dy = cur.counter_y.wrapping_sub(old.counter_y) as i8;
                    if dx != 0 || dy != 0 {
                        self.lines.push((
                            secs,
                            format!(
                                "mouse-after {} {dx} {dy}{}",
                                fmt_secs(secs),
                                mouse_port_suffix(port)
                            ),
                        ));
                    }
                    for (idx, (name, read)) in MOUSE_BUTTONS.iter().enumerate() {
                        match (read(old), read(cur)) {
                            (false, true) => self.open_clicks[port][idx] = Some(secs),
                            (true, false) => {
                                if let Some(press) = self.open_clicks[port][idx].take() {
                                    self.emit_click(port, name, press, secs);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                PortDevice::Joystick | PortDevice::Cd32Pad => {
                    for (idx, (name, read)) in JOY_CONTROLS.iter().enumerate() {
                        match (read(old), read(cur)) {
                            (false, true) => self.open_joys[port][idx] = Some(secs),
                            (true, false) => {
                                if let Some(press) = self.open_joys[port][idx].take() {
                                    self.emit_joy(port, name, press, secs);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                PortDevice::Analogue => {
                    if let Some((x, y)) = pot_positions(cur) {
                        if pot_positions(old) != Some((x, y)) {
                            self.lines.push((
                                secs,
                                format!(
                                    "pot-after {} {x} {y}{}",
                                    fmt_secs(secs),
                                    joy_port_suffix(port)
                                ),
                            ));
                        }
                    }
                }
                PortDevice::None => {}
            }
        }
    }

    /// Close any still-held controls at the last observed time and render
    /// the script, sorted by event time.
    pub fn finish(mut self) -> String {
        let end = self.last_secs;
        let open_keys: Vec<(u8, f64)> = self.open_keys.drain().collect();
        for (rawkey, press) in open_keys {
            self.lines.push((
                press,
                format!(
                    "key-after {} 0x{rawkey:02X} {}",
                    fmt_secs(press),
                    hold_ms(press, end)
                ),
            ));
        }
        for port in 0..2 {
            self.close_port_holds(port, end);
        }
        self.lines
            .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut out = String::new();
        let _ = writeln!(out, "# Copperline input script (recorded live session)");
        let _ = writeln!(
            out,
            "# Replay: copperline --config <config> --script <this file>"
        );
        let _ = writeln!(out, "# Times are absolute emulated seconds.");
        if let Some(devices) = self.first_devices {
            if devices != [PortDevice::Mouse, PortDevice::Joystick] {
                let _ = writeln!(
                    out,
                    "# ports: port1={} port2={}",
                    devices[0].label(),
                    devices[1].label()
                );
            }
        }
        for (_, line) in &self.lines {
            let _ = writeln!(out, "{line}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_input() -> InputState {
        let mut input = InputState::default();
        // The stock machine wiring: mouse in port 1, joystick in port 2.
        input.set_port_device(1, PortDevice::Joystick);
        input
    }

    #[test]
    fn key_pairs_merge_into_key_after_holds() {
        let mut rec = InputRecorder::new(1.0);
        rec.record_key(0x45, true, 1.5);
        rec.record_key(0x45, false, 1.75);
        // Release without a recorded press is dropped, not paired backwards.
        rec.record_key(0x44, false, 1.8);
        let script = rec.finish();
        assert!(script.contains("key-after 1.500 0x45 250"), "{script}");
        assert!(!script.contains("0x44"), "{script}");
    }

    #[test]
    fn keys_still_held_at_stop_close_at_last_observed_time() {
        let mut rec = InputRecorder::new(0.0);
        rec.observe(&base_input(), 2.0);
        rec.record_key(0x60, true, 2.0);
        rec.observe(&base_input(), 3.0);
        let script = rec.finish();
        assert!(script.contains("key-after 2.000 0x60 1000"), "{script}");
    }

    #[test]
    fn mouse_motion_diffs_wrap_and_coalesce_per_observe() {
        let mut rec = InputRecorder::new(0.0);
        let mut input = base_input();
        rec.observe(&input, 1.0);
        // Forward motion, including a wrap through 255 -> 2.
        input.ports[0].counter_x = 253;
        input.ports[0].counter_y = 10;
        rec.observe(&input, 1.02);
        input.ports[0].counter_x = 2;
        rec.observe(&input, 1.04);
        // No motion: no line.
        rec.observe(&input, 1.06);
        let script = rec.finish();
        assert!(script.contains("mouse-after 1.020 -3 10"), "{script}");
        assert!(script.contains("mouse-after 1.040 5 0"), "{script}");
        assert_eq!(script.matches("mouse-after").count(), 2, "{script}");
    }

    #[test]
    fn buttons_and_pad_controls_pair_across_observes() {
        let mut rec = InputRecorder::new(0.0);
        let mut input = base_input();
        rec.observe(&input, 5.0);
        input.ports[0].fire = true;
        input.ports[1].cd32_green = true;
        rec.observe(&input, 5.5);
        input.ports[0].fire = false;
        rec.observe(&input, 6.0);
        input.ports[1].cd32_green = false;
        rec.observe(&input, 6.5);
        let script = rec.finish();
        assert!(script.contains("click-after 5.500 left 500"), "{script}");
        assert!(script.contains("joy-after 5.500 green 1000"), "{script}");
    }

    #[test]
    fn default_wiring_emits_no_port_tokens_or_ports_header() {
        let mut rec = InputRecorder::new(0.0);
        let mut input = base_input();
        rec.observe(&input, 1.0);
        input.ports[0].fire = true;
        input.ports[1].up = true;
        rec.observe(&input, 1.5);
        input.ports[0].fire = false;
        input.ports[1].up = false;
        rec.observe(&input, 2.0);
        let script = rec.finish();
        assert!(script.contains("click-after 1.500 left 500\n"), "{script}");
        assert!(script.contains("joy-after 1.500 up 500\n"), "{script}");
        assert!(!script.contains("# ports:"), "{script}");
    }

    #[test]
    fn swapped_ports_emit_port_tokens_and_a_ports_header() {
        let mut rec = InputRecorder::new(0.0);
        let mut input = InputState::default();
        input.set_port_device(0, PortDevice::Joystick);
        input.set_port_device(1, PortDevice::Mouse);
        rec.observe(&input, 1.0);
        input.ports[0].up = true;
        input.ports[1].fire = true;
        input.ports[1].counter_x = 7;
        rec.observe(&input, 1.5);
        input.ports[0].up = false;
        input.ports[1].fire = false;
        rec.observe(&input, 2.0);
        let script = rec.finish();
        assert!(
            script.contains("# ports: port1=joystick port2=mouse"),
            "{script}"
        );
        assert!(script.contains("joy-after 1.500 up 500 1"), "{script}");
        assert!(script.contains("click-after 1.500 left 500 2"), "{script}");
        assert!(script.contains("mouse-after 1.500 7 0 2"), "{script}");
    }

    #[test]
    fn analogue_position_changes_emit_pot_after_lines() {
        let mut rec = InputRecorder::new(0.0);
        let mut input = base_input();
        input.set_analogue(1, 128, 128);
        rec.observe(&input, 1.0);
        input.set_analogue(1, 50, 200);
        rec.observe(&input, 1.5);
        // Unchanged position: no line.
        rec.observe(&input, 2.0);
        let script = rec.finish();
        assert!(script.contains("pot-after 1.500 50 200\n"), "{script}");
        assert_eq!(script.matches("pot-after").count(), 1, "{script}");
    }

    #[test]
    fn hot_plug_closes_the_old_devices_open_holds() {
        let mut rec = InputRecorder::new(0.0);
        let mut input = base_input();
        rec.observe(&input, 1.0);
        input.ports[1].fire = true;
        rec.observe(&input, 1.5);
        input.set_port_device(1, PortDevice::Mouse);
        rec.observe(&input, 2.0);
        let script = rec.finish();
        assert!(script.contains("joy-after 1.500 red 500"), "{script}");
    }

    #[test]
    fn sub_quantum_pairs_get_a_minimum_hold() {
        let mut rec = InputRecorder::new(0.0);
        rec.record_key(0x35, true, 1.0);
        rec.record_key(0x35, false, 1.0);
        let script = rec.finish();
        assert!(script.contains("key-after 1.000 0x35 20"), "{script}");
    }

    #[test]
    fn disk_inserts_quote_paths_with_spaces() {
        let mut rec = InputRecorder::new(0.0);
        rec.record_disk_insert(0, Path::new("/tmp/plain.adf"), 3.0);
        rec.record_disk_insert(1, Path::new("/tmp/with space.adf"), 4.0);
        let script = rec.finish();
        assert!(
            script.contains("insert-disk-after 3.000 df0 /tmp/plain.adf"),
            "{script}"
        );
        assert!(
            script.contains("insert-disk-after 4.000 df1 \"/tmp/with space.adf\""),
            "{script}"
        );
    }

    #[test]
    fn output_is_sorted_by_event_time() {
        let mut rec = InputRecorder::new(0.0);
        let mut input = base_input();
        rec.observe(&input, 1.0);
        rec.record_key(0x20, true, 9.0);
        rec.record_key(0x20, false, 9.5);
        input.ports[0].counter_x = 4;
        rec.observe(&input, 2.0);
        let script = rec.finish();
        let mouse_pos = script.find("mouse-after").unwrap();
        let key_pos = script.find("key-after").unwrap();
        assert!(mouse_pos < key_pos, "{script}");
    }
}
