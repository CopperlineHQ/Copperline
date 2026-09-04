// SPDX-License-Identifier: GPL-3.0-or-later

//! The transport-free core of the GDB stub: packet semantics against the
//! machine, with no socket and no ownership of the emulator. The headless
//! driver ([`super::headless`]) runs it from a blocking loop; a windowed
//! driver can run the same core from a frame-boundary drain.

use crate::debugger::{custom_reg_name, parse_custom_reg};
use crate::emulator::Emulator;
use crate::timetravel::ReverseOutcome;
use anyhow::{anyhow, Context, Result};

/// Instruction budget for the `monitor stepover` / `monitor finish` helpers, so
/// a call or subroutine that never returns cannot wedge the debug server.
const MONITOR_STEP_BUDGET: usize = 5_000_000;
/// Machine word watches installed per one GDB connection.
pub(crate) const GDB_WATCH_WORD_CAP: usize = 8;

pub(crate) const TARGET_XML: &str = r#"<?xml version="1.0"?>
<target>
  <architecture>m68k</architecture>
  <feature name="org.gnu.gdb.m68k.core">
    <reg name="d0" bitsize="32" regnum="0"/>
    <reg name="d1" bitsize="32" regnum="1"/>
    <reg name="d2" bitsize="32" regnum="2"/>
    <reg name="d3" bitsize="32" regnum="3"/>
    <reg name="d4" bitsize="32" regnum="4"/>
    <reg name="d5" bitsize="32" regnum="5"/>
    <reg name="d6" bitsize="32" regnum="6"/>
    <reg name="d7" bitsize="32" regnum="7"/>
    <reg name="a0" bitsize="32" regnum="8"/>
    <reg name="a1" bitsize="32" regnum="9"/>
    <reg name="a2" bitsize="32" regnum="10"/>
    <reg name="a3" bitsize="32" regnum="11"/>
    <reg name="a4" bitsize="32" regnum="12"/>
    <reg name="a5" bitsize="32" regnum="13"/>
    <reg name="fp" bitsize="32" regnum="14"/>
    <reg name="sp" bitsize="32" regnum="15" type="data_ptr"/>
    <reg name="ps" bitsize="32" regnum="16"/>
    <reg name="pc" bitsize="32" regnum="17" type="code_ptr"/>
  </feature>
