// SPDX-License-Identifier: GPL-3.0-or-later

//! Per-connection session state shared by both control-server modes:
//! server-assigned breakpoint ids over the machine's interactive break
//! store, input scheduled on the emulated timeline, and the journaling
//! hooks that keep a control-driven session deterministically
//! replayable (`tt_note_input` for reverse replay, `InputRecorder` for
//! `--record-input`).

use crate::debugger::{BreakCond, WatchSource};
use crate::emulator::Emulator;
use crate::inputrec::InputRecorder;
use crate::inputsched::{JoyState, ReplayAction};
use std::collections::BTreeMap;

/// One installed break of any kind, as requested over the protocol.
/// Mirrors the parameter set of the `ui_*` install calls so removal can
/// re-toggle the exact same point.
#[derive(Clone, Debug, PartialEq)]
pub enum BreakSpec {
    Pc {
        addr: u32,
        cond: Option<BreakCond>,
        ignore: u32,
    },
    Watch {
        addr: u32,
        source: Option<WatchSource>,
        /// Stop only when this instruction made the access.
        pc: Option<u32>,
    },
    RegWatch {
        off: u16,
    },
    Beam {
        vpos: u16,
        hpos: Option<u16>,
    },
    Copper {
        addr: u32,
    },
    Catch {
        vector: u16,
    },
}

/// One machine-visible input transition a client asked for. Applied
/// through the same bus primitives as live and scripted input, so it
/// journals identically.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputAction {
    Key {
        rawkey: u8,
        pressed: bool,
    },
    /// Mouse button on a port (0 = port 1, 1 = port 2): index 0 = left,
    /// 1 = right, 2 = middle.
    MouseButton {
        port: u8,
        index: u8,
        pressed: bool,
    },
    MouseMove {
        port: u8,
        dx: i32,
        dy: i32,
    },
    Joy {
        port: u8,
        state: JoyState,
    },
    /// Analogue pot positions on a port.
    Pot {
        port: u8,
        x: u8,
        y: u8,
    },
}

/// An input action deferred to an emulated-time boundary (`at_seconds`
/// scheduling and `hold_ms` releases).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScheduledInput {
    pub at_seconds: f64,
    pub action: InputAction,
}

/// Session state carried across requests on one connection.
pub struct SessionCtx {
    /// Server-assigned id -> installed break. Ordered so `break.list`
    /// output is stable.
    breaks: BTreeMap<u32, BreakSpec>,
    next_break_id: u32,
    /// Input waiting for its emulated time; drained by the driver at
    /// its command boundary.
    pub scheduled: Vec<ScheduledInput>,
    /// Journaling recorder owned by the headless driver
    /// (`--record-input`); the windowed drain journals through the
    /// App's recorder instead and leaves this `None`.
    pub recorder: Option<InputRecorder>,
    /// Host-state flags the driver keeps updated for `status`.
    pub running: bool,
    /// A resume verb's response is still outstanding.
    pub pending: bool,
    pub powered_on: bool,
    observations: super::observe::Observer,
}

impl SessionCtx {
    pub fn new() -> Self {
        Self {
            breaks: BTreeMap::new(),
            next_break_id: 1,
            scheduled: Vec::new(),
            recorder: None,
            running: false,
            pending: false,
            powered_on: true,
            observations: super::observe::Observer::new(),
        }
    }

    /// Install `spec` on the machine and assign it an id. Refuses a
    /// point that already exists (whether set by this session or the
    /// local GUI), because the underlying store toggles: installing
    /// twice would silently remove it.
    pub fn install_break(&mut self, emu: &mut Emulator, spec: BreakSpec) -> Result<u32, String> {
        // Store the spec exactly as the machine's break stores key it,
        // so existence checks, `break.list` id attachment, and removal
        // all compare like with like whatever address form the client
        // sent.
        let spec = normalize_spec(emu, spec);
        if self.break_exists(emu, &spec) {
            return Err(format!("already set: {}", describe_spec(&spec)));
        }
        let installed = toggle_spec(emu, &spec);
        if !installed {
            // A toggle that reports "removed" against a point we just
            // checked as absent means the store disagreed; restore and
            // report rather than leave it half-changed.
            toggle_spec(emu, &spec);
            return Err(format!("could not install: {}", describe_spec(&spec)));
        }
        let id = self.next_break_id;
        self.next_break_id += 1;
        self.breaks.insert(id, spec);
        Ok(id)
    }

