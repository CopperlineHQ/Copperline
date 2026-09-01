// SPDX-License-Identifier: GPL-3.0-or-later

//! The windowed control-protocol drain: commands enqueued by the socket
//! threads (`control::windowed`) are executed here, on the winit thread,
//! at the top of `about_to_wait` -- the same deterministic boundary the
//! scheduled-input scripting uses. Split out of `window.rs` for size;
//! this is the same `App`, with full access to its private state, and it
//! routes input/media through the exact helpers live input uses so
//! journaling (input recorder, reverse-replay log) is identical.

use super::app_session::WarpSource;
use super::*;
use crate::control::exec::{
    self, CoreOp, HostOp, Request, ResumeKind, ResumeVerb, RunTarget, StableStep, StableWatch,
    CCK_FINE_WINDOW, RUN_BUDGET,
};
use crate::control::proto::{self, CtlError};
use crate::control::session::{InputAction, MachineInputState, SessionCtx};
use crate::control::windowed::{ControlHandle, CtlMsg};
use crate::debugger::DebugStop;
use serde_json::{json, Value};

/// Per-connection control state owned by the `App`.
pub(super) struct ControlState {
    pub(super) handle: ControlHandle,
    ctx: SessionCtx,
    /// Deferred input follows the machine across connection turnover.
    input: MachineInputState,
    pending: Option<PendingResume>,
    /// A `run_until pc` target breakpoint this session planted (and must
    /// remove on completion); `None` when the target address already had
    /// a user breakpoint.
    temp_pc_break: Option<u32>,
    exit_requested: bool,
    reverse_budget_mb: usize,
    reverse_interval_frames: u64,
}

/// A resume verb whose JSON-RPC response is deferred until the machine
/// stops.
struct PendingResume {
    id: Value,
    collect: Vec<CoreOp>,
    /// Stop-translation targets: a matching stop reports
    /// `reason_on_target` instead of its raw trap reason.
    pc_target: Option<u32>,
    beam_target: Option<u16>,
    frame_target: Option<u64>,
    cck_target: Option<u64>,
    /// A `run_until {stable_frames}` watcher, sampled once per emulated
    /// frame by the burst loop.
    stable: Option<StableWatch>,
    reason_on_target: &'static str,
}

impl PendingResume {
    fn new(id: Value, collect: Vec<CoreOp>) -> Self {
        Self {
            id,
            collect,
            pc_target: None,
            beam_target: None,
            frame_target: None,
            cck_target: None,
            stable: None,
            reason_on_target: "target",
        }
    }
}

impl App {
    /// Adopt a bound control server; called from `main` between
    /// `App::new` and `run()`.
    pub fn attach_control(&mut self, handle: ControlHandle, config: &crate::control::Config) {
        self.control = Some(ControlState {
            handle,
            ctx: SessionCtx::new(),
            input: MachineInputState::default(),
            pending: None,
            temp_pc_break: None,
            exit_requested: false,
            reverse_budget_mb: config.reverse_budget_mb,
            reverse_interval_frames: config.reverse_interval_frames,
        });
    }

    pub(super) fn control_exit_requested(&self) -> bool {
        self.control.as_ref().is_some_and(|c| c.exit_requested)
    }

    /// Drain queued control commands. First statement of
    /// `about_to_wait`, before the machine steps, so commands land at a
    /// frame boundary; also callable directly from tests (no sockets or
    /// event loop required).
    pub(super) fn drain_control(&mut self) {
        if self.control.is_none() {
            return;
        }
        self.control_apply_due_scheduled();
        while let Some(msg) = self.control.as_ref().and_then(|c| c.handle.try_recv()) {
            match msg {
                CtlMsg::Connected => self.control_on_connected(),
                CtlMsg::Disconnected => self.control_on_disconnected(),
                CtlMsg::Shutdown { id } => {
                    self.control_send(proto::ok_line(&id, json!({})));
                    if let Some(ctl) = self.control.as_mut() {
                        ctl.exit_requested = true;
                    }
                }
                CtlMsg::Request { id, req } => self.control_dispatch(id, req),
            }
        }
        self.control_emit_events();
    }

    fn control_send(&self, line: String) {
        if let Some(ctl) = &self.control {
            ctl.handle.send(line);
        }
    }

