// SPDX-License-Identifier: GPL-3.0-or-later

//! The windowed GDB drain (`--gdb-gui`): packets enqueued by the socket
//! threads (`gdbstub::windowed`) are executed here, on the winit thread,
//! at the top of `about_to_wait` -- the same frame boundary the windowed
//! control server drains at. The packet semantics are the shared
//! transport-free [`GdbCore`]; this module owns what differs in a
//! windowed session:
//!
//! - The machine runs paced and visible, so an open-ended continue
//!   cannot spin a per-instruction socket-polling loop the way the
//!   headless driver does. Instead, `c`/`vCont;c` defers the stop reply
//!   (`pending`) and lets the ordinary frame loop run; stops surface
//!   through `surface_debug_stop` and answer the client via
//!   [`App::gdb_completes_stop`].
//! - The core's session breakpoint list is polled per instruction by the
//!   headless continue loop and never seen by the CPU hot loop, so here
//!   every core-held break translates into machine-installed debug state
//!   ([`GdbBreaks`]), with the control session's ownership rule: share
//!   compatible local points, temporarily union incompatible memory-watch
//!   access classes, and restore local state on detach.
//! - `monitor warp` engages the App's warp hold (`WarpSource::Gdb`), and
//!   the register-watch monitors go through the machine's toggle API so
//!   they compose with GUI-set watches instead of clobbering them.
//! - `k` detaches instead of killing the session: the window is the
//!   user's, and VS Code sends `k` on Stop Debugging.

use super::app_session::WarpSource;
use super::*;
use crate::debugger::DebugStop;
use crate::gdbstub::core::{hex_decode, CoreReply, GdbCore, ResumeRequest, GDB_WATCH_WORD_CAP};
use crate::gdbstub::windowed::{GdbHandle, GdbMsg};

/// Per-connection GDB state owned by the `App`.
pub(super) struct GdbGuiState {
    pub(super) handle: GdbHandle,
    core: GdbCore,
    breaks: GdbBreaks,
    /// A continue whose stop reply is deferred until the machine stops.
    pending: bool,
    /// `--run` + `--gdb-gui`: re-armed per connection, so a reconnecting
    /// client gets the same break-at-entry.
    stop_on_load: Option<String>,
    reverse_budget_mb: usize,
    reverse_interval_frames: u64,
}