    /// Remove the break with server id `id`. Returns false only for an
    /// unknown id; a point the GUI already removed still counts as
    /// success (the goal state is reached either way).
    pub fn remove_break(&mut self, emu: &mut Emulator, id: u32) -> bool {
        let Some(spec) = self.breaks.remove(&id) else {
            return false;
        };
        if self.break_exists(emu, &spec) {
            toggle_spec(emu, &spec);
        }
        true
    }

    /// Remove every break this session installed (teardown on
    /// disconnect). GUI-set points are left alone.
    pub fn remove_all_breaks(&mut self, emu: &mut Emulator) {
        let ids: Vec<u32> = self.breaks.keys().copied().collect();
        for id in ids {
            self.remove_break(emu, id);
        }
    }

    /// The server id of an installed point matching `other`'s kind and
    /// coordinates. Condition/ignore differences do not matter for
    /// identification: the machine store keys points the same way.
    pub fn id_for(&self, other: &BreakSpec) -> Option<u32> {
        self.breaks
            .iter()
            .find(|(_, s)| same_point(s, other))
            .map(|(id, _)| *id)
    }

    pub fn breaks(&self) -> impl Iterator<Item = (u32, &BreakSpec)> {
        self.breaks.iter().map(|(id, s)| (*id, s))
    }

    /// Whether `spec`'s point currently exists in the machine's break
    /// store (regardless of who installed it).
    pub fn break_exists(&self, emu: &Emulator, spec: &BreakSpec) -> bool {
        match spec {
            BreakSpec::Pc { addr, .. } => emu.machine.ui_breaks().is_breakpoint(*addr),
            BreakSpec::Watch { addr, .. } => {
                let masked = addr & emu.machine.ui_breaks().addr_mask & !1;
                emu.machine
                    .ui_breaks()
                    .watches
                    .iter()
                    .any(|w| w.addr == masked)
            }
            BreakSpec::RegWatch { off } => emu.machine.ui_breaks().reg_watches.contains(off),
            BreakSpec::Beam { vpos, hpos } => emu
                .bus()
                .ui_beam_traps()
                .iter()
                .any(|t| !t.once && t.vpos == *vpos && t.hpos == *hpos),
            BreakSpec::Copper { addr } => emu.bus().ui_copper_breaks().contains(addr),
            BreakSpec::Catch { vector } => emu.machine.ui_breaks().catches.contains(vector),
        }
    }

    /// Queue `action` for `at_seconds` on the emulated clock, keeping
    /// the queue sorted by time.
    pub fn schedule(&mut self, at_seconds: f64, action: InputAction) {
        self.scheduled.push(ScheduledInput { at_seconds, action });
        self.scheduled
            .sort_by(|a, b| a.at_seconds.total_cmp(&b.at_seconds));
    }

    /// Apply every scheduled action whose time has arrived. Called by
    /// the headless driver at each quantum boundary (the windowed drain
    /// maps scheduling onto the App's own scheduled-input lists
    /// instead).
    pub fn apply_due_scheduled(&mut self, emu: &mut Emulator) {
        let now = emu.bus().emulated_seconds();
        while let Some(first) = self.scheduled.first() {
            if first.at_seconds > now {
                break;
            }
            let entry = self.scheduled.remove(0);
            inject_input(emu, &mut self.recorder, entry.action);
        }
    }

    /// Apply `action` now, journaled. Returns the emulated time it
    /// landed at.
    pub fn inject_now(&mut self, emu: &mut Emulator, action: InputAction) -> f64 {
        inject_input(emu, &mut self.recorder, action)
    }

    pub fn subscribe_events(
        &mut self,
        emu: &mut Emulator,
        events: &[super::observe::EventKind],
        frame_interval: Option<u64>,
        frame_digest: Option<bool>,
    ) -> serde_json::Value {
        self.observations
            .subscribe(emu, events, frame_interval, frame_digest)
    }

    pub fn unsubscribe_events(
        &mut self,
        emu: &mut Emulator,
        events: Option<&[super::observe::EventKind]>,
    ) -> serde_json::Value {
        self.observations.unsubscribe(emu, events)
    }

    pub fn event_subscriptions(&self) -> serde_json::Value {
        self.observations.list_value()
    }

    pub fn poll_events(&mut self, emu: &mut Emulator) -> Vec<String> {
        self.observations.poll(emu)
    }

    pub fn note_event_notification_dropped(&mut self) {
        self.observations.note_notification_dropped();
    }

    pub fn disable_events(&mut self, emu: &mut Emulator) {
        self.observations.disable(emu);
    }
}

impl Default for SessionCtx {
    fn default() -> Self {
        Self::new()
    }
}