</target>
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Watchpoint {
    pub(crate) addr: u32,
    pub(crate) len: usize,
    pub(crate) access: crate::debugger::WatchAccess,
    last: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StopReason {
    Attached,
    Exception(u16),
    Step,
    Breakpoint,
    Watchpoint(u32, crate::debugger::WatchAccess),
    RegisterWatch,
    BeamTrap,
    CopperBreak,
    Reverse,
    Interrupted,
    /// A new program was LoadSeg'd; gdb re-reads the library list,
    /// binds pending breakpoints, and resumes on its own.
    LibraryLoad,
    /// Same trigger, requested via `monitor loadseg-break`: a plain
    /// user-visible stop.
    LoadSeg,
}

/// The transport-free half of a GDB session: every packet's semantics
/// against the machine, with no socket and no ownership of the
/// [`Emulator`]. The headless [`Session`] drives it from its blocking
/// loop; a windowed driver can drive the same core from a frame-boundary
/// drain. Console output (qRcmd results, loadseg notices) accumulates as
/// data for the driver to deliver as `O` packets.
pub(crate) struct GdbCore {
    pub(crate) bartman: bool,
    pub(crate) outside_rom: bool,
    bartman_entry: Option<Vec<u8>>,
    pub(crate) breakpoints: Vec<u32>,
    pub(crate) watchpoints: Vec<Watchpoint>,
    pub(crate) reg_watches: Vec<u16>,
    pub(crate) stop: StopReason,
    pub(crate) cpu_idle: bool,
    /// True once the client has fetched qXfer:libraries:read: from then
    /// on new LoadSegs stop with a `library:` event.
    pub(crate) lib_events_armed: bool,
    /// `monitor loadseg-break`: new LoadSegs cause a user-visible stop.
    pub(crate) loadseg_break: bool,
    /// `--run` + `--gdb`: one-shot stop when a program with this name is
    /// loaded (case-insensitive), before its first instruction.
    pub(crate) run_stop: Option<String>,
    pub(crate) tracker: crate::amigaos::LibraryTracker,
    /// Library-list XML cached at offset 0 so multi-chunk reads are
    /// self-consistent.
    pub(crate) libraries_xml: String,
    /// Console lines awaiting delivery as `O` packets.
    pub(crate) console: Vec<String>,
}

/// What the core wants the driver to do with a packet's outcome.
#[derive(Debug)]
pub(crate) enum CoreReply {
    /// Send this reply (after flushing any queued console output).
    Packet(String),
    /// The client asked the machine to run; the driver owns how, and the
    /// eventual stop reply.
    Resume(ResumeRequest),
    /// `D`: acknowledge and keep serving.
    Disconnect,
    /// `k`.
    Kill,
    Profile(crate::profile::bartman::Request),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResumeRequest {
    Step,
    Continue,
}

/// Hard cap on a single packet's payload, well above the `PacketSize=4000`
/// this stub advertises in qSupported but far short of unbounded: this is
/// network-facing input (the stub can bind non-loopback), so a peer that
/// never sends the `#` terminator must not be able to grow `payload`
/// without limit.
pub(crate) const MAX_PACKET_PAYLOAD_BYTES: usize = 1 << 20;

impl GdbCore {
    pub(crate) fn new(emu: &Emulator, run_stop: Option<String>) -> Self {
        let mut tracker = crate::amigaos::LibraryTracker::default();
        if run_stop.is_some() {
            // Arm now so a program already running when the client attaches
            // is absorbed rather than reported. Pre-boot this is a no-op
            // (exec is not up yet) and arm() is idempotent, so the later
            // qXfer arming path is unaffected.
            crate::amigaos::with_bus_memory(emu.bus(), |os| tracker.arm(os));
        }
        Self {
            bartman: false,
            outside_rom: false,
            bartman_entry: None,
            breakpoints: Vec::new(),
            watchpoints: Vec::new(),
            reg_watches: Vec::new(),
            stop: StopReason::Attached,
            cpu_idle: false,
            lib_events_armed: false,
            loadseg_break: false,
            run_stop,
            tracker,
            libraries_xml: String::new(),
            console: Vec::new(),
        }
    }

    /// The console lines queued since the last take, in emission order.
    pub(crate) fn take_console(&mut self) -> Vec<String> {
        let lines = std::mem::take(&mut self.console);
        if self.bartman {
            lines
                .into_iter()
                .map(|text| text.lines().map(|line| format!("DBG: {line}\n")).collect())
                .collect()
        } else {
            lines
        }
    }

    /// One machine step on the driver's behalf (the continue loop, and
    /// anything else that advances one slice at a time).
    pub(crate) fn step_once(&mut self, emu: &mut Emulator) -> Result<()> {
        emu.debug_step_for_gdb(&mut self.cpu_idle)
    }

    pub(crate) fn handle_packet(&mut self, emu: &mut Emulator, packet: &str) -> Result<CoreReply> {
        // GDB can read and cache registers before asking qOffsets. Complete
        // the launch at the initial stop query so PC and relocation describe
        // the same state. No asynchronous console packets accompany this query.
        if self.bartman
            && packet == "?"
            && self.run_stop.is_some()
            && self.query_offsets(emu) == "E01"
        {
            return Ok(CoreReply::Packet("E01".into()));
        }
        let reply = match packet {
            "!" => "OK".to_string(),
            "?" => self.stop_reply(),
            "g" => self.read_all_registers(emu),
            "qC" => "QC1".to_string(),
            "qAttached" | "qAttached:1" => "1".to_string(),
            "qfThreadInfo" => "m1".to_string(),
            "qsThreadInfo" => "l".to_string(),
            "vCont?" if self.bartman => "vCont;c;C;s;S".to_string(),
            "vCont?" => "vCont;c;s".to_string(),
            "D" | "D;1" => return Ok(CoreReply::Disconnect),
            "k" => return Ok(if self.bartman { CoreReply::Disconnect } else { CoreReply::Kill }),
            _ if packet.starts_with("qSupported") && self.bartman => {
                "PacketSize=4000;QStartNoAckMode+;swbreak+;hwbreak+;vContSupported+".to_string()
            }
            _ if packet.starts_with("qSupported") => {
                "PacketSize=4000;QStartNoAckMode+;qXfer:features:read+;qXfer:libraries:read+;hwbreak+;ReverseStep+;ReverseContinue+".to_string()
            }
            _ if packet.starts_with("qXfer:features:read:target.xml:") => {
                self.read_target_xml(packet)?
            }
            _ if packet.starts_with("qXfer:libraries:read::") => {
                self.read_libraries_xml(emu, packet)?
            }
            _ if packet.starts_with("qOffsets") => self.query_offsets(emu),
            _ if packet.starts_with("qRcmd,") => {
                let command = String::from_utf8(hex_decode(&packet[6..])?)
                    .context("decoding monitor command")?;
                if self.bartman && command.split_whitespace().next() == Some("profile") {
                    return Ok(CoreReply::Profile(crate::profile::bartman::Request::parse(&command)?));
                }
                let output = self.handle_monitor(emu, command.trim())?;
                self.console.push(output);
                "OK".to_string()
            }
            _ if packet.starts_with('H') => "OK".to_string(),
            _ if packet.starts_with('T') => "OK".to_string(),
            _ if packet.starts_with('p') => self.read_register(emu, &packet[1..])?,
            _ if packet.starts_with('P') => self.write_register(emu, &packet[1..])?,
            _ if packet.starts_with('m') => self.read_memory(emu, &packet[1..])?,
            _ if packet.starts_with('M') => self.write_memory(emu, &packet[1..])?,
            _ if packet.starts_with("Z0,") || packet.starts_with("Z1,") => {
                self.add_breakpoint(emu, packet)?
            }
            _ if packet.starts_with("z0,") || packet.starts_with("z1,") => {
                self.remove_breakpoint(emu, packet)?
            }
            _ if packet.starts_with("Z2,") || packet.starts_with("Z3,") || packet.starts_with("Z4,") => {
                self.add_watchpoint(emu, packet)?
            }
            _ if packet.starts_with("z2,") || packet.starts_with("z3,") || packet.starts_with("z4,") => {
                self.remove_watchpoint(emu, packet)?
            }
            _ if self.bartman && (packet.starts_with('C') || packet.starts_with('S')) => {
                let (signal, address) = packet[1..].split_once(';').map_or(
                    (&packet[1..], None), |(signal, address)| (signal, Some(address)),
                );
                validate_resume_signal(signal)?;
                if let Some(address) = address {
                    emu.machine.debug_set_register(17, parse_hex_u32(address)?);
                }
                return Ok(CoreReply::Resume(if packet.starts_with('S') {
                    ResumeRequest::Step
                } else {
                    ResumeRequest::Continue
                }));
            }
            _ if self.bartman && (packet.starts_with("vCont;C") || packet.starts_with("vCont;S")) => {
                let action = packet[6..].split(';').next().unwrap();
                let signal = action[1..].split(':').next().unwrap();
                validate_resume_signal(signal)?;
                return Ok(CoreReply::Resume(if action.starts_with('S') {
                    ResumeRequest::Step
                } else {
                    ResumeRequest::Continue
                }));
            }
            _ if packet.starts_with('s') => {
                if let Some(addr) = packet.strip_prefix('s').filter(|s| !s.is_empty()) {
                    let pc = parse_hex_u32(addr)?;
                    emu.machine.debug_set_register(17, pc);
                }
                return Ok(CoreReply::Resume(ResumeRequest::Step));
            }
            _ if packet.starts_with('c') => {
                if let Some(addr) = packet.strip_prefix('c').filter(|s| !s.is_empty()) {
                    let pc = parse_hex_u32(addr)?;
                    emu.machine.debug_set_register(17, pc);
                }
                return Ok(CoreReply::Resume(ResumeRequest::Continue));
            }
            _ if packet.starts_with("vCont;c") => {
                return Ok(CoreReply::Resume(ResumeRequest::Continue));
            }
            _ if packet.starts_with("vCont;s") => {
                return Ok(CoreReply::Resume(ResumeRequest::Step));
            }
            "bs" => self.reverse_step(emu)?,
            "bc" => self.reverse_continue(emu)?,
            _ => String::new(),
        };
        Ok(CoreReply::Packet(reply))
    }

    pub(crate) fn stop_reply(&self) -> String {
        if self.bartman {
            return match self.stop {
                StopReason::Exception(3) => "S0A",
                StopReason::Exception(4) => "S04",
                _ => "S05",
            }
            .to_string();
        }
        match &self.stop {
            StopReason::Watchpoint(addr, access) => {
                let key = match access {
                    crate::debugger::WatchAccess::Write => "watch",
                    crate::debugger::WatchAccess::Read => "rwatch",
                    crate::debugger::WatchAccess::Access => "awatch",
                };
                format!("T05{key}:{addr:x};thread:1;")
            }
            StopReason::RegisterWatch => "T05thread:1;".to_string(),
            StopReason::Breakpoint => "T05hwbreak:;thread:1;".to_string(),
            StopReason::LibraryLoad => "T05library:;thread:1;".to_string(),
            _ => "T05thread:1;".to_string(),
        }
    }

    fn read_all_registers(&self, emu: &Emulator) -> String {
        let mut out = String::with_capacity(18 * 8);
        for reg in 0..18 {
            let value = emu.machine.debug_register(reg).unwrap_or(0);
            out.push_str(&format!("{value:08x}"));
        }
        out
    }

    fn read_register(&self, emu: &Emulator, reg: &str) -> Result<String> {
        let reg = parse_hex_usize(reg)?;
        Ok(match emu.machine.debug_register(reg) {
            Some(value) => format!("{value:08x}"),
            None => "E00".to_string(),
        })
    }

    fn write_register(&mut self, emu: &mut Emulator, payload: &str) -> Result<String> {
        let Some((reg_s, value_s)) = payload.split_once('=') else {
            return Ok("E01".to_string());
        };
        let reg = parse_hex_usize(reg_s)?;
        let bytes = hex_decode(value_s)?;
        let mut value = 0u32;
        for byte in bytes.iter().take(4) {
            value = (value << 8) | u32::from(*byte);
        }
        Ok(if emu.machine.debug_set_register(reg, value) {
            emu.machine.refresh_irq_line();
            "OK".to_string()
        } else {
            "E00".to_string()
        })
    }

    fn read_memory(&self, emu: &Emulator, payload: &str) -> Result<String> {
        let Some((addr_s, len_s)) = payload.split_once(',') else {
            return Ok("E01".to_string());
        };
        let addr = parse_hex_u32(addr_s)? & emu.machine.ui_addr_mask();
        let len = parse_hex_usize(len_s)?;
        // `len` is a hex value the peer controls directly (independent of
        // the packet's own byte length), so a tiny packet like "m0,ffffffff"
        // could otherwise demand a multi-GB allocation. No real GDB request
        // needs more than a small fraction of the address space at once.
        if len > MAX_PACKET_PAYLOAD_BYTES {
            return Ok("E01".to_string());
        }
        if self.bartman && addr >= 0x00df_f000 && u64::from(addr) + len as u64 <= 0x00df_f200 {
            let bytes: Vec<_> = (0..len)
                .map(|i| {
                    let offset = ((addr + i as u32) & 0x1ff) as u16;
                    let word = emu.bus().debug_custom_word(offset & !1).unwrap_or(0);
                    if offset & 1 == 0 {
                        (word >> 8) as u8
                    } else {
                        word as u8
                    }
                })
                .collect();
            return Ok(hex_encode(&bytes));
        }
        Ok(hex_encode(&emu.machine.debug_read_memory(addr, len)))
    }

    fn write_memory(&mut self, emu: &mut Emulator, payload: &str) -> Result<String> {
        let Some((range, data_s)) = payload.split_once(':') else {
            return Ok("E01".to_string());
        };
        let Some((addr_s, len_s)) = range.split_once(',') else {
            return Ok("E01".to_string());
        };
        let addr = parse_hex_u32(addr_s)? & emu.machine.ui_addr_mask();
        let len = parse_hex_usize(len_s)?;
        let data = hex_decode(data_s)?;
        if data.len() != len {
            return Ok("E02".to_string());
        }
        let written = emu.machine.debug_write_memory(addr, &data);
        self.refresh_watchpoints(emu);
        Ok(if written == len {
            "OK".to_string()
        } else {
            "E03".to_string()
        })
    }

    pub(crate) fn add_breakpoint_addr(&mut self, addr: u32) {
        if !self.breakpoints.contains(&addr) {
            self.breakpoints.push(addr);
        }
    }

    pub(crate) fn remove_breakpoint_addr(&mut self, addr: u32) {
        self.breakpoints.retain(|&candidate| candidate != addr);
    }

    fn add_breakpoint(&mut self, emu: &Emulator, packet: &str) -> Result<String> {
        let (addr, _) = parse_z_packet(packet)?;
        if self.bartman && addr == u32::MAX {
            self.outside_rom = true;
        } else {
            self.add_breakpoint_addr(addr & emu.machine.ui_addr_mask());
        }
        Ok("OK".to_string())
    }

    fn remove_breakpoint(&mut self, emu: &Emulator, packet: &str) -> Result<String> {
        let (addr, _) = parse_z_packet(packet)?;
        if self.bartman && addr == u32::MAX {
            self.outside_rom = false;
        } else {
            self.remove_breakpoint_addr(addr & emu.machine.ui_addr_mask());
        }
        Ok("OK".to_string())
    }

    fn add_watchpoint(&mut self, emu: &Emulator, packet: &str) -> Result<String> {
        let (addr, len) = parse_z_packet(packet)?;
        let access = watch_access_from_packet(packet)?;
        let addr = addr & emu.machine.ui_addr_mask();
        let len = len.max(1);
        let last = emu.machine.debug_read_memory(addr, len);
        if let Some(existing) = self
            .watchpoints
            .iter_mut()
            .find(|w| w.addr == addr && w.len == len && w.access == access)
        {
            existing.last = last;
        } else {
            self.watchpoints.push(Watchpoint {
                addr,
                len,
                access,
                last,
            });
        }
        Ok("OK".to_string())
    }

    fn remove_watchpoint(&mut self, emu: &Emulator, packet: &str) -> Result<String> {
        let (addr, len) = parse_z_packet(packet)?;
        let access = watch_access_from_packet(packet)?;
        let addr = addr & emu.machine.ui_addr_mask();
        self.watchpoints.retain(|watch| {
            watch.addr != addr || watch.len != len.max(1) || watch.access != access
        });
        Ok("OK".to_string())
    }

    pub(crate) fn step_forward(&mut self, emu: &mut Emulator) -> Result<String> {
        self.stop = StopReason::Step;
        emu.debug_step_for_gdb(&mut self.cpu_idle)?;
        if let Some(stop) = self.check_stop(emu)? {
            self.stop = stop;
        }
        Ok(self.stop_reply())
    }

    fn reverse_step(&mut self, emu: &mut Emulator) -> Result<String> {
        match emu.tt_reverse_step(1)? {
            ReverseOutcome::Found(_) => {
                self.cpu_idle = false;
                self.refresh_watchpoints(emu);
                self.stop = StopReason::Reverse;
                Ok(self.stop_reply())
            }
            ReverseOutcome::NotFound | ReverseOutcome::BeyondHistory => Ok("E01".to_string()),
        }
    }

    fn reverse_continue(&mut self, emu: &mut Emulator) -> Result<String> {
        match emu.tt_reverse_continue_to(&self.breakpoints)? {
            ReverseOutcome::Found(_) => {
                self.cpu_idle = false;
                self.refresh_watchpoints(emu);
                self.stop = StopReason::Breakpoint;
                Ok(self.stop_reply())
            }
            ReverseOutcome::NotFound | ReverseOutcome::BeyondHistory => Ok("E01".to_string()),
        }
    }

    pub(crate) fn check_stop(&mut self, emu: &mut Emulator) -> Result<Option<StopReason>> {
        if let Some(stop) = emu.machine.take_ui_debug_stop() {
            match stop {
                crate::debugger::DebugStop::Watch { addr, access, .. } => {
                    let access = self
                        .watch_access_at(addr, emu.machine.ui_addr_mask())
                        .unwrap_or(access);
                    self.refresh_watchpoints(emu);
                    return Ok(Some(StopReason::Watchpoint(addr, access)));
                }
                crate::debugger::DebugStop::Exception { vector, .. } => {
                    return Ok(Some(StopReason::Exception(vector)))
                }
                _ => {}
            }
        }
        if self.outside_rom
            && !(crate::memory::ROM_BASE as u32..=0x00ff_ffff).contains(&emu.machine.pc())
        {
            self.outside_rom = false;
            return Ok(Some(StopReason::Breakpoint));
        }
        if emu.bus_mut().take_ui_reg_hit().is_some() {
            return Ok(Some(StopReason::RegisterWatch));
        }
        if emu.bus_mut().take_ui_beam_hit().is_some() {
            return Ok(Some(StopReason::BeamTrap));
        }
        if emu.bus_mut().take_ui_copper_hit().is_some() {
            return Ok(Some(StopReason::CopperBreak));
        }
        let pc = emu.machine.pc() & emu.machine.ui_addr_mask();
        if self.breakpoints.contains(&pc) {
            return Ok(Some(StopReason::Breakpoint));
        }
        for watch in &mut self.watchpoints {
            if !watch.access.writes() {
                continue;
            }
            let cur = emu.machine.debug_read_memory(watch.addr, watch.len);
            if cur != watch.last {
                watch.last = cur;
                return Ok(Some(StopReason::Watchpoint(watch.addr, watch.access)));
            }
        }
        if self.lib_events_armed || self.loadseg_break || self.run_stop.is_some() {
            let tracker = &mut self.tracker;
            let event = crate::amigaos::with_bus_memory(emu.bus(), |os| {
                tracker.observe(os).map(|module| {
                    let first = module.segments.first().map_or(0, |seg| seg.start);
                    (module.name.clone(), first)
                })
            });
            if let Some((name, first_hunk)) = event {
                if self
                    .run_stop
                    .as_deref()
                    .is_some_and(|target| name.eq_ignore_ascii_case(target))
                {
                    // One-shot: the target rerunning later is ordinary
                    // execution, not a fresh launch.
                    self.run_stop = None;
                    self.console.push(format!(
                        "run target loaded: {name} first hunk ${first_hunk:06X} \
                         (monitor segments / add-symbol-file FILE 0x{first_hunk:X})\n"
                    ));
                    return Ok(Some(StopReason::LoadSeg));
                }
                if self.loadseg_break {
                    self.console.push(format!(
                        "loadseg: {name} first hunk ${first_hunk:06X} \
                         (monitor segments / add-symbol-file FILE 0x{first_hunk:X})\n"
                    ));
                    return Ok(Some(StopReason::LoadSeg));
                }
                if self.lib_events_armed {
                    return Ok(Some(StopReason::LibraryLoad));
                }
                // Only the stop-on-load target is armed and this was some
                // other program: keep running.
            }
        }
        Ok(None)
    }

    /// Expand byte-range GDB watchpoints into the machine's word-granular
    /// watch store. Overlapping read and write requests become one access
    /// watch instead of toggling the same machine word twice.
    pub(crate) fn machine_watch_words(
        &self,
        mask: u32,
    ) -> (Vec<(u32, crate::debugger::WatchAccess)>, bool) {
        let mut words: Vec<(u32, crate::debugger::WatchAccess)> = Vec::new();
        let mut truncated = false;
        'watches: for watch in &self.watchpoints {
            let start = watch.addr & mask & !1;
            let last = watch.addr.wrapping_add(watch.len.max(1) as u32 - 1) & mask & !1;
            let mut addr = start;
            loop {
                if let Some((_, access)) = words.iter_mut().find(|(word, _)| *word == addr) {
                    *access = access.union(watch.access);
                } else if words.len() == GDB_WATCH_WORD_CAP {
                    truncated = true;
                    break 'watches;
                } else {
                    words.push((addr, watch.access));
                }
                if addr == last {
                    break;
                }
                addr = addr.wrapping_add(2) & mask;
            }
        }
        (words, truncated)
    }

    pub(crate) fn watch_access_at(
        &self,
        addr: u32,
        mask: u32,
    ) -> Option<crate::debugger::WatchAccess> {
        self.machine_watch_words(mask)
            .0
            .into_iter()
            .find_map(|(word, access)| (word == addr).then_some(access))
    }

    fn refresh_watchpoints(&mut self, emu: &Emulator) {
        for watch in &mut self.watchpoints {
            watch.last = emu.machine.debug_read_memory(watch.addr, watch.len);
        }
    }

    fn read_target_xml(&self, packet: &str) -> Result<String> {
        let Some((_, range)) = packet.rsplit_once(':') else {
            return Ok("E01".to_string());
        };
        xml_chunk_reply(TARGET_XML, range)
    }

    /// qXfer:libraries:read: the programs LoadSeg has scattered through
    /// RAM, as gdb's library-list XML. Fetching it arms LoadSeg
    /// detection, so clients that never ask see no behavior change.
    fn read_libraries_xml(&mut self, emu: &Emulator, packet: &str) -> Result<String> {
        let Some(range) = packet.strip_prefix("qXfer:libraries:read::") else {
            return Ok("E01".to_string());
        };
        let offset = range
            .split_once(',')
            .map(|(offset_s, _)| parse_hex_usize(offset_s))
            .transpose()?;
        // Regenerate only at offset 0 so a multi-chunk read stays
        // self-consistent.
        if offset == Some(0) {
            let tracker = &mut self.tracker;
            crate::amigaos::with_bus_memory(emu.bus(), |os| {
                tracker.arm(os);
                tracker.absorb_current(os);
            });
            self.lib_events_armed = true;
            self.libraries_xml = build_library_list_xml(self.tracker.modules());
        }
        xml_chunk_reply(&self.libraries_xml, range)
    }

    /// qOffsets: relocate the debugged executable's sections to where
    /// LoadSeg put them. TextSeg is the first hunk; a second hunk (the
    /// usual data hunk of an amiga-gcc build) becomes DataSeg. Empty
    /// reply (packet unsupported) when no process seglist is walkable,
    /// so plain ROM-level sessions are unaffected.
    fn query_offsets(&mut self, emu: &mut Emulator) -> String {
        // Their patched GDB queries offsets during attach, before its first
        // continue. Complete --run's load handshake before answering it.
        if self.bartman && self.run_stop.is_some() {
            let console_len = self.console.len();
            let limit = emu.bus().emulated_frames().saturating_add(6000);
            while self.run_stop.is_some() && emu.bus().emulated_frames() < limit {
                if self.step_once(emu).is_err() || emu.machine.cpu_double_faulted() {
                    return "E01".into();
                }
                match self.check_stop(emu) {
                    Ok(Some(StopReason::LoadSeg)) => {
                        // qOffsets is a plain query. Patched GDB interprets an
                        // O packet here as the offsets and rejects its 'O'.
                        self.console.truncate(console_len);
                        break;
                    }
                    Ok(Some(StopReason::Exception(_))) | Err(_) => return "E01".into(),
                    _ => {}
                }
            }
            if self.run_stop.is_some() {
                return "E01".into();
            }
            match emu.save_state_bytes() {
                Ok(state) => self.bartman_entry = Some(state),
                Err(error) => {
                    log::warn!("gdb: could not save Bartman restart state: {error:#}");
                    return "E01".into();
                }
            }
        }
        match crate::amigaos::segments_on_bus(emu.bus()) {
            Ok(segs) if !segs.is_empty() => {
                if self.bartman {
                    return segs
                        .iter()
                        .map(|s| format!("{:08x}", s.start))
                        .collect::<Vec<_>>()
                        .join(";");
                }
                let mut reply = format!("TextSeg={:X}", segs[0].start);
                if let Some(data) = segs.get(1) {
                    reply.push_str(&format!(";DataSeg={:X}", data.start));
                }
                reply
            }
            _ => String::new(),
        }
    }

    fn monitor_segments(&self, emu: &Emulator) -> String {
        match crate::amigaos::segments_on_bus(emu.bus()) {
            Err(reason) => format!("{reason}\n"),
            Ok(segs) if segs.is_empty() => {
                "current task has no walkable segment list\n".to_string()
            }
            Ok(segs) => {
                let mut out = String::new();
                for (i, seg) in segs.iter().enumerate() {
                    out.push_str(&format!(
                        "hunk {i}: {:06X}..{:06X}  ({} bytes)\n",
                        seg.start,
                        seg.start + seg.size,
                        seg.size
                    ));
                }
                out
            }
        }
    }

    /// Run one of the shared exec dumps against a validated ExecBase and
    /// join it into a monitor reply, or report why the OS is not
    /// walkable. The `!` an error line carries in the console is dropped
    /// here: gdb prints monitor output verbatim.
    fn monitor_os(
        &self,
        emu: &Emulator,
        dump: impl FnOnce(&crate::amigaos::OsMemory, u32) -> Vec<String>,
    ) -> String {
        let lines = crate::amigaos::with_bus_memory(emu.bus(), |os| match os.exec_base() {
            Ok(base) => dump(os, base),
            Err(reason) => vec![reason],
        });
        lines
            .iter()
            .map(|line| format!("{}\n", line.strip_prefix('!').unwrap_or(line)))
            .collect()
    }

    fn monitor_loadseg_list(&mut self, emu: &Emulator) -> String {
        // Fold in the current process so the list is useful even before
        // any load event fired. Arming the tracker here does not enable
        // stops; those are gated on lib_events_armed / loadseg_break.
        let tracker = &mut self.tracker;
        crate::amigaos::with_bus_memory(emu.bus(), |os| {
            tracker.arm(os);
            tracker.absorb_current(os);
        });
        if self.tracker.modules().is_empty() {
            return "no tracked program loads (no walkable process seglist yet)\n".to_string();
        }
        let mut out = String::new();
        for module in self.tracker.modules() {
            out.push_str(&format!("{}:", module.name));
            for seg in &module.segments {
                out.push_str(&format!(" ${:06X} ({} bytes)", seg.start, seg.size));
            }
            out.push('\n');
        }
        out
    }

    pub(crate) fn handle_monitor(&mut self, emu: &mut Emulator, command: &str) -> Result<String> {
        let mut parts = command.split_whitespace();
        let Some(cmd) = parts.next() else {
            return Ok(monitor_help());
        };
        match cmd {
            "help" => Ok(monitor_help()),
            "reset" => {
                if let Some(state) = self.bartman_entry.as_ref().filter(|_| self.bartman) {
                    emu.load_state_bytes(state)?;
                } else {
                    emu.keyboard_reset()?;
                }
                self.cpu_idle = false;
                self.outside_rom = false;
                self.stop = StopReason::Attached;
                self.refresh_watchpoints(emu);
                Ok("machine reset\n".into())
            }
            "status" => Ok(self.monitor_status(emu)),
            "beam" => Ok(format!(
                "beam vpos={} hpos={} frame={} cck={} pos={}\n",
                emu.bus().agnus.vpos,
                emu.bus().agnus.hpos,
                emu.bus().emulated_frames(),
                emu.bus().emulated_cck(),
                emu.retired_instructions()
            )),
            "custom" => Ok(self.monitor_custom(emu)),
            "segments" => Ok(self.monitor_segments(emu)),
            "who" => {
                let Some(addr) = parts.next() else {
                    return Ok("usage: monitor who ADDR\n".to_string());
                };
                let addr = parse_hex_u32(addr)?;
                let snapshot = crate::amigaos::symbols::snapshot_on_bus(emu.bus());
                Ok(match snapshot.resolve(addr) {
                    Some(symbol) => format!(
                        "${addr:08X} {} (symbol ${:08X})\n",
                        symbol.display_name(),
                        symbol.address
                    ),
                    None if snapshot.is_rom_address(addr) => {
                        format!("${addr:08X} ROM (no named resident or live LVO)\n")
                    }
                    None => format!("${addr:08X} no live AmigaOS symbol\n"),
                })
            }
            "execbase" => Ok(self.monitor_os(emu, crate::amigaos::dump::exec)),
            "tasks" => Ok(self.monitor_os(emu, crate::amigaos::dump::task_list)),
            "task" => {
                let spec = parts.collect::<Vec<_>>().join(" ");
                let sp = crate::amigaos::dump::LiveSp {
                    a7: emu.machine.a(7),
                    usp: emu.machine.usp(),
                };
                Ok(self.monitor_os(emu, |os, base| {
                    crate::amigaos::dump::task(os, base, &spec, sp)
                }))
            }
            "memlist" => Ok(self.monitor_os(emu, crate::amigaos::dump::memory)),
            "loadseg-break" => {
                self.loadseg_break = !self.loadseg_break;
                if self.loadseg_break {
                    let tracker = &mut self.tracker;
                    crate::amigaos::with_bus_memory(emu.bus(), |os| tracker.arm(os));
                    Ok(
                        "loadseg break armed: continue stops when a new program is loaded\n"
                            .to_string(),
                    )
                } else {
                    Ok("loadseg break disarmed\n".to_string())
                }
            }
            "loadseg-list" => Ok(self.monitor_loadseg_list(emu)),
            "stepover" => {
                // Step over a BSR/JSR/TRAP call (single step otherwise),
                // bounded so a call that never returns cannot hang the server.
                self.cpu_idle = false;
                emu.debug_step_over(MONITOR_STEP_BUDGET)?;
                self.refresh_watchpoints(emu);
                Ok(format!("pc=${:08X}\n", emu.machine.pc()))
            }
            "finish" => {
                // Run until the current subroutine returns to its caller.
                self.cpu_idle = false;
                emu.debug_step_out(MONITOR_STEP_BUDGET)?;
                self.refresh_watchpoints(emu);
                Ok(format!("pc=${:08X}\n", emu.machine.pc()))
            }
            "return-to-program" => {
                self.cpu_idle = false;
                let reached = emu.debug_run_until_pc_outside(
                    crate::memory::ROM_BASE as u32,
                    0x00FF_FFFF,
                    MONITOR_STEP_BUDGET,
                )?;
                self.refresh_watchpoints(emu);
                Ok(format!(
                    "pc=${:08X}{}\n",
                    emu.machine.pc(),
                    if reached { "" } else { " (not reached)" }
                ))
            }
            "reg" => {
                let Some(name) = parts.next() else {
                    return Ok("usage: monitor reg NAME|OFFSET\n".to_string());
                };
                let Some(off) = parse_custom_reg(name) else {
                    return Ok(format!("unknown custom register {name}\n"));
                };
                Ok(self.monitor_reg(emu, off))
            }
            "write-reg" => {
                let Some(name) = parts.next() else {
                    return Ok("usage: monitor write-reg NAME|OFFSET VALUE\n".to_string());
                };
                let Some(value_s) = parts.next() else {
                    return Ok("usage: monitor write-reg NAME|OFFSET VALUE\n".to_string());
                };
                let Some(off) = parse_custom_reg(name) else {
                    return Ok(format!("unknown custom register {name}\n"));
                };
                let value = parse_hex_u16(value_s)?;
                let irq = emu
                    .bus_mut()
                    .custom_write(u64::from(off), 2, u64::from(value));
                if irq {
                    emu.machine.refresh_irq_line();
                }
                Ok(format!(
                    "{} ${off:03X} <- ${value:04X}\n",
                    custom_reg_name(off)
                ))
            }
            "watch-reg" => {
                let Some(name) = parts.next() else {
                    return Ok("usage: monitor watch-reg NAME|OFFSET\n".to_string());
                };
                let Some(off) = parse_custom_reg(name) else {
                    return Ok(format!("unknown custom register {name}\n"));
                };
                if !self.reg_watches.contains(&off) {
                    self.reg_watches.push(off);
                    emu.bus_mut().set_ui_reg_watches(&self.reg_watches);
                }
                Ok(format!("watching {} ${off:03X}\n", custom_reg_name(off)))
            }
            "unwatch-reg" => {
                let Some(name) = parts.next() else {
                    return Ok("usage: monitor unwatch-reg NAME|OFFSET\n".to_string());
                };
                let Some(off) = parse_custom_reg(name) else {
                    return Ok(format!("unknown custom register {name}\n"));
                };
                self.reg_watches.retain(|&candidate| candidate != off);
                emu.bus_mut().set_ui_reg_watches(&self.reg_watches);
                Ok(format!(
                    "not watching {} ${off:03X}\n",
                    custom_reg_name(off)
                ))
            }
            "clear-reg-watches" => {
                self.reg_watches.clear();
                emu.bus_mut().set_ui_reg_watches(&[]);
                Ok("cleared custom-register watches\n".to_string())
            }
            "beam-trap" => {
                let usage = "usage: monitor beam-trap VPOS [HPOS] (decimal; halts when the beam gets there)\n";
                let Some(vpos) = parts.next().and_then(|v| v.parse::<u16>().ok()) else {
                    return Ok(usage.to_string());
                };
                let hpos = match parts.next() {
                    Some(h) => match h.parse::<u16>() {
                        Ok(h) => Some(h),
                        Err(_) => return Ok(usage.to_string()),
                    },
                    None => None,
                };
                let set = emu.bus_mut().ui_toggle_beam_trap(vpos, hpos);
                Ok(format!(
                    "beam trap v{vpos}{} {}\n",
                    hpos.map(|h| format!(" h{h}")).unwrap_or_default(),
                    if set { "set" } else { "removed" }
                ))
            }
            "clear-beam-traps" => {
                emu.bus_mut().ui_clear_beam_traps();
                Ok("cleared beam traps\n".to_string())
            }
            "copper-break" => {
                let Some(addr) = parts.next() else {
                    return Ok(
                        "usage: monitor copper-break ADDR (hex Copper-list address)\n".to_string(),
                    );
                };
                let addr = parse_hex_u32(addr)?;
                let set = emu.bus_mut().ui_toggle_copper_break(addr);
                Ok(format!(
                    "copper breakpoint ${:06X} {}\n",
                    addr & 0x00FF_FFFE,
                    if set { "set" } else { "removed" }
                ))
            }
            "clear-copper-breaks" => {
                emu.bus_mut().ui_clear_copper_breaks();
                Ok("cleared copper breakpoints\n".to_string())
            }
            "copper" => self.monitor_copper(emu, parts.collect()),
            "last-writer" => {
                let Some(addr_s) = parts.next() else {
                    return Ok("usage: monitor last-writer ADDR\n".to_string());
                };
                let addr = parse_hex_u32(addr_s)? & !1;
                let before = emu.retired_instructions();
                match emu.tt_last_writer(addr, before)? {
                    ReverseOutcome::Found(rec) => {
                        self.cpu_idle = false;
                        self.refresh_watchpoints(emu);
                        Ok(format!(
                            "last writer ${:06X}: {:04X}->{:04X} pc=${:08X} pos={} frame={} cck={}\n",
                            rec.addr, rec.old, rec.new, rec.pc, rec.pos, rec.frame, rec.cck
                        ))
                    }
                    ReverseOutcome::NotFound => Ok(format!(
                        "no write to ${addr:06X} found in retained history\n"
                    )),
                    ReverseOutcome::BeyondHistory => Ok(format!(
                        "last write to ${addr:06X} predates retained history\n"
                    )),
                }
            }
            _ => Ok(format!("unknown monitor command {cmd}\n{}", monitor_help())),
        }
    }

    fn monitor_status(&self, emu: &Emulator) -> String {
        format!(
            "pc=${:08X} sr=${:04X} frame={} beam=({}, {}) pos={} reverse={}\n",
            emu.machine.pc(),
            emu.machine.sr(),
            emu.bus().emulated_frames(),
            emu.bus().agnus.vpos,
            emu.bus().agnus.hpos,
            emu.retired_instructions(),
            if emu.time_travel_enabled() {
                "armed"
            } else {
                "off"
            }
        )
    }

    fn monitor_custom(&self, emu: &Emulator) -> String {
        let bus = emu.bus();
        let mut out = String::new();
        out.push_str(&format!(
            "beam vpos={} hpos={} frame={}\n",
            bus.agnus.vpos,
            bus.agnus.hpos,
            bus.emulated_frames()
        ));
        out.push_str(&format!("{}\n", bus.debug_display_state()));
        for off in [
            0x002, 0x004, 0x006, 0x010, 0x01C, 0x01E, 0x080, 0x082, 0x084, 0x086, 0x096, 0x09A,
            0x09C, 0x09E, 0x100, 0x102, 0x104, 0x106, 0x108, 0x10A, 0x1FC,
        ] {
            if let Some(value) = bus.debug_custom_word(off) {
                out.push_str(&format!(
                    "{:<8} ${off:03X} = ${value:04X}\n",
                    custom_reg_name(off)
                ));
            }
        }
        out
    }

    fn monitor_reg(&self, emu: &Emulator, off: u16) -> String {
        match emu.bus().debug_custom_word(off) {
            Some(value) => format!("{} ${off:03X} = ${value:04X}\n", custom_reg_name(off)),
            None => format!("{} ${off:03X}: no debug latch\n", custom_reg_name(off)),
        }
    }

    fn monitor_copper(&self, emu: &Emulator, args: Vec<&str>) -> Result<String> {
        let bus = emu.bus();
        let start = match args.first().copied() {
            None | Some("auto") => bus.agnus.cop1lc,
            Some("pc") => bus.copper.pc(),
            Some(addr) => parse_hex_u32(addr)?,
        };
        let count = match args.get(1).copied() {
            Some(count) => parse_hex_usize(count)?,
            None => 64,
        };
        let mut out = format!(
            "COP1LC ${:06X} COP2LC ${:06X} COPPC ${:06X} ({})\n",
            bus.agnus.cop1lc,
            bus.agnus.cop2lc,
            bus.copper.pc(),
            if bus.copper.is_running() {
                "running"
            } else {
                "stopped"
            }
        );
        for (addr, text) in
            crate::disasm::dump_copper_list(|addr| emu.bus().peek_word_any(addr), start, count)
        {
            out.push_str(&format!("{addr:06X}  {text}\n"));
        }
        Ok(out)
    }
}

/// One qXfer chunk of `xml` for an "OFFSET,LENGTH" range, with the RSP
/// more/last prefix.
pub(crate) fn xml_chunk_reply(xml: &str, range: &str) -> Result<String> {
    let Some((offset_s, len_s)) = range.split_once(',') else {
        return Ok("E01".to_string());
    };
    let offset = parse_hex_usize(offset_s)?;
    let len = parse_hex_usize(len_s)?;
    let bytes = xml.as_bytes();
    if offset >= bytes.len() {
        return Ok("l".to_string());
    }
    let end = offset.saturating_add(len).min(bytes.len());
    let prefix = if end == bytes.len() { 'l' } else { 'm' };
    // The XML is pure ASCII today, so any offset/len is a valid char
    // boundary, but both come straight from the peer: don't let a future
    // non-ASCII addition (or a boundary that happens to split a
    // multi-byte char) turn into a panic instead of a clean error.
    let chunk = std::str::from_utf8(&bytes[offset..end])
        .map_err(|_| anyhow!("qXfer XML slice landed on a non-UTF-8 boundary"))?;
    Ok(format!("{prefix}{chunk}"))
}

/// gdb's library-list XML: one `<library>` per tracked program with a
/// `<segment>` per hunk. gdb pairs the segments with the loadable
/// sections of the file it finds under the library's name (see
/// `set solib-search-path`).
pub(crate) fn build_library_list_xml(modules: &[crate::amigaos::TrackedModule]) -> String {
    if modules.is_empty() {
        return "<library-list version=\"1.0\"/>".to_string();
    }
    let mut xml = String::from("<library-list version=\"1.0\">");
    for module in modules {
        xml.push_str(&format!("<library name=\"{}\">", xml_escape(&module.name)));
        for seg in &module.segments {
            xml.push_str(&format!("<segment address=\"0x{:08x}\"/>", seg.start));
        }
        xml.push_str("</library>");
    }
    xml.push_str("</library-list>");
    xml
}

/// Names come from guest memory, so escape them before they land in an
/// XML attribute.
pub(crate) fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

pub(crate) fn monitor_help() -> String {
    "monitor commands:\n\
     help\n\
     status | beam | custom\n\
     stepover | finish | return-to-program\n\
     reg NAME|OFFSET\n\
     write-reg NAME|OFFSET VALUE\n\
     watch-reg NAME|OFFSET | unwatch-reg NAME|OFFSET | clear-reg-watches\n\
     beam-trap VPOS [HPOS] | clear-beam-traps\n\
     copper-break ADDR | clear-copper-breaks\n\
     copper [auto|pc|ADDR] [COUNT]\n\
     last-writer ADDR\n\
     segments\n\
     who ADDR\n\
     execbase | tasks | task [ADDR|NAME] | memlist\n\
     loadseg-break | loadseg-list\n"
        .to_string()
}

pub(crate) fn parse_z_packet(packet: &str) -> Result<(u32, usize)> {
    let mut fields = packet.split(',');
    let _kind = fields.next();
    let addr = fields
        .next()
        .ok_or_else(|| anyhow!("missing Z/z address"))?;
    let kind = fields
        .next()
        .ok_or_else(|| anyhow!("missing Z/z kind"))?
        .split(';')
        .next()
        .unwrap_or("1");
    Ok((parse_hex_u32(addr)?, parse_hex_usize(kind)?))
}

// RSP signals describe CPU exceptions already taken by the whole-machine
// target. A resume acknowledges that stop; there is no host process signal
// to inject into the guest a second time.
fn validate_resume_signal(signal: &str) -> Result<()> {
    if signal.len() != 2 || !signal.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(anyhow!("resume signal must be two hex digits"));
    }
    Ok(())
}

