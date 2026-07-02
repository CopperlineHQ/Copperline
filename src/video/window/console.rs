// SPDX-License-Identifier: GPL-3.0-or-later

//! The debugger console's command interpreter: a GDB-flavoured command
//! line over the same machinery as the debugger window and GDB stub.
//! Split out of `window.rs` for size; this is the same `App`, with full
//! access to its private state.

use super::*;

/// What a submitted command asks the console window to do besides
/// printing its output.
pub(super) struct ConsoleOutcome {
    pub(super) lines: Vec<String>,
    pub(super) clear: bool,
    pub(super) close: bool,
}

impl ConsoleOutcome {
    fn lines(lines: Vec<String>) -> Self {
        Self {
            lines,
            clear: false,
            close: false,
        }
    }

    fn one(line: impl Into<String>) -> Self {
        Self::lines(vec![line.into()])
    }

    fn error(line: impl Into<String>) -> Self {
        Self::one(format!("!{}", line.into()))
    }
}

/// Instruction budgets for the bounded run commands, matching the
/// debugger window's transport buttons.
const CONSOLE_STEP_BUDGET: usize = 5_000_000;
const CONSOLE_RUN_TO_BUDGET: usize = 2_000_000;

fn hex32(token: &str) -> Option<u32> {
    u32::from_str_radix(token.trim_start_matches('$'), 16).ok()
}

fn dec_u16(token: &str) -> Option<u16> {
    token.parse::<u16>().ok()
}

/// GDB-style register index from a name: D0-D7, A0-A7, SP, SR, PC.
fn reg_index(token: &str) -> Option<usize> {
    let token = token.to_ascii_uppercase();
    if let Some(n) = token.strip_prefix('D') {
        let n = n.parse::<usize>().ok()?;
        return (n < 8).then_some(n);
    }
    if let Some(n) = token.strip_prefix('A') {
        let n = n.parse::<usize>().ok()?;
        return (n < 8).then_some(8 + n);
    }
    match token.as_str() {
        "SP" => Some(15),
        "SR" => Some(16),
        "PC" => Some(17),
        _ => None,
    }
}

/// Search CPU-visible memory for `pattern`, starting at `start` and
/// wrapping the 24-bit space once. Shared by the console FIND command
/// and the Memory tab's Find button.
pub(super) fn search_cpu_memory(
    machine: &crate::cpu::M68kMachine,
    pattern: &[u8],
    start: u32,
) -> Option<u32> {
    const SPACE: u64 = 0x0100_0000;
    const CHUNK: usize = 4096;
    let mut offset = 0u64;
    while offset < SPACE {
        let base = ((u64::from(start) + offset) % SPACE) as u32;
        // Overlap chunks by the pattern length so matches spanning a
        // chunk boundary are seen.
        let bytes = machine.debug_read_memory(base, CHUNK + pattern.len() - 1);
        if let Some(hit) = bytes
            .windows(pattern.len())
            .position(|window| window == pattern)
        {
            return Some(base.wrapping_add(hit as u32) & 0x00FF_FFFF);
        }
        offset += CHUNK as u64;
    }
    None
}

fn parse_hex_pattern(tokens: &[&str]) -> Option<Vec<u8>> {
    let joined: String = tokens.concat();
    if joined.is_empty() || !joined.len().is_multiple_of(2) {
        return None;
    }
    (0..joined.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&joined[i..i + 2], 16).ok())
        .collect()
}

const CONSOLE_HELP: &[&str] = &[
    "execution:  run  pause  step/s [N]  over  out  frame/f  line  cstep",
    "            runto ADDR   toslot V [H]   rstep [N]  rframe  rrun",
    "stops:      break/b ADDR [COND] [IGN N]   watch/w ADDR   rwatch REG",
    "            btrap V [H]   cbreak ADDR   catch irq N|trap N|vec N",
    "            breaks (list)   clearbreaks",
    "inspect:    status  regs/r  mem/m ADDR [BYTES]  dis/d [ADDR] [N]",
    "            copper [pc|ADDR] [N]   custom   find HEX [START]   writer ADDR",
    "modify:     poke ADDR VAL   setreg REG VAL",
    "console:    help  clear  close",
    "Addresses and values are hex; beam positions (V, H) are decimal.",
    "Cmd/Ctrl+V pastes; a multi-line paste runs each line in order.",
];

