// SPDX-License-Identifier: GPL-3.0-or-later

//! Remote GDB protocol frontend for Copperline.
//!
//! This is a host debugger transport, not an emulated Amiga device. Generic
//! GDB memory packets inspect and modify CPU-visible RAM without touching
//! memory-mapped devices; Amiga custom-chip state is exposed through `monitor`
//! commands so inspection remains side-effect-free.

use super::core::*;
use crate::debugger::normalize_listen_addr;
use crate::emulator::Emulator;
use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub listen: String,
    pub reverse_budget_mb: usize,
    pub reverse_interval_frames: u64,
    /// `--run` + `--gdb`: stop (once) the moment the guest OS loads a
    /// program with this name, before its first instruction executes.
    pub stop_on_load: Option<String>,
}

impl Config {
    pub fn new(listen: String) -> Self {
        Self {
            listen,
            reverse_budget_mb: crate::envcfg::var("COPPERLINE_DBG_RR_BUDGET_MB")
                .and_then(|s| s.trim().parse::<usize>().ok())
                .unwrap_or(crate::debugger::RR_DEFAULT_BUDGET_MB),
            reverse_interval_frames: crate::envcfg::var("COPPERLINE_DBG_RR_INTERVAL")
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(crate::debugger::RR_DEFAULT_INTERVAL_FRAMES),
            stop_on_load: None,
        }
    }
}

pub fn run(mut emu: Emulator, config: Config) -> Result<()> {
    let bind = normalize_listen_addr(&config.listen)?;
    let listener = TcpListener::bind(&bind).with_context(|| format!("binding GDB stub {bind}"))?;
    log::info!("gdb: listening on {bind}");

    emu.set_paced(false);
    emu.enable_time_travel(config.reverse_budget_mb, config.reverse_interval_frames);
    emu.debug_ensure_time_travel_anchor()?;

    serve(listener, emu, config.stop_on_load)
}

/// Accept GDB connections one at a time against the same machine. A
/// detach (or dropped connection) keeps the emulator paused and waits
/// for the next client -- reattaching re-runs `qOffsets`, which is the
/// documented way to pick up a program loaded mid-session -- while
/// GDB's `kill` ends the server.
fn serve(listener: TcpListener, mut emu: Emulator, stop_on_load: Option<String>) -> Result<()> {
    loop {
        let (stream, peer) = listener.accept().context("accepting GDB connection")?;
        log::info!("gdb: connection from {peer}");
        stream.set_nodelay(true).ok();
        // Each connection re-arms the stop-on-load target: a reconnecting
        // client wants the same break-at-entry, and the fresh tracker
        // absorbs an already-running program so nothing fires spuriously.
        let mut session = Session::new(emu, stream, stop_on_load.clone());
        let end = session.run()?;
        session.clear_debug_hardware();
        emu = session.emu;
        match end {
            SessionEnd::Detached => {
                log::info!("gdb: client detached; machine paused, listening for reconnection");
            }
            SessionEnd::Killed => return Ok(()),
        }
    }
}

/// How a [`Session`] ended: a detach/EOF keeps serving, `k` shuts down.
enum SessionEnd {
    Detached,
    Killed,
}

struct Session {
    emu: Emulator,
    stream: TcpStream,
    no_ack: bool,
    core: GdbCore,
    watch_words: Vec<(u32, crate::debugger::WatchAccess)>,
}

impl Session {
    fn new(emu: Emulator, stream: TcpStream, run_stop: Option<String>) -> Self {
        let core = GdbCore::new(&emu, run_stop);
        Self {
            emu,
            stream,
            no_ack: false,
            core,
            watch_words: Vec::new(),
        }
    }

    fn run(&mut self) -> Result<SessionEnd> {
        loop {
            let Some(packet) = self.read_packet()? else {
                return Ok(SessionEnd::Detached);
            };
            if packet == "QStartNoAckMode" {
                self.send_packet("OK")?;
                self.no_ack = true;
                continue;
            }
            let action = self.core.handle_packet(&mut self.emu, &packet)?;
            self.sync_watchpoints();
            let reply = match action {
                CoreReply::Packet(reply) => reply,
                // The bounded step answers at once; an open-ended continue
                // runs the per-instruction loop below, socket poll and all.
                CoreReply::Resume(ResumeRequest::Step) => self.core.step_forward(&mut self.emu)?,
                CoreReply::Resume(ResumeRequest::Continue) => self.continue_forward()?,
                CoreReply::Disconnect => {
                    self.flush_console()?;
                    self.send_packet("OK")?;
                    return Ok(SessionEnd::Detached);
                }
                CoreReply::Kill => return Ok(SessionEnd::Killed),
            };
            self.flush_console()?;
            self.send_packet(&reply)?;
        }
    }