fn watch_access_from_packet(packet: &str) -> Result<crate::debugger::WatchAccess> {
    match packet.as_bytes().get(1) {
        Some(b'2') => Ok(crate::debugger::WatchAccess::Write),
        Some(b'3') => Ok(crate::debugger::WatchAccess::Read),
        Some(b'4') => Ok(crate::debugger::WatchAccess::Access),
        _ => Err(anyhow!("not a watchpoint packet")),
    }
}

pub(crate) fn parse_hex_u16(input: &str) -> Result<u16> {
    let value = parse_hex_u32(input)?;
    Ok(value as u16)
}

pub(crate) fn parse_hex_u32(input: &str) -> Result<u32> {
    let trimmed = input
        .trim()
        .trim_start_matches('$')
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    u32::from_str_radix(trimmed, 16).with_context(|| format!("invalid hex value {input:?}"))
}

pub(crate) fn parse_hex_usize(input: &str) -> Result<usize> {
    let value = parse_hex_u32(input)?;
    Ok(value as usize)
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub(crate) fn hex_decode(input: &str) -> Result<Vec<u8>> {
    let bytes = input.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(anyhow!("hex string has odd length"));
    }
    bytes
        .chunks(2)
        .map(|pair| parse_hex_byte(pair[0], pair[1]))
        .collect()
}