    /// Sample subscribed event families and enqueue notifications without
    /// ever blocking the winit/emulation thread behind a slow client.
    pub(super) fn control_emit_events(&mut self) {
        let lines = {
            let Some(ctl) = self.control.as_mut() else {
                return;
            };
            ctl.ctx.poll_events(&mut self.emu)
        };
        for line in lines {
            let sent = self
                .control
                .as_ref()
                .is_some_and(|ctl| ctl.handle.try_send_event(line));
            if !sent {
                if let Some(ctl) = self.control.as_mut() {
                    ctl.ctx.note_event_notification_dropped();
                }
            }
        }
    }

    fn control_on_connected(&mut self) {
        let (budget, interval) = {
            let ctl = self
                .control
                .as_mut()
                .expect("connected without control state");
            ctl.ctx = SessionCtx::new();
            ctl.pending = None;
            ctl.temp_pc_break = None;
            (ctl.reverse_budget_mb, ctl.reverse_interval_frames)
        };
        // Arm the reverse-debug ring for this client, like the headless
        // server and the debugger window do.
        self.emu.enable_time_travel(budget, interval);
        if let Err(e) = self.emu.debug_ensure_time_travel_anchor() {
            warn!("control: arming time travel failed: {e:#}");
        }
        self.emu.machine.ui_set_pc_history_enabled(true);
        self.show_osd("Control client attached");
        self.request_redraw();
    }

    fn control_on_disconnected(&mut self) {
        let Some(ctl) = self.control.as_mut() else {
            return;
        };
        // A pending resume has no client left to answer; the machine
        // keeps whatever run state it is in (the window still owns it).
        ctl.pending = None;
        let temp_pc = ctl.temp_pc_break.take();
        ctl.ctx.disable_events(&mut self.emu);
        let mut ctx = std::mem::take(&mut ctl.ctx);
        if let Some(addr) = temp_pc {
            self.emu.machine.ui_set_breakpoint(addr, None, 0);
        }
        self.emu.bus_mut().ui_disarm_beam_trap_once();
        ctx.remove_all_breaks(&mut self.emu);
        // A warp the client engaged has no client left to release it.
        if self.warp_hold == Some(WarpSource::Control) {
            self.set_warp(false, WarpSource::Control);
        }
        self.show_osd("Control client detached");
        self.request_redraw();
    }

    fn control_dispatch(&mut self, id: Value, req: Request) {
        match req {
            Request::Core(op) => {
                let pending = self.control.as_ref().is_some_and(|c| c.pending.is_some());
                if pending && !op.allowed_while_running() {
                    self.control_send(proto::err_line(
                        &id,
                        &CtlError::invalid_state("pause before repositioning the machine"),
                    ));
                    return;
                }
                // Keep the host-state flags `status` reports current.
                let repositions = !op.allowed_while_running();
                let (paused, powered_on, halted) = (self.paused, self.powered_on, self.cpu_halted);
                let result = {
                    let ctl = self
                        .control
                        .as_mut()
                        .expect("dispatch without control state");
                    ctl.ctx.running = powered_on && !halted && !paused;
                    ctl.ctx.pending = pending;
                    ctl.ctx.powered_on = powered_on;
                    exec::exec_core(&mut self.emu, &mut ctl.ctx, &op)
                };
                let line = match result {
                    Ok(value) => {
                        // A memory.heatmap request that took effect makes
                        // the protocol the map's owner, whether it armed,
                        // re-windowed, or disarmed it: the analyzer pane
                        // must no longer release the map when it closes.
                        if matches!(op, CoreOp::HeatMapSet { .. }) {
                            self.heatmap_armed_by_panel = false;
                        }
                        // profile.stop disarms the analyzer it armed; an
                        // open pane still wants its trace, so re-arm for it.
                        if matches!(op, CoreOp::ProfileStop) && self.frame_analyzer_panel.is_some()
                        {
                            self.emu.bus_mut().set_frame_analyzer_enabled(true);
                        }
                        proto::ok_line(&id, value)
                    }
                    Err(err) => proto::err_line(&id, &err),
                };
                self.control_send(line);
                self.control_emit_events();
                if repositions {
                    // Reverse steps / last-writer moved the timeline;
                    // refresh the presentation like the debugger's own
                    // reverse transports do.
                    self.finish_render_for_current_frame();
                    self.request_redraw();
                }
            }
            Request::Host(op) => self.control_dispatch_host(id, op),
        }
    }