impl App {
    /// Execute the console's current input line: echo it, dispatch the
    /// command, and append the results to the scrollback.
    pub(super) fn console_submit(&mut self) {
        let Some(line) = self.console_panel.as_mut().map(|panel| {
            panel.scroll = 0;
            panel.history_pos = None;
            std::mem::take(&mut panel.input)
        }) else {
            return;
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            return;
        }
        if let Some(panel) = self.console_panel.as_mut() {
            panel.push_output(format!("> {line}"));
            if panel.history.last() != Some(&line) {
                panel.history.push(line.clone());
            }
        }
        let outcome = self.console_execute(&line);
        if outcome.close {
            self.close_tool_panel(ToolPanelKind::Console);
            return;
        }
        if let Some(panel) = self.console_panel.as_mut() {
            if outcome.clear {
                panel.output.clear();
            }
            for line in outcome.lines {
                panel.push_output(line);
            }
        }
    }

    /// Host text input for the console window: the paste shortcut
    /// (Cmd+V on macOS, Ctrl+V anywhere) and layout-aware typed text.
    /// Returns false for everything else so editing and command keys
    /// reach the keycode handler.
    pub(super) fn console_handle_text_input(&mut self, code: KeyCode, text: Option<&str>) -> bool {
        if code == KeyCode::KeyV
            && (host_shortcut_modifier_pressed(self.modifiers) || self.modifiers.control_key())
        {
            self.console_paste();
            return true;
        }
        // Text typed with a command modifier held is a shortcut, not input.
        if host_shortcut_modifier_pressed(self.modifiers) || self.modifiers.control_key() {
            return false;
        }
        let Some(text) = text else {
            return false;
        };
        let printable: String = text.chars().filter(|c| (' '..='~').contains(c)).collect();
        if printable.is_empty() {
            return false;
        }
        self.console_insert_text(&printable);
        true
    }

    /// Insert text into the console prompt, executing the line for every
    /// newline: a multi-line paste runs as a script, and the trailing
    /// fragment stays in the prompt for editing.
    pub(super) fn console_insert_text(&mut self, text: &str) {
        for ch in text.chars() {
            if ch == '\n' {
                self.console_submit();
                continue;
            }
            if let Some(panel) = self.console_panel.as_mut() {
                panel.push_input_char(ch);
            }
        }
        self.request_redraw();
    }