/// The machine-installed debug state this GDB session owns. Everything
/// here went in through the same toggle APIs the GUI debugger uses. PC and
/// register points that already existed are shared untouched; incompatible
/// memory watches are saved, replaced by their unfiltered access union, and
/// restored when the session releases them.
#[derive(Default)]
struct GdbBreaks {
    pc: Vec<u32>,
    watch_words: Vec<GdbWatchWord>,
    reg_watches: Vec<u16>,
    /// `Some(filter)` when this session armed the machine loadseg catch.
    loadseg: Option<Option<String>>,
    catches: Vec<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SavedWatch {
    filter: Option<crate::debugger::WatchSource>,
    pc: Option<u32>,
    access: crate::debugger::WatchAccess,
}

#[derive(Clone, Copy, Debug)]
struct GdbWatchWord {
    addr: u32,
    requested: crate::debugger::WatchAccess,
    installed: crate::debugger::WatchAccess,
    /// A local GUI watch displaced by the unfiltered union installed for GDB.
    /// It is restored when the GDB request is removed or the client detaches.
    prior: Option<SavedWatch>,
}

fn watch_at(emu: &Emulator, addr: u32) -> Option<SavedWatch> {
    emu.machine
        .ui_breaks()
        .watches
        .iter()
        .find(|watch| watch.addr == addr)
        .map(|watch| SavedWatch {
            filter: watch.filter,
            pc: watch.pc,
            access: watch.access,
        })
}

fn toggle_watch(emu: &mut Emulator, addr: u32, watch: SavedWatch) {
    emu.machine
        .ui_toggle_watch_access(addr, watch.filter, watch.pc, watch.access);
}

impl GdbBreaks {
    /// Reconcile the machine's debug stores with what the core now
    /// holds, returning console notes (a truncated watchpoint, say).
    fn sync(&mut self, emu: &mut Emulator, core: &GdbCore) -> Vec<String> {
        let mut notes = Vec::new();
        let mask = emu.machine.ui_addr_mask();
        if core.bartman && core.run_stop.is_none() {
            for vector in [3, 4, 39] {
                if !emu.machine.ui_breaks().catches.contains(&vector) {
                    emu.machine.ui_toggle_catch(vector);
                    self.catches.push(vector);
                }
            }
        }

        // PC breakpoints: the core list is the desired set.
        let desired: Vec<u32> = core.breakpoints.iter().map(|&a| a & mask).collect();
        self.pc.retain(|&addr| {
            if desired.contains(&addr) {
                // The GUI may have toggled our point off meanwhile;
                // dropping the stale ownership entry lets the insertion
                // pass restore it. The client's acknowledged break state
                // is authoritative for its own points.
                return emu.machine.ui_breaks().is_breakpoint(addr);
            }
            if emu.machine.ui_breaks().is_breakpoint(addr) {
                emu.machine.ui_set_breakpoint(addr, None, 0);
            }
            false
        });
        for &addr in &desired {
            if self.pc.contains(&addr) || emu.machine.ui_breaks().is_breakpoint(addr) {
                continue; // ours already, or the GUI owns it
            }
            emu.machine.ui_set_breakpoint(addr, None, 0);
            self.pc.push(addr);
        }

        // Watchpoints: every covered word becomes a machine word watch,
        // bounded by the cap.
        let (desired_words, truncated) = core.machine_watch_words(mask);
        if truncated {
            notes.push(format!(
                "watchpoints cover more than {GDB_WATCH_WORD_CAP} words; \
                 watching the first {GDB_WATCH_WORD_CAP}\n"
            ));
        }
        // Reconcile owned words first. When the machine still contains the
        // exact unfiltered union we installed, remove it and restore any GUI
        // watch it displaced. A GUI edit/removal wins and is never overwritten
        // with stale saved state.
        let mut kept = Vec::new();
        for owned in self.watch_words.drain(..) {
            let current = watch_at(emu, owned.addr);
            let installed = SavedWatch {
                filter: None,
                pc: None,
                access: owned.installed,
            };
            if desired_words.contains(&(owned.addr, owned.requested)) && current == Some(installed)
            {
                kept.push(owned);
                continue;
            }
            if current == Some(installed) {
                emu.machine.ui_toggle_watch(owned.addr);
                if let Some(prior) = owned.prior {
                    toggle_watch(emu, owned.addr, prior);
                }
            }
        }
        self.watch_words = kept;
        for &(addr, requested) in &desired_words {
            if self
                .watch_words
                .iter()
                .any(|owned| owned.addr == addr && owned.requested == requested)
            {
                continue;
            }
            let prior = watch_at(emu, addr);
            if prior.is_some_and(|watch| {
                watch.filter.is_none()
                    && watch.pc.is_none()
                    && watch.access.union(requested) == watch.access
            }) {
                continue; // an unqualified GUI watch already covers GDB
            }
            if prior.is_some() {
                emu.machine.ui_toggle_watch(addr);
            }
            let installed = prior.map_or(requested, |watch| watch.access.union(requested));
            emu.machine
                .ui_toggle_watch_access(addr, None, None, installed);
            self.watch_words.push(GdbWatchWord {
                addr,
                requested,
                installed,
                prior,
            });
        }

        // Chipset register watches (fed by the intercepted monitor
        // commands below), through the per-register toggle so GUI-set
        // watches survive.
        let reg_watched =
            |emu: &Emulator, off: u16| emu.machine.ui_breaks().reg_watches.contains(&off);
        self.reg_watches.retain(|&off| {
            if core.reg_watches.contains(&off) {
                // Same rule: a GUI-removed watch comes back below.
                return reg_watched(emu, off);
            }
            if reg_watched(emu, off) {
                emu.machine.ui_toggle_reg_watch(off);
            }
            false
        });
        for &off in &core.reg_watches {
            if self.reg_watches.contains(&off) || reg_watched(emu, off) {
                continue;
            }
            emu.machine.ui_toggle_reg_watch(off);
            self.reg_watches.push(off);
        }

        // The machine-level loadseg catch stands in for the headless
        // driver's per-instruction tracker poll: armed while the client
        // wants library events, a loadseg break, or the one-shot --run
        // stop; the name filter narrows to the --run target when that is
        // the only consumer.
        let want = core.lib_events_armed || core.loadseg_break || core.run_stop.is_some();
        let filter = if core.lib_events_armed || core.loadseg_break {
            None
        } else {
            core.run_stop.clone()
        };
        let have = emu.machine.ui_breaks().loadseg_catch.is_some();
        match (&self.loadseg, want, have) {
            (Some(installed), true, true) if *installed != filter => {
                emu.machine.ui_toggle_loadseg_catch(None); // off (ours)
                emu.machine.ui_toggle_loadseg_catch(filter.clone());
                self.loadseg = Some(filter);
            }
            (Some(_), false, true) => {
                emu.machine.ui_toggle_loadseg_catch(None);
                self.loadseg = None;
            }
            (Some(_), false, false) => self.loadseg = None, // GUI beat us to it
            (Some(_), true, false) => {
                // The GUI removed our catch while the client still wants
                // it; restore it, like the other break kinds.
                emu.machine.ui_toggle_loadseg_catch(filter.clone());
                self.loadseg = Some(filter);
            }
            (None, true, false) => {
                emu.machine.ui_toggle_loadseg_catch(filter.clone());
                self.loadseg = Some(filter);
            }
            // GUI owns the catch, or the state already matches.
            _ => {}
        }
        notes
    }