/// Rewrite `spec`'s coordinates into the canonical form the machine's
/// break stores use, mirroring each `ui_*` install call's own masking:
/// PC breakpoints and memory watches compare through the address-bus
/// mask (watches also word-align), Copper breakpoints mask to an even
/// 24-bit chip address. Beam traps and catches match their inputs
/// exactly.
fn normalize_spec(emu: &Emulator, spec: BreakSpec) -> BreakSpec {
    let addr_mask = emu.machine.ui_addr_mask();
    match spec {
        BreakSpec::Pc { addr, cond, ignore } => BreakSpec::Pc {
            addr: addr & addr_mask,
            cond,
            ignore,
        },
        BreakSpec::Watch { addr, source, pc } => BreakSpec::Watch {
            addr: addr & addr_mask & !1,
            source,
            // Instruction addresses are even; an odd one could never
            // equal a writer PC, so the watch would never fire.
            pc: pc.map(|pc| pc & addr_mask & !1),
        },
        BreakSpec::Copper { addr } => BreakSpec::Copper {
            addr: addr & 0x00FF_FFFE,
        },
        other @ (BreakSpec::RegWatch { .. } | BreakSpec::Beam { .. } | BreakSpec::Catch { .. }) => {
            other
        }
    }
}

/// Whether two specs address the same point in the break store (the
/// coordinates the machine keys on), regardless of condition or ignore
/// count.
fn same_point(a: &BreakSpec, b: &BreakSpec) -> bool {
    match (a, b) {
        (BreakSpec::Pc { addr: x, .. }, BreakSpec::Pc { addr: y, .. }) => x == y,
        (BreakSpec::Watch { addr: x, .. }, BreakSpec::Watch { addr: y, .. }) => x == y,
        (BreakSpec::RegWatch { off: x }, BreakSpec::RegWatch { off: y }) => x == y,
        (BreakSpec::Beam { vpos: xv, hpos: xh }, BreakSpec::Beam { vpos: yv, hpos: yh }) => {
            xv == yv && xh == yh
        }
        (BreakSpec::Copper { addr: x }, BreakSpec::Copper { addr: y }) => x == y,
        (BreakSpec::Catch { vector: x }, BreakSpec::Catch { vector: y }) => x == y,
        _ => false,
    }
}

/// Toggle `spec`'s point in the machine's break store; returns whether
/// it is now set.
fn toggle_spec(emu: &mut Emulator, spec: &BreakSpec) -> bool {
    match spec {
        BreakSpec::Pc { addr, cond, ignore } => {
            emu.machine.ui_set_breakpoint(*addr, *cond, *ignore)
        }
        BreakSpec::Watch { addr, source, pc } => {
            emu.machine.ui_toggle_watch_qualified(*addr, *source, *pc)
        }
        BreakSpec::RegWatch { off } => emu.machine.ui_toggle_reg_watch(*off),
        BreakSpec::Beam { vpos, hpos } => emu.bus_mut().ui_toggle_beam_trap(*vpos, *hpos),
        BreakSpec::Copper { addr } => emu.bus_mut().ui_toggle_copper_break(*addr),
        BreakSpec::Catch { vector } => emu.machine.ui_toggle_catch(*vector),
    }
}

/// Human-readable description of a break spec for error messages and
/// `break.list`.
pub fn describe_spec(spec: &BreakSpec) -> String {
    match spec {
        BreakSpec::Pc { addr, cond, ignore } => {
            let mut s = format!("pc breakpoint at ${addr:06X}");
            if let Some(cond) = cond {
                s.push_str(&format!(" if {}", cond.describe()));
            }
            if *ignore > 0 {
                s.push_str(&format!(" ignore {ignore}"));
            }
            s
        }
        BreakSpec::Watch { addr, source, pc } => format!(
            "memory watch at ${addr:06X}{}{}",
            source
                .map(|f| format!(" ({} accesses)", f.describe()))
                .unwrap_or_default(),
            pc.map(|pc| format!(" from ${pc:06X}")).unwrap_or_default()
        ),
        BreakSpec::RegWatch { off } => format!(
            "register watch {} (${off:03X})",
            crate::debugger::custom_reg_name(*off)
        ),
        BreakSpec::Beam { vpos, hpos } => format!(
            "beam trap v{vpos}{}",
            hpos.map(|h| format!(" h{h}")).unwrap_or_default()
        ),
        BreakSpec::Copper { addr } => format!("copper breakpoint at ${addr:06X}"),
        BreakSpec::Catch { vector } => format!(
            "catch {} (vector {vector})",
            crate::debugger::exception_vector_name(*vector)
        ),
    }
}

