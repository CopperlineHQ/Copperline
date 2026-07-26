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

/// A running memory hunt (trainer-style delta search): the last
/// snapshot of every scanned region plus the surviving candidates.
pub(super) struct HuntState {
    /// Search width in bytes (1 or 2).
    width: u32,
    /// (base, bytes) snapshot of each scanned region at the last filter.
    snapshot: Vec<(u32, Vec<u8>)>,
    /// Surviving candidate addresses; None before the first filter
    /// (everything is still a candidate).
    candidates: Option<Vec<u32>>,
}

impl HuntState {
    fn value_in(bytes: &[u8], off: usize, width: u32) -> u32 {
        if width == 1 {
            u32::from(bytes[off])
        } else {
            (u32::from(bytes[off]) << 8) | u32::from(bytes[off + 1])
        }
    }

    fn candidate_count(&self) -> Option<usize> {
        self.candidates.as_ref().map(|c| c.len())
    }
}

/// How a hunt filter compares a candidate's current value.
enum HuntFilter {
    Cmp(std::cmp::Ordering, u32),
    NotEqual(u32),
    Same,
    Different,
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

/// Search one `[from, end)` span of CPU-visible memory for `pattern`.
///
/// Every read runs `pattern.len() - 1` bytes past its chunk, including the
/// last chunk of the span. That tail is deliberate twice over: it is what
/// lets a match straddle a chunk boundary, and at the end of a span it is
/// what lets a match straddle two banks that abut in the map (a full
/// motherboard bank ends at $08000000, exactly where the CPU-slot bank
/// begins). The tail only ever supplies trailing bytes -- the window count
/// is the chunk length, so a reported hit always starts inside
/// `[from, end)` -- and the bytes it reads are what the CPU would see
/// there, unmapped space included.
fn search_span(
    machine: &crate::cpu::M68kMachine,
    pattern: &[u8],
    from: u32,
    end: u64,
) -> Option<u32> {
    const CHUNK: usize = 4096;
    let mut addr = u64::from(from);
    while addr < end {
        let span = (end - addr) as usize;
        let bytes = machine.debug_read_memory(addr as u32, span.min(CHUNK) + pattern.len() - 1);
        if let Some(hit) = bytes
            .windows(pattern.len())
            .position(|window| window == pattern)
        {
            return Some((addr as u32).wrapping_add(hit as u32));
        }
        addr += CHUNK as u64;
    }
    None
}

/// Search CPU-visible memory for `pattern`, starting at `start` and
/// wrapping the decoded map once. `regions` is the machine's
/// [`crate::bus::Bus::searchable_regions`], so RAM above the 24-bit space
/// (motherboard, CPU-slot, and Zorro III banks) is covered and the
/// undecoded gaps between the banks are skipped. Shared by the console
/// FIND command and the Memory tab's Find button.
pub(super) fn search_cpu_memory(
    machine: &crate::cpu::M68kMachine,
    regions: &[(u32, u32)],
    pattern: &[u8],
    start: u32,
) -> Option<u32> {
    // Two sweeps: the map from `start` up to its top, then its bottom back
    // up to `start`, so the search wraps the whole map exactly once.
    for (base, len) in regions {
        let end = u64::from(*base) + u64::from(*len);
        let from = u64::from(start).max(u64::from(*base));
        if from < end {
            if let Some(hit) = search_span(machine, pattern, from as u32, end) {
                return Some(hit);
            }
        }
    }
    for (base, len) in regions {
        let end = (u64::from(*base) + u64::from(*len)).min(u64::from(start));
        if u64::from(*base) < end {
            if let Some(hit) = search_span(machine, pattern, *base, end) {
                return Some(hit);
            }
        }
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

/// Human-readable status lines for a waveform capture, shared by the
/// console WAVE command and the debugger window's Waveform tab.
pub(super) fn wave_status_lines(status: &crate::waveform::WaveStatus) -> Vec<String> {
    let mut lines = vec![
        format!(
            "waveform {}: trigger {}, duration {}, signals {}",
            status.state, status.trigger, status.duration, status.signals
        ),
        format!("  -> {}", status.path.display()),
    ];
    match status.state {
        "capturing" => lines.push(format!(
            "  {} / {} cck, {} samples",
            status.captured_cck,
            status.window_cck.unwrap_or(0),
            status.samples
        )),
        "done" => lines.push(format!(
            "  {} cck captured, {} samples",
            status.captured_cck, status.samples
        )),
        _ => {}
    }
    lines
}

const CONSOLE_HELP: &[&str] = &[
    "execution:  run  pause  step/s [N]  over  out  frame/f  line  cstep",
    "            runto ADDR   toslot V [H]   rstep [N]  rframe  rrun",
    "stops:      break/b ADDR [COND] [IGN N]   watch/w ADDR [CPU|BLITTER|DISK]",
    "            rwatch REG",
    "            btrap V [H]   cbreak ADDR   catch irq N|trap N|vec N",
    "            catchtask [NAME]   catchalert   breaks (list)   clearbreaks",
    "inspect:    status  regs/r  mem/m ADDR [BYTES]  dis/d [ADDR] [N]",
    "            copper [pc|ADDR] [N]   custom   blits   find HEX [START]",
    "            writer ADDR",
    "            history/h [N]   stack/bt",
    "os:         tasks  task [ADDR|NAME]  execbase  memlist  segments",
    "            libs  devs  resources  ports  guru [CODE]",
    "hunt:       hunt start [B|W]  hunt eq/ne/lt/gt VAL  hunt same|diff  hunt list",
    "modify:     poke ADDR VAL   setreg REG VAL   trace start [PATH]|stop",
    "waveform:   wave start [PATH] [TRIGGER] [DURATION] [SIGNALS]   wave stop   wave",
    "            TRIGGER: now  pc=ADDR  beam=V[:H]  reg=OFF  time=SECS",
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
    /// caller applies the outcome so borrows stay simple. Arguments keep
    /// their case (file paths); every parser is case-insensitive.
    fn console_execute(&mut self, line: &str) -> ConsoleOutcome {
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
            "RRUN" | "RC" => {
                use crate::timetravel::ReverseOutcome;
                self.paused = true;
                self.paused_before_console = true;
                self.sync_live_audio_suspension();
                self.last_debug_stop = None;
                let mut lines = Vec::new();
                match self.emu.tt_reverse_continue() {
                    Ok(ReverseOutcome::Found((_, reason))) => {
                        self.last_debug_stop = Some(reason.clone());
                        lines.push(format!("!{reason}"));
                    }
                    Ok(ReverseOutcome::NotFound) => {
                        lines.push("reverse: no earlier stop hit".to_string())
                    }
                    Ok(ReverseOutcome::BeyondHistory) => {
                        lines.push("reverse: beyond recorded history".to_string())
                    }
                    Err(e) => {
                        error!("console reverse run halted: {e:?}");
                        return ConsoleOutcome::error(format!("reverse failed: {e}"));
                    }
                }
                lines.extend(self.console_status_lines());
                self.finish_render_for_current_frame();
                ConsoleOutcome::lines(lines)
            }
            "BREAK" | "B" => {
                let spec = args.join(" ").to_ascii_uppercase();
                let Some((addr, cond, ignore)) = ui::parse_break_spec(&spec) else {
                    return ConsoleOutcome::error("usage: BREAK ADDR [LHS OP RHS] [IGN N]");
                };
                let set = self.emu.machine.ui_set_breakpoint(addr, cond, ignore);
                ConsoleOutcome::one(format!(
                    "breakpoint ${:06X} {}",
                    addr & self.emu.machine.ui_addr_mask(),
                    if set { "set" } else { "removed" }
                ))
            }
            "WATCH" | "W" => {
                let Some(addr) = args.first().and_then(|t| hex32(t)) else {
                    return ConsoleOutcome::error("usage: WATCH ADDR [CLASS] [PC=ADDR]");
                };
                // Trailing PC=ADDR qualifies the watch to one instruction;
                // the remaining token, if any, is the access class.
                let mut filter = None;
                let mut pc = None;
                // Each qualifier may appear once. Letting a later token
                // win would make `WATCH addr CPU BLITTER` install a
                // blitter watch with no sign that the CPU was ignored.
                for token in args.iter().skip(1) {
                    if let Some(value) = token
                        .strip_prefix("PC=")
                        .or_else(|| token.strip_prefix("pc="))
                    {
                        if pc.is_some() {
                            return ConsoleOutcome::error("PC= given more than once");
                        }
                        match hex32(value) {
                            Some(addr) => pc = Some(addr),
                            None => return ConsoleOutcome::error("PC= wants an address"),
                        }
                        continue;
                    }
                    match crate::debugger::WatchSource::parse(token) {
                        Some(_) if filter.is_some() => {
                            return ConsoleOutcome::error("watch class given more than once")
                        }
                        Some(source) => filter = Some(source),
                        None => {
                            return ConsoleOutcome::error(
                                "watch class is CPU, BLITTER, DISK, COPPER, or a DMA \
                                 channel (BPL1..BPL8, SPR0..SPR7, AUD0..AUD3)",
                            )
                        }
                    }
                }
                // Only the CPU has an instruction behind an access, so a
                // channel filter paired with PC= describes something that
                // cannot happen and would never fire.
                if pc.is_some() && filter.is_some_and(|f| !f.takes_pc_qualifier()) {
                    return ConsoleOutcome::error(
                        "PC= only qualifies CPU accesses; a DMA engine's access has no \
                         instruction behind it",
                    );
                }
                let set = self.emu.machine.ui_toggle_watch_qualified(addr, filter, pc);
                ConsoleOutcome::one(format!(
                    "watchpoint ${:06X}{}{} {}",
                    addr & self.emu.machine.ui_addr_mask() & !1,
                    filter
                        .map(|f| format!(" ({} accesses only)", f.describe()))
                        .unwrap_or_default(),
                    pc.map(|pc| format!(" from ${pc:06X} only"))
                        .unwrap_or_default(),
                    if set { "set" } else { "removed" }
                ))
            }
            "RWATCH" | "RW" => {
                let Some(off) = args
                    .first()
                    .and_then(|t| crate::gdbstub::parse_custom_reg(&t.to_ascii_uppercase()))
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
            "TASKS" => {
                ConsoleOutcome::lines(self.console_with_exec(crate::amigaos::dump::task_list))
            }
            "TASK" => ConsoleOutcome::lines(self.console_task(args)),
            "EXECBASE" | "EXEC" => {
                ConsoleOutcome::lines(self.console_with_exec(crate::amigaos::dump::exec))
            }
            "MEMLIST" | "AVAIL" => {
                ConsoleOutcome::lines(self.console_with_exec(crate::amigaos::dump::memory))
            }
            "LIBS" | "LIBRARIES" => {
                ConsoleOutcome::lines(self.console_os_list(crate::amigaos::OsList::Libraries))
            }
            "DEVS" | "DEVICES" => {
                ConsoleOutcome::lines(self.console_os_list(crate::amigaos::OsList::Devices))
            }
            "RESOURCES" => {
                ConsoleOutcome::lines(self.console_os_list(crate::amigaos::OsList::Resources))
            }
            "PORTS" => ConsoleOutcome::lines(self.console_os_list(crate::amigaos::OsList::Ports)),
            "SEGMENTS" => ConsoleOutcome::lines(self.console_segments()),
            "CATCHTASK" => {
                if args.is_empty() {
                    self.emu.machine.ui_set_task_catch(None);
                    return ConsoleOutcome::one("task catch cleared");
                }
                let target = args.join(" ");
                self.emu.machine.ui_set_task_catch(Some(target.clone()));
                ConsoleOutcome::one(format!(
                    "stopping when a task whose name contains \"{target}\" is scheduled"
                ))
            }
            "CATCHALERT" => {
                let lvo = self.console_with_exec(|_, base| {
                    // exec's Alert() lives at LVO -108; the jump-table
                    // entry itself executes, so a PC breakpoint there
                    // fires on every alert with D7 = the guru code.
                    vec![format!(
                        "{:06X}",
                        base.wrapping_sub(108) & self.emu.machine.ui_addr_mask()
                    )]
                });
                let Some(addr) = lvo
                    .first()
                    .filter(|line| !line.starts_with('!'))
                    .and_then(|line| u32::from_str_radix(line, 16).ok())
                else {
                    return ConsoleOutcome::lines(lvo);
                };
                let set = self.emu.machine.ui_set_breakpoint(addr, None, 0);
                ConsoleOutcome::one(if set {
                    format!(
                        "break at exec Alert() (${addr:06X}); on stop D7 holds the code -- GURU decodes it"
                    )
                } else {
                    format!("alert catch removed (${addr:06X})")
                })
            }
            "GURU" => {
                let code = match args.first() {
                    Some(token) => match hex32(token) {
                        Some(code) => code,
                        None => return ConsoleOutcome::error("usage: GURU [HEXCODE] (default D7)"),
                    },
                    None => self.emu.machine.d(7),
                };
                ConsoleOutcome::one(format!(
                    "{code:08X}: {}",
                    crate::debugger::guru_decode(code)
                ))
            }
            "BREAKS" | "INFO" => ConsoleOutcome::lines(self.console_breaks_lines()),
            "CLEARBREAKS" => {
                self.emu.machine.ui_breaks_clear();
                self.last_debug_stop = None;
                ConsoleOutcome::one("cleared all breakpoints, watchpoints, traps, and catches")
            }
            "REGS" | "R" => ConsoleOutcome::lines(self.console_regs_lines()),
            "HISTORY" | "H" => {
                let count = args
                    .first()
                    .and_then(|t| t.parse::<usize>().ok())
                    .unwrap_or(16)
                    .clamp(1, crate::cpu::UI_PC_HISTORY_CAP);
                let history = self.emu.machine.ui_pc_history();
                if history.is_empty() {
                    return ConsoleOutcome::one(
                        "no history yet (recorded while a debug window is open)",
                    );
                }
                let cpu_type = self.emu.machine.cpu_type();
                let bus = self.emu.bus();
                ConsoleOutcome::lines(
                    history
                        .iter()
                        .rev()
                        .take(count)
                        .rev()
                        .map(|&pc| {
                            let (text, _) =
                                crate::disasm::disassemble(|a| bus.peek_word_any(a), pc, cpu_type);
                            format!("{pc:06X}  {text}")
                        })
                        .collect(),
                )
            }
            "STACK" | "BT" => ConsoleOutcome::lines(self.console_stack_lines()),
            "MEM" | "M" => {
                let Some(addr) = args.first().and_then(|t| hex32(t)) else {
                    return ConsoleOutcome::error("usage: MEM ADDR [BYTES] (hex)");
                };
                let len = args
                    .get(1)
                    .and_then(|t| hex32(t))
                    .unwrap_or(0x40)
                    .clamp(1, 0x400) as usize;
                let base = addr & !0xF;
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
            "HUNT" => self.console_hunt(args),
            "TRACE" => {
                const TRACE_LINE_CAP: u64 = 1_000_000;
                let sub = args.first().map(|t| t.to_ascii_uppercase());
                match sub.as_deref() {
                    Some("START") => {
                        let path = match args.get(1) {
                            Some(path) => std::path::PathBuf::from(path),
                            None => {
                                let stamp = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0);
                                std::path::PathBuf::from(format!("copperline-trace-{stamp}.txt"))
                            }
                        };
                        match self
                            .emu
                            .machine
                            .ui_trace_start(path.clone(), TRACE_LINE_CAP)
                        {
                            Ok(()) => ConsoleOutcome::one(format!(
                                "tracing to {} (cap {TRACE_LINE_CAP} lines; TRACE STOP ends it)",
                                path.display()
                            )),
                            Err(e) => ConsoleOutcome::error(format!(
                                "cannot open {}: {e}",
                                path.display()
                            )),
                        }
                    }
                    Some("STOP") => match self.emu.machine.ui_trace_stop() {
                        Some((path, lines)) => ConsoleOutcome::one(format!(
                            "trace stopped: {lines} lines in {}",
                            path.display()
                        )),
                        None => ConsoleOutcome::one("no trace running"),
                    },
                    None => match self.emu.machine.ui_trace_status() {
                        Some((path, lines)) => ConsoleOutcome::one(format!(
                            "tracing to {} ({lines} lines so far)",
                            path.display()
                        )),
                        None => ConsoleOutcome::one("no trace running (TRACE START [PATH])"),
                    },
                    Some(_) => ConsoleOutcome::error("usage: TRACE [START [PATH] | STOP]"),
                }
            }
            "WAVE" | "WAVEFORM" => {
                let sub = args.first().map(|t| t.to_ascii_uppercase());
                match sub.as_deref() {
                    Some("START") => {
                        let opts = match crate::waveform::parse_wave_args(args[1..].iter().copied())
                        {
                            Ok(opts) => opts,
                            Err(e) => return ConsoleOutcome::error(format!("WAVE START: {e}")),
                        };
                        let summary = format!(
                            "waveform armed: trigger {}, duration {}, signals {} -> {}",
                            opts.trigger,
                            opts.duration,
                            opts.signals,
                            opts.path.display()
                        );
                        match self.emu.machine.ui_wave_start(opts) {
                            Ok(()) => ConsoleOutcome::one(summary),
                            Err(e) => ConsoleOutcome::error(format!("cannot arm waveform: {e}")),
                        }
                    }
                    Some("STOP") => match self.emu.machine.ui_wave_stop() {
                        Some(status) => ConsoleOutcome::one(format!(
                            "waveform stopped: {} samples in {}",
                            status.samples,
                            status.path.display()
                        )),
                        None => ConsoleOutcome::one("no waveform capture"),
                    },
                    None => match self.emu.machine.ui_wave_status() {
                        Some(status) => ConsoleOutcome::lines(wave_status_lines(&status)),
                        None => ConsoleOutcome::one("no waveform capture (WAVE START [PATH] ...)"),
                    },
                    Some(_) => ConsoleOutcome::error(
                        "usage: WAVE [START [PATH] [TRIGGER] [DURATION] [SIGNALS] | STOP]",
                    ),
                }
            }
            "BLITS" => {
                let Some(trace) = self.emu.bus().frame_bus_trace() else {
                    return ConsoleOutcome::one(
                        "no frame trace: open the Frame Analyzer (its Frame button captures one)",
                    );
                };
                if trace.blits.is_empty() {
                    return ConsoleOutcome::one(format!(
                        "no blits started in frame {}",
                        trace.frame
                    ));
                }
                let mut lines = vec![format!(
                    "{} blit(s) in frame {}:",
                    trace.blits.len(),
                    trace.frame
                )];
                for (i, blit) in trace.blits.iter().enumerate() {
                    let end = match blit.end {
                        Some((v, h)) => format!("v{v} h{h}"),
                        None => "(running)".to_string(),
                    };
                    let c1 = blit.bltcon1;
                    lines.push(format!(
                        "#{i:<2} v{} h{} -> {end}  con0 {:04X} con1 {:04X}  {}x{}{}{}{}",
                        blit.start.0,
                        blit.start.1,
                        blit.bltcon0,
                        c1,
                        blit.width_words,
                        blit.height,
                        if c1 & 0x0001 != 0 { "  LINE" } else { "" },
                        if c1 & 0x0008 != 0 { "  FILL" } else { "" },
                        if c1 & 0x0002 != 0 { "  DESC" } else { "" },
                    ));
                    lines.push(format!(
                        "     A ${:06X}  B ${:06X}  C ${:06X}  D ${:06X}",
                        blit.apt & 0x00FF_FFFF,
                        blit.bpt & 0x00FF_FFFF,
                        blit.cpt & 0x00FF_FFFF,
                        blit.dpt & 0x00FF_FFFF,
                    ));
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
                // Through the machine's address bus, as the Memory tab's
                // Find does: the sweep must start where the reads will
                // land, or a START past a 24-bit bus would skip the
                // whole map and silently restart from the bottom.
                let start = start & self.emu.machine.ui_addr_mask();
                let regions = self.emu.bus().searchable_regions();
                match search_cpu_memory(&self.emu.machine, &regions, &pattern, start) {
                    Some(addr) => ConsoleOutcome::one(format!("found at ${addr:06X}")),
                    None => ConsoleOutcome::one("pattern not found"),
                }
            }
            "WRITER" => {
                let Some(addr) = args.first().and_then(|t| hex32(t)) else {
                    return ConsoleOutcome::error("usage: WRITER ADDR (hex, word)");
                };
                let addr = addr & self.emu.machine.ui_addr_mask() & !1;
                let before = self.emu.retired_instructions();
                let outcome = match self.emu.tt_last_writer(addr, before) {
                    Ok(crate::timetravel::ReverseOutcome::Found(rec)) => {
                        ConsoleOutcome::one(format!(
                            "${:06X}: {:04X}->{:04X} by pc ${:06X} (frame {})",
                            rec.addr,
                            rec.old,
                            rec.new,
                            rec.pc & self.emu.machine.ui_addr_mask(),
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

    /// The HUNT command family: a trainer-style delta search over
    /// writable RAM. START snapshots, EQ/NE/LT/GT/SAME/DIFF filter the
    /// candidates against live memory (then re-snapshot), LIST shows
    /// survivors, OFF clears.
    fn console_hunt(&mut self, args: &[&str]) -> ConsoleOutcome {
        let sub = args.first().map(|t| t.to_ascii_uppercase());
        match sub.as_deref() {
            None => match &self.hunt {
                Some(hunt) => ConsoleOutcome::one(format!(
                    "hunt active ({}-bit): {}",
                    hunt.width * 8,
                    match hunt.candidate_count() {
                        Some(n) => format!("{n} candidate(s)"),
                        None => "no filter applied yet".to_string(),
                    }
                )),
                None => ConsoleOutcome::one("no hunt (HUNT START [B|W] begins one)"),
            },
            Some("OFF") => {
                self.hunt = None;
                ConsoleOutcome::one("hunt cleared")
            }
            Some("START") => {
                let width = match args.get(1).map(|t| t.to_ascii_uppercase()).as_deref() {
                    Some("B") => 1,
                    Some("W") | None => 2,
                    Some(_) => return ConsoleOutcome::error("usage: HUNT START [B|W]"),
                };
                let snapshot = self.console_hunt_snapshot();
                let total: usize = snapshot.iter().map(|(_, bytes)| bytes.len()).sum();
                self.hunt = Some(HuntState {
                    width,
                    snapshot,
                    candidates: None,
                });
                ConsoleOutcome::one(format!(
                    "hunting {}-bit values across {total} bytes of RAM; filter with \
                     HUNT EQ/NE/LT/GT VALUE or HUNT SAME/DIFF",
                    width * 8
                ))
            }
            Some("LIST") => {
                let Some(hunt) = &self.hunt else {
                    return ConsoleOutcome::one("no hunt running");
                };
                let Some(candidates) = &hunt.candidates else {
                    return ConsoleOutcome::one("no filter applied yet");
                };
                let cap = args
                    .get(1)
                    .and_then(|t| t.parse::<usize>().ok())
                    .unwrap_or(16)
                    .clamp(1, 64);
                let width = hunt.width;
                let mut lines = vec![format!("{} candidate(s):", candidates.len())];
                for &addr in candidates.iter().take(cap) {
                    let bytes = self.emu.machine.debug_read_memory(addr, width as usize);
                    let value = HuntState::value_in(&bytes, 0, width);
                    lines.push(format!(
                        "  ${addr:06X} = {value:0w$X}",
                        w = width as usize * 2
                    ));
                }
                if candidates.len() > cap {
                    lines.push(format!("  ... {} more", candidates.len() - cap));
                }
                ConsoleOutcome::lines(lines)
            }
            Some(op @ ("EQ" | "NE" | "LT" | "GT" | "SAME" | "DIFF")) => {
                let filter = match op {
                    "EQ" => match args.get(1).and_then(|t| hex32(t)) {
                        Some(v) => HuntFilter::Cmp(std::cmp::Ordering::Equal, v),
                        None => return ConsoleOutcome::error("usage: HUNT EQ VALUE (hex)"),
                    },
                    "NE" => match args.get(1).and_then(|t| hex32(t)) {
                        Some(v) => HuntFilter::NotEqual(v),
                        None => return ConsoleOutcome::error("usage: HUNT NE VALUE (hex)"),
                    },
                    "LT" => match args.get(1).and_then(|t| hex32(t)) {
                        Some(v) => HuntFilter::Cmp(std::cmp::Ordering::Less, v),
                        None => return ConsoleOutcome::error("usage: HUNT LT VALUE (hex)"),
                    },
                    "GT" => match args.get(1).and_then(|t| hex32(t)) {
                        Some(v) => HuntFilter::Cmp(std::cmp::Ordering::Greater, v),
                        None => return ConsoleOutcome::error("usage: HUNT GT VALUE (hex)"),
                    },
                    "SAME" => HuntFilter::Same,
                    _ => HuntFilter::Different,
                };
                self.console_hunt_filter(filter)
            }
            Some(_) => ConsoleOutcome::error(
                "usage: HUNT [START [B|W] | EQ/NE/LT/GT VALUE | SAME | DIFF | LIST [N] | OFF]",
            ),
        }
    }

    fn console_hunt_snapshot(&self) -> Vec<(u32, Vec<u8>)> {
        self.emu
            .bus()
            .writable_ram_regions()
            .into_iter()
            .map(|(base, len)| (base, self.emu.machine.debug_read_memory(base, len as usize)))
            .collect()
    }

    fn console_hunt_filter(&mut self, filter: HuntFilter) -> ConsoleOutcome {
        let Some(mut hunt) = self.hunt.take() else {
            return ConsoleOutcome::one("no hunt running (HUNT START first)");
        };
        let current = self.console_hunt_snapshot();
        let width = hunt.width;
        let survives = |old_value: u32, new_value: u32| -> bool {
            match &filter {
                HuntFilter::Cmp(ordering, v) => new_value.cmp(v) == *ordering,
                HuntFilter::NotEqual(v) => new_value != *v,
                HuntFilter::Same => new_value == old_value,
                HuntFilter::Different => new_value != old_value,
            }
        };
        let mut next: Vec<u32> = Vec::new();
        match hunt.candidates.take() {
            Some(candidates) => {
                for addr in candidates {
                    let old_value = Self::hunt_value_at(&hunt.snapshot, addr, width);
                    let new_value = Self::hunt_value_at(&current, addr, width);
                    if let (Some(old_value), Some(new_value)) = (old_value, new_value) {
                        if survives(old_value, new_value) {
                            next.push(addr);
                        }
                    }
                }
            }
            None => {
                // First filter: scan everything.
                for ((base, old_bytes), (_, new_bytes)) in hunt.snapshot.iter().zip(current.iter())
                {
                    let step = width as usize;
                    let mut off = 0;
                    while off + step <= old_bytes.len() {
                        let old_value = HuntState::value_in(old_bytes, off, width);
                        let new_value = HuntState::value_in(new_bytes, off, width);
                        if survives(old_value, new_value) {
                            next.push(base + off as u32);
                        }
                        off += step;
                    }
                }
            }
        }
        let count = next.len();
        hunt.candidates = Some(next);
        hunt.snapshot = current;
        self.hunt = Some(hunt);
        ConsoleOutcome::one(format!(
            "{count} candidate(s) remain{}",
            if count > 0 && count <= 16 {
                "  (HUNT LIST shows them)"
            } else {
                ""
            }
        ))
    }

    fn hunt_value_at(snapshot: &[(u32, Vec<u8>)], addr: u32, width: u32) -> Option<u32> {
        for (base, bytes) in snapshot {
            if addr >= *base && (addr - base) as usize + width as usize <= bytes.len() {
                return Some(HuntState::value_in(bytes, (addr - base) as usize, width));
            }
        }
        None
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

    /// SEGMENTS: the current process's loaded hunks (its CLI command's
    /// segment list when there is one), plus the add-symbol-file line a
    /// source-level GDB session needs.
    fn console_segments(&self) -> Vec<String> {
        match crate::amigaos::segments_on_bus(self.emu.bus()) {
            Err(reason) => vec![format!("!{reason}")],
            Ok(segs) if segs.is_empty() => {
                vec!["current task has no walkable segment list".to_string()]
            }
            Ok(segs) => {
                let mut lines = Vec::new();
                for (i, seg) in segs.iter().enumerate() {
                    lines.push(format!(
                        "hunk {i}: ${:06X}..${:06X}  ({} bytes)",
                        seg.start,
                        seg.start + seg.size,
                        seg.size
                    ));
                }
                lines.push(format!(
                    "gdb: add-symbol-file prog.elf 0x{:X}",
                    segs[0].start
                ));
                lines
            }
        }
    }

    /// Run `walk` against a validated ExecBase using peeks over the bus,
    /// or report why the OS structures are not walkable.
    fn console_with_exec(
        &self,
        walk: impl FnOnce(&crate::amigaos::OsMemory, u32) -> Vec<String>,
    ) -> Vec<String> {
        let bus = self.emu.bus();
        let peek8 = |addr: u32| {
            let word = bus.peek_word_any(addr & !1);
            if addr & 1 == 0 {
                (word >> 8) as u8
            } else {
                word as u8
            }
        };
        let peek32 = |addr: u32| {
            (u32::from(bus.peek_word_any(addr)) << 16)
                | u32::from(bus.peek_word_any(addr.wrapping_add(2)))
        };
        let os = crate::amigaos::OsMemory {
            peek8: &peek8,
            peek32: &peek32,
        };
        match os.exec_base() {
            Ok(base) => walk(&os, base),
            Err(reason) => vec![format!("!{reason}")],
        }
    }

    /// TASK [ADDR|NAME]: one task or process in full, defaulting to the
    /// scheduled one. The CPU's live stack pointers go with it, since
    /// they are the running task's real stack pointer.
    fn console_task(&self, args: &[&str]) -> Vec<String> {
        let spec = args.join(" ");
        let sp = crate::amigaos::dump::LiveSp {
            a7: self.emu.machine.a(7),
            usp: self.emu.machine.usp(),
        };
        self.console_with_exec(|os, base| crate::amigaos::dump::task(os, base, &spec, sp))
    }

    /// LIBS/DEVS/RESOURCES/PORTS: one line per node.
    fn console_os_list(&self, list: crate::amigaos::OsList) -> Vec<String> {
        let library_shaped = matches!(
            list,
            crate::amigaos::OsList::Libraries | crate::amigaos::OsList::Devices
        );
        self.console_with_exec(|os, base| {
            let nodes = os.walk(base, list);
            if nodes.is_empty() {
                return vec!["(empty list)".to_string()];
            }
            nodes
                .iter()
                .map(|node| {
                    if library_shaped {
                        format!(
                            "${:06X}  v{}.{:<4} {}",
                            node.addr, node.version, node.revision, node.name
                        )
                    } else {
                        format!("${:06X}  pri {:>4}  {}", node.addr, node.pri, node.name)
                    }
                })
                .collect()
        })
    }

    /// Heuristic 68k call-stack walk: scan up the stack for longwords
    /// that look like return addresses (even, on the CPU's address bus,
    /// and immediately preceded by a JSR or BSR encoding). Heuristic by
    /// nature -- data words that happen to follow call opcodes can slip
    /// in -- but each frame shows its stack slot so it can be judged.
    fn console_stack_lines(&self) -> Vec<String> {
        const SLOTS: u32 = 64;
        const FRAMES: usize = 8;
        let machine = &self.emu.machine;
        let bus = self.emu.bus();
        let peek16 = |addr: u32| bus.peek_word_any(addr);
        let peek32 =
            |addr: u32| (u32::from(peek16(addr)) << 16) | u32::from(peek16(addr.wrapping_add(2)));
        // A0-A23 on the 24-bit models, the full 32 bits on 020+, so code
        // running from motherboard, CPU-slot, or Zorro III RAM is walked
        // rather than rejected. Unmapped words peek as 0, which matches no
        // JSR/BSR encoding, so the opcode test still does the filtering.
        let addr_mask = machine.ui_addr_mask();
        let looks_like_return = |addr: u32| -> bool {
            if addr == 0 || addr & 1 != 0 || addr & addr_mask != addr {
                return false;
            }
            // JSR (An)/-(An)+modes and BSR.B end 2 bytes before the
            // return address; JSR abs.w/d16/d8-index/PC-rel and BSR.W end
            // 4 before; JSR abs.l and BSR.L (020+) end 6 before.
            let w2 = peek16(addr.wrapping_sub(2));
            if (0x4E90..=0x4E97).contains(&w2)
                || (w2 & 0xFF00 == 0x6100 && w2 & 0x00FF != 0 && w2 & 0x00FF != 0xFF)
            {
                return true;
            }
            let w4 = peek16(addr.wrapping_sub(4));
            if w4 == 0x6100
                || w4 == 0x4EB8
                || w4 == 0x4EBA
                || w4 == 0x4EBB
                || (0x4EA8..=0x4EB7).contains(&w4)
            {
                return true;
            }
            let w6 = peek16(addr.wrapping_sub(6));
            w6 == 0x4EB9 || w6 == 0x61FF
        };
        let sp = machine.a(7);
        let cpu_type = machine.cpu_type();
        let mut lines = vec![format!(
            "#0 pc ${:06X}  sp ${:06X}",
            machine.pc() & machine.ui_addr_mask(),
            sp & machine.ui_addr_mask()
        )];
        let mut frame = 1usize;
        for slot in 0..SLOTS {
            if frame > FRAMES {
                break;
            }
            let slot_addr = sp.wrapping_add(slot * 4);
            let value = peek32(slot_addr) & machine.ui_addr_mask();
            if !looks_like_return(value) {
                continue;
            }
            let (text, _) = crate::disasm::disassemble(|a| bus.peek_word_any(a), value, cpu_type);
            lines.push(format!(
                "#{frame} ret ${value:06X}  (at sp+{:03X})  {text}",
                slot * 4
            ));
            frame += 1;
        }
        if lines.len() == 1 {
            lines.push("no return-address candidates on the stack".to_string());
        }
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
                "watch  ${:06X}  now {:04X}{}",
                watch.addr,
                bus.peek_word_any(watch.addr),
                watch
                    .filter
                    .map(|f| format!("  [{} only]", f.label()))
                    .unwrap_or_default()
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
        if let Some(target) = &breaks.task_catch {
            lines.push(format!("ctask  name contains \"{target}\""));
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