    /// Remove everything this session installed (detach). GUI-set points
    /// are left alone; a point the GUI already removed counts as done.
    fn remove_all(&mut self, emu: &mut Emulator) {
        for vector in self.catches.drain(..) {
            if emu.machine.ui_breaks().catches.contains(&vector) {
                emu.machine.ui_toggle_catch(vector);
            }
        }
        for addr in self.pc.drain(..) {
            if emu.machine.ui_breaks().is_breakpoint(addr) {
                emu.machine.ui_set_breakpoint(addr, None, 0);
            }
        }
        for owned in self.watch_words.drain(..) {
            let installed = SavedWatch {
                filter: None,
                pc: None,
                access: owned.installed,
            };
            if watch_at(emu, owned.addr) == Some(installed) {
                emu.machine.ui_toggle_watch(owned.addr);
                if let Some(prior) = owned.prior {
                    toggle_watch(emu, owned.addr, prior);
                }
            }
        }
        for off in self.reg_watches.drain(..) {
            if emu.machine.ui_breaks().reg_watches.contains(&off) {
                emu.machine.ui_toggle_reg_watch(off);
            }
        }
        if self.loadseg.take().is_some() && emu.machine.ui_breaks().loadseg_catch.is_some() {
            emu.machine.ui_toggle_loadseg_catch(None);
        }
    }
}

/// Whether `packet` moves the machine's position rather than reading or
/// resuming it in place: reverse step/continue, `sADDR` / `cADDR` (the
/// core writes the PC before returning the resume), and a write to the
/// PC register (regnum 17, `P11=`). Bare `s`/`c`, memory writes and other
/// register writes are in-place, like the control protocol's own
/// `allowed_while_running` set.
fn repositions_machine(packet: &str) -> bool {
    packet == "bs"
        || packet == "bc"
        || (packet.len() > 1 && (packet.starts_with('s') || packet.starts_with('c')))
        || packet.starts_with("P11=")
        || packet == "qOffsets"
        || ((packet.starts_with('C') || packet.starts_with('S')) && packet.contains(';'))
        || decode_qrcmd(packet).is_some_and(|command| {
            matches!(command.split_whitespace().next(), Some("profile" | "reset"))
        })
}

/// The decoded text of a qRcmd (monitor) packet, when it is one.
fn decode_qrcmd(packet: &str) -> Option<String> {
    let hex = packet.strip_prefix("qRcmd,")?;
    let bytes = hex_decode(hex).ok()?;
    String::from_utf8(bytes).ok().map(|s| s.trim().to_string())
}

impl App {
    /// Adopt a bound GDB server; called from `main` between `App::new`
    /// and `run()`.
    pub fn attach_gdb(&mut self, handle: GdbHandle, config: &crate::gdbstub::Config) {
        let mut core = GdbCore::new(&self.emu, config.stop_on_load.clone());
        core.bartman = config.bartman;
        self.gdb = Some(GdbGuiState {
            handle,
            core,
            breaks: GdbBreaks::default(),
            pending: false,
            stop_on_load: config.stop_on_load.clone(),
            reverse_budget_mb: config.reverse_budget_mb,
            reverse_interval_frames: config.reverse_interval_frames,
        });
    }