/// Apply one input action through the bus primitives, note it for
/// reverse replay, and journal it in the recorder when one is active.
/// Returns the emulated time the action landed at.
pub fn inject_input(
    emu: &mut Emulator,
    recorder: &mut Option<InputRecorder>,
    action: InputAction,
) -> f64 {
    let secs = emu.bus().emulated_seconds();
    match action {
        InputAction::Key { rawkey, pressed } => {
            if pressed {
                emu.bus_mut().enqueue_key(rawkey);
            } else {
                emu.bus_mut().enqueue_key_event(rawkey, false);
            }
            emu.tt_note_input(ReplayAction::Key { rawkey, pressed });
            if let Some(rec) = recorder.as_mut() {
                rec.record_key(rawkey, pressed, secs);
            }
        }
        InputAction::MouseButton {
            port,
            index,
            pressed,
        } => {
            emu.bus_mut()
                .input
                .set_mouse_button(port as usize, index, pressed);
            emu.tt_note_input(ReplayAction::MouseButton {
                port,
                index,
                pressed,
            });
            observe_recorder(emu, recorder, secs);
        }
        InputAction::MouseMove { port, dx, dy } => {
            emu.bus_mut().input.add_mouse_delta(port as usize, dx, dy);
            emu.tt_note_input(ReplayAction::MouseMove { port, dx, dy });
            observe_recorder(emu, recorder, secs);
        }
        InputAction::Joy { port, state: j } => {
            let input = &mut emu.bus_mut().input;
            input.set_joystick(port as usize, j.up, j.down, j.left, j.right, j.red, j.blue);
            input.set_cd32_buttons(port as usize, j.play, j.rwd, j.ffw, j.green, j.yellow);
            emu.tt_note_input(ReplayAction::Joy { port, state: j });
            observe_recorder(emu, recorder, secs);
        }
        InputAction::Pot { port, x, y } => {
            emu.bus_mut().input.set_analogue(port as usize, x, y);
            emu.tt_note_input(ReplayAction::Pot { port, x, y });
            observe_recorder(emu, recorder, secs);
        }
    }
    secs
}