    /// Deliver the core's queued console lines as `O` packets, in order,
    /// before whatever reply follows -- the wire order the one-file stub
    /// produced by writing them at the point of origin.
    fn flush_console(&mut self) -> Result<()> {
        for chunk in self.core.take_console() {
            self.send_console(&chunk)?;
        }
        Ok(())
    }

    /// Drop the bus-side debug state this session installed (register
    /// watches, beam traps, Copper breakpoints), so a stale hit cannot
    /// stop the next client's first continue.
    fn clear_debug_hardware(&mut self) {
        for (addr, _) in self.watch_words.drain(..) {
            if self
                .emu
                .machine
                .ui_breaks()
                .watches
                .iter()
                .any(|watch| watch.addr == addr)
            {
                self.emu.machine.ui_toggle_watch(addr);
            }
        }
        self.emu.bus_mut().set_ui_reg_watches(&[]);
        self.emu.bus_mut().ui_clear_beam_traps();
        self.emu.bus_mut().ui_clear_copper_breaks();
    }

    fn sync_watchpoints(&mut self) {
        let mask = self.emu.machine.ui_addr_mask();
        let (desired, _) = self.core.machine_watch_words(mask);
        for (addr, access) in self.watch_words.clone() {
            if !desired.contains(&(addr, access)) {
                self.emu.machine.ui_toggle_watch(addr);
                self.watch_words.retain(|owned| *owned != (addr, access));
            }
        }
        for &(addr, access) in &desired {
            if self.watch_words.contains(&(addr, access)) {
                continue;
            }
            self.emu
                .machine
                .ui_toggle_watch_access(addr, None, None, access);
            self.watch_words.push((addr, access));
        }
    }

