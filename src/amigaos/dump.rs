// SPDX-License-Identifier: GPL-3.0-or-later

//! Text dumps of the exec structures, one line per `Vec` entry, shared
//! by the debugger console (`TASKS`, `TASK`, `EXECBASE`, `MEMLIST`) and
//! the GDB stub's matching `monitor` commands so both interfaces report
//! the same thing. A line beginning with `!` is an error the caller
//! should surface as such; everything else is data.
//!
//! Every dump starts from a validated ExecBase and reads only through
//! [`OsMemory`]'s side-effect-free peeks, so it is safe to run at any
//! point in a session -- including before the OS exists, where the
//! validation failure is what gets printed.

use super::{OsList, OsMemory, TaskInfo};

/// EXECBASE: exec's own state -- the scheduler counters and nesting
/// counts that say what exec is doing, then the machine facts it
/// recorded at boot.
pub fn exec(os: &OsMemory, base: u32) -> Vec<String> {
    let info = os.exec_info(base);
    let flags = |names: Vec<&str>| {
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(" ")
        }
    };
    let nesting = |count: i8, enabled: &str, disabled: &str| {
        if count < 0 {
            enabled.to_string()
        } else {
            format!("{disabled}, nesting {}", i32::from(count) + 1)
        }
    };
    let mut lines = vec![
        format!(
            "ExecBase ${:06X}  exec.library v{}.{}  SoftVer {}",
            info.base, info.version, info.revision, info.soft_ver
        ),
        format!(
            "sched  IdleCount {}  DispCount {}  Quantum {}  Elapsed {}",
            info.idle_count, info.disp_count, info.quantum, info.elapsed
        ),
        format!(
            "sched  SysFlags ${:04X} ({})  AttnResched ${:04X}",
            info.sys_flags,
            flags(super::sys_flag_names(info.sys_flags)),
            info.attn_resched
        ),
        format!(
            "sched  IDNestCnt {} ({})",
            info.id_nest_cnt,
            nesting(info.id_nest_cnt, "interrupts enabled", "Disable()d")
        ),
        format!(
            "sched  TDNestCnt {} ({})",
            info.td_nest_cnt,
            nesting(info.td_nest_cnt, "task switching enabled", "Forbid()den")
        ),
        format!(
            "cpu    AttnFlags ${:04X} ({})",
            info.attn_flags,
            flags(super::attn_flag_names(info.attn_flags))
        ),
        format!(
            "task   ThisTask ${:06X}  {}",
            info.this_task,
            if info.this_task_name.is_empty() {
                "<not plausible>"
            } else {
                &info.this_task_name
            }
        ),
        format!(
            "task   TaskSigAlloc ${:08X}  TaskTrapAlloc ${:04X}",
            info.task_sig_alloc, info.task_trap_alloc
        ),
        format!(
            "mem    MaxLocMem ${:06X}  MaxExtMem ${:06X}",
            info.max_loc_mem, info.max_ext_mem
        ),
        format!(
            "mem    SysStk ${:06X}-${:06X}  ResModules ${:06X}",
            info.sys_stk_lower, info.sys_stk_upper, info.res_modules
        ),
        format!(
            "time   VBlank {} Hz  PSU {} Hz  EClock {}",
            info.vblank_freq,
            info.power_freq,
            match info.eclock_freq {
                Some(hz) => format!("{hz} Hz"),
                None => "n/a (pre-V36 ExecBase)".to_string(),
            }
        ),
        format!(
            "capt   Cold ${:06X}  Cool ${:06X}  Warm ${:06X}",
            info.cold_capture, info.cool_capture, info.warm_capture
        ),
    ];
    lines.push(match info.last_alert[0] {
        // Exec parks -1 in LastAlert[0] when nothing has alerted; both
        // Kickstart 3.1 and AROS boot with it that way.
        0xFFFF_FFFF => "alert  none since reset".to_string(),
        // A zero is not that sentinel: it is the field as a cold reset
        // left it, or (in principle) an Alert(0). Nothing distinguishes
        // the two from the field alone and no subsystem raises code 0,
        // so it is shown raw rather than decoded or called "none".
        0 => "alert  LastAlert $00000000".to_string(),
        code => format!("alert  ${code:08X}: {}", crate::debugger::guru_decode(code)),
    });
    lines
}