/// The recorder captures mouse/joystick state by diffing `InputState`
/// snapshots (the same once-per-quantum pattern the window uses).
fn observe_recorder(emu: &Emulator, recorder: &mut Option<InputRecorder>, secs: f64) {
    if let Some(rec) = recorder.as_mut() {
        rec.observe(&emu.bus().input, secs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::test_emulator;

    #[test]
    fn scheduled_input_applies_in_time_order_on_the_emulated_clock() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        // Push out of order; the queue must sort by emulated time.
        ctx.schedule(
            0.030,
            InputAction::Key {
                rawkey: 0x22,
                pressed: false,
            },
        );
        ctx.schedule(
            0.001,
            InputAction::Key {
                rawkey: 0x22,
                pressed: true,
            },
        );
        assert!(ctx.scheduled[0].at_seconds < ctx.scheduled[1].at_seconds);

        // Nothing due at power-on.
        ctx.apply_due_scheduled(&mut emu);
        assert_eq!(ctx.scheduled.len(), 2);

        // One frame (~0.02s PAL) passes the press but not the release.
        emu.step_frame().unwrap();
        ctx.apply_due_scheduled(&mut emu);
        assert_eq!(ctx.scheduled.len(), 1);

        emu.step_frame().unwrap();
        ctx.apply_due_scheduled(&mut emu);
        assert!(ctx.scheduled.is_empty());
    }

    #[test]
    fn injected_input_routes_to_the_named_port() {
        use crate::bus::PortDevice;
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();

        // Mouse motion and buttons land on the named port's lines.
        ctx.inject_now(
            &mut emu,
            InputAction::MouseMove {
                port: 1,
                dx: 7,
                dy: -2,
            },
        );
        ctx.inject_now(
            &mut emu,
            InputAction::MouseButton {
                port: 1,
                index: 0,
                pressed: true,
            },
        );
        assert_eq!(emu.bus().input.ports[1].counter_x, 7);
        assert_eq!(emu.bus().input.ports[1].counter_y, 0xFE);
        assert!(emu.bus().input.ports[1].fire);
        assert_eq!(emu.bus().input.ports[0].counter_x, 0);

        // A joystick state on port 1 engages the device there.
        ctx.inject_now(
            &mut emu,
            InputAction::Joy {
                port: 0,
                state: JoyState {
                    up: true,
                    red: true,
                    ..JoyState::default()
                },
            },
        );
        assert_eq!(emu.bus().input.device(0), PortDevice::Joystick);
        assert!(emu.bus().input.ports[0].up);
        assert!(emu.bus().input.ports[0].fire);

        // Analogue positions engage the Analogue device and set the pots.
        ctx.inject_now(
            &mut emu,
            InputAction::Pot {
                port: 1,
                x: 50,
                y: 200,
            },
        );
        assert_eq!(emu.bus().input.device(1), PortDevice::Analogue);
        assert!(emu.bus().input.ports[1].pot_x_ohms.is_some());
    }

    #[test]
    fn injected_input_is_journaled_in_the_recorder() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        ctx.recorder = Some(InputRecorder::new(0.0));
        ctx.inject_now(
            &mut emu,
            InputAction::Key {
                rawkey: 0x45,
                pressed: true,
            },
        );
        ctx.inject_now(
            &mut emu,
            InputAction::Key {
                rawkey: 0x45,
                pressed: false,
            },
        );
        let script = ctx.recorder.take().unwrap().finish();
        assert!(
            script.contains("key-after") && script.contains("0x45"),
            "recording should carry the injected key: {script}"
        );
    }

    #[test]
    fn break_install_remove_and_teardown_leave_the_store_clean() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        let pc = ctx
            .install_break(
                &mut emu,
                BreakSpec::Pc {
                    addr: 0xF80012,
                    cond: None,
                    ignore: 0,
                },
            )
            .unwrap();
        ctx.install_break(
            &mut emu,
            BreakSpec::Beam {
                vpos: 120,
                hpos: Some(60),
            },
        )
        .unwrap();
        ctx.install_break(&mut emu, BreakSpec::Copper { addr: 0x20000 })
            .unwrap();
        assert!(emu.machine.ui_breaks().is_breakpoint(0xF80012));
        assert_eq!(emu.bus().ui_beam_traps().len(), 1);
        assert_eq!(emu.bus().ui_copper_breaks().len(), 1);

        assert!(ctx.remove_break(&mut emu, pc));
        assert!(!emu.machine.ui_breaks().is_breakpoint(0xF80012));

        ctx.remove_all_breaks(&mut emu);
        assert!(emu.bus().ui_beam_traps().is_empty());
        assert!(emu.bus().ui_copper_breaks().is_empty());
    }

    #[test]
    fn specs_are_normalized_like_the_machine_stores_key_them() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        // A 24-bit machine masks PC breakpoint addresses; the session
        // spec must key the same way or break.list/remove lose track.
        let id = ctx
            .install_break(
                &mut emu,
                BreakSpec::Pc {
                    addr: 0xFFF8_001A,
                    cond: None,
                    ignore: 0,
                },
            )
            .unwrap();
        assert!(emu.machine.ui_breaks().is_breakpoint(0xF8_001A));
        assert_eq!(
            ctx.id_for(&BreakSpec::Pc {
                addr: 0xF8_001A,
                cond: None,
                ignore: 0
            }),
            Some(id),
            "the masked machine-store address must map back to the id"
        );

        // Copper breaks mask to an even 24-bit address; an odd raw spec
        // must still toggle back off on removal (not leak).
        let copper = ctx
            .install_break(&mut emu, BreakSpec::Copper { addr: 0x2_0001 })
            .unwrap();
        assert_eq!(emu.bus().ui_copper_breaks(), &[0x2_0000]);
        assert!(ctx.remove_break(&mut emu, copper));
        assert!(emu.bus().ui_copper_breaks().is_empty());

        // Watches word-align; duplicate adds through a different raw
        // form of the same word are refused instead of toggled away.
        ctx.install_break(
            &mut emu,
            BreakSpec::Watch {
                addr: 0x3_0001,
                source: None,
                pc: None,
            },
        )
        .unwrap();
        let dup = ctx.install_break(
            &mut emu,
            BreakSpec::Watch {
                addr: 0x3_0000,
                source: None,
                pc: None,
            },
        );
        assert!(dup.is_err());
        assert_eq!(emu.machine.ui_breaks().watches.len(), 1);
    }

    #[test]
    fn teardown_leaves_gui_owned_points_alone() {
        let mut emu = test_emulator();
        let mut ctx = SessionCtx::new();
        emu.machine.ui_set_breakpoint(0xF80010, None, 0); // "GUI" point
        ctx.install_break(
            &mut emu,
            BreakSpec::Pc {
                addr: 0xF80014,
                cond: None,
                ignore: 0,
            },
        )
        .unwrap();
        ctx.remove_all_breaks(&mut emu);
        assert!(emu.machine.ui_breaks().is_breakpoint(0xF80010));
        assert!(!emu.machine.ui_breaks().is_breakpoint(0xF80014));
    }
}
