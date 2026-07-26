// SPDX-License-Identifier: GPL-3.0-or-later

//! The headless control server (`--control ADDR`): owns the [`Emulator`]
//! and drives it directly from the connection, exactly the GDB stub's
//! ownership shape -- one client at a time, the machine paused between
//! sessions, the Emulator moved back out of a finished session for the
//! next one.
//!
//! Determinism: the machine starts paused at power-on and advances only
//! on resume verbs. While running, the socket is polled once per
//! quantum (a frame, or an instruction chunk for `run_until pc`); every
//! deterministic stop condition is still detected per instruction by
//! the core, so poll cadence only affects where a host-timed `pause`
//! lands -- which is inherently wall-clock, like a GDB Ctrl-C.

use super::exec::{
    self, HostOp, Request, ResumeKind, ResumeVerb, RunTarget, StableStep, StableWatch,
    CCK_FINE_WINDOW, RUN_BUDGET,
};
use super::proto::{self, AuthGate, CtlError, Gate, MAX_LINE_BYTES};
use super::session::SessionCtx;
use super::Config;
use crate::debugger::DebugStop;
use crate::emulator::Emulator;
use crate::inputrec::InputRecorder;
use crate::inputsched::ReplayAction;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{self, Read};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// Instructions executed between socket polls when running to a PC
/// target instruction-by-instruction. At emulated 68000 speeds this is
/// well under a frame of host time.
const PC_POLL_CHUNK: usize = 4096;

pub fn run(mut emu: Emulator, config: Config) -> Result<()> {
    let bind = crate::gdbstub::normalize_listen_addr(&config.listen)?;
    let listener =
        TcpListener::bind(&bind).with_context(|| format!("binding control server {bind}"))?;
    let local = listener.local_addr().context("resolving control address")?;
    let token = config.resolve_token();
    super::announce(&local, &token, config.info_file.as_ref())?;
    log::info!("control: listening on {local}");

    emu.set_paced(false);
    emu.enable_time_travel(config.reverse_budget_mb, config.reverse_interval_frames);
    emu.debug_ensure_time_travel_anchor()?;
    emu.machine.ui_set_pc_history_enabled(true);

    let mut recorder = config
        .record_input
        .as_ref()
        .map(|_| InputRecorder::new(emu.bus().emulated_seconds()));

    loop {
        let (stream, peer) = listener.accept().context("accepting control connection")?;
        log::info!("control: connection from {peer}");
        stream.set_nodelay(true).ok();
        let mut session = Session::new(emu, stream, &token, &config, recorder.take());
        let end = match session.serve() {
            Ok(end) => end,
            Err(e) => {
                log::warn!("control: session ended with error: {e:#}");
                SessionEnd::Detached
            }
        };
        session.teardown();
        recorder = session.ctx.recorder.take();
        emu = session.emu;
        match end {
            SessionEnd::Detached => {
                log::info!("control: client detached; machine paused, listening again");
            }
            SessionEnd::Killed => break,
        }
    }
    if let (Some(rec), Some(path)) = (recorder, config.record_input.as_ref()) {
        let events = rec.events_recorded();
        std::fs::write(path, rec.finish())
            .with_context(|| format!("writing input recording {}", path.display()))?;
        log::info!(
            "control: input recording saved: {} ({events} events)",
            path.display()
        );
    }
    Ok(())
}

/// How a session ended: detach/EOF keeps serving, `shutdown` ends the
/// server.
enum SessionEnd {
    Detached,
    Killed,
}

/// A newline-delimited reader whose partial-line buffer survives across
/// poll attempts, so a command split over TCP segments is never lost to
/// a read timeout mid-line.
struct LineReader {
    stream: TcpStream,
    buf: Vec<u8>,
}

enum Polled {
    Line(String),
    Empty,
    Eof,
}