    /// Whether this client's continue is outstanding.
    pub(super) fn gdb_resume_pending(&self) -> bool {
        self.gdb.as_ref().is_some_and(|g| g.pending)
    }

    /// Drain queued GDB messages. Runs at the top of `about_to_wait`,
    /// before the machine steps, so packets land at a frame boundary;
    /// also callable directly from tests (no sockets or event loop).
    pub(super) fn drain_gdb(&mut self) {
        if self.gdb.is_none() {
            return;
        }
        while let Some(msg) = self.gdb.as_ref().and_then(|g| g.handle.try_recv()) {
            match msg {
                GdbMsg::Connected => self.gdb_on_connected(),
                GdbMsg::Disconnected => self.gdb_detach_cleanup("GDB client detached"),
                GdbMsg::Interrupt => self.gdb_on_interrupt(),
                GdbMsg::Packet(packet) => self.gdb_on_packet(&packet),
            }
        }
    }

    fn gdb_on_connected(&mut self) {
        // Stale installs from a client that vanished without detaching
        // come off before the fresh session starts.
        if let Some(g) = self.gdb.as_mut() {
            g.breaks.remove_all(&mut self.emu);
            g.pending = false;
            // A fresh core re-arms the stop-on-load target; its tracker
            // absorbs an already-running program so nothing fires
            // spuriously (same as the headless per-connection session).
            let bartman = g.core.bartman;
            g.core = GdbCore::new(&self.emu, g.stop_on_load.clone());
            g.core.bartman = bartman;
        }
        let (budget, interval) = match self.gdb.as_ref() {
            Some(g) => (g.reverse_budget_mb, g.reverse_interval_frames),
            None => return,
        };
        // Arm the reverse-debug ring for this client, like the headless
        // driver does. No pc history: the headless stub leaves it off,
        // and enabling it would change JIT/run-ahead behaviour.
        self.emu.enable_time_travel(budget, interval);
        if let Err(e) = self.emu.debug_ensure_time_travel_anchor() {
            warn!("gdb: arming time travel failed: {e:#}");
        }
        // Attaching stops the target: gdb's first packets read registers
        // and memory expecting a stopped inferior. A control client's
        // resume outstanding on the same window ends here (this client's
        // own pending was cleared above, so no stale stop reply is sent).
        if !self.paused {
            self.paused = true;
            self.sync_live_audio_suspension();
        }
        self.complete_remote_resumes("pause", "paused: gdb client attached");
        self.show_osd("GDB client attached");
        self.request_redraw();
    }

    /// Tear down session-installed state. Used for a socket disconnect,
    /// a `D` detach, and the `k` packet; safe to run more than once.
    fn gdb_detach_cleanup(&mut self, why: &str) {
        let Some(g) = self.gdb.as_mut() else {
            return;
        };
        g.pending = false;
        g.breaks.remove_all(&mut self.emu);
        // A warp the client engaged has no client left to release it;
        // another holder's warp (a control client, the guest) stays.
        if self.warp_holds.contains(WarpSource::Gdb) {
            self.set_warp(false, WarpSource::Gdb);
        }
        self.show_osd(why.to_string());
        self.request_redraw();
    }