    fn control_dispatch_host(&mut self, id: Value, op: HostOp) {
        match op {
            HostOp::Pause => {
                let had_pending = self.control_complete_pending("pause", "paused by client");
                let was_paused = self.paused;
                if !was_paused {
                    self.paused = true;
                    self.sync_live_audio_suspension();
                    self.request_redraw();
                }
                let detail = if had_pending {
                    "paused by client"
                } else if was_paused {
                    "already paused"
                } else {
                    "paused"
                };
                let stop = exec::stop_snapshot(&self.emu, "pause", detail);
                match serde_json::to_value(&stop) {
                    Ok(value) => self.control_send(proto::ok_line(&id, value)),
                    Err(e) => {
                        self.control_send(proto::err_line(&id, &CtlError::internal(e.to_string())))
                    }
                }
            }
            HostOp::Resume(verb) => self.control_start_resume(id, verb),
            HostOp::Input(cmd) => {
                let now = self.emu.bus().emulated_seconds();
                let (immediate, later) = cmd.expand(now);
                for action in immediate {
                    self.control_apply_input(action);
                }
                let scheduled = later.len();
                if let Some(ctl) = self.control.as_mut() {
                    for entry in later {
                        ctl.input.schedule(entry.at_seconds, entry.action);
                    }
                }
                self.control_send(proto::ok_line(
                    &id,
                    json!({"applied_at_seconds": now, "scheduled": scheduled}),
                ));
            }
            HostOp::MouseTo {
                port,
                x,
                y,
                tolerance,
                max_frames,
            } => {
                // Refused mid-resume, as the headless server does and as
                // the protocol documents: the servo advances the machine
                // itself, so it would step frames out from under a
                // pending run_until -- past a pc target, or through
                // frames a stable_frames watcher never got to sample.
                if self.control.as_ref().is_some_and(|c| c.pending.is_some()) {
                    self.control_send(proto::err_line(
                        &id,
                        &CtlError::invalid_state("pause before servoing the pointer"),
                    ));
                    return;
                }
                // The servo runs the machine for up to max_frames frames
                // right here, like the bounded step verbs above: it needs
                // to see each frame it caused before choosing the next
                // delta, which a frame-boundary drain cannot do.
                let line = match exec::mouse_to(
                    &mut self.emu,
                    port,
                    (x, y),
                    tolerance,
                    max_frames,
                    |emu, action| {
                        crate::control::session::inject_input(emu, &mut None, action);
                    },
                ) {
                    Ok(value) => proto::ok_line(&id, value),
                    Err(e) => proto::err_line(&id, &e),
                };
                self.control_send(line);
                self.control_emit_events();
                self.finish_render_for_current_frame();
                self.request_redraw();
            }
            HostOp::FloppyInsert {
                drive,
                path,
                write_protected,
            } => {
                let line = if self.insert_disk_image(drive, path, write_protected) {
                    let name = self.emu.bus().floppy.inserted_disk_name(drive);
                    proto::ok_line(&id, json!({"drive": drive, "name": name}))
                } else {
                    proto::err_line(&id, &CtlError::io("floppy insert failed (see log)"))
                };
                self.control_send(line);
            }
            HostOp::FloppyEject { drive } => {
                self.eject_drive_disk(drive);
                self.control_send(proto::ok_line(&id, json!({})));
            }
            HostOp::CdInsert { path } => {
                let line = if !self.emu.bus().cd_drive_present() {
                    proto::err_line(&id, &CtlError::unsupported("no CD drive on this machine"))
                } else {
                    self.insert_cd_image_from_path(&path);
                    if self.emu.bus().cd_disc_inserted() {
                        proto::ok_line(&id, json!({}))
                    } else {
                        proto::err_line(&id, &CtlError::io("CD image load failed (see log)"))
                    }
                };
                self.control_send(line);
            }
            HostOp::CdEject => {
                let line = if !self.emu.bus().cd_drive_present() {
                    proto::err_line(&id, &CtlError::unsupported("no CD drive on this machine"))
                } else {
                    self.eject_cd();
                    proto::ok_line(&id, json!({}))
                };
                self.control_send(line);
            }
            HostOp::SetPortDevice { port, device } => {
                self.hot_plug_port_device(port as usize, device);
                self.show_osd(format!("Port {}: {}", port + 1, device.label()));
                self.control_send(proto::ok_line(
                    &id,
                    json!({"port": port + 1, "device": device.label()}),
                ));
            }
            HostOp::StateLoad { path } => {
                if self.control.as_ref().is_some_and(|c| c.pending.is_some()) {
                    self.control_send(proto::err_line(
                        &id,
                        &CtlError::invalid_state("pause before loading a state"),
                    ));
                    return;
                }
                let line = if self.load_state_from_path(&path) {
                    if let Some(ctl) = self.control.as_mut() {
                        ctl.input.clear_scheduled();
                    }
                    // The snapshot ring's positions belong to the old
                    // timeline; re-arm on the loaded one.
                    let (budget, interval) = self
                        .control
                        .as_ref()
                        .map(|c| (c.reverse_budget_mb, c.reverse_interval_frames))
                        .unwrap_or((
                            crate::debugger::RR_DEFAULT_BUDGET_MB,
                            crate::debugger::RR_DEFAULT_INTERVAL_FRAMES,
                        ));
                    self.emu.enable_time_travel(budget, interval);
                    if let Err(e) = self.emu.debug_ensure_time_travel_anchor() {
                        warn!("control: re-arming time travel failed: {e:#}");
                    }
                    proto::ok_line(&id, json!({"seconds": self.emu.bus().emulated_seconds()}))
                } else {
                    proto::err_line(&id, &CtlError::io("state load failed (see log)"))
                };
                self.control_send(line);
            }
            HostOp::WarpGet => {
                let value = self.warp_report_value(None);
                self.control_send(proto::ok_line(&id, value));
            }
            HostOp::WarpSet { on } => {
                // No pending-resume refusal: like pause and input, this does
                // not reposition the machine, and flipping warp mid-continue
                // is the point.
                let outcome = self.set_warp(on, WarpSource::Control);
                let value = self.warp_report_value(outcome.note);
                self.control_send(proto::ok_line(&id, value));
            }
            HostOp::Reset { warm } => {
                if warm {
                    self.reset_emulator(true);
                } else {
                    // Cold reset = power cycle: power_off parks a cold-boot
                    // state, then power back on to run from the reset vector.
                    self.power_off();
                    self.powered_on = true;
                    self.sync_live_audio_suspension();
                    self.request_redraw();
                }
                if let Some(ctl) = self.control.as_mut() {
                    ctl.input.clear_scheduled();
                }
                self.control_send(proto::ok_line(&id, json!({})));
            }
        }
    }