pub(crate) fn parse_hex_byte(hi: u8, lo: u8) -> Result<u8> {
    let hi = hex_nibble(hi)?;
    let lo = hex_nibble(lo)?;
    Ok((hi << 4) | lo)
}

pub(crate) fn hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(anyhow!("invalid hex digit {:?}", byte as char)),
    }
}

pub(crate) fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bartman_continues_and_steps_after_exception_signals() -> Result<()> {
        let mut emu = super::super::testkit::emulator_with_loadseg_program();
        let mut core = GdbCore::new(&emu, None);
        core.bartman = true;
        assert!(matches!(core.handle_packet(&mut emu, "vCont?")?,
            CoreReply::Packet(s) if s == "vCont;c;C;s;S"));
        for packet in ["C04", "C0a;14004", "vCont;C04:1;c"] {
            assert!(matches!(
                core.handle_packet(&mut emu, packet)?,
                CoreReply::Resume(ResumeRequest::Continue)
            ));
        }
        assert_eq!(emu.machine.pc(), 0x14004);
        for packet in ["S04", "S0a;15004", "vCont;S04:1;c"] {
            assert!(matches!(
                core.handle_packet(&mut emu, packet)?,
                CoreReply::Resume(ResumeRequest::Step)
            ));
        }
        assert_eq!(emu.machine.pc(), 0x15004);
        for packet in ["C", "S100", "Cxx", "vCont;C:1", "vCont;Sgg:1"] {
            assert!(core.handle_packet(&mut emu, packet).is_err(), "{packet}");
        }
        Ok(())
    }

    #[test]
    fn bartman_offsets_register_order_signals_and_rom_sentinel() -> Result<()> {
        let mut emu = super::super::testkit::emulator_with_loadseg_program();
        let mut core = GdbCore::new(&emu, Some("hello".into()));
        core.bartman = true;
        assert!(matches!(core.handle_packet(&mut emu, "?")?, CoreReply::Packet(s) if s == "S05"));
        let attached_pc = emu.machine.pc();
        assert!(
            matches!(core.handle_packet(&mut emu, "qOffsets")?, CoreReply::Packet(s) if s == "00014004;00015004")
        );
        assert_eq!(
            emu.machine.pc(),
            attached_pc,
            "offset query must not move the attached PC"
        );
        assert!(core.run_stop.is_none());
        assert!(
            core.take_console().is_empty(),
            "qOffsets cannot carry O packets"
        );
        let entry_pc = emu.machine.pc();
        emu.machine.debug_set_register(17, 0x9000);
        core.handle_monitor(&mut emu, "reset")?;
        assert_eq!(emu.machine.pc(), entry_pc);
        emu.machine.debug_set_register(16, 0x2700);
        emu.machine.debug_set_register(17, 0xf8001c);
        let regs = core.read_all_registers(&emu);
        assert_eq!(&regs[16 * 8..], "0000270000f8001c");
        core.add_breakpoint(&emu, "Z0,ffffffff,2")?;
        assert!(core.outside_rom);
        assert!(core.breakpoints.is_empty());
        emu.machine.debug_set_register(17, 0x100);
        assert_eq!(core.check_stop(&mut emu)?, Some(StopReason::Breakpoint));
        assert!(!core.outside_rom);
        core.stop = StopReason::Exception(3);
        assert_eq!(core.stop_reply(), "S0A");
        core.stop = StopReason::Exception(4);
        assert_eq!(core.stop_reply(), "S04");
        assert!(matches!(
            core.handle_packet(&mut emu, "k")?,
            CoreReply::Disconnect
        ));
        Ok(())
    }

    /// The transport-free core never writes monitor output anywhere: a
    /// qRcmd replies "OK" and the text accumulates as console data for
    /// whichever driver delivers it (headless `O` packets today, the
    /// windowed drain tomorrow).
    #[test]
    fn core_buffers_monitor_output_as_console_data() -> Result<()> {
        let mut emu = crate::gdbstub::testkit::emulator_with_loadseg_program();
        let mut core = GdbCore::new(&emu, None);
        let packet = format!("qRcmd,{}", hex_encode(b"help"));
        match core.handle_packet(&mut emu, &packet)? {
            CoreReply::Packet(reply) => assert_eq!(reply, "OK"),
            other => panic!("monitor command must reply in-band: {other:?}"),
        }
        let console = core.take_console();
        assert_eq!(console.len(), 1);
        assert!(console[0].contains("segments"), "help text: {}", console[0]);
        assert!(core.take_console().is_empty(), "take_console drains");
        Ok(())
    }

    #[test]
    fn monitor_who_accepts_a_rom_address() -> Result<()> {
        let mut emu = crate::gdbstub::testkit::emulator_with_loadseg_program();
        let mut core = GdbCore::new(&emu, None);
        let output = core.handle_monitor(&mut emu, "who F80010")?;
        assert!(output.contains("$00F80010"), "{output}");
        assert!(monitor_help().contains("who ADDR"));
        Ok(())
    }

    #[test]
    fn z2_z3_and_z4_preserve_watch_access_type() -> Result<()> {
        let mut emu = crate::gdbstub::testkit::emulator_with_loadseg_program();
        let mut core = GdbCore::new(&emu, None);
        for (packet, expected) in [
            ("Z2,1000,2", crate::debugger::WatchAccess::Write),
            ("Z3,1002,2", crate::debugger::WatchAccess::Read),
            ("Z4,1004,2", crate::debugger::WatchAccess::Access),
        ] {
            assert!(matches!(
                core.handle_packet(&mut emu, packet)?,
                CoreReply::Packet(reply) if reply == "OK"
            ));
            assert_eq!(core.watchpoints.last().unwrap().access, expected);
        }
        assert_eq!(
            watch_access_from_packet("z3,1002,2")?,
            crate::debugger::WatchAccess::Read
        );
        assert!(matches!(
            core.handle_packet(&mut emu, "Z3,1000,2")?,
            CoreReply::Packet(reply) if reply == "OK"
        ));
        let (words, truncated) = core.machine_watch_words(0x00FF_FFFF);
        assert!(!truncated);
        assert!(words.contains(&(0x1000, crate::debugger::WatchAccess::Access)));
        Ok(())
    }
}