    fn gdb_on_interrupt(&mut self) {
        if let Some(g) = self.gdb.as_mut() {
            g.pending = false;
        }
        if !self.paused {
            self.paused = true;
            self.sync_live_audio_suspension();
        }
        // A control client's resume outstanding on the same window ends
        // with the interrupt (this client's own pending is already
        // cleared, so exactly one stop reply follows).
        self.complete_remote_resumes("pause", "interrupted by gdb");
        // Ctrl-C always gets a stop reply, pending continue or not (the
        // headless driver answers the raw 0x03 the same way).
        self.gdb_send_packet(self.gdb_plain_stop());
        self.last_debug_stop = Some("interrupted by gdb".to_string());
        self.show_osd("GDB: interrupted");
        self.finish_render_for_current_frame();
        self.request_redraw();
    }

    fn gdb_on_packet(&mut self, packet: &str) {
        if packet == "QStartNoAckMode" {
            // The reader thread already flipped its ack mode.
            self.gdb_send_packet("OK");
            return;
        }
        // Monitor commands that must run against the App rather than the
        // core: warp (the App owns pacing) and the register-watch set
        // (the core's own handlers write the whole bus list, which would
        // clobber GUI-set watches; the machine toggle API composes).
        if let Some(command) = decode_qrcmd(packet) {
            if let Some(output) = self.gdb_monitor_intercept(&command) {
                if let Some(g) = self.gdb.as_mut() {
                    g.core.console.push(output);
                }
                self.gdb_sync_machine_debug_state();
                self.gdb_flush_console();
                self.gdb_send_packet("OK");
                return;
            }
        }
        // Packets that reposition the machine -- reverse execution, a
        // resume at an address, a PC write -- are refused while a control
        // client's resume is outstanding, as the control protocol refuses
        // its own repositioning verbs while a GDB continue runs.
        let bootstraps_bartman = packet == "?"
            && self
                .gdb
                .as_ref()
                .is_some_and(|g| g.core.bartman && g.core.run_stop.is_some());
        if (repositions_machine(packet) || bootstraps_bartman) && self.remote_resume_pending() {
            if let Some(g) = self.gdb.as_mut() {
                g.core.console.push(
                    "machine is running (a control client resume is pending); pause first\n"
                        .to_string(),
                );
            }
            self.gdb_flush_console();
            self.gdb_send_packet("E01");
            return;
        }
        let Some(mut g) = self.gdb.take() else {
            return;
        };
        let result = g.core.handle_packet(&mut self.emu, packet);
        let result = if g.core.outside_rom
            && matches!(result, Ok(CoreReply::Resume(ResumeRequest::Continue)))
        {
            match self.emu.debug_run_until_pc_outside(
                crate::memory::ROM_BASE as u32,
                0x00ff_ffff,
                5_000_000,
            ) {
                Ok(true) => {
                    g.core.outside_rom = false;
                    Ok(CoreReply::Packet("S05".into()))
                }
                Ok(false) => Ok(CoreReply::Packet("E01".into())),
                Err(e) => Err(e),
            }
        } else {
            result
        };
        self.gdb = Some(g);
        match result {
            Err(e) => {
                warn!("gdb: packet {packet:?} failed: {e:#}");
                self.gdb_flush_console();
                self.gdb_send_packet("E01");
            }
            Ok(CoreReply::Packet(reply)) => {
                if packet.starts_with('M') {
                    // The core refreshed its own watch snapshots; the
                    // machine-installed word watches need the same, or
                    // the write itself fires them on the next step
                    // (CCP's memory.write does likewise).
                    self.emu.machine.ui_rebaseline_watches();
                }
                self.gdb_sync_machine_debug_state();
                self.gdb_flush_console();
                self.gdb_send_packet(&reply);
                if packet == "bs" || packet == "bc" {
                    // Reverse execution moved the timeline; refresh the
                    // presentation like the debugger's own transports.
                    self.finish_render_for_current_frame();
                    self.request_redraw();
                }
            }
            Ok(CoreReply::Resume(request)) => {
                // A resume against a machine that cannot run would leave
                // the client waiting on a stop that never comes.
                if !self.powered_on || self.emu.machine.cpu_double_faulted() {
                    if let Some(g) = self.gdb.as_mut() {
                        g.core.console.push(if self.powered_on {
                            "CPU is double-faulted; reset the machine\n".to_string()
                        } else {
                            "machine is powered off\n".to_string()
                        });
                    }
                    self.gdb_flush_console();
                    self.gdb_send_packet("E01");
                    return;
                }
                match request {
                    ResumeRequest::Step => self.gdb_sync_step(),
                    ResumeRequest::Continue => {
                        self.gdb_sync_machine_debug_state();
                        if let Some(g) = self.gdb.as_mut() {
                            g.pending = true;
                        }
                        if self.paused {
                            self.paused = false;
                            self.sync_live_audio_suspension();
                        }
                        self.request_redraw();
                    }
                }
            }
            Ok(CoreReply::Disconnect) => {
                self.gdb_flush_console();
                self.gdb_send_packet("OK");
                // The socket stays with the reader; its EOF follows and
                // repeats this cleanup harmlessly.
                self.gdb_detach_cleanup("GDB client detached");
            }
            Ok(CoreReply::Profile(request)) => {
                let mut g = self.gdb.take().unwrap();
                g.breaks.remove_all(&mut self.emu);
                let reply =
                    match crate::profile::bartman::capture(&mut self.emu, &request, |line| {
                        g.handle.send_console(line);
                        Ok(())
                    }) {
                        Ok(()) => "OK",
                        Err(error) => {
                            g.handle.send_console(&format!("DBG: {error:#}\n"));
                            "E01"
                        }
                    };
                self.gdb = Some(g);
                self.gdb_sync_machine_debug_state();
                self.gdb_send_packet(reply);
                self.finish_render_for_current_frame();
                self.request_redraw();
            }
            Ok(CoreReply::Kill) => {
                // A windowed session outlives its debugger: `k` detaches
                // (VS Code sends it on Stop Debugging; killing the
                // user's window would be hostile). Documented divergence
                // from the headless driver.
                self.gdb_detach_cleanup("GDB kill request: detached (window stays open)");
                if let Some(g) = self.gdb.as_ref() {
                    g.handle.disconnect("kill request");
                }
            }
        }
    }