    fn control_start_resume(&mut self, id: Value, verb: ResumeVerb) {
        if self.control.as_ref().is_some_and(|c| c.pending.is_some()) {
            self.control_send(proto::err_line(
                &id,
                &CtlError::new(proto::RESUME_PENDING, "a resume is already pending"),
            ));
            return;
        }
        if !self.powered_on {
            self.control_send(proto::err_line(
                &id,
                &CtlError::invalid_state("machine is powered off"),
            ));
            return;
        }
        if self.emu.machine.cpu_double_faulted() {
            self.control_send(proto::err_line(
                &id,
                &CtlError::invalid_state("CPU is double-faulted; reset the machine"),
            ));
            return;
        }
        match verb.kind {
            // Bounded step verbs run synchronously at this boundary,
            // like the debugger window's own step buttons.
            ResumeKind::Step { .. }
            | ResumeKind::StepOver
            | ResumeKind::StepOut
            | ResumeKind::StepCopper => {
                if !self.paused {
                    self.control_send(proto::err_line(
                        &id,
                        &CtlError::invalid_state("machine is running; pause first"),
                    ));
                    return;
                }
                self.control_sync_step(id, verb);
            }
            ResumeKind::StepFrame { n } => {
                let mut pending = PendingResume::new(id, verb.collect);
                pending.frame_target = Some(self.emu.bus().emulated_frames() + u64::from(n.max(1)));
                pending.reason_on_target = "step";
                self.control_arm_pending(pending);
            }
            ResumeKind::Continue => {
                self.control_arm_pending(PendingResume::new(id, verb.collect));
            }
            ResumeKind::RunUntil(target) => {
                let mut pending = PendingResume::new(id, verb.collect);
                match target {
                    RunTarget::Pc(pc) => {
                        pending.pc_target = Some(pc);
                        if !self.emu.machine.ui_breaks().is_breakpoint(pc) {
                            self.emu.machine.ui_set_breakpoint(pc, None, 0);
                            if let Some(ctl) = self.control.as_mut() {
                                ctl.temp_pc_break = Some(pc);
                            }
                        }
                    }
                    RunTarget::Beam { vpos, hpos } => {
                        pending.beam_target = Some(vpos);
                        self.emu.bus_mut().ui_arm_beam_trap_once(vpos, hpos);
                    }
                    RunTarget::Frame(frame) => pending.frame_target = Some(frame),
                    RunTarget::Cck(cck) => pending.cck_target = Some(cck),
                    RunTarget::Stable(spec) => pending.stable = Some(StableWatch::new(spec)),
                    RunTarget::Seconds(secs) => {
                        pending.cck_target = Some(
                            (secs * f64::from(crate::chipset::paula::PAULA_CLOCK_HZ)).ceil() as u64,
                        );
                    }
                }
                self.control_arm_pending(pending);
            }
        }
    }