    /// Paste the host clipboard into the prompt.
    fn console_paste(&mut self) {
        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
            Ok(text) => {
                // Normalize CRLF so a Windows-clipboard script does not
                // submit a blank line per line.
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                self.console_insert_text(&text);
            }
            Err(e) => {
                if let Some(panel) = self.console_panel.as_mut() {
                    panel.push_output(format!("!clipboard unavailable: {e}"));
                }
                self.request_redraw();
            }
        }
    }

    /// Dispatch one command line. Never touches `console_panel`; the
    /// caller applies the outcome so borrows stay simple.
    fn console_execute(&mut self, line: &str) -> ConsoleOutcome {
        // Commands and arguments are case-insensitive (hex, register
        // names, catch grammar); pasted lowercase runs as typed.
        let line = line.to_ascii_uppercase();
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some((&cmd, args)) = tokens.split_first() else {
            return ConsoleOutcome::lines(Vec::new());
        };
        let cmd = cmd.to_ascii_uppercase();
        match cmd.as_str() {
            "HELP" | "?" => {
                ConsoleOutcome::lines(CONSOLE_HELP.iter().map(|s| s.to_string()).collect())
            }
            "CLEAR" => ConsoleOutcome {
                lines: Vec::new(),
                clear: true,
                close: false,
            },
            "CLOSE" | "QUIT" | "EXIT" => ConsoleOutcome {
                lines: Vec::new(),
                clear: false,
                close: true,
            },
            "STATUS" => ConsoleOutcome::lines(self.console_status_lines()),
            "RUN" | "GO" | "CONTINUE" | "C" => {
                self.paused = false;
                self.paused_before_console = false;
                self.sync_live_audio_suspension();
                ConsoleOutcome::one("running (PAUSE stops; breakpoints report here or on stop)")
            }
            "PAUSE" => {
                self.paused = true;
                self.paused_before_console = true;
                self.sync_live_audio_suspension();
                let mut lines = vec!["paused".to_string()];
                lines.extend(self.console_status_lines());
                self.finish_render_for_current_frame();
                ConsoleOutcome::lines(lines)
            }
            "STEP" | "S" => {
                let count = args
                    .first()
                    .and_then(|t| t.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 1_000_000);
                self.console_exec_op(|app| app.emu.debug_step_instructions(count))
            }
            "OVER" | "NEXT" | "N" => {
                self.console_exec_op(|app| app.emu.debug_step_over(CONSOLE_STEP_BUDGET))
            }
            "OUT" | "FINISH" => {
                self.console_exec_op(|app| app.emu.debug_step_out(CONSOLE_STEP_BUDGET))
            }
            "FRAME" | "F" => self.console_exec_op(|app| app.emu.step_frame()),
            "LINE" => {
                let (vpos, frame_lines) = {
                    let bus = self.emu.bus();
                    (bus.agnus.vpos, bus.agnus.current_frame_lines())
                };
                let target = ((vpos + 1) % frame_lines.max(1)).min(u32::from(u16::MAX)) as u16;
                self.console_run_to_beam(target, None)
            }
            "CSTEP" => self.console_exec_report(|app| {
                let advanced = app.emu.debug_step_copper(CONSOLE_RUN_TO_BUDGET)?;
                Ok((!advanced).then(|| "copper did not advance (stopped or DMA off)".to_string()))
            }),
            "RUNTO" => {
                let Some(addr) = args.first().and_then(|t| hex32(t)) else {
                    return ConsoleOutcome::error("usage: RUNTO ADDR (hex)");
                };
                self.console_exec_report(move |app| {
                    let reached = app.emu.debug_run_to_pc(addr, CONSOLE_RUN_TO_BUDGET)?;
                    Ok((!reached).then(|| format!("${addr:06X} not reached (budget)")))
                })
            }
            "TOSLOT" => {
                let Some(vpos) = args.first().and_then(|t| dec_u16(t)) else {
                    return ConsoleOutcome::error("usage: TOSLOT VPOS [HPOS] (decimal)");
                };
                let hpos = args.get(1).and_then(|t| dec_u16(t));
                self.console_run_to_beam(vpos, hpos)
            }
            "RSTEP" | "RS" => {
                let count = args
                    .first()
                    .and_then(|t| t.parse::<u64>().ok())
                    .unwrap_or(1)
                    .clamp(1, 1_000_000);
                self.console_reverse_op(|app| app.emu.tt_reverse_step(count))
            }
            "RFRAME" => self.console_reverse_op(|app| app.emu.tt_reverse_frame()),
            "RRUN" | "RC" => self.console_reverse_op(|app| app.emu.tt_reverse_continue()),
            "BREAK" | "B" => {
                let spec = args.join(" ");
                let Some((addr, cond, ignore)) = ui::parse_break_spec(&spec) else {
                    return ConsoleOutcome::error("usage: BREAK ADDR [LHS OP RHS] [IGN N]");
                };
                let set = self.emu.machine.ui_set_breakpoint(addr, cond, ignore);
                ConsoleOutcome::one(format!(
                    "breakpoint ${:06X} {}",
                    addr & 0x00FF_FFFF,
                    if set { "set" } else { "removed" }
                ))
            }
            "WATCH" | "W" => {
                let Some(addr) = args.first().and_then(|t| hex32(t)) else {
                    return ConsoleOutcome::error("usage: WATCH ADDR (hex, word)");
                };
                let set = self.emu.machine.ui_toggle_watch(addr);
                ConsoleOutcome::one(format!(
                    "watchpoint ${:06X} {}",
                    addr & 0x00FF_FFFE,
                    if set { "set" } else { "removed" }
                ))
            }
            "RWATCH" | "RW" => {
                let Some(off) = args
                    .first()
                    .and_then(|t| crate::gdbstub::parse_custom_reg(t))
                else {
                    return ConsoleOutcome::error("usage: RWATCH NAME|OFFSET (e.g. DMACON or 96)");
                };
                let set = self.emu.machine.ui_toggle_reg_watch(off);
                ConsoleOutcome::one(format!(
                    "register watch {} (${off:03X}) {}",
                    crate::debugger::custom_reg_name(off),
                    if set { "set" } else { "removed" }
                ))
            }
            "BTRAP" => {
                let Some(vpos) = args.first().and_then(|t| dec_u16(t)) else {
                    return ConsoleOutcome::error("usage: BTRAP VPOS [HPOS] (decimal)");
                };
                let hpos = args.get(1).and_then(|t| dec_u16(t));
                let set = self.emu.bus_mut().ui_toggle_beam_trap(vpos, hpos);
                ConsoleOutcome::one(format!(
                    "beam trap v{vpos}{} {}",
                    hpos.map(|h| format!(" h{h}")).unwrap_or_default(),
                    if set { "set" } else { "removed" }
                ))
            }
            "CBREAK" => {
                let Some(addr) = args.first().and_then(|t| hex32(t)) else {
                    return ConsoleOutcome::error("usage: CBREAK ADDR (hex copper-list address)");
                };
                let set = self.emu.bus_mut().ui_toggle_copper_break(addr);
                ConsoleOutcome::one(format!(
                    "copper breakpoint ${:06X} {}",
                    addr & 0x00FF_FFFE,
                    if set { "set" } else { "removed" }
                ))
            }
            "CATCH" => {
                let spec = args.join(" ");
                let Some(vector) = ui::parse_catch_spec(&spec) else {
                    return ConsoleOutcome::error("usage: CATCH IRQ N | TRAP N | VEC N");
                };
                let set = self.emu.machine.ui_toggle_catch(vector);
                ConsoleOutcome::one(format!(
                    "catch {} {}",
                    crate::debugger::exception_vector_name(vector),
                    if set { "set" } else { "removed" }
                ))
            }
            "BREAKS" | "INFO" => ConsoleOutcome::lines(self.console_breaks_lines()),
            "CLEARBREAKS" => {
                self.emu.machine.ui_breaks_clear();
                self.last_debug_stop = None;
                ConsoleOutcome::one("cleared all breakpoints, watchpoints, traps, and catches")
            }
            "REGS" | "R" => ConsoleOutcome::lines(self.console_regs_lines()),
            "MEM" | "M" => {
                let Some(addr) = args.first().and_then(|t| hex32(t)) else {
                    return ConsoleOutcome::error("usage: MEM ADDR [BYTES] (hex)");
                };
                let len = args
                    .get(1)
                    .and_then(|t| hex32(t))
                    .unwrap_or(0x40)
                    .clamp(1, 0x400) as usize;
                let base = addr & 0x00FF_FFF0;
                let bytes = self
                    .emu
                    .machine
                    .debug_read_memory(base, len.div_ceil(16) * 16);
                ConsoleOutcome::lines(
                    bytes
                        .chunks(16)
                        .enumerate()
                        .map(|(row, chunk)| {
                            ui::hex_dump_row(base.wrapping_add(row as u32 * 16), chunk)
                        })
                        .collect(),
                )
            }
            "DIS" | "D" => {
                let mut pc = args
                    .first()
                    .and_then(|t| hex32(t))
                    .unwrap_or(self.emu.machine.pc())
                    & !1;
                let count = args
                    .get(1)
                    .and_then(|t| t.parse::<usize>().ok())
                    .unwrap_or(8)
                    .clamp(1, 32);
                let cpu_type = self.emu.machine.cpu_type();
                let bus = self.emu.bus();
                let mut lines = Vec::with_capacity(count);
                for _ in 0..count {
                    let (text, len) =
                        crate::disasm::disassemble(|a| bus.peek_word_any(a), pc, cpu_type);
                    lines.push(format!("{pc:06X}  {text}"));
                    pc = pc.wrapping_add(len.max(2));
                }
                ConsoleOutcome::lines(lines)
            }
            "COPPER" => {
                let bus = self.emu.bus();
                let start = match args.first().map(|t| t.to_ascii_uppercase()) {
                    None => bus.copper.pc().saturating_sub(4 * 4),
                    Some(s) if s == "PC" => bus.copper.pc().saturating_sub(4 * 4),
                    Some(s) => match hex32(&s) {
                        Some(addr) => addr,
                        None => return ConsoleOutcome::error("usage: COPPER [PC|ADDR] [COUNT]"),
                    },
                };
                let count = args
                    .get(1)
                    .and_then(|t| t.parse::<usize>().ok())
                    .unwrap_or(16)
                    .clamp(1, 64);
                let copper_pc = bus.copper.pc();
                let mut lines = vec![format!(
                    "COP1LC {:06X}  COP2LC {:06X}  COPPC {:06X} ({})",
                    bus.agnus.cop1lc,
                    bus.agnus.cop2lc,
                    copper_pc,
                    if bus.copper.is_running() {
                        "running"
                    } else {
                        "stopped"
                    }
                )];
                for (addr, text) in
                    crate::disasm::dump_copper_list(|a| bus.peek_word_any(a), start, count)
                {
                    let cursor = if addr == copper_pc { ">" } else { " " };
                    lines.push(format!("{cursor}{addr:06X}  {text}"));
                }
                ConsoleOutcome::lines(lines)
            }
            "CUSTOM" => {
                let bus = self.emu.bus();
                let mut lines = vec![format!(
                    "beam v{} h{}  frame {}",
                    bus.agnus.vpos,
                    bus.agnus.hpos,
                    bus.emulated_frames()
                )];
                for offs in [
                    0x002u16, 0x010, 0x01C, 0x01E, 0x096, 0x100, 0x102, 0x104, 0x108,
                ]
                .chunks(3)
                {
                    let mut row = String::new();
                    for &off in offs {
                        if let Some(value) = bus.debug_custom_word(off) {
                            row.push_str(&format!(
                                "{:<8} ${value:04X}   ",
                                crate::debugger::custom_reg_name(off)
                            ));
                        }
                    }
                    lines.push(row.trim_end().to_string());
                }
                ConsoleOutcome::lines(lines)
            }
            "POKE" => {
                let (Some(addr), Some(value)) = (
                    args.first().and_then(|t| hex32(t)),
                    args.get(1).and_then(|t| hex32(t)),
                ) else {
                    return ConsoleOutcome::error("usage: POKE ADDR VALUE (hex word)");
                };
                let addr = addr & !1;
                let written = self
                    .emu
                    .machine
                    .debug_write_memory(addr, &(value as u16).to_be_bytes());
                if written == 2 {
                    ConsoleOutcome::one(format!("poked ${:04X} -> ${addr:06X}", value as u16))
                } else {
                    ConsoleOutcome::error(format!("${addr:06X} is not writable RAM"))
                }
            }
            "SETREG" => {
                let (Some(reg), Some(value)) = (
                    args.first().and_then(|t| reg_index(t)),
                    args.get(1).and_then(|t| hex32(t)),
                ) else {
                    return ConsoleOutcome::error("usage: SETREG D0-D7|A0-A7|SP|SR|PC VALUE (hex)");
                };
                self.emu.machine.debug_set_register(reg, value);
                ConsoleOutcome::one(format!("{} <- ${value:X}", args[0].to_ascii_uppercase()))
            }
            "FIND" => {
                if args.is_empty() {
                    return ConsoleOutcome::error("usage: FIND HEXBYTES [START]");
                }
                // A trailing token that parses as an address is the start.
                let (pattern_tokens, start) = match args.split_last() {
                    Some((last, rest))
                        if !rest.is_empty() && parse_hex_pattern(&[last]).is_none() =>
                    {
                        match hex32(last) {
                            Some(addr) => (rest, addr),
                            None => (args, 0),
                        }
                    }
                    _ => (args, 0),
                };
                let Some(pattern) = parse_hex_pattern(pattern_tokens) else {
                    return ConsoleOutcome::error("FIND takes hex byte pairs (e.g. 4E75)");
                };
                match search_cpu_memory(&self.emu.machine, &pattern, start) {
                    Some(addr) => ConsoleOutcome::one(format!("found at ${addr:06X}")),
                    None => ConsoleOutcome::one("pattern not found"),
                }
            }
            "WRITER" => {
                let Some(addr) = args.first().and_then(|t| hex32(t)) else {
                    return ConsoleOutcome::error("usage: WRITER ADDR (hex, word)");
                };
                let addr = addr & 0x00FF_FFFE;
                let before = self.emu.retired_instructions();
                let outcome = match self.emu.tt_last_writer(addr, before) {
                    Ok(crate::timetravel::ReverseOutcome::Found(rec)) => {
                        ConsoleOutcome::one(format!(
                            "${:06X}: {:04X}->{:04X} by pc ${:06X} (frame {})",
                            rec.addr,
                            rec.old,
                            rec.new,
                            rec.pc & 0x00FF_FFFF,
                            rec.frame
                        ))
                    }
                    Ok(crate::timetravel::ReverseOutcome::NotFound) => {
                        ConsoleOutcome::one(format!("no write to ${addr:06X} in retained history"))
                    }
                    Ok(crate::timetravel::ReverseOutcome::BeyondHistory) => {
                        ConsoleOutcome::one(format!("write to ${addr:06X} predates history"))
                    }
                    Err(e) => ConsoleOutcome::error(format!("last-writer failed: {e:?}")),
                };
                self.finish_render_for_current_frame();
                outcome
            }
            _ => ConsoleOutcome::error(format!("unknown command {cmd} (try HELP)")),
        }
    }

    /// Run a bounded forward-execution operation and report where the
    /// machine ended up (stop reason first if one fired).
    fn console_exec_op(
        &mut self,
        op: impl FnOnce(&mut Self) -> anyhow::Result<()>,
    ) -> ConsoleOutcome {
        self.console_exec_report(|app| {
            op(app)?;
            Ok(None)
        })
    }

    /// Shared tail for execution commands: pause bookkeeping, the
    /// operation, stop reporting, and the display refresh. `op` may
    /// return an extra note line (budget exhaustion and the like).
    fn console_exec_report(
        &mut self,
        op: impl FnOnce(&mut Self) -> anyhow::Result<Option<String>>,
    ) -> ConsoleOutcome {
        self.paused = true;
        self.paused_before_console = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        let note = match op(self) {
            Ok(note) => note,
            Err(e) => {
                error!("console execution halted: {e:?}");
                self.cpu_halted = true;
                self.sync_live_audio_suspension();
                return ConsoleOutcome::error(format!("execution halted: {e}"));
            }
        };
        let mut lines = Vec::new();
        if let Some(stop) = self.emu.machine.take_ui_debug_stop() {
            let message = stop.describe();
            self.last_debug_stop = Some(message.clone());
            lines.push(format!("!{message}"));
        }
        if let Some(note) = note {
            lines.push(note);
        }
        lines.extend(self.console_status_lines());
        self.finish_render_for_current_frame();
        ConsoleOutcome::lines(lines)
    }

    fn console_run_to_beam(&mut self, vpos: u16, hpos: Option<u16>) -> ConsoleOutcome {
        self.console_exec_report(move |app| {
            let reached = app
                .emu
                .debug_run_to_beam(vpos, hpos, CONSOLE_RUN_TO_BUDGET)?;
            Ok((!reached).then(|| "beam target not reached (budget)".to_string()))
        })
    }

    /// Run a reverse operation and report the landing position.
    fn console_reverse_op<T>(
        &mut self,
        op: impl FnOnce(&mut Self) -> anyhow::Result<crate::timetravel::ReverseOutcome<T>>,
    ) -> ConsoleOutcome {
        use crate::timetravel::ReverseOutcome;
        self.paused = true;
        self.paused_before_console = true;
        self.sync_live_audio_suspension();
        self.last_debug_stop = None;
        let outcome = match op(self) {
            Ok(outcome) => outcome,
            Err(e) => {
                error!("console reverse op halted: {e:?}");
                return ConsoleOutcome::error(format!("reverse failed: {e}"));
            }
        };
        let mut lines = Vec::new();
        match outcome {
            ReverseOutcome::Found(_) => {}
            ReverseOutcome::NotFound => lines.push("reverse: nothing earlier to land on".into()),
            ReverseOutcome::BeyondHistory => lines.push("reverse: beyond recorded history".into()),
        }
        lines.extend(self.console_status_lines());
        self.finish_render_for_current_frame();
        ConsoleOutcome::lines(lines)
    }

    fn console_status_lines(&self) -> Vec<String> {
        let machine = &self.emu.machine;
        let bus = self.emu.bus();
        let pc = machine.pc();
        let cpu_type = machine.cpu_type();
        let (text, _) = crate::disasm::disassemble(|a| bus.peek_word_any(a), pc, cpu_type);
        vec![format!(
            "pc ${pc:06X}  {text}   sr {:04X}  beam v{} h{}  frame {}",
            machine.sr(),
            bus.agnus.vpos,
            bus.agnus.hpos,
            bus.emulated_frames(),
        )]
    }

    fn console_regs_lines(&self) -> Vec<String> {
        let machine = &self.emu.machine;
        let mut lines = Vec::with_capacity(3);
        for (label, read) in [
            (
                "D",
                Box::new(|n: usize| machine.d(n)) as Box<dyn Fn(usize) -> u32>,
            ),
            ("A", Box::new(|n: usize| machine.a(n))),
        ] {
            let row: Vec<String> = (0..8).map(|n| format!("{:08X}", read(n))).collect();
            lines.push(format!("{label}0-{label}7 {}", row.join(" ")));
        }
        lines.push(format!(
            "PC {:08X}  SR {:04X} [{}]{}",
            machine.pc(),
            machine.sr(),
            ui::sr_flags(machine.sr()),
            if machine.stopped() { "  STOPPED" } else { "" }
        ));
        lines
    }

    fn console_breaks_lines(&self) -> Vec<String> {
        let breaks = self.emu.machine.ui_breaks();
        let bus = self.emu.bus();
        let mut lines = Vec::new();
        let mut any = false;
        for bp in &breaks.breakpoints {
            let mut text = format!("break  ${:06X}", bp.addr);
            if let Some(cond) = &bp.cond {
                text.push_str(&format!("  {}", cond.describe()));
            }
            if bp.ignore > 0 {
                text.push_str(&format!("  ign {}/{}", bp.hits, bp.ignore));
            }
            lines.push(text);
            any = true;
        }
        for watch in &breaks.watches {
            lines.push(format!(
                "watch  ${:06X}  now {:04X}",
                watch.addr,
                bus.peek_word_any(watch.addr)
            ));
            any = true;
        }
        for off in &breaks.reg_watches {
            lines.push(format!(
                "rwatch {} (${off:03X})",
                crate::debugger::custom_reg_name(*off)
            ));
            any = true;
        }
        for vector in &breaks.catches {
            lines.push(format!(
                "catch  {} (vector {vector})",
                crate::debugger::exception_vector_name(*vector)
            ));
            any = true;
        }
        for trap in bus.ui_beam_traps() {
            lines.push(format!(
                "btrap  v{}{}{}",
                trap.vpos,
                trap.hpos.map(|h| format!(" h{h}")).unwrap_or_default(),
                if trap.once { "  once" } else { "" }
            ));
            any = true;
        }
        for addr in bus.ui_copper_breaks() {
            lines.push(format!("cbreak ${addr:06X}"));
            any = true;
        }
        if !any {
            lines.push("no breakpoints, watchpoints, traps, or catches".to_string());
        }
        lines
    }
}