/// TASKS: the scheduled task (marked `>`) plus the ready and waiting
/// lists, one line each.
///
/// The `>` line shows `ThisTask`'s own `tc_State`, so an idle machine reads
/// `wait` there and lists the task again below: exec leaves `ThisTask`
/// naming the task it dispatched last.
pub fn task_list(os: &OsMemory, base: u32) -> Vec<String> {
    let mut lines = Vec::new();
    match os.this_task(base) {
        Some(task) => {
            let state = super::task_state_name(task.state);
            let state = if state == "?" { "this" } else { state };
            lines.push(format!(
                "> ${:06X}  pri {:>4}  {:<7} {}",
                task.addr, task.pri, state, task.name
            ))
        }
        None => lines.push("!ThisTask is not plausible".to_string()),
    }
    for (list, label) in [(OsList::TaskReady, "ready"), (OsList::TaskWait, "wait")] {
        for node in os.walk(base, list) {
            let state = super::task_state_name(node.state);
            let state = if state == "?" { label } else { state };
            lines.push(format!(
                "  ${:06X}  pri {:>4}  {:<7} {}",
                node.addr, node.pri, state, node.name
            ));
        }
    }
    if lines.len() == 1 {
        lines.push("  (no other tasks)".to_string());
    }
    lines
}

/// The CPU's live stack pointers, which are what the running task's
/// stack pointer really is: exec tasks run in user mode, so USP is the
/// task's own SP whenever the snapshot lands in an interrupt or
/// exception, and A7 (which then addresses the supervisor stack) is it
/// otherwise -- in user mode the two are the same register. A task that
/// runs supervisor-mode code is unusual but legal, so both are offered
/// and whichever lands inside the task's stack is the one reported.
#[derive(Clone, Copy)]
pub struct LiveSp {
    pub a7: u32,
    pub usp: u32,
}

impl LiveSp {
    /// The live stack pointer to report for `task`, with the register
    /// name it came from.
    fn pick(self, task: &TaskInfo) -> (u32, &'static str) {
        if task.stack_usage(self.usp).is_some() || task.stack_usage(self.a7).is_none() {
            (self.usp, "live USP")
        } else {
            (self.a7, "live A7")
        }
    }
}

/// TASK [ADDR|NAME]: one task or process in full. An empty `spec` dumps
/// ExecBase->ThisTask; a `$`-prefixed spec is always an address; any
/// other text is matched case-insensitively against the task names exec
/// knows, falling back to a bare hex address when nothing matches.
pub fn task(os: &OsMemory, base: u32, spec: &str, cpu_sp: LiveSp) -> Vec<String> {
    let spec = spec.trim();
    let hex = |token: &str| u32::from_str_radix(token.trim_start_matches('$'), 16).ok();
    let single_token = !spec.contains(char::is_whitespace);
    let this_task = os.exec_info(base).this_task;
    let addr = match spec
        .strip_prefix('$')
        .filter(|_| single_token)
        .and_then(hex)
    {
        Some(addr) => addr,
        None if spec.is_empty() => this_task,
        None => {
            let matches = os.find_tasks(base, spec);
            match matches[..] {
                [] => match hex(spec).filter(|_| single_token) {
                    Some(addr) => addr,
                    None => return vec![format!("!no task matches \"{spec}\"")],
                },
                [addr] => addr,
                _ => {
                    let mut lines = vec![format!("\"{spec}\" matches several tasks:")];
                    for addr in matches {
                        lines.push(format!("  ${addr:06X}  {}", os.node_name(addr)));
                    }
                    lines.push("dump one with TASK $ADDR".to_string());
                    return lines;
                }
            }
        }
    };
    if !super::plausible_ptr(addr) {
        return vec![format!(
            "!${addr:06X} is not where a task structure can live"
        )];
    }
    let info = os.task_info(addr);
    // tc_SPReg is only current for a task exec has switched away from;
    // the running task's stack pointer is live in the CPU.
    let live_sp = (addr == this_task).then(|| cpu_sp.pick(&info));
    task_lines(os, &info, live_sp)
}