    /// `s` / `vCont;s`: one instruction, synchronously at the drain,
    /// like the debugger window's own step button.
    fn gdb_sync_step(&mut self) {
        if !self.paused {
            // A step against a free-running machine would race the burst
            // loop; gdb only ever steps a stopped target anyway. A control
            // client's resume that was running the machine ends here.
            self.paused = true;
            self.sync_live_audio_suspension();
            self.complete_remote_resumes("pause", "paused for a gdb step");
        }
        let reply = match self.emu.debug_step_realtime() {
            Err(e) => {
                error!("gdb: step halted the machine: {e:?}");
                self.cpu_halted = true;
                self.sync_live_audio_suspension();
                self.gdb_plain_stop().to_string()
            }
            Ok(()) => match self.emu.machine.take_ui_debug_stop() {
                Some(stop) => self.gdb_map_stop(&stop),
                None => self.gdb_plain_stop().to_string(),
            },
        };
        self.gdb_sync_machine_debug_state();
        self.gdb_flush_console();
        self.gdb_send_packet(&reply);
        self.finish_render_for_current_frame();
        self.request_redraw();
    }

    /// Monitor commands the windowed driver answers itself. Returns the
    /// console output, or None to let the core handle the command.
    fn gdb_monitor_intercept(&mut self, command: &str) -> Option<String> {
        let mut parts = command.split_whitespace();
        let cmd = parts.next()?;
        match cmd {
            "warp" => Some(match parts.next() {
                Some("on") => {
                    let outcome = self.set_warp(true, WarpSource::Gdb);
                    match outcome.note {
                        Some(note) => format!("warp request refused: {note}\n"),
                        None => "warp on (emulation unpaced)\n".to_string(),
                    }
                }
                Some("off") => {
                    let outcome = self.set_warp(false, WarpSource::Gdb);
                    match outcome.note {
                        // Another holder keeps the machine warping.
                        Some(note) => format!("warp off for gdb; {note}\n"),
                        None => "warp off (real-time pacing)\n".to_string(),
                    }
                }
                Some("status") | None => {
                    let held = if self.warp_holds.is_empty() {
                        String::new()
                    } else {
                        format!(" (held by {})", self.warp_holds.describe())
                    };
                    format!(
                        "warp {}{held}\n",
                        if self.emu.paced() {
                            "off (paced)"
                        } else {
                            "on (unpaced)"
                        }
                    )
                }
                Some(_) => "usage: monitor warp on|off|status\n".to_string(),
            }),
            "watch-reg" | "unwatch-reg" => {
                let usage = format!("usage: monitor {cmd} NAME|OFFSET\n");
                let Some(name) = parts.next() else {
                    return Some(usage);
                };
                let Some(off) = crate::debugger::parse_custom_reg(name) else {
                    return Some(format!("unknown custom register {name}\n"));
                };
                let g = self.gdb.as_mut()?;
                if cmd == "watch-reg" {
                    if !g.core.reg_watches.contains(&off) {
                        g.core.reg_watches.push(off);
                    }
                    Some(format!(
                        "watching {} ${off:03X}\n",
                        crate::debugger::custom_reg_name(off)
                    ))
                } else {
                    g.core.reg_watches.retain(|&candidate| candidate != off);
                    Some(format!(
                        "not watching {} ${off:03X}\n",
                        crate::debugger::custom_reg_name(off)
                    ))
                }
            }
            "clear-reg-watches" => {
                let g = self.gdb.as_mut()?;
                g.core.reg_watches.clear();
                Some("cleared custom-register watches\n".to_string())
            }
            _ => None,
        }
    }