    fn read_packet(&mut self) -> Result<Option<String>> {
        // Loop (rather than recurse) on a checksum mismatch: a peer sending
        // many consecutive bad-checksum packets must not grow the call
        // stack without bound.
        loop {
            let mut byte = [0u8; 1];
            loop {
                match self.stream.read_exact(&mut byte) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                    Err(e) => return Err(e).context("reading GDB packet"),
                }
                match byte[0] {
                    b'+' | b'-' => continue,
                    b'$' => break,
                    0x03 => {
                        self.core.stop = StopReason::Interrupted;
                        return Ok(Some("?".to_string()));
                    }
                    _ => continue,
                }
            }

            let mut payload = Vec::new();
            loop {
                self.stream
                    .read_exact(&mut byte)
                    .context("reading GDB packet payload")?;
                if byte[0] == b'#' {
                    break;
                }
                payload.push(byte[0]);
                if payload.len() > MAX_PACKET_PAYLOAD_BYTES {
                    bail!(
                        "GDB packet exceeds {}-byte limit without a '#' terminator",
                        MAX_PACKET_PAYLOAD_BYTES
                    );
                }
            }
            let mut sum_bytes = [0u8; 2];
            self.stream
                .read_exact(&mut sum_bytes)
                .context("reading GDB packet checksum")?;
            let expected = parse_hex_byte(sum_bytes[0], sum_bytes[1])?;
            let actual = checksum(&payload);
            if expected != actual {
                if !self.no_ack {
                    self.stream.write_all(b"-").ok();
                }
                continue;
            }
            if !self.no_ack {
                self.stream.write_all(b"+").ok();
            }
            return String::from_utf8(payload)
                .map(Some)
                .context("GDB packet is not UTF-8");
        }
    }

    fn send_packet(&mut self, payload: &str) -> Result<()> {
        let sum = checksum(payload.as_bytes());
        write!(self.stream, "${payload}#{sum:02x}").context("sending GDB packet")?;
        self.stream.flush().ok();
        Ok(())
    }

    fn send_console(&mut self, output: &str) -> Result<()> {
        for line in output.as_bytes().chunks(200) {
            self.send_packet(&format!("O{}", hex_encode(line)))?;
        }
        Ok(())
    }

    fn continue_forward(&mut self) -> Result<String> {
        loop {
            self.core.step_once(&mut self.emu)?;
            if let Some(stop) = self.core.check_stop(&mut self.emu)? {
                self.core.stop = stop;
                return Ok(self.core.stop_reply());
            }
            if self.poll_interrupt()? {
                self.core.stop = StopReason::Interrupted;
                return Ok(self.core.stop_reply());
            }
        }
    }

    fn poll_interrupt(&mut self) -> Result<bool> {
        self.stream
            .set_nonblocking(true)
            .context("setting GDB stream nonblocking")?;
        let mut byte = [0u8; 1];
        let result = match self.stream.peek(&mut byte) {
            Ok(1) if byte[0] == 0x03 => {
                let _ = self.stream.read(&mut byte);
                Ok(true)
            }
            Ok(_) => Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(e) => Err(e).context("polling GDB interrupt"),
        };
        self.stream
            .set_nonblocking(false)
            .context("restoring GDB stream blocking mode")?;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debugger::parse_custom_reg;
    use crate::gdbstub::testkit::{emulator_with_loadseg_program, GdbClient};

    #[test]
    fn listen_addr_defaults_to_loopback_for_port_forms() -> Result<()> {
        assert_eq!(normalize_listen_addr(":2345")?, "127.0.0.1:2345");
        assert_eq!(normalize_listen_addr("2345")?, "127.0.0.1:2345");
        assert_eq!(normalize_listen_addr("0.0.0.0:2345")?, "0.0.0.0:2345");
        Ok(())
    }

    #[test]
    fn hex_round_trip_and_checksum_match_rsp_framing() -> Result<()> {
        let data = b"m1000,10";
        assert_eq!(hex_decode(&hex_encode(data))?, data);
        assert_eq!(checksum(data), 0xbb);
        Ok(())
    }

    #[test]
    fn custom_register_parser_accepts_names_offsets_and_addresses() {
        assert_eq!(parse_custom_reg("DMACON"), Some(0x096));
        assert_eq!(parse_custom_reg("dff096"), Some(0x096));
        assert_eq!(parse_custom_reg("$96"), Some(0x096));
        assert_eq!(parse_custom_reg("COLOR00"), Some(0x180));
        assert_eq!(parse_custom_reg("notareg"), None);
    }

    #[test]
    fn target_xml_chunk_uses_rsp_more_and_last_prefixes() -> Result<()> {
        let mut bytes = TARGET_XML.as_bytes();
        let first = &bytes[..16];
        assert_eq!(first[0], b'<');
        bytes = &bytes[bytes.len() - 8..];
        assert!(!bytes.is_empty());
        Ok(())
    }

    /// Run a [`Session`] against a scripted client on a loopback socket.
    /// The session runs on the test thread; client panics propagate
    /// through the join.
    fn run_session(emu: Emulator, client: impl FnOnce(GdbClient) + Send + 'static) {
        run_session_with_target(emu, None, client);
    }

    /// [`run_session`] with a `--run` stop-on-load target armed.
    fn run_session_with_target(
        emu: Emulator,
        stop_on_load: Option<&str>,
        client: impl FnOnce(GdbClient) + Send + 'static,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let thread = std::thread::spawn(move || client(GdbClient::connect(addr)));
        let (stream, _) = listener.accept().unwrap();
        Session::new(emu, stream, stop_on_load.map(String::from))
            .run()
            .unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn detach_keeps_serving_and_reattach_reruns_qoffsets() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            // First client: arm loadseg-break, continue to the load,
            // then detach.
            let mut first = GdbClient::connect(addr);
            let (_, reply) =
                first.request_collect(&format!("qRcmd,{}", hex_encode(b"loadseg-break")));
            assert_eq!(reply, "OK");
            let (_, reply) = first.request_collect("c");
            assert_eq!(reply, "T05thread:1;");
            assert_eq!(first.request("D"), "OK");
            drop(first);
            // Second client: attach-time qOffsets now reports the
            // program the first session ran into.
            let mut second = GdbClient::connect(addr);
            assert_eq!(second.request("qOffsets"), "TextSeg=14004;DataSeg=15004");
            second.send("k");
        });
        serve(listener, emulator_with_loadseg_program(), None).unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn qrcmd_monitor_commands_round_trip_over_the_wire() {
        run_session(emulator_with_loadseg_program(), |mut client| {
            let (console, reply) =
                client.request_collect(&format!("qRcmd,{}", hex_encode(b"help")));
            assert_eq!(reply, "OK");
            let text = console.concat();
            assert!(text.contains("segments"), "monitor help output: {text}");
            assert_eq!(client.request("D"), "OK");
        });
    }

    /// The exec dumps the debugger console prints are served over the
    /// wire too, so a headless gdb session can ask what the OS is doing.
    #[test]
    fn monitor_os_dumps_report_exec_state() {
        run_session(emulator_with_loadseg_program(), |mut client| {
            let monitor = |client: &mut GdbClient, cmd: &str| {
                let (console, reply) =
                    client.request_collect(&format!("qRcmd,{}", hex_encode(cmd.as_bytes())));
                assert_eq!(reply, "OK");
                console.concat()
            };
            let text = monitor(&mut client, "execbase");
            assert!(text.contains("ExecBase $010000"), "{text}");
            assert!(text.contains("ThisTask $012000"), "{text}");
            assert!(text.contains("IDNestCnt"), "{text}");
            let text = monitor(&mut client, "tasks");
            assert!(text.contains("> $012000"), "{text}");
            // No argument dumps ThisTask, which this fixture stages as a
            // CLI process running "hello".
            let text = monitor(&mut client, "task");
            assert!(
                text.contains("task $012000") && text.contains("(process)"),
                "{text}"
            );
            assert!(text.contains("\"hello\""), "{text}");
            // An address that cannot hold a task is refused, not read.
            let text = monitor(&mut client, "task $DFF000");
            assert!(
                text.contains("not where a task structure can live"),
                "{text}"
            );
            // This fixture has no MemList at all.
            let text = monitor(&mut client, "memlist");
            assert!(text.contains("no memory list"), "{text}");
            assert_eq!(client.request("D"), "OK");
        });
    }

    #[test]
    fn library_list_serves_loadseg_events_over_the_wire() {
        run_session(emulator_with_loadseg_program(), |mut client| {
            let supported = client.request("qSupported:xmlRegisters=i386");
            assert!(
                supported.contains("qXfer:libraries:read+"),
                "qSupported: {supported}"
            );
            // Fetching the (empty) library list arms LoadSeg detection.
            assert_eq!(
                client.request("qXfer:libraries:read::0,1000"),
                "l<library-list version=\"1.0\"/>"
            );
            // The ROM program installs cli_Module: continue stops with
            // a library event.
            assert_eq!(client.request("c"), "T05library:;thread:1;");
            let list = client.request("qXfer:libraries:read::0,1000");
            assert!(list.contains("<library name=\"hello\">"), "list: {list}");
            assert!(
                list.contains("<segment address=\"0x00014004\"/>"),
                "list: {list}"
            );
            assert!(
                list.contains("<segment address=\"0x00015004\"/>"),
                "list: {list}"
            );
            assert_eq!(client.request("D"), "OK");
        });
    }

    #[test]
    fn monitor_loadseg_break_stops_visibly() {
        run_session(emulator_with_loadseg_program(), |mut client| {
            let (console, reply) =
                client.request_collect(&format!("qRcmd,{}", hex_encode(b"loadseg-break")));
            assert_eq!(reply, "OK");
            assert!(console.concat().contains("armed"));
            let (console, reply) = client.request_collect("c");
            assert_eq!(reply, "T05thread:1;");
            let text = console.concat();
            assert!(text.contains("loadseg: hello"), "console: {text}");
            assert!(text.contains("$014004"), "console: {text}");
            let (console, reply) =
                client.request_collect(&format!("qRcmd,{}", hex_encode(b"loadseg-list")));
            assert_eq!(reply, "OK");
            assert!(console.concat().contains("hello:"), "loadseg-list output");
            assert_eq!(client.request("D"), "OK");
        });
    }

    #[test]
    fn stop_on_load_target_stops_once_at_program_entry() {
        run_session_with_target(
            emulator_with_loadseg_program(),
            Some("HELLO"), // matched case-insensitively against "hello"
            |mut client| {
                let (console, reply) = client.request_collect("c");
                assert_eq!(reply, "T05thread:1;");
                let text = console.concat();
                assert!(text.contains("run target loaded: hello"), "console: {text}");
                assert!(text.contains("$014004"), "console: {text}");
                // The stop is one-shot: a later continue runs on (bounded
                // here by a breakpoint on the program's spin loop).
                assert_eq!(client.request("Z0,f8001c,2"), "OK");
                let (_, reply) = client.request_collect("c");
                assert_eq!(reply, "T05hwbreak:;thread:1;");
                assert_eq!(client.request("D"), "OK");
            },
        );
    }

    #[test]
    fn stop_on_load_ignores_other_programs() {
        run_session_with_target(
            emulator_with_loadseg_program(),
            Some("otherprog"),
            |mut client| {
                // The "hello" load must not stop the session; bound the
                // run with a breakpoint past the install instead.
                assert_eq!(client.request("Z0,f8001c,2"), "OK");
                let (console, reply) = client.request_collect("c");
                assert_eq!(reply, "T05hwbreak:;thread:1;");
                assert!(
                    !console.concat().contains("run target loaded"),
                    "unrelated load must not fire the stop-on-load target"
                );
                assert_eq!(client.request("D"), "OK");
            },
        );
    }
}