    fn control_arm_pending(&mut self, pending: PendingResume) {
        if let Some(ctl) = self.control.as_mut() {
            ctl.pending = Some(pending);
        }
        if self.paused {
            self.paused = false;
            self.sync_live_audio_suspension();
        }
        self.request_redraw();
    }

    /// Run a bounded step verb synchronously and reply immediately.
    fn control_sync_step(&mut self, id: Value, verb: ResumeVerb) {
        let mut label = "stepped";
        let result = (|| -> anyhow::Result<()> {
            match verb.kind {
                ResumeKind::Step { n } => {
                    label = "instruction step";
                    for _ in 0..n {
                        self.emu.debug_step_realtime()?;
                        if self.emu.machine.ui_debug_stop_pending() {
                            break;
                        }
                    }
                }
                ResumeKind::StepOver => {
                    label = "stepped over";
                    self.emu.debug_step_over(RUN_BUDGET)?;
                }
                ResumeKind::StepOut => {
                    label = "stepped out";
                    self.emu.debug_step_out(RUN_BUDGET)?;
                }
                ResumeKind::StepCopper => {
                    label = "copper instruction retired";
                    self.emu.debug_step_copper(RUN_BUDGET)?;
                }
                _ => unreachable!("sync step called with an unbounded verb"),
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.cpu_halted = true;
            self.sync_live_audio_suspension();
            self.control_send(proto::err_line(&id, &CtlError::internal(format!("{e:#}"))));
            return;
        }
        let (reason, detail) = if self.emu.machine.cpu_double_faulted() {
            ("double_fault", "CPU double fault (halted)".to_string())
        } else if let Some(stop) = self.emu.machine.take_ui_debug_stop() {
            let (reason, detail) = exec::stop_reason_of(&stop);
            (reason, detail)
        } else {
            ("step", label.to_string())
        };
        self.control_reply_stop(&id, verb.collect, reason, &detail);
        self.control_emit_events();
        self.last_debug_stop = Some(detail);
        self.finish_render_for_current_frame();
        self.request_redraw();
    }

    /// Build a stop event (with collect) and send it as the response to
    /// `id`.
    fn control_reply_stop(&mut self, id: &Value, collect: Vec<CoreOp>, reason: &str, detail: &str) {
        let mut stop = exec::stop_snapshot(&self.emu, reason, detail);
        if !collect.is_empty() {
            let collected = {
                let ctl = self.control.as_mut().expect("reply without control state");
                exec::eval_collect(&mut self.emu, &mut ctl.ctx, &collect)
            };
            stop.collect = Some(collected);
        }
        match serde_json::to_value(&stop) {
            Ok(value) => self.control_send(proto::ok_line(id, value)),
            Err(e) => self.control_send(proto::err_line(id, &CtlError::internal(e.to_string()))),
        }
    }

    /// Complete the pending resume, if any, with the given stop reason.
    /// Returns whether one was completed.
    pub(super) fn control_complete_pending(&mut self, reason: &str, detail: &str) -> bool {
        let Some((pending, temp_pc)) = self
            .control
            .as_mut()
            .and_then(|ctl| ctl.pending.take().map(|p| (p, ctl.temp_pc_break.take())))
        else {
            return false;
        };
        if let Some(addr) = temp_pc {
            // Toggle our temporary run-to breakpoint back off.
            self.emu.machine.ui_set_breakpoint(addr, None, 0);
        }
        self.emu.bus_mut().ui_disarm_beam_trap_once();
        self.control_reply_stop(&pending.id.clone(), pending.collect, reason, detail);
        true
    }

    /// Hook for `surface_debug_stop`: when a remote resume is pending,
    /// the stop answers the client instead of commandeering the local
    /// debugger window. Returns whether the stop was consumed remotely.
    pub(super) fn control_completes_stop(&mut self, stop: &DebugStop) -> bool {
        let Some(ctl) = self.control.as_ref() else {
            return false;
        };
        let Some(pending) = ctl.pending.as_ref() else {
            self.control_notify_stopped_of(stop);
            return false;
        };
        let (mut reason, detail) = exec::stop_reason_of(stop);
        match stop {
            DebugStop::Breakpoint { pc }
                if pending.pc_target.is_some_and(|t| {
                    t & self.emu.machine.ui_addr_mask() == *pc & self.emu.machine.ui_addr_mask()
                }) =>
            {
                reason = pending.reason_on_target;
            }
            DebugStop::Beam { vpos, .. } if pending.beam_target == Some(*vpos) => {
                reason = pending.reason_on_target;
            }
            _ => {}
        }
        self.control_complete_pending(reason, &detail);
        true
    }

    /// Notify an attached client of a stop it did not request (a GUI
    /// breakpoint, a user pause) as an `event.stopped` notification.
    fn control_notify_stopped_of(&self, stop: &DebugStop) {
        let (reason, detail) = exec::stop_reason_of(stop);
        self.control_notify_stopped(reason, &detail);
    }

    pub(super) fn control_notify_stopped(&self, reason: &str, detail: &str) {
        let Some(ctl) = &self.control else {
            return;
        };
        if ctl.pending.is_some() || !ctl.handle.connected() {
            return;
        }
        let stop = exec::stop_snapshot(&self.emu, reason, detail);
        if let Ok(value) = serde_json::to_value(&stop) {
            ctl.handle.send(proto::event_line("event.stopped", value));
        }
    }

    /// The `warp.get` / `warp.set` reply: the pacing state and who holds
    /// warp (a programmatic hold, an engaged boot gate, or the manual
    /// toggle), plus a note when a request could not be honoured.
    fn warp_report_value(&self, note: Option<&str>) -> Value {
        let paced = self.emu.paced();
        let source = if paced {
            "none"
        } else if self.headless_capture_active() {
            // --control-gui combined with --screenshot-after/--dump-frames:
            // unpaced end to end with no hold or gate, not a manual toggle.
            "capture"
        } else if let Some(hold) = self.warp_hold {
            hold.label()
        } else if self.warp_launch.as_ref().is_some_and(|l| l.engaged) {
            "launch"
        } else if self.warp_boot.as_ref().is_some_and(|g| g.engaged) {
            "boot"
        } else {
            "manual"
        };
        let mut value = json!({
            "on": !paced,
            "paced": paced,
            "source": source,
            "headless": false,
        });
        if let Some(note) = note {
            value["note"] = json!(note);
        }
        value
    }

    /// Tell an attached client that warp changed for a reason other than
    /// its own `warp.set` (the hotkey, the guest, a boot gate, power off)
    /// as an `event.warp` notification. Not gated on a pending resume: a
    /// change during a `continue` is exactly what a client wants to see.
    /// Delivery is best effort so a full outbound queue never disconnects
    /// the client over an informational event.
    pub(super) fn control_notify_warp(&mut self, source: &'static str) {
        let paced = self.emu.paced();
        let position = crate::control::observe::position(&self.emu);
        let Some(ctl) = self.control.as_mut() else {
            return;
        };
        if !ctl.handle.connected() {
            return;
        }
        let line = proto::event_line(
            "event.warp",
            json!({
                "on": !paced,
                "paced": paced,
                "source": source,
                "position": position,
            }),
        );
        if !ctl.handle.try_send_event(line) {
            ctl.ctx.note_event_notification_dropped();
        }
    }

    /// Burst-loop check: complete a pending frame/cck target when the
    /// machine has reached it. Returns true when the run finished (the
    /// burst should end).
    pub(super) fn control_run_target_reached(&mut self) -> bool {
        let Some(pending) = self.control.as_ref().and_then(|c| c.pending.as_ref()) else {
            return false;
        };
        let (frame_target, cck_target, reason) = (
            pending.frame_target,
            pending.cck_target,
            pending.reason_on_target,
        );
        // The burst loop calls this once per emulated frame, which is the
        // sampling cadence the stable-frame watcher is defined on. The
        // watcher is lifted out for the sample because sampling renders
        // the frame, which needs the emulator the pending borrow holds.
        if let Some(mut watch) = self
            .control
            .as_mut()
            .and_then(|c| c.pending.as_mut())
            .and_then(|p| p.stable.take())
        {
            let (reason, detail) = match watch.sample(&self.emu) {
                StableStep::Running => {
                    if let Some(p) = self.control.as_mut().and_then(|c| c.pending.as_mut()) {
                        p.stable = Some(watch);
                    }
                    // A stable target is exclusive, so there is no
                    // frame/cck target left to check below.
                    return false;
                }
                StableStep::Settled(detail) => (reason, detail),
                StableStep::GaveUp(detail) => ("budget", detail),
            };
            self.paused = true;
            self.sync_live_audio_suspension();
            self.control_complete_pending(reason, &detail);
            self.request_redraw();
            return true;
        }
        if let Some(frame) = frame_target {
            if self.emu.bus().emulated_frames() >= frame {
                self.paused = true;
                self.sync_live_audio_suspension();
                self.control_complete_pending(reason, &format!("frame {frame}"));
                self.request_redraw();
                return true;
            }
        }
        if let Some(cck) = cck_target {
            let current = self.emu.bus().emulated_cck();
            if current >= cck || cck - current < CCK_FINE_WINDOW {
                // Land on the first instruction boundary at or past the
                // target (bounded: less than a frame away).
                while self.emu.bus().emulated_cck() < cck {
                    if let Err(e) = self.emu.debug_step_realtime() {
                        error!("emulator step halted: {e:?}");
                        self.cpu_halted = true;
                        break;
                    }
                    if self.emu.machine.ui_debug_stop_pending() {
                        // A trap fired first; let the normal stop path
                        // complete the pending with its reason.
                        return self.surface_debug_stop();
                    }
                }
                self.paused = true;
                self.sync_live_audio_suspension();
                self.control_complete_pending(reason, &format!("cck {cck}"));
                self.request_redraw();
                return true;
            }
        }
        false
    }

    /// Apply control-scheduled input whose emulated time has arrived,
    /// through the same App helpers live input uses.
    fn control_apply_due_scheduled(&mut self) {
        let now = self.emu.bus().emulated_seconds();
        let due: Vec<InputAction> = {
            let Some(ctl) = self.control.as_mut() else {
                return;
            };
            ctl.input.take_due(now)
        };
        for action in due {
            self.control_apply_input(action);
        }
    }

    fn control_apply_input(&mut self, action: InputAction) {
        match action {
            InputAction::Key { rawkey, pressed } => self.handle_amiga_key_event(rawkey, pressed),
            InputAction::MouseButton {
                port,
                index,
                pressed,
            } => {
                let kind = match index {
                    0 => MouseButtonKind::Left,
                    1 => MouseButtonKind::Right,
                    _ => MouseButtonKind::Middle,
                };
                set_mouse_button(&mut self.emu, port, kind, pressed);
            }
            InputAction::MouseMove { port, dx, dy } => {
                self.emu
                    .bus_mut()
                    .input
                    .add_mouse_delta(port as usize, dx, dy);
                self.emu
                    .tt_note_input(crate::inputsched::ReplayAction::MouseMove { port, dx, dy });
            }
            InputAction::Joy { port, state: j } => {
                self.auto_joy_held[port as usize] = AutoJoyHeld {
                    up: j.up,
                    down: j.down,
                    left: j.left,
                    right: j.right,
                    red: j.red,
                    blue: j.blue,
                    green: j.green,
                    yellow: j.yellow,
                    play: j.play,
                    rwd: j.rwd,
                    ffw: j.ffw,
                };
                self.apply_auto_joy_state(port as usize);
            }
            InputAction::Pot { port, x, y } => {
                self.emu.bus_mut().input.set_analogue(port as usize, x, y);
                self.emu
                    .tt_note_input(crate::inputsched::ReplayAction::Pot { port, x, y });
            }
        }
    }
}