    /// Map a machine stop to its RSP stop reply, consuming the one-shot
    /// `--run` target and queueing the loadseg console notices the
    /// headless check_stop prints.
    fn gdb_map_stop(&mut self, stop: &DebugStop) -> String {
        if self.gdb.as_ref().is_some_and(|g| g.core.bartman)
            && !matches!(stop, DebugStop::LoadSeg { .. })
        {
            return match stop {
                DebugStop::Exception { vector: 3, .. } => "S0A",
                DebugStop::Exception { vector: 4, .. } => "S04",
                _ => "S05",
            }
            .into();
        }
        match stop {
            DebugStop::Breakpoint { .. } => "T05hwbreak:;thread:1;".to_string(),
            DebugStop::Watch { addr, access, .. } => {
                let requested = self.gdb.as_ref().and_then(|g| {
                    g.core
                        .watch_access_at(*addr, self.emu.machine.ui_addr_mask())
                });
                // A GUI watch may contribute the other half of a temporary
                // union at this address. Only label the stop as the GDB
                // request when that request includes the access that actually
                // fired; otherwise retain the machine's real access class.
                let access = requested
                    .filter(|requested| match access {
                        crate::debugger::WatchAccess::Read => requested.reads(),
                        crate::debugger::WatchAccess::Write => requested.writes(),
                        crate::debugger::WatchAccess::Access => {
                            *requested == crate::debugger::WatchAccess::Access
                        }
                    })
                    .unwrap_or(*access);
                let key = match access {
                    crate::debugger::WatchAccess::Write => "watch",
                    crate::debugger::WatchAccess::Read => "rwatch",
                    crate::debugger::WatchAccess::Access => "awatch",
                };
                format!("T05{key}:{addr:x};thread:1;")
            }
            DebugStop::LoadSeg { name, addr } => {
                let plain = self.gdb_plain_stop().to_string();
                let Some(g) = self.gdb.as_mut() else {
                    return plain;
                };
                let hint = format!(
                    "first hunk ${addr:06X} (monitor segments / add-symbol-file FILE 0x{addr:X})\n"
                );
                if g.core
                    .run_stop
                    .as_deref()
                    .is_some_and(|target| name.eq_ignore_ascii_case(target))
                {
                    // One-shot: the target rerunning later is ordinary
                    // execution, not a fresh launch.
                    g.core.run_stop = None;
                    g.core
                        .console
                        .push(format!("run target loaded: {name} {hint}"));
                    plain.clone()
                } else if g.core.loadseg_break {
                    g.core.console.push(format!("loadseg: {name} {hint}"));
                    plain.clone()
                } else if g.core.lib_events_armed {
                    "T05library:;thread:1;".to_string()
                } else {
                    plain.clone()
                }
            }
            _ => self.gdb_plain_stop().to_string(),
        }
    }