/// Format one `struct Task` (and its process half, when it has one).
/// `live_sp` is the CPU's stack pointer (and the register it came from)
/// when this is the running task, whose tc_SPReg is stale until exec
/// switches away from it.
fn task_lines(os: &OsMemory, task: &TaskInfo, live_sp: Option<(u32, &str)>) -> Vec<String> {
    let flag_names = super::task_flag_names(task.flags);
    let mut lines = vec![
        format!(
            "task ${:06X}  {}  ({})  pri {}  state {}",
            task.addr,
            task.name,
            super::node_type_name(task.node_type),
            task.pri,
            super::task_state_name(task.state)
        ),
        format!(
            "  flags  ${:02X} ({})  IDNestCnt {}  TDNestCnt {}",
            task.flags,
            if flag_names.is_empty() {
                "none".to_string()
            } else {
                flag_names.join(" ")
            },
            task.id_nest_cnt,
            task.td_nest_cnt
        ),
        format!(
            "  sigs   alloc ${:08X}  wait ${:08X}  recvd ${:08X}",
            task.sig_alloc, task.sig_wait, task.sig_recvd
        ),
        format!(
            "  sigs   except ${:08X}  {}",
            task.sig_except,
            match task.etask {
                // TF_ETASK: the trap words are an ETask pointer instead.
                Some(etask) => format!("ETask ${etask:06X}"),
                None => format!(
                    "trap alloc ${:04X} able ${:04X}",
                    task.trap_alloc, task.trap_able
                ),
            }
        ),
        format!(
            "  vecs   trap ${:06X}/${:06X}  except ${:06X}/${:06X}",
            task.trap_code, task.trap_data, task.except_code, task.except_data
        ),
        format!(
            "  vecs   switch ${:06X}  launch ${:06X}  userdata ${:06X}",
            task.switch_fn, task.launch_fn, task.user_data
        ),
    ];
    let (sp, source) = live_sp.unwrap_or((task.sp_reg, "SPReg"));
    lines.push(match task.stack_usage(sp) {
        Some((size, used)) => format!(
            "  stack  ${:06X}-${:06X} ({size} bytes)  sp ${sp:06X} ({source}), {used} used",
            task.sp_lower, task.sp_upper
        ),
        None => format!(
            "  stack  ${:06X}-${:06X}  sp ${sp:06X} ({source}, outside the stack?)",
            task.sp_lower, task.sp_upper
        ),
    });
    let Some(proc) = &task.process else {
        return lines;
    };
    lines.push(format!(
        "  proc   CLI {}  {}  StackSize {}  Result2 {}",
        proc.task_num,
        if proc.command.is_empty() {
            "<no command>".to_string()
        } else {
            format!("\"{}\"", proc.command)
        },
        proc.stack_size,
        proc.result2
    ));
    if proc.cli != 0 {
        lines.push(format!(
            "  cli    ${:06X}  rc {}  fail {}  {}  stack {}",
            proc.cli << 2,
            proc.return_code,
            proc.fail_level,
            if proc.background {
                "background"
            } else {
                "foreground"
            },
            proc.default_stack
        ));
        if !proc.set_name.is_empty() {
            lines.push(format!("  cli    dir \"{}\"", proc.set_name));
        }
    }
    lines.push(format!(
        "  dos    CurrentDir ${:06X}  HomeDir ${:06X}  StackBase ${:06X}",
        proc.current_dir << 2,
        proc.home_dir << 2,
        proc.stack_base << 2
    ));
    lines.push(format!(
        "  dos    ConsoleTask ${:06X}{}  WindowPtr ${:06X}",
        proc.console_task,
        // The console handler is a message port, so it carries a name;
        // a null or wild pointer just gets no name rather than garbage.
        if super::plausible_ptr(proc.console_task) {
            format!(" {}", os.node_name(proc.console_task))
        } else {
            String::new()
        },
        proc.window_ptr
    ));
    if proc.seglist != 0 {
        lines.push(format!("  seg    BPTR ${:06X}", proc.seglist << 2));
        for (i, seg) in proc.segments.iter().enumerate() {
            lines.push(format!(
                "  hunk {i}  ${:06X}..${:06X}  ({} bytes)",
                seg.start,
                seg.start + seg.size,
                seg.size
            ));
        }
    }
    lines
}

/// MEMLIST: exec's memory list, two lines per MemHeader plus a total.
pub fn memory(os: &OsMemory, base: u32) -> Vec<String> {
    let regions = os.mem_list(base);
    if regions.is_empty() {
        return vec!["(exec has no memory list yet)".to_string()];
    }
    let mut lines = Vec::new();
    let (mut total, mut total_free) = (0u64, 0u64);
    for region in &regions {
        total += u64::from(region.size());
        total_free += u64::from(region.free);
        lines.push(format!(
            "${:06X}-${:06X}  {:>8} free of {:>8}  pri {:>4}  {}",
            region.lower,
            region.upper,
            region.free,
            region.size(),
            region.pri,
            region.name
        ));
        lines.push(format!(
            "    attrs ${:04X} ({})  largest {}  chunks {}",
            region.attributes,
            match super::mem_attr_names(region.attributes) {
                names if names.is_empty() => "none".to_string(),
                names => names.join(" "),
            },
            region.largest,
            region.chunks
        ));
    }
    lines.push(format!(
        "total  {total_free} free of {total} bytes in {} region{}",
        regions.len(),
        if regions.len() == 1 { "" } else { "s" }
    ));
    lines
}