impl LineReader {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
        }
    }

    /// Extract the next complete, non-blank line from the buffer.
    fn take_buffered_line(&mut self) -> io::Result<Option<String>> {
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).take(pos).collect();
            if line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            let line = String::from_utf8(line).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "control message is not UTF-8")
            })?;
            return Ok(Some(line));
        }
        if self.buf.len() > MAX_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "control message exceeds the line limit",
            ));
        }
        Ok(None)
    }

    /// Block until a full line, EOF, or an error.
    fn read_blocking(&mut self) -> io::Result<Polled> {
        self.stream.set_read_timeout(None)?;
        loop {
            if let Some(line) = self.take_buffered_line()? {
                return Ok(Polled::Line(line));
            }
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(Polled::Eof),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Non-blocking-ish poll used while the machine is running: returns
    /// `Empty` when no complete line has arrived yet.
    fn poll(&mut self) -> io::Result<Polled> {
        if let Some(line) = self.take_buffered_line()? {
            return Ok(Polled::Line(line));
        }
        self.stream
            .set_read_timeout(Some(Duration::from_millis(1)))?;
        let mut chunk = [0u8; 4096];
        match self.stream.read(&mut chunk) {
            Ok(0) => Ok(Polled::Eof),
            Ok(n) => {
                self.buf.extend_from_slice(&chunk[..n]);
                Ok(match self.take_buffered_line()? {
                    Some(line) => Polled::Line(line),
                    None => Polled::Empty,
                })
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::TimedOut
                    || e.kind() == io::ErrorKind::Interrupted =>
            {
                Ok(Polled::Empty)
            }
            Err(e) => Err(e),
        }
    }
}

/// What the mid-run socket poll decided.
enum MidRun {
    /// Nothing that ends the run; keep going.
    Kept,
    /// `pause` arrived: end the run, reply to these extra request ids
    /// with the stop position too.
    Pause,
    /// `shutdown` arrived: end the run, then end the server.
    Kill,
    /// The client vanished.
    Lost(SessionEnd),
}

/// The outcome of driving a resume verb.
enum RunOutcome {
    Stop {
        reason: String,
        detail: String,
        /// `pause`/`shutdown` requests that arrived mid-run and also
        /// get the stop position as their response.
        extra_ids: Vec<Value>,
        /// End the server after replying (a mid-run `shutdown`).
        kill_after: bool,
    },
    ClientLost(SessionEnd),
}

struct Session {
    emu: Emulator,
    reader: LineReader,
    writer: TcpStream,
    gate: AuthGate,
    ctx: SessionCtx,
    reverse_budget_mb: usize,
    reverse_interval_frames: u64,
}

impl Session {
    fn new(
        emu: Emulator,
        stream: TcpStream,
        token: &str,
        config: &Config,
        recorder: Option<InputRecorder>,
    ) -> Self {
        let reader = LineReader::new(stream.try_clone().expect("cloning control stream"));
        stream
            .set_write_timeout(Some(Duration::from_millis(250)))
            .ok();
        let mut ctx = SessionCtx::new();
        ctx.recorder = recorder;
        Self {
            emu,
            reader,
            writer: stream,
            gate: AuthGate::new(token.to_string()),
            ctx,
            reverse_budget_mb: config.reverse_budget_mb,
            reverse_interval_frames: config.reverse_interval_frames,
        }
    }

    fn serve(&mut self) -> Result<SessionEnd> {
        loop {
            let line = match self.reader.read_blocking()? {
                Polled::Eof => return Ok(SessionEnd::Detached),
                Polled::Line(line) => line,
                Polled::Empty => continue,
            };
            if let Some(end) = self.handle_line(&line)? {
                return Ok(end);
            }
        }
    }

    /// Remove everything this session installed and drain any pending
    /// stop, so a stale hit cannot ambush the next client.
    fn teardown(&mut self) {
        self.ctx.disable_events(&mut self.emu);
        self.ctx.remove_all_breaks(&mut self.emu);
        self.emu.bus_mut().ui_disarm_beam_trap_once();
        while self.emu.machine.take_ui_debug_stop().is_some() {}
    }

    fn write(&mut self, line: &str) -> io::Result<()> {
        proto::write_line(&mut self.writer, line)
    }

    fn emit_events(&mut self) -> Result<()> {
        let lines = self.ctx.poll_events(&mut self.emu);
        for line in lines {
            self.write(&line)?;
        }
        Ok(())
    }

    fn handle_line(&mut self, line: &str) -> Result<Option<SessionEnd>> {
        let req = match proto::parse_request(line) {
            Ok(req) => req,
            Err(reply) => {
                self.write(&reply)?;
                return Ok(None);
            }
        };
        match self.gate.handle(&req) {
            Gate::Reply(reply) => {
                self.write(&reply)?;
                Ok(None)
            }
            Gate::ReplyAndClose(reply) => {
                self.write(&reply)?;
                Ok(Some(SessionEnd::Detached))
            }
            Gate::Pass => self.dispatch(req),
        }
    }

    fn dispatch(&mut self, req: proto::RpcRequest) -> Result<Option<SessionEnd>> {
        if req.method == "shutdown" {
            self.write(&proto::ok_line(&req.id, json!({})))?;
            return Ok(Some(SessionEnd::Killed));
        }
        let parsed = match exec::parse_method(&req.method, &req.params) {
            Ok(parsed) => parsed,
            Err(err) => {
                self.write(&proto::err_line(&req.id, &err))?;
                return Ok(None);
            }
        };
        match parsed {
            Request::Core(op) => {
                let reply = match exec::exec_core(&mut self.emu, &mut self.ctx, &op) {
                    Ok(result) => proto::ok_line(&req.id, result),
                    Err(err) => proto::err_line(&req.id, &err),
                };
                self.write(&reply)?;
                self.emit_events()?;
                Ok(None)
            }
            Request::Host(op) => {
                let end = self.dispatch_host(req.id, op)?;
                if end.is_none() {
                    self.emit_events()?;
                }
                Ok(end)
            }
        }
    }

    fn dispatch_host(&mut self, id: Value, op: HostOp) -> Result<Option<SessionEnd>> {
        match op {
            HostOp::Pause => {
                // Already paused: reply with the current position.
                let stop = exec::stop_snapshot(&self.emu, "pause", "already paused");
                let value = serde_json::to_value(&stop)?;
                self.write(&proto::ok_line(&id, value))?;
                Ok(None)
            }
            HostOp::Resume(verb) => self.run_resume(id, verb),
            HostOp::Input(cmd) => {
                let now = self.emu.bus().emulated_seconds();
                let (immediate, later) = cmd.expand(now);
                for action in immediate {
                    self.ctx.inject_now(&mut self.emu, action);
                }
                let scheduled = later.len();
                for entry in later {
                    self.ctx.schedule(entry.at_seconds, entry.action);
                }
                self.write(&proto::ok_line(
                    &id,
                    json!({"applied_at_seconds": now, "scheduled": scheduled}),
                ))?;
                Ok(None)
            }
            HostOp::MouseTo {
                port,
                x,
                y,
                tolerance,
                max_frames,
            } => {
                let ctx = &mut self.ctx;
                let reply = match exec::mouse_to(
                    &mut self.emu,
                    port,
                    (x, y),
                    tolerance,
                    max_frames,
                    |emu, action| {
                        ctx.inject_now(emu, action);
                    },
                ) {
                    Ok(value) => proto::ok_line(&id, value),
                    Err(e) => proto::err_line(&id, &e),
                };
                self.write(&reply)?;
                self.emit_events()?;
                Ok(None)
            }
            HostOp::FloppyInsert {
                drive,
                path,
                write_protected,
            } => {
                let result = self.emu.bus_mut().floppy.insert_disk_image(
                    drive,
                    path.clone(),
                    write_protected,
                );
                let reply = match result {
                    Ok(()) => {
                        self.note_media_change(drive, Some(&path));
                        let name = self.emu.bus().floppy.inserted_disk_name(drive);
                        proto::ok_line(&id, json!({"drive": drive, "name": name}))
                    }
                    Err(e) => proto::err_line(&id, &CtlError::io(format!("{e:#}"))),
                };
                self.write(&reply)?;
                Ok(None)
            }
            HostOp::FloppyEject { drive } => {
                let reply = match self.emu.bus_mut().floppy.eject_disk_image(drive) {
                    Ok(()) => {
                        self.note_media_change(drive, None);
                        proto::ok_line(&id, json!({}))
                    }
                    Err(e) => proto::err_line(&id, &CtlError::io(format!("{e:#}"))),
                };
                self.write(&reply)?;
                Ok(None)
            }
            HostOp::CdInsert { path } => {
                let reply = if !self.emu.bus().cd_drive_present() {
                    proto::err_line(&id, &CtlError::unsupported("no CD drive on this machine"))
                } else {
                    match crate::cdrom::CdImage::load(&path) {
                        Ok(image) => {
                            let describe = image.describe();
                            self.emu.bus_mut().cd_insert_disc(image, &path);
                            proto::ok_line(&id, json!({"disc": describe}))
                        }
                        Err(e) => proto::err_line(&id, &CtlError::io(format!("{e:#}"))),
                    }
                };
                self.write(&reply)?;
                Ok(None)
            }
            HostOp::CdEject => {
                let reply = if !self.emu.bus().cd_drive_present() {
                    proto::err_line(&id, &CtlError::unsupported("no CD drive on this machine"))
                } else {
                    self.emu.bus_mut().cd_eject_disc();
                    proto::ok_line(&id, json!({}))
                };
                self.write(&reply)?;
                Ok(None)
            }
            HostOp::SetPortDevice { port, device } => {
                self.emu
                    .bus_mut()
                    .input
                    .set_port_device(port as usize, device);
                self.write(&proto::ok_line(
                    &id,
                    json!({"port": port + 1, "device": device.label()}),
                ))?;
                Ok(None)
            }
            HostOp::StateLoad { path } => {
                let reply = match self.emu.load_state(&path) {
                    Ok(outcome) => {
                        // The snapshot ring's positions belong to the old
                        // timeline; re-arm on the loaded one.
                        self.emu.enable_time_travel(
                            self.reverse_budget_mb,
                            self.reverse_interval_frames,
                        );
                        self.emu.debug_ensure_time_travel_anchor()?;
                        proto::ok_line(
                            &id,
                            json!({
                                "summary": outcome.summary,
                                "reconfigured": outcome.reconfigured,
                                "seconds": self.emu.bus().emulated_seconds(),
                            }),
                        )
                    }
                    Err(e) => proto::err_line(&id, &CtlError::io(format!("{e:#}"))),
                };
                self.write(&reply)?;
                Ok(None)
            }
            HostOp::Reset { warm } => {
                let result = if warm {
                    self.emu.keyboard_reset()
                } else {
                    self.emu.power_on_reset()
                };
                let reply = match result {
                    Ok(()) => proto::ok_line(&id, json!({})),
                    Err(e) => proto::err_line(&id, &CtlError::internal(format!("{e:#}"))),
                };
                self.write(&reply)?;
                Ok(None)
            }
        }
    }

    /// Journal a floppy media change like the window does: note it for
    /// reverse replay and record it in the input recording.
    fn note_media_change(&mut self, drive: usize, inserted: Option<&std::path::Path>) {
        self.emu.tt_note_input(ReplayAction::DiskChange);
        if let (Some(rec), Some(path)) = (self.ctx.recorder.as_mut(), inserted) {
            let secs = self.emu.bus().emulated_seconds();
            rec.record_disk_insert(drive, path, secs);
        }
    }

    fn run_resume(&mut self, id: Value, verb: ResumeVerb) -> Result<Option<SessionEnd>> {
        if self.emu.machine.cpu_double_faulted() {
            self.write(&proto::err_line(
                &id,
                &CtlError::invalid_state("CPU is double-faulted; reset the machine"),
            ))?;
            return Ok(None);
        }
        self.ctx.running = true;
        self.ctx.pending = true;
        let outcome = self.drive(&verb.kind);
        self.ctx.running = false;
        self.ctx.pending = false;
        match outcome? {
            RunOutcome::Stop {
                reason,
                detail,
                extra_ids,
                kill_after,
            } => {
                let mut stop = exec::stop_snapshot(&self.emu, &reason, &detail);
                if !verb.collect.is_empty() {
                    stop.collect = Some(exec::eval_collect(
                        &mut self.emu,
                        &mut self.ctx,
                        &verb.collect,
                    ));
                }
                self.write(&proto::ok_line(&id, serde_json::to_value(&stop)?))?;
                // pause/shutdown requests that ended the run get the
                // same position (without the collect payload).
                let plain = exec::stop_snapshot(&self.emu, &reason, &detail);
                let plain = serde_json::to_value(&plain)?;
                for extra in extra_ids {
                    self.write(&proto::ok_line(&extra, plain.clone()))?;
                }
                Ok(kill_after.then_some(SessionEnd::Killed))
            }
            RunOutcome::ClientLost(end) => Ok(Some(end)),
        }
    }

    /// Drive the machine for one resume verb until a stop condition,
    /// polling the socket at quantum boundaries.
    fn drive(&mut self, kind: &ResumeKind) -> Result<RunOutcome> {
        let stop = |reason: &str, detail: String| {
            Ok(RunOutcome::Stop {
                reason: reason.to_string(),
                detail,
                extra_ids: Vec::new(),
                kill_after: false,
            })
        };
        let mut cpu_idle = false;

        // Bounded verbs run to completion without polling; they are
        // over in at most RUN_BUDGET instructions.
        match kind {
            ResumeKind::Step { n } => {
                for _ in 0..*n {
                    self.emu.debug_step_for_gdb(&mut cpu_idle)?;
                    self.ctx.apply_due_scheduled(&mut self.emu);
                    self.emit_events()?;
                    if let Some((reason, detail)) = self.take_stop() {
                        return stop(reason, detail);
                    }
                }
                return stop("step", format!("{n} instruction(s)"));
            }
            ResumeKind::StepOver => {
                self.emu.debug_step_over(RUN_BUDGET)?;
                self.emit_events()?;
                return self.bounded_result("stepped over");
            }
            ResumeKind::StepOut => {
                self.emu.debug_step_out(RUN_BUDGET)?;
                self.emit_events()?;
                return self.bounded_result("stepped out");
            }
            ResumeKind::StepCopper => {
                let advanced = self.emu.debug_step_copper(RUN_BUDGET)?;
                self.emit_events()?;
                if let Some((reason, detail)) = self.take_stop() {
                    return stop(reason, detail);
                }
                return if advanced {
                    stop("step", "copper instruction retired".to_string())
                } else {
                    stop(
                        "budget",
                        "copper did not advance (stopped or DMA off)".to_string(),
                    )
                };
            }
            ResumeKind::StepFrame { n } => {
                for _ in 0..*n {
                    self.emu.step_frame()?;
                    self.ctx.apply_due_scheduled(&mut self.emu);
                    self.emit_events()?;
                    if let Some((reason, detail)) = self.take_stop() {
                        return stop(reason, detail);
                    }
                }
                return stop("step", format!("{n} frame(s)"));
            }
            _ => {}
        }

        // Unbounded runs: continue / run_until. One socket poll per
        // quantum; interrupted by pause, EOF, or shutdown.
        let target = match kind {
            ResumeKind::Continue => None,
            ResumeKind::RunUntil(target) => Some(*target),
            _ => unreachable!("bounded verbs returned above"),
        };
        if let Some(RunTarget::Beam { vpos, hpos }) = target {
            self.emu.bus_mut().ui_arm_beam_trap_once(vpos, hpos);
        }
        let cck_target = match target {
            Some(RunTarget::Cck(cck)) => Some(cck),
            Some(RunTarget::Seconds(secs)) => {
                Some((secs * f64::from(crate::chipset::paula::PAULA_CLOCK_HZ)).ceil() as u64)
            }
            _ => None,
        };
        let mut stable = match target {
            Some(RunTarget::Stable(spec)) => Some(StableWatch::new(spec)),
            _ => None,
        };
        let mut extra_ids = Vec::new();
        let finish = |reason: &str, detail: String, extra_ids: Vec<Value>, kill: bool| {
            Ok(RunOutcome::Stop {
                reason: reason.to_string(),
                detail,
                extra_ids,
                kill_after: kill,
            })
        };
        loop {
            // Target already met (or met exactly at the last quantum)?
            match target {
                Some(RunTarget::Pc(pc))
                    if self.emu.machine.pc() & self.emu.machine.ui_addr_mask()
                        == pc & self.emu.machine.ui_addr_mask() =>
                {
                    return finish("target", format!("pc ${pc:06X}"), extra_ids, false);
                }
                Some(RunTarget::Frame(frame)) if self.emu.bus().emulated_frames() >= frame => {
                    return finish("target", format!("frame {frame}"), extra_ids, false);
                }
                _ => {}
            }
            if let Some(cck) = cck_target {
                if self.emu.bus().emulated_cck() >= cck {
                    return finish("target", format!("cck {cck}"), extra_ids, false);
                }
            }
            // Sampled before the quantum, so the frame already on screen
            // when the client asked is the baseline for "unchanged".
            if let Some(watch) = stable.as_mut() {
                match watch.sample(&self.emu) {
                    StableStep::Running => {}
                    StableStep::Settled(detail) => {
                        return finish("target", detail, extra_ids, false)
                    }
                    StableStep::GaveUp(detail) => {
                        return finish("budget", detail, extra_ids, false)
                    }
                }
            }

            // One quantum.
            match target {
                Some(RunTarget::Pc(pc)) => {
                    // Instruction-granular so the landing is exact;
                    // debug_step_for_gdb keeps reverse-debug captures
                    // happening at frame crossings. The hit itself is
                    // seen by the checks at the top of the loop.
                    let mask = self.emu.machine.ui_addr_mask();
                    for _ in 0..PC_POLL_CHUNK {
                        self.emu.debug_step_for_gdb(&mut cpu_idle)?;
                        if self.emu.machine.pc() & mask == pc & mask
                            || self.emu.machine.ui_debug_stop_pending()
                        {
                            break;
                        }
                    }
                }
                _ if cck_target
                    .is_some_and(|cck| cck - self.emu.bus().emulated_cck() < CCK_FINE_WINDOW) =>
                {
                    // Close to a cck/seconds target: land on the first
                    // instruction boundary at or past it.
                    let cck = cck_target.expect("guarded by is_some_and");
                    while self.emu.bus().emulated_cck() < cck {
                        self.emu.debug_step_for_gdb(&mut cpu_idle)?;
                        if self.emu.machine.ui_debug_stop_pending() {
                            break;
                        }
                    }
                }
                _ => {
                    // Frame-granular: step_frame ends early on a debug
                    // stop and takes reverse-debug snapshots when due.
                    self.emu.step_frame()?;
                }
            }
            self.ctx.apply_due_scheduled(&mut self.emu);
            self.emit_events()?;

            if self.emu.machine.cpu_double_faulted() {
                return finish(
                    "double_fault",
                    "CPU double fault (halted)".to_string(),
                    extra_ids,
                    false,
                );
            }
            if let Some(debug_stop) = self.emu.machine.take_ui_debug_stop() {
                let (mut reason, detail) = exec::stop_reason_of(&debug_stop);
                if let (Some(RunTarget::Beam { vpos, .. }), DebugStop::Beam { vpos: at, .. }) =
                    (target, &debug_stop)
                {
                    if *at == vpos {
                        reason = "target";
                    }
                }
                return finish(reason, detail, extra_ids, false);
            }

            match self.poll_mid_run(&mut extra_ids)? {
                MidRun::Kept => {}
                MidRun::Pause => {
                    return finish("pause", "paused by client".to_string(), extra_ids, false)
                }
                MidRun::Kill => return finish("pause", "shutdown".to_string(), extra_ids, true),
                MidRun::Lost(end) => {
                    self.emu.bus_mut().ui_disarm_beam_trap_once();
                    return Ok(RunOutcome::ClientLost(end));
                }
            }
        }
    }

    /// Report the end of a bounded step-over/step-out: a debug stop hit
    /// on the way wins, otherwise it is a plain step.
    fn bounded_result(&mut self, what: &str) -> Result<RunOutcome> {
        let (reason, detail) = self
            .take_stop()
            .map(|(r, d)| (r.to_string(), d))
            .unwrap_or_else(|| ("step".to_string(), what.to_string()));
        Ok(RunOutcome::Stop {
            reason,
            detail,
            extra_ids: Vec::new(),
            kill_after: false,
        })
    }

    /// Drain the machine's pending stop, mapping it onto the protocol
    /// reason. Double fault is checked first: it is not a `ui_*` stop.
    fn take_stop(&mut self) -> Option<(&'static str, String)> {
        if self.emu.machine.cpu_double_faulted() {
            return Some(("double_fault", "CPU double fault (halted)".to_string()));
        }
        self.emu
            .machine
            .take_ui_debug_stop()
            .map(|stop| exec::stop_reason_of(&stop))
    }

    /// Service requests that arrive while the machine is running. Runs
    /// at a quantum boundary, so inspection sees consistent state.
    fn poll_mid_run(&mut self, extra_ids: &mut Vec<Value>) -> Result<MidRun> {
        loop {
            let line = match self.reader.poll()? {
                Polled::Empty => return Ok(MidRun::Kept),
                Polled::Eof => return Ok(MidRun::Lost(SessionEnd::Detached)),
                Polled::Line(line) => line,
            };
            let req = match proto::parse_request(&line) {
                Ok(req) => req,
                Err(reply) => {
                    self.write(&reply)?;
                    continue;
                }
            };
            match self.gate.handle(&req) {
                Gate::Reply(reply) => {
                    self.write(&reply)?;
                    continue;
                }
                Gate::ReplyAndClose(reply) => {
                    self.write(&reply)?;
                    return Ok(MidRun::Lost(SessionEnd::Detached));
                }
                Gate::Pass => {}
            }
            if req.method == "shutdown" {
                self.write(&proto::ok_line(&req.id, json!({})))?;
                extra_ids.push(req.id);
                return Ok(MidRun::Kill);
            }
            let parsed = match exec::parse_method(&req.method, &req.params) {
                Ok(parsed) => parsed,
                Err(err) => {
                    self.write(&proto::err_line(&req.id, &err))?;
                    continue;
                }
            };
            match parsed {
                Request::Host(HostOp::Pause) => {
                    extra_ids.push(req.id);
                    return Ok(MidRun::Pause);
                }
                Request::Host(HostOp::Resume(_)) => {
                    self.write(&proto::err_line(
                        &req.id,
                        &CtlError::new(proto::RESUME_PENDING, "a resume is already pending"),
                    ))?;
                }
                Request::Host(HostOp::Input(cmd)) => {
                    let now = self.emu.bus().emulated_seconds();
                    let (immediate, later) = cmd.expand(now);
                    for action in immediate {
                        self.ctx.inject_now(&mut self.emu, action);
                    }
                    let scheduled = later.len();
                    for entry in later {
                        self.ctx.schedule(entry.at_seconds, entry.action);
                    }
                    self.write(&proto::ok_line(
                        &req.id,
                        json!({"applied_at_seconds": now, "scheduled": scheduled}),
                    ))?;
                }
                Request::Host(HostOp::StateLoad { .. }) => {
                    self.write(&proto::err_line(
                        &req.id,
                        &CtlError::invalid_state("pause before loading a state"),
                    ))?;
                }
                Request::Host(HostOp::MouseTo { .. }) => {
                    // The servo advances the machine frame by frame to
                    // watch what its own deltas did, so it cannot share
                    // the timeline with an in-flight resume.
                    self.write(&proto::err_line(
                        &req.id,
                        &CtlError::invalid_state("pause before servoing the pointer"),
                    ))?;
                }
                Request::Host(
                    op @ (HostOp::FloppyInsert { .. }
                    | HostOp::FloppyEject { .. }
                    | HostOp::CdInsert { .. }
                    | HostOp::CdEject
                    | HostOp::SetPortDevice { .. }
                    | HostOp::Reset { .. }),
                ) => {
                    // Media changes, controller hot-plug, and reset are
                    // ordinary live events; apply them at this boundary.
                    if let Some(end) = self.dispatch_host(req.id, op)? {
                        return Ok(MidRun::Lost(end));
                    }
                    self.emit_events()?;
                }
                Request::Core(op) if op.allowed_while_running() => {
                    let reply = match exec::exec_core(&mut self.emu, &mut self.ctx, &op) {
                        Ok(result) => proto::ok_line(&req.id, result),
                        Err(err) => proto::err_line(&req.id, &err),
                    };
                    self.write(&reply)?;
                    self.emit_events()?;
                }
                Request::Core(_) => {
                    self.write(&proto::err_line(
                        &req.id,
                        &CtlError::invalid_state("pause before repositioning the machine"),
                    ))?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::test_emulator;
    use std::io::BufRead;

    const TOKEN: &str = "sesame";

    /// A minimal JSON-RPC line client for driving a [`Session`] over
    /// loopback, with out-of-order response matching by id.
    struct Client {
        reader: std::io::BufReader<TcpStream>,
        writer: TcpStream,
        next_id: u64,
        stash: Vec<Value>,
    }

    impl Client {
        fn connect(addr: std::net::SocketAddr) -> Self {
            let stream = TcpStream::connect(addr).expect("connecting to control session");
            stream.set_nodelay(true).ok();
            Self {
                reader: std::io::BufReader::new(stream.try_clone().expect("cloning client stream")),
                writer: stream,
                next_id: 1,
                stash: Vec::new(),
            }
        }

        fn send(&mut self, method: &str, params: Value) -> u64 {
            let id = self.next_id;
            self.next_id += 1;
            let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
            proto::write_line(&mut self.writer, &msg.to_string()).expect("sending request");
            id
        }

        fn recv(&mut self) -> Value {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).expect("reading response");
            assert!(n > 0, "server closed the connection unexpectedly");
            serde_json::from_str(line.trim()).expect("response is JSON")
        }

        /// The full response envelope for `id`, stashing any other
        /// responses that arrive first.
        fn wait_for(&mut self, id: u64) -> Value {
            if let Some(pos) = self.stash.iter().position(|v| v["id"].as_u64() == Some(id)) {
                return self.stash.remove(pos);
            }
            loop {
                let msg = self.recv();
                if msg["id"].as_u64() == Some(id) {
                    return msg;
                }
                self.stash.push(msg);
            }
        }

        fn call(&mut self, method: &str, params: Value) -> Value {
            let id = self.send(method, params);
            self.wait_for(id)
        }

        /// Call and unwrap the `result`, panicking on an error reply.
        fn result(&mut self, method: &str, params: Value) -> Value {
            let msg = self.call(method, params);
            assert!(
                msg.get("error").is_none(),
                "{method} failed: {}",
                msg["error"]
            );
            msg["result"].clone()
        }

        fn auth(&mut self) {
            let hello = self.result("hello", json!({"token": TOKEN}));
            assert_eq!(hello["authed"], true);
        }
    }

    /// Run one session against the test emulator: the client closure
    /// drives it from a spawned thread while the session runs on the
    /// test thread (the gdbstub `run_session` pattern). Time travel is
    /// armed like `run()` arms it. Returns the session context and how
    /// it ended, for post-conditions. The session (and its sockets) is
    /// dropped before joining the client, exactly as `run()`'s serve
    /// loop drops it per iteration -- a client waiting for EOF after a
    /// server-side close would otherwise deadlock the harness.
    fn run_session(
        recorder: Option<InputRecorder>,
        client_fn: impl FnOnce(&mut Client) + Send + 'static,
    ) -> (SessionCtx, SessionEnd) {
        let mut emu = test_emulator();
        emu.enable_time_travel(64, 1);
        emu.debug_ensure_time_travel_anchor().unwrap();
        emu.machine.ui_set_pc_history_enabled(true);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let mut client = Client::connect(addr);
            client_fn(&mut client);
        });
        let (stream, _) = listener.accept().unwrap();
        let config = Config::new(":0".into());
        let mut session = Session::new(emu, stream, TOKEN, &config, recorder);
        let end = session.serve().expect("session should not error");
        session.teardown();
        // Close the session's stream handles NOW: fields matched by
        // `..` in a partial-move destructure live until end of scope,
        // which would be after the join below.
        let Session {
            ctx,
            reader,
            writer,
            ..
        } = session;
        drop(reader);
        drop(writer);
        handle.join().expect("client assertions failed");
        (ctx, end)
    }

    fn scratch_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ccp-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn hello_auth_status() {
        run_session(None, |c| {
            // Bare hello: version fields only, not authenticated.
            let hello = c.result("hello", json!({}));
            assert_eq!(hello["proto"], proto::PROTO_VERSION);
            assert_eq!(hello["authed"], false);
            // Anything else is refused pre-auth.
            let refused = c.call("status", json!({}));
            assert_eq!(refused["error"]["code"], proto::NOT_AUTHED);
            // Token via auth.
            let authed = c.result("auth", json!({"token": TOKEN}));
            assert_eq!(authed["authed"], true);
            let status = c.result("status", json!({}));
            assert_eq!(status["state"], "paused");
            assert_eq!(status["pc"], 0xF80010);
            assert_eq!(status["tt_armed"], true);
        });
    }

    #[test]
    fn wrong_token_gets_one_error_then_close() {
        let (_, end) = run_session(None, |c| {
            let refused = c.call("auth", json!({"token": "wrong"}));
            assert_eq!(refused["error"]["code"], proto::AUTH_FAILED);
            // The server drops the connection after the reply.
            let mut line = String::new();
            let n = c.reader.read_line(&mut line).expect("read after close");
            assert_eq!(n, 0, "expected EOF after failed auth");
        });
        assert!(matches!(end, SessionEnd::Detached));
    }

    #[test]
    fn step_advances_pc() {
        run_session(None, |c| {
            c.auth();
            let stop = c.result("step", json!({"n": 1}));
            assert_eq!(stop["reason"], "step");
            assert_eq!(stop["pc"], 0xF80012); // past the NOP
            let before = stop["retired_instructions"].as_u64().unwrap();
            let stop = c.result("step", json!({"n": 2}));
            assert_eq!(stop["pc"], 0xF8001A); // ADDQ, MOVE.W executed
            assert!(stop["retired_instructions"].as_u64().unwrap() > before);
        });
    }

    #[test]
    fn breakpoint_hit_on_continue() {
        run_session(None, |c| {
            c.auth();
            let id = c.result("break.add", json!({"kind": "pc", "addr": "$F8001A"}));
            assert_eq!(id["id"], 1);
            let stop = c.result("continue", json!({}));
            assert_eq!(stop["reason"], "breakpoint");
            assert_eq!(stop["pc"], 0xF8001A);
        });
    }

    #[test]
    fn run_until_pc_and_beam() {
        run_session(None, |c| {
            c.auth();
            let stop = c.result("run_until", json!({"pc": "$F80014"}));
            assert_eq!(stop["reason"], "target");
            assert_eq!(stop["pc"], 0xF80014);
            let stop = c.result("run_until", json!({"vpos": 150}));
            assert_eq!(stop["reason"], "target");
            assert_eq!(stop["vpos"], 150);
        });
    }

    #[test]
    fn memory_rw_roundtrip_reports_replay_unsafe() {
        run_session(None, |c| {
            c.auth();
            let write = c.result("mem.write", json!({"addr": "$30000", "data": "cafef00d"}));
            assert_eq!(write["written"], 4);
            assert_eq!(write["replay_unsafe"], true, "tt is armed in this session");
            let read = c.result(
                "mem.read",
                json!({"addr": "$30000", "len": 4, "encoding": "base64"}),
            );
            assert_eq!(
                proto::decode_base64(read["data"].as_str().unwrap()).unwrap(),
                vec![0xca, 0xfe, 0xf0, 0x0d]
            );
        });
    }

    #[test]
    fn screenshot_and_digest_are_deterministic() {
        run_session(None, |c| {
            c.auth();
            let path = scratch_path("shot.png");
            let shot = c.result(
                "capture.screenshot",
                json!({"path": path.display().to_string()}),
            );
            assert_eq!(shot["width"], 716);
            assert!(path.exists(), "screenshot file written");
            std::fs::remove_file(&path).ok();
            let a = c.result("capture.digest", json!({}));
            let b = c.result("capture.digest", json!({}));
            assert_eq!(a["digest"], b["digest"]);
        });
    }

    #[test]
    fn mouse_to_reports_a_guest_with_no_sprite_pointer() {
        run_session(None, |c| {
            c.auth();
            // The test ROM draws no sprites, so there is no pointer to
            // observe. That must be a loud, specific failure rather than
            // a blind guess at relative motion.
            let refused = c.call("input.mouse_to", json!({"x": 200, "y": 100}));
            assert_eq!(refused["error"]["code"], proto::INVALID_STATE);
            assert!(
                refused["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("sprite 0 is not being drawn"),
                "{}",
                refused["error"]["message"]
            );
        });
    }

    #[test]
    fn mouse_to_is_refused_while_a_resume_is_in_flight() {
        run_session(None, |c| {
            c.auth();
            c.send("continue", json!({}));
            let refused = c.call("input.mouse_to", json!({"x": 10, "y": 10}));
            assert_eq!(refused["error"]["code"], proto::INVALID_STATE);
            c.result("pause", json!({}));
        });
    }

    #[test]
    fn run_until_stable_frames_settles_and_respects_its_budget() {
        run_session(None, |c| {
            c.auth();
            // The test ROM drives no display DMA, so the picture is
            // already still and the second sample confirms it.
            let stop = c.result("run_until", json!({"stable_frames": 2}));
            assert_eq!(stop["reason"], "target");
            assert!(
                stop["detail"].as_str().unwrap().contains("stable for 2"),
                "{}",
                stop["detail"]
            );
            // A region of that same still frame settles too, and the
            // stop lands without advancing to any wall-clock deadline.
            let stop = c.result(
                "run_until",
                json!({"stable_frames": 2, "max_frames": 600, "x": 8, "y": 8, "w": 32, "h": 32}),
            );
            assert_eq!(stop["reason"], "target");
        });
    }

    #[test]
    fn region_digest_reports_its_rectangle_and_rejects_stale_coordinates() {
        run_session(None, |c| {
            c.auth();
            let region = c.result(
                "capture.region_digest",
                json!({"x": 8, "y": 16, "w": 32, "h": 8}),
            );
            assert_eq!(region["w"], 32);
            assert_eq!(region["width"], 716);
            // Coordinates that outran the frame must fail loudly rather
            // than silently digesting a clamped rectangle.
            let stale = c.call("capture.region_digest", json!({"x": 700, "w": 64, "h": 8}));
            assert_eq!(stale["error"]["code"], proto::INVALID_PARAMS);
        });
    }

    #[test]
    fn frame_events_stream_before_the_resume_response() {
        run_session(None, |c| {
            c.auth();
            let subscribed = c.result(
                "events.subscribe",
                json!({
                    "events": ["frame"],
                    "frame_interval": 1,
                    "frame_digest": true,
                }),
            );
            assert_eq!(subscribed["active"], json!(["frame"]));

            let stop = c.result("step_frame", json!({"n": 3}));
            let frame_events: Vec<&Value> = c
                .stash
                .iter()
                .filter(|message| message["method"] == "event.frame")
                .collect();
            let event = frame_events
                .last()
                .expect("frame notification should precede the stop response");
            assert_eq!(
                event["params"]["position"]["frame"], stop["frame"],
                "last notification and stop describe the same completed frame"
            );
            assert_eq!(event["params"]["digest"]["algo"], "fnv1a64");

            let state = c.result("events.unsubscribe", json!({}));
            assert_eq!(state["active"], json!([]));
        });
    }

    #[test]
    fn save_load_state_roundtrip() {
        run_session(None, |c| {
            c.auth();
            let path = scratch_path("state.clstate");
            c.result("step_frame", json!({"n": 2}));
            let saved_at = c.result("beam.get", json!({}));
            c.result("state.save", json!({"path": path.display().to_string()}));
            c.result("step_frame", json!({"n": 3}));
            let loaded = c.result("state.load", json!({"path": path.display().to_string()}));
            assert_eq!(loaded["reconfigured"], false);
            let now = c.result("beam.get", json!({}));
            assert_eq!(now["cck"], saved_at["cck"], "timeline restored");
            std::fs::remove_file(&path).ok();
        });
    }

    #[test]
    fn input_key_is_journaled_and_stamped() {
        let (ctx, _) = run_session(Some(InputRecorder::new(0.0)), |c| {
            c.auth();
            c.result("step_frame", json!({"n": 1}));
            let applied = c.result("input.key", json!({"rawkey": 0x45, "action": "press"}));
            let at = applied["applied_at_seconds"].as_f64().unwrap();
            assert!(at > 0.0, "landed on the emulated clock: {at}");
            // A tap schedules its release on the emulated timeline.
            let tap = c.result("input.key", json!({"rawkey": 0x20, "hold_ms": 50}));
            assert_eq!(tap["scheduled"], 1);
        });
        let script = ctx.recorder.map(|r| r.finish()).unwrap_or_default();
        assert!(
            script.contains("0x45"),
            "recording carries the injected key: {script}"
        );
    }

    #[test]
    fn input_port_methods_hot_plug_and_report_devices() {
        run_session(None, |c| {
            c.auth();
            // A fresh machine (no config layer here) has two mouse ports.
            let ports = c.result("input.get_ports", json!({}));
            assert_eq!(ports["port1"], "mouse");
            assert_eq!(ports["port2"], "mouse");

            // Hot-plug a CD32 pad into port 1 and report it back.
            let set = c.result("input.set_port", json!({"port": 1, "device": "cd32"}));
            assert_eq!(set["port"], 1);
            assert_eq!(set["device"], "cd32");
            let ports = c.result("input.get_ports", json!({}));
            assert_eq!(ports["port1"], "cd32");

            // input.joy/analogue take an optional port; port 3 is refused.
            c.result("input.joy", json!({"port": 1, "up": true, "red": true}));
            c.result("input.analogue", json!({"port": 2, "x": 50, "y": 200}));
            let ports = c.result("input.get_ports", json!({}));
            assert_eq!(ports["port2"], "analogue");
            let bad = c.call("input.mouse", json!({"port": 3, "dx": 1}));
            assert_eq!(bad["error"]["code"], proto::INVALID_PARAMS);
            let bad = c.call("input.set_port", json!({"port": 1, "device": "trackball"}));
            assert_eq!(bad["error"]["code"], proto::INVALID_PARAMS);
        });
    }

    #[test]
    fn reverse_step_and_last_writer() {
        run_session(None, |c| {
            c.auth();
            let stop = c.result("step", json!({"n": 200}));
            let before = stop["retired_instructions"].as_u64().unwrap();
            // The ROM loop has been writing D0 to $20000.
            let mem = c.result("mem.read", json!({"addr": "$20000", "len": 2}));
            assert_ne!(mem["data"], "0000", "the loop stored a nonzero counter");

            let back = c.result("reverse_step", json!({}));
            assert_eq!(back["reason"], "reverse");
            assert_eq!(back["retired_instructions"].as_u64().unwrap(), before - 1);

            let writer = c.result("last_writer", json!({"addr": "$20000"}));
            assert_eq!(writer["outcome"], "found");
            assert_eq!(
                writer["record"]["pc"], 0xF80014,
                "the MOVE.W D0,(abs).L instruction is the writer"
            );
        });
    }

    #[test]
    fn pause_interrupts_continue_and_second_resume_is_refused() {
        run_session(None, |c| {
            c.auth();
            // The ROM program loops forever; only pause can stop it.
            let cont = c.send("continue", json!({}));
            let second = c.send("step_frame", json!({}));
            let pause = c.send("pause", json!({}));

            let refused = c.wait_for(second);
            assert_eq!(refused["error"]["code"], proto::RESUME_PENDING);

            let stop = c.wait_for(cont);
            assert_eq!(stop["result"]["reason"], "pause");
            let also = c.wait_for(pause);
            assert_eq!(
                also["result"]["cck"], stop["result"]["cck"],
                "pause reports the same stop position"
            );
        });
    }

    #[test]
    fn inspection_is_serviced_mid_run() {
        run_session(None, |c| {
            c.auth();
            let cont = c.send("continue", json!({}));
            let regs = c.call("regs.get", json!({}));
            assert!(regs.get("error").is_none(), "inspection works mid-run");
            let denied = c.call("reverse_step", json!({}));
            assert_eq!(denied["error"]["code"], proto::INVALID_STATE);
            let stop = c.call("pause", json!({}));
            assert_eq!(stop["result"]["reason"], "pause");
            c.wait_for(cont);
        });
    }

    #[test]
    fn shutdown_ends_the_server() {
        let (_, end) = run_session(None, |c| {
            c.auth();
            c.result("shutdown", json!({}));
        });
        assert!(matches!(end, SessionEnd::Killed));
    }

    #[test]
    fn continue_with_collect_returns_data_at_the_stop() {
        run_session(None, |c| {
            c.auth();
            c.result("break.add", json!({"kind": "pc", "addr": "$F8001A"}));
            let stop = c.result(
                "continue",
                json!({"collect": [
                    {"method": "regs.get"},
                    {"method": "mem.read", "params": {"addr": "$20000", "len": 2}},
                ]}),
            );
            assert_eq!(stop["reason"], "breakpoint");
            let collect = stop["collect"].as_array().unwrap();
            assert_eq!(collect.len(), 2);
            assert_eq!(collect[0]["ok"]["pc"], stop["pc"]);
            assert!(collect[1]["ok"]["data"].is_string());
        });
    }
}