    /// Hook for `surface_debug_stop`: when a GDB continue is pending,
    /// the stop answers the client instead of commandeering the local
    /// debugger window. Returns whether the stop was consumed remotely.
    pub(super) fn gdb_completes_stop(&mut self, stop: &DebugStop) -> bool {
        if !self.gdb.as_ref().is_some_and(|g| g.pending) {
            return false;
        }
        if let Some(g) = self.gdb.as_mut() {
            g.pending = false;
        }
        let reply = self.gdb_map_stop(stop);
        // The one-shot --run target may have been consumed; the machine
        // loadseg catch narrows or disarms with it.
        self.gdb_sync_machine_debug_state();
        self.gdb_flush_console();
        self.gdb_send_packet(&reply);
        true
    }

    /// A host-side stop with no DebugStop payload (user pause, power
    /// off, double fault) completes a pending continue with a plain stop
    /// reply so the client is not left hanging. Returns whether one was
    /// completed.
    pub(super) fn gdb_complete_pending_stop(&mut self, detail: &str) -> bool {
        if !self.gdb.as_ref().is_some_and(|g| g.pending) {
            return false;
        }
        if let Some(g) = self.gdb.as_mut() {
            g.pending = false;
        }
        if let Some(g) = self.gdb.as_ref() {
            g.handle.send_console(&format!(
                "{}{detail}\n",
                if g.core.bartman { "DBG: " } else { "" }
            ));
        }
        self.gdb_send_packet(self.gdb_plain_stop());
        true
    }

    fn gdb_plain_stop(&self) -> &'static str {
        if self.gdb.as_ref().is_some_and(|g| g.core.bartman) {
            "S05"
        } else {
            "T05thread:1;"
        }
    }

    fn gdb_send_packet(&self, payload: &str) {
        if let Some(g) = &self.gdb {
            g.handle.send_packet(payload);
        }
    }

    pub(super) fn gdb_log_lines(&self, lines: &[String]) {
        if let Some(g) = self.gdb.as_ref().filter(|g| g.core.bartman) {
            for text in lines {
                for line in text.lines() {
                    g.handle.send_console(&format!("DBG: {line}\n"));
                }
            }
        }
    }

    /// Deliver the core's queued console lines as `O` packets, in order,
    /// before whatever reply follows (the headless wire order).
    fn gdb_flush_console(&mut self) {
        let lines = match self.gdb.as_mut() {
            Some(g) => g.core.take_console(),
            None => return,
        };
        if let Some(g) = self.gdb.as_ref() {
            for chunk in lines {
                g.handle.send_console(&chunk);
            }
        }
    }

    /// Translate the core's session state into machine-installed debug
    /// state after any packet that may have changed it.
    fn gdb_sync_machine_debug_state(&mut self) {
        let Some(mut g) = self.gdb.take() else {
            return;
        };
        let notes = g.breaks.sync(&mut self.emu, &g.core);
        g.core.console.extend(notes);
        self.gdb = Some(g);
    }
}
