// SPDX-License-Identifier: GPL-3.0-or-later

//! Read-only AmigaOS (exec.library) structure walking for the debugger:
//! the task, library, device, resource, and port lists, ExecBase's own
//! scheduler state, one task or process in full, and the memory list,
//! all reached from ExecBase via side-effect-free memory peeks. The
//! offsets are exec's public ABI (execbase.h / nodes.h / lists.h /
//! tasks.h / memory.h), stable from Kickstart 1.x through 3.x and AROS,
//! so no version sniffing is needed -- only pointer plausibility checks,
//! since the OS may simply not be up yet.

pub mod dos;
pub mod dump;

use std::collections::HashMap;

/// ExecBase field offsets (execbase.h).
const EXECBASE_PTR: u32 = 4;
const SOFT_VER: u32 = 0x22;
const CHKBASE: u32 = 0x26;
const COLD_CAPTURE: u32 = 0x2A;
const COOL_CAPTURE: u32 = 0x2E;
const WARM_CAPTURE: u32 = 0x32;
const SYS_STK_UPPER: u32 = 0x36;
const SYS_STK_LOWER: u32 = 0x3A;
const MAX_LOC_MEM: u32 = 0x3E;
const MAX_EXT_MEM: u32 = 0x4E;
const THIS_TASK: u32 = 0x114;
const IDLE_COUNT: u32 = 0x118;
const DISP_COUNT: u32 = 0x11C;
const QUANTUM: u32 = 0x120;
const ELAPSED: u32 = 0x122;
const SYS_FLAGS: u32 = 0x124;
const ID_NEST_CNT: u32 = 0x126;
const TD_NEST_CNT: u32 = 0x127;
const ATTN_FLAGS: u32 = 0x128;
const ATTN_RESCHED: u32 = 0x12A;
const RES_MODULES: u32 = 0x12C;
const TASK_SIG_ALLOC: u32 = 0x13C;
const TASK_TRAP_ALLOC: u32 = 0x140;
const MEM_LIST: u32 = 0x142;
const RESOURCE_LIST: u32 = 0x150;
const DEVICE_LIST: u32 = 0x15E;
const INTR_LIST: u32 = 0x16C;
const LIB_LIST: u32 = 0x17A;
const PORT_LIST: u32 = 0x188;
const TASK_READY: u32 = 0x196;
const TASK_WAIT: u32 = 0x1A4;
const LAST_ALERT: u32 = 0x202;
const VBLANK_FREQUENCY: u32 = 0x212;
const POWER_SUPPLY_FREQUENCY: u32 = 0x213;
/// V36+ only; reads as garbage on a 1.x ExecBase, so it is only
/// reported when it looks like a real E-clock rate.
const ECLOCK_FREQUENCY: u32 = 0x238;

/// Node field offsets (nodes.h).
const LN_TYPE: u32 = 8;
const LN_PRI: u32 = 9;
const LN_NAME: u32 = 10;
/// Task field offsets (tasks.h).
const TC_FLAGS: u32 = 14;
const TC_STATE: u32 = 15;
const TC_ID_NEST_CNT: u32 = 0x10;
const TC_TD_NEST_CNT: u32 = 0x11;
const TC_SIG_ALLOC: u32 = 0x12;
const TC_SIG_WAIT: u32 = 0x16;
const TC_SIG_RECVD: u32 = 0x1A;
const TC_SIG_EXCEPT: u32 = 0x1E;
/// tc_TrapAlloc/tc_TrapAble, or the ETask pointer that overlays them
/// (tc_UnionETask) once TF_ETASK says the task has one -- which is how
/// AROS and OS 4-era exec keep per-task extended state.
const TC_TRAP_ALLOC: u32 = 0x22;
const TC_TRAP_ABLE: u32 = 0x24;
const TF_ETASK: u8 = 0x08;
const TC_EXCEPT_DATA: u32 = 0x26;
const TC_EXCEPT_CODE: u32 = 0x2A;
const TC_TRAP_DATA: u32 = 0x2E;
const TC_TRAP_CODE: u32 = 0x32;
const TC_SP_REG: u32 = 0x36;
const TC_SP_LOWER: u32 = 0x3A;
const TC_SP_UPPER: u32 = 0x3E;
const TC_SWITCH: u32 = 0x42;
const TC_LAUNCH: u32 = 0x46;
const TC_USER_DATA: u32 = 0x58;
/// Library field offsets (libraries.h).
const LIB_VERSION: u32 = 20;
const LIB_REVISION: u32 = 22;
/// MemHeader field offsets (memory.h).
const MH_ATTRIBUTES: u32 = 0x0E;
const MH_FIRST: u32 = 0x10;
const MH_LOWER: u32 = 0x14;
const MH_UPPER: u32 = 0x18;
const MH_FREE: u32 = 0x1C;
/// MemChunk: mc_Next, then mc_Bytes.
const MC_BYTES: u32 = 4;

/// Nodes returned per list, bounding walks over corrupt lists.
const LIST_CAP: usize = 200;
const NAME_CAP: usize = 30;

/// One node lifted out of an exec list.
pub struct OsNode {
    pub addr: u32,
    pub name: String,
    pub node_type: u8,
    pub pri: i8,
    /// tc_State for task lists, 0 otherwise.
    pub state: u8,
    /// lib_Version/lib_Revision for library-shaped nodes, 0 otherwise.
    pub version: u16,
    pub revision: u16,
}

/// Which exec list to walk.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OsList {
    TaskReady,
    TaskWait,
    Libraries,
    Devices,
    Resources,
    Ports,
    Interrupts,
}

impl OsList {
    fn execbase_offset(self) -> u32 {
        match self {
            OsList::TaskReady => TASK_READY,
            OsList::TaskWait => TASK_WAIT,
            OsList::Libraries => LIB_LIST,
            OsList::Devices => DEVICE_LIST,
            OsList::Resources => RESOURCE_LIST,
            OsList::Ports => PORT_LIST,
            OsList::Interrupts => INTR_LIST,
        }
    }

    fn library_shaped(self) -> bool {
        matches!(self, OsList::Libraries | OsList::Devices)
    }

    fn task_shaped(self) -> bool {
        matches!(self, OsList::TaskReady | OsList::TaskWait)
    }
}

/// Human name of a tc_State value (tasks.h).
pub fn task_state_name(state: u8) -> &'static str {
    match state {
        0 => "invalid",
        1 => "added",
        2 => "run",
        3 => "ready",
        4 => "wait",
        5 => "except",
        6 => "removed",
        _ => "?",
    }
}

/// Human name of an ln_Type value (nodes.h).
pub fn node_type_name(node_type: u8) -> &'static str {
    match node_type {
        0 => "unknown",
        1 => "task",
        2 => "interrupt",
        3 => "device",
        4 => "msgport",
        5 => "message",
        6 => "freemsg",
        7 => "replymsg",
        8 => "resource",
        9 => "library",
        10 => "memory",
        11 => "softint",
        12 => "font",
        13 => "process",
        14 => "semaphore",
        15 => "signalsem",
        16 => "bootnode",
        17 => "kickmem",
        18 => "graphics",
        19 => "deathmessage",
        254 => "user",
        255 => "extended",
        _ => "?",
    }
}

/// Set tc_Flags bit names (tasks.h).
pub fn task_flag_names(flags: u8) -> Vec<&'static str> {
    const NAMES: [(u8, &str); 6] = [
        (0x01, "PROCTIME"),
        (0x08, "ETASK"),
        (0x10, "STACKCHK"),
        (0x20, "EXCEPT"),
        (0x40, "SWITCH"),
        (0x80, "LAUNCH"),
    ];
    NAMES
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// Set AttnFlags bit names: the CPU and FPU exec detected at boot
/// (execbase.h). Bit 7 is the 68060 flag the 68060 support libraries
/// and OS 3.5+ set; Commodore's own includes leave it reserved.
pub fn attn_flag_names(flags: u16) -> Vec<&'static str> {
    const NAMES: [(u16, &str); 8] = [
        (0x0001, "68010"),
        (0x0002, "68020"),
        (0x0004, "68030"),
        (0x0008, "68040"),
        (0x0010, "68881"),
        (0x0020, "68882"),
        (0x0040, "FPU40"),
        (0x0080, "68060"),
    ];
    NAMES
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// Set SysFlags bit names: exec's private scheduler attention bits.
pub fn sys_flag_names(flags: u16) -> Vec<&'static str> {
    const NAMES: [(u16, &str); 3] = [
        (0x2000, "SINT"), // a software interrupt is pending
        (0x4000, "TQE"),  // the running task's quantum expired
        (0x8000, "SAR"),  // scheduling attention required
    ];
    NAMES
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// Set MemHeader mh_Attributes bit names (memory.h).
pub fn mem_attr_names(attrs: u16) -> Vec<&'static str> {
    const NAMES: [(u16, &str); 6] = [
        (0x0001, "PUBLIC"),
        (0x0002, "CHIP"),
        (0x0004, "FAST"),
        (0x0100, "LOCAL"),
        (0x0200, "24BITDMA"),
        (0x0400, "KICK"),
    ];
    NAMES
        .iter()
        .filter(|(bit, _)| attrs & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// Heuristic: even and in an address range where exec structures can
/// live -- chip+Z2 RAM, slow RAM, the big-box motherboard and CPU-slot
/// fast-RAM windows, or Zorro III space (OS 3.2+ SetPatch moves ExecBase
/// to fast RAM, which on a big-box machine is well above the 24-bit space).
pub(crate) fn plausible_ptr(addr: u32) -> bool {
    addr & 1 == 0
        && ((0x100..0x00A0_0000).contains(&addr)           // chip + Z2 fast
            || (0x00C0_0000..0x00D8_0000).contains(&addr)  // slow
            || (0x0400_0000..0x0800_0000).contains(&addr)  // A3000/A4000 fast
            || (0x0800_0000..0x1000_0000).contains(&addr)  // CPU-slot RAM
            || (0x1000_0000..0x8000_0000).contains(&addr)) // Zorro III
}

/// The same test for a BPTR (an address shifted right by two, as AmigaDOS
/// stores pointers). The pointed-at address must be longword aligned, so
/// the shift is exact; BPTRs past a quarter of the address space cannot
/// name a real address at all and are rejected before the shift.
fn plausible_bptr(bptr: u32) -> bool {
    bptr < 0x4000_0000 && plausible_ptr(bptr << 2)
}

/// Read-only view of guest memory, built from the debugger's peek
/// primitives. `peek8`/`peek32` must be side-effect-free.
pub struct OsMemory<'a> {
    pub peek8: &'a dyn Fn(u32) -> u8,
    pub peek32: &'a dyn Fn(u32) -> u32,
}

impl OsMemory<'_> {
    fn peek16(&self, addr: u32) -> u16 {
        ((self.peek32)(addr) >> 16) as u16
    }

    /// ExecBase, validated: the pointer at address 4 must be plausible
    /// and its ChkBase complement should match (a mismatch usually means
    /// the OS is not up yet). Err carries a human-readable reason.
    pub fn exec_base(&self) -> Result<u32, String> {
        let base = (self.peek32)(EXECBASE_PTR);
        if !plausible_ptr(base) {
            return Err(format!(
                "no ExecBase (address 4 holds ${base:08X}; OS not booted?)"
            ));
        }
        let chk = (self.peek32)(base.wrapping_add(CHKBASE));
        if chk != !base {
            return Err(format!(
                "ExecBase ${base:06X} fails its ChkBase complement (OS booting or trashed?)"
            ));
        }
        Ok(base)
    }

    /// The currently scheduled task, with its name.
    pub fn this_task(&self, execbase: u32) -> Option<OsNode> {
        let task = (self.peek32)(execbase.wrapping_add(THIS_TASK));
        plausible_ptr(task).then(|| self.node(task, true, false))
    }

    /// Walk one exec list into owned nodes (bounded).
    pub fn walk(&self, execbase: u32, list: OsList) -> Vec<OsNode> {
        let mut out = Vec::new();
        let mut node = (self.peek32)(execbase.wrapping_add(list.execbase_offset()));
        while plausible_ptr(node) && (self.peek32)(node) != 0 && out.len() < LIST_CAP {
            out.push(self.node(node, list.task_shaped(), list.library_shaped()));
            node = (self.peek32)(node);
        }
        out
    }

    fn node(&self, addr: u32, task_shaped: bool, library_shaped: bool) -> OsNode {
        let (version, revision) = if library_shaped {
            (
                self.peek16(addr.wrapping_add(LIB_VERSION)),
                self.peek16(addr.wrapping_add(LIB_REVISION)),
            )
        } else {
            (0, 0)
        };
        OsNode {
            addr,
            name: self.node_name(addr),
            node_type: (self.peek8)(addr.wrapping_add(LN_TYPE)),
            pri: (self.peek8)(addr.wrapping_add(LN_PRI)) as i8,
            state: if task_shaped {
                (self.peek8)(addr.wrapping_add(TC_STATE))
            } else {
                0
            },
            version,
            revision,
        }
    }

    /// A node's ln_Name string: printable ASCII, bounded, with a
    /// placeholder for null/implausible name pointers.
    pub fn node_name(&self, node: u32) -> String {
        let name_ptr = (self.peek32)(node.wrapping_add(LN_NAME));
        // Names may live in ROM or at odd addresses, so no plausible_ptr
        // here; unmapped pointers read as zero bytes and fall through to
        // the <unnamed> placeholder below.
        if name_ptr == 0 {
            return "<unnamed>".to_string();
        }
        let mut name = String::new();
        for i in 0..NAME_CAP as u32 {
            let byte = (self.peek8)(name_ptr.wrapping_add(i));
            if byte == 0 {
                break;
            }
            name.push(if (0x20..0x7F).contains(&byte) {
                byte as char
            } else {
                '.'
            });
        }
        if name.is_empty() {
            "<unnamed>".to_string()
        } else {
            name
        }
    }
}

/// ExecBase's own state: the scheduler fields that say what exec is
/// doing right now, plus the machine facts it recorded at boot. Only
/// what a dump wants is lifted; the rest stays a memory peek away.
pub struct ExecInfo {
    pub base: u32,
    /// exec.library's own lib_Version/lib_Revision and SoftVer.
    pub version: u16,
    pub revision: u16,
    pub soft_ver: u16,
    /// Scheduler state (the fields that move on every dispatch).
    pub idle_count: u32,
    pub disp_count: u32,
    pub quantum: u16,
    pub elapsed: u16,
    pub sys_flags: u16,
    /// Disable()/Forbid() nesting. -1 means enabled; 0 and up is the
    /// nesting depth, so a positive IDNestCnt means interrupts are off.
    pub id_nest_cnt: i8,
    pub td_nest_cnt: i8,
    pub attn_flags: u16,
    pub attn_resched: u16,
    /// ThisTask and its name (empty when the pointer is implausible).
    pub this_task: u32,
    pub this_task_name: String,
    /// Signals and traps exec hands newly created tasks.
    pub task_sig_alloc: u32,
    pub task_trap_alloc: u16,
    /// Memory exec sized at boot, and its own supervisor stack.
    pub max_loc_mem: u32,
    pub max_ext_mem: u32,
    pub sys_stk_lower: u32,
    pub sys_stk_upper: u32,
    /// Timing, as exec measured it. `eclock_freq` is a V36+ field and is
    /// None on a 1.x ExecBase, where the longword is something else.
    pub vblank_freq: u8,
    pub power_freq: u8,
    pub eclock_freq: Option<u32>,
    /// The last alert exec put up (LastAlert[4]).
    pub last_alert: [u32; 4],
    /// Reset-survival vectors and the resident list.
    pub cold_capture: u32,
    pub cool_capture: u32,
    pub warm_capture: u32,
    pub res_modules: u32,
}

/// One MemHeader off ExecBase's MemList, with its free list summarized.
pub struct MemRegion {
    pub addr: u32,
    pub name: String,
    pub pri: i8,
    pub attributes: u16,
    pub lower: u32,
    pub upper: u32,
    /// mh_Free as exec maintains it.
    pub free: u32,
    /// Largest free chunk and the chunk count, walked from mh_First.
    pub largest: u32,
    pub chunks: usize,
}

impl MemRegion {
    /// Bytes between mh_Lower and mh_Upper, 0 for a nonsensical header.
    pub fn size(&self) -> u32 {
        self.upper.saturating_sub(self.lower)
    }
}

/// Free chunks walked per MemHeader, bounding a corrupt free list.
const CHUNK_CAP: usize = 4096;

impl OsMemory<'_> {
    /// ExecBase's scheduler and machine state.
    pub fn exec_info(&self, base: u32) -> ExecInfo {
        let peek32 = |off: u32| (self.peek32)(base.wrapping_add(off));
        let peek16 = |off: u32| self.peek16(base.wrapping_add(off));
        let peek8 = |off: u32| (self.peek8)(base.wrapping_add(off));
        let this_task = peek32(THIS_TASK);
        // PAL 709379 Hz, NTSC 715909 Hz; anything far off that is a 1.x
        // ExecBase whose structure simply ends before this field.
        let eclock = peek32(ECLOCK_FREQUENCY);
        ExecInfo {
            base,
            version: peek16(LIB_VERSION),
            revision: peek16(LIB_REVISION),
            soft_ver: peek16(SOFT_VER),
            idle_count: peek32(IDLE_COUNT),
            disp_count: peek32(DISP_COUNT),
            quantum: peek16(QUANTUM),
            elapsed: peek16(ELAPSED),
            sys_flags: peek16(SYS_FLAGS),
            id_nest_cnt: peek8(ID_NEST_CNT) as i8,
            td_nest_cnt: peek8(TD_NEST_CNT) as i8,
            attn_flags: peek16(ATTN_FLAGS),
            attn_resched: peek16(ATTN_RESCHED),
            this_task,
            this_task_name: if plausible_ptr(this_task) {
                self.node_name(this_task)
            } else {
                String::new()
            },
            task_sig_alloc: peek32(TASK_SIG_ALLOC),
            task_trap_alloc: peek16(TASK_TRAP_ALLOC),
            max_loc_mem: peek32(MAX_LOC_MEM),
            max_ext_mem: peek32(MAX_EXT_MEM),
            sys_stk_lower: peek32(SYS_STK_LOWER),
            sys_stk_upper: peek32(SYS_STK_UPPER),
            vblank_freq: peek8(VBLANK_FREQUENCY),
            power_freq: peek8(POWER_SUPPLY_FREQUENCY),
            eclock_freq: (600_000..1_000_000).contains(&eclock).then_some(eclock),
            last_alert: [
                peek32(LAST_ALERT),
                peek32(LAST_ALERT + 4),
                peek32(LAST_ALERT + 8),
                peek32(LAST_ALERT + 12),
            ],
            cold_capture: peek32(COLD_CAPTURE),
            cool_capture: peek32(COOL_CAPTURE),
            warm_capture: peek32(WARM_CAPTURE),
            res_modules: peek32(RES_MODULES),
        }
    }

    /// Walk ExecBase's MemList, summarizing each region's free list.
    pub fn mem_list(&self, execbase: u32) -> Vec<MemRegion> {
        let mut out = Vec::new();
        let mut node = (self.peek32)(execbase.wrapping_add(MEM_LIST));
        while plausible_ptr(node) && (self.peek32)(node) != 0 && out.len() < LIST_CAP {
            let (largest, chunks) = self.free_chunks(node);
            out.push(MemRegion {
                addr: node,
                name: self.node_name(node),
                pri: (self.peek8)(node.wrapping_add(LN_PRI)) as i8,
                attributes: self.peek16(node.wrapping_add(MH_ATTRIBUTES)),
                lower: (self.peek32)(node.wrapping_add(MH_LOWER)),
                upper: (self.peek32)(node.wrapping_add(MH_UPPER)),
                free: (self.peek32)(node.wrapping_add(MH_FREE)),
                largest,
                chunks,
            });
            node = (self.peek32)(node);
        }
        out
    }

    /// (largest chunk, chunk count) of one MemHeader's free list. Chunks
    /// must sit inside the header's own bounds and ascend, which is what
    /// exec guarantees and what stops a trashed list from looping.
    fn free_chunks(&self, header: u32) -> (u32, usize) {
        let lower = (self.peek32)(header.wrapping_add(MH_LOWER));
        let upper = (self.peek32)(header.wrapping_add(MH_UPPER));
        let mut chunk = (self.peek32)(header.wrapping_add(MH_FIRST));
        let (mut largest, mut count, mut prev) = (0u32, 0usize, 0u32);
        while chunk >= lower && chunk < upper && chunk > prev && count < CHUNK_CAP {
            largest = largest.max((self.peek32)(chunk.wrapping_add(MC_BYTES)));
            count += 1;
            prev = chunk;
            chunk = (self.peek32)(chunk);
        }
        (largest, count)
    }
}

/// One hunk of a loaded DOS segment list.
pub struct Segment {
    /// First byte of the hunk's payload (code/data).
    pub start: u32,
    /// Payload bytes (the loader's 8-byte size/next header excluded).
    pub size: u32,
}

/// Process field offsets (dos/dosextens.h). pr_SegList/pr_CLI and the
/// directory and stack fields are BPTRs.
const PR_SEGLIST: u32 = 0x80;
const PR_STACK_SIZE: u32 = 0x84;
const PR_TASK_NUM: u32 = 0x8C;
const PR_STACK_BASE: u32 = 0x90;
const PR_RESULT2: u32 = 0x94;
const PR_CURRENT_DIR: u32 = 0x98;
const PR_CONSOLE_TASK: u32 = 0xA4;
const PR_CLI: u32 = 0xAC;
const PR_WINDOW_PTR: u32 = 0xB8;
const PR_HOME_DIR: u32 = 0xBC;
/// CommandLineInterface field offsets: cli_Module is the BPTR to the
/// currently running command's segment list, cli_CommandName the BSTR
/// naming the command it belongs to.
const CLI_SET_NAME: u32 = 0x04;
const CLI_RETURN_CODE: u32 = 0x0C;
const CLI_COMMAND_NAME: u32 = 0x10;
const CLI_FAIL_LEVEL: u32 = 0x14;
const CLI_BACKGROUND: u32 = 0x2C;
const CLI_DEFAULT_STACK: u32 = 0x34;
const CLI_MODULE: u32 = 0x3C;
/// NT_PROCESS in ln_Type.
const NT_PROCESS: u8 = 13;

impl OsMemory<'_> {
    /// The segment list of the program running in `task` (a Process):
    /// the CLI's loaded command when there is one, else the process's
    /// own creation seglist (entry 3 of the pr_SegList array).
    pub fn process_seglist(&self, task: u32) -> Option<u32> {
        if (self.peek8)(task.wrapping_add(LN_TYPE)) != NT_PROCESS {
            return None;
        }
        let cli = (self.peek32)(task.wrapping_add(PR_CLI));
        if plausible_bptr(cli) {
            let module = (self.peek32)((cli << 2).wrapping_add(CLI_MODULE));
            if module != 0 {
                return Some(module);
            }
        }
        let array = (self.peek32)(task.wrapping_add(PR_SEGLIST));
        if !plausible_bptr(array) {
            return None;
        }
        let count = (self.peek32)(array << 2);
        if count < 3 {
            return None;
        }
        let seg = (self.peek32)((array << 2).wrapping_add(3 * 4));
        (seg != 0).then_some(seg)
    }

    /// Walk a BPTR segment list into (start, size) hunks. Each segment's
    /// BPTR addresses a next-pointer longword, preceded by the loader's
    /// size longword; the payload follows the next pointer.
    pub fn walk_seglist(&self, mut bptr: u32) -> Vec<Segment> {
        let mut out = Vec::new();
        while plausible_bptr(bptr) && out.len() < 64 {
            let addr = bptr << 2;
            let size = (self.peek32)(addr.wrapping_sub(4));
            let next = (self.peek32)(addr);
            out.push(Segment {
                start: addr.wrapping_add(4),
                size: size.saturating_sub(8),
            });
            bptr = next;
        }
        out
    }

    /// The currently scheduled process's loaded segments, when it is a
    /// process with a walkable seglist.
    pub fn current_process_segments(&self, execbase: u32) -> Vec<Segment> {
        let task = (self.peek32)(execbase.wrapping_add(THIS_TASK));
        if !plausible_ptr(task) {
            return Vec::new();
        }
        match self.process_seglist(task) {
            Some(seglist) => self.walk_seglist(seglist),
            None => Vec::new(),
        }
    }

    /// Read a BSTR (BPTR to a length-prefixed string): printable ASCII,
    /// bounded by `cap`, empty for implausible pointers.
    pub fn read_bstr(&self, bptr: u32, cap: usize) -> String {
        if !plausible_bptr(bptr) {
            return String::new();
        }
        let addr = bptr << 2;
        let len = (self.peek8)(addr) as usize;
        let mut out = String::new();
        for i in 0..len.min(cap) as u32 {
            let byte = (self.peek8)(addr.wrapping_add(1 + i));
            out.push(if (0x20..0x7F).contains(&byte) {
                byte as char
            } else {
                '.'
            });
        }
        out
    }

    /// The name of the program a process is running: the basename of
    /// cli_CommandName when the task has a CLI, else the task's ln_Name.
    pub fn process_command_name(&self, task: u32) -> String {
        if (self.peek8)(task.wrapping_add(LN_TYPE)) == NT_PROCESS {
            let cli = (self.peek32)(task.wrapping_add(PR_CLI));
            if plausible_bptr(cli) {
                let name_bptr = (self.peek32)((cli << 2).wrapping_add(CLI_COMMAND_NAME));
                // A full AmigaDOS path fits in a 255-byte BSTR.
                let name = self.read_bstr(name_bptr, 255);
                let base = command_basename(&name);
                if !base.is_empty() {
                    return base.to_string();
                }
            }
        }
        self.node_name(task)
    }

    /// The addresses of every task exec knows about right now: ThisTask
    /// plus the ready and waiting lists (bounded like `walk`).
    pub fn task_addrs(&self, execbase: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let this = (self.peek32)(execbase.wrapping_add(THIS_TASK));
        if plausible_ptr(this) {
            out.push(this);
        }
        for list in [TASK_READY, TASK_WAIT] {
            let mut node = (self.peek32)(execbase.wrapping_add(list));
            while plausible_ptr(node) && (self.peek32)(node) != 0 && out.len() < LIST_CAP {
                out.push(node);
                node = (self.peek32)(node);
            }
        }
        out
    }
}

/// The basename of an AmigaDOS path: the text after the last `/` or the
/// volume/device colon.
pub fn command_basename(path: &str) -> &str {
    path.rsplit(['/', ':']).next().unwrap_or(path)
}

/// One task in full: struct Task as exec keeps it (tasks.h), plus the
/// process half when ln_Type says the structure continues.
pub struct TaskInfo {
    pub addr: u32,
    pub name: String,
    pub node_type: u8,
    pub pri: i8,
    pub flags: u8,
    pub state: u8,
    /// Per-task Disable()/Forbid() nesting, saved across a switch.
    pub id_nest_cnt: i8,
    pub td_nest_cnt: i8,
    pub sig_alloc: u32,
    pub sig_wait: u32,
    pub sig_recvd: u32,
    pub sig_except: u32,
    pub trap_alloc: u16,
    pub trap_able: u16,
    /// tc_UnionETask: with TF_ETASK set, the two trap words are really a
    /// pointer to the task's ETask, and `trap_alloc`/`trap_able` are the
    /// halves of that pointer rather than trap masks.
    pub etask: Option<u32>,
    pub except_code: u32,
    pub except_data: u32,
    pub trap_code: u32,
    pub trap_data: u32,
    /// tc_SPReg is the stack pointer exec saved at the last switch; for
    /// the running task the live A7 is the truthful one.
    pub sp_reg: u32,
    pub sp_lower: u32,
    pub sp_upper: u32,
    pub switch_fn: u32,
    pub launch_fn: u32,
    pub user_data: u32,
    pub process: Option<ProcessInfo>,
}

/// The DOS half of a Process, and the CLI behind it when it has one.
pub struct ProcessInfo {
    /// pr_TaskNum: the CLI number, 0 for a non-CLI process.
    pub task_num: i32,
    pub stack_size: u32,
    /// pr_StackBase (BPTR) and the IoErr() value pr_Result2.
    pub stack_base: u32,
    pub result2: i32,
    /// BPTR locks and pointers worth showing raw.
    pub current_dir: u32,
    pub home_dir: u32,
    pub console_task: u32,
    pub window_ptr: u32,
    pub seglist: u32,
    /// The CLI, when the process has one: its BPTR, the command it is
    /// running, and the shell state a crash investigation wants.
    pub cli: u32,
    pub command: String,
    pub set_name: String,
    pub return_code: i32,
    pub fail_level: i32,
    pub background: bool,
    pub default_stack: u32,
    /// Hunks of the program the process is running, when walkable.
    pub segments: Vec<Segment>,
}

impl TaskInfo {
    /// (stack size, bytes in use) measured from `sp`, or None when the
    /// bounds are not a sane stack. Pass the live A7 for the running
    /// task; tc_SPReg is stale until exec switches away from it.
    pub fn stack_usage(&self, sp: u32) -> Option<(u32, u32)> {
        if self.sp_lower == 0 || self.sp_upper <= self.sp_lower {
            return None;
        }
        let size = self.sp_upper - self.sp_lower;
        // A stack larger than 16 MiB is a misread, not a stack.
        if size > 0x0100_0000 || !(self.sp_lower..=self.sp_upper).contains(&sp) {
            return None;
        }
        Some((size, self.sp_upper - sp))
    }
}

impl OsMemory<'_> {
    /// Read one task (or process) structure. The caller decides that
    /// `addr` is worth reading; nothing here writes or faults.
    pub fn task_info(&self, addr: u32) -> TaskInfo {
        let peek32 = |off: u32| (self.peek32)(addr.wrapping_add(off));
        let peek16 = |off: u32| self.peek16(addr.wrapping_add(off));
        let peek8 = |off: u32| (self.peek8)(addr.wrapping_add(off));
        let node_type = peek8(LN_TYPE);
        let flags = peek8(TC_FLAGS);
        TaskInfo {
            addr,
            name: self.node_name(addr),
            node_type,
            pri: peek8(LN_PRI) as i8,
            flags,
            state: peek8(TC_STATE),
            id_nest_cnt: peek8(TC_ID_NEST_CNT) as i8,
            td_nest_cnt: peek8(TC_TD_NEST_CNT) as i8,
            sig_alloc: peek32(TC_SIG_ALLOC),
            sig_wait: peek32(TC_SIG_WAIT),
            sig_recvd: peek32(TC_SIG_RECVD),
            sig_except: peek32(TC_SIG_EXCEPT),
            trap_alloc: peek16(TC_TRAP_ALLOC),
            trap_able: peek16(TC_TRAP_ABLE),
            etask: (flags & TF_ETASK != 0).then(|| peek32(TC_TRAP_ALLOC)),
            except_code: peek32(TC_EXCEPT_CODE),
            except_data: peek32(TC_EXCEPT_DATA),
            trap_code: peek32(TC_TRAP_CODE),
            trap_data: peek32(TC_TRAP_DATA),
            sp_reg: peek32(TC_SP_REG),
            sp_lower: peek32(TC_SP_LOWER),
            sp_upper: peek32(TC_SP_UPPER),
            switch_fn: peek32(TC_SWITCH),
            launch_fn: peek32(TC_LAUNCH),
            user_data: peek32(TC_USER_DATA),
            process: (node_type == NT_PROCESS).then(|| self.process_info(addr)),
        }
    }

    /// The DOS half of a process structure.
    fn process_info(&self, task: u32) -> ProcessInfo {
        let peek32 = |off: u32| (self.peek32)(task.wrapping_add(off));
        let cli = peek32(PR_CLI);
        let cli_addr = plausible_bptr(cli).then_some(cli << 2);
        let cli32 = |off: u32| cli_addr.map_or(0, |a| (self.peek32)(a.wrapping_add(off)));
        let seglist = self.process_seglist(task).unwrap_or(0);
        ProcessInfo {
            task_num: peek32(PR_TASK_NUM) as i32,
            stack_size: peek32(PR_STACK_SIZE),
            stack_base: peek32(PR_STACK_BASE),
            result2: peek32(PR_RESULT2) as i32,
            current_dir: peek32(PR_CURRENT_DIR),
            home_dir: peek32(PR_HOME_DIR),
            console_task: peek32(PR_CONSOLE_TASK),
            window_ptr: peek32(PR_WINDOW_PTR),
            seglist,
            cli,
            // A full AmigaDOS path fits in a 255-byte BSTR.
            command: command_basename(&self.read_bstr(cli32(CLI_COMMAND_NAME), 255)).to_string(),
            set_name: self.read_bstr(cli32(CLI_SET_NAME), 255),
            return_code: cli32(CLI_RETURN_CODE) as i32,
            fail_level: cli32(CLI_FAIL_LEVEL) as i32,
            background: cli32(CLI_BACKGROUND) != 0,
            default_stack: cli32(CLI_DEFAULT_STACK),
            segments: if seglist != 0 {
                self.walk_seglist(seglist)
            } else {
                Vec::new()
            },
        }
    }

    /// Every task exec knows about whose name contains `needle`, matched
    /// case-insensitively -- the same rule as the debugger's task catch.
    pub fn find_tasks(&self, execbase: u32, needle: &str) -> Vec<u32> {
        let needle = needle.to_ascii_lowercase();
        let mut out: Vec<u32> = Vec::new();
        for addr in self.task_addrs(execbase) {
            // ThisTask can also appear on a list; one hit per task keeps
            // a name lookup unambiguous.
            if !out.contains(&addr) && self.node_name(addr).to_ascii_lowercase().contains(&needle) {
                out.push(addr);
            }
        }
        out
    }
}

/// A loaded hunk must sit in RAM exec could have allocated and carry a
/// believable size. The 16 MiB size ceiling is a sanity bound on the
/// loader's size longword, not an address-space limit: the hunk itself may
/// sit anywhere `plausible_ptr` accepts, including the 32-bit fast-RAM
/// windows of a big-box machine.
fn segment_plausible(seg: &Segment) -> bool {
    plausible_ptr(seg.start) && seg.size < 0x0100_0000
}

/// One program the debugger has seen `LoadSeg()`'d into memory: its
/// seglist BPTR, the process running it, its command name, and the hunk
/// addresses walked at the moment it was recorded.
pub struct TrackedModule {
    pub seglist: u32,
    pub task: u32,
    pub name: String,
    pub segments: Vec<Segment>,
}

/// Tracked programs kept at once; gdb re-reads the whole list on every
/// load event, so the cap only bounds guest-driven growth.
const MODULE_CAP: usize = 16;
/// Task->module pairs remembered before the table is reset; exceeding it
/// costs at worst one spurious (auto-resuming) library event.
const TASK_TABLE_CAP: usize = 256;

/// Bounded record of DOS-loaded programs, and the change detector that
/// turns a new `LoadSeg()` result appearing in the scheduled process
/// into a debugger event. Purely observational: built from
/// side-effect-free peeks, never written back to guest memory.
#[derive(Default)]
pub struct LibraryTracker {
    armed: bool,
    /// Tasks that already existed when the tracker was armed: a program
    /// first seen in one of these merely became visible via a context
    /// switch, so it is recorded without firing an event.
    known_tasks: Vec<u32>,
    /// Last module BPTR observed per task (0 = none), so the common
    /// nothing-changed case costs one map probe.
    task_modules: HashMap<u32, u32>,
    modules: Vec<TrackedModule>,
}

impl LibraryTracker {
    /// Snapshot the live task set and absorb the current process's
    /// program so neither fires a load event later. Idempotent.
    pub fn arm(&mut self, os: &OsMemory) {
        if self.armed {
            return;
        }
        self.armed = true;
        if let Ok(base) = os.exec_base() {
            self.known_tasks = os.task_addrs(base);
        }
        self.absorb_current(os);
    }

    pub fn armed(&self) -> bool {
        self.armed
    }

    /// Record the current process's program without firing an event.
    pub fn absorb_current(&mut self, os: &OsMemory) {
        let _ = self.observe(os);
    }

    pub fn modules(&self) -> &[TrackedModule] {
        &self.modules
    }

    /// Compare the scheduled process's seglist against what was last
    /// seen. Returns the newly recorded program when a genuine load
    /// event fired: the watched task's module changed to an unseen
    /// seglist (AmigaDOS `LoadSeg()` + `RunCommand()`), or a process
    /// created after arming showed up with one (`Run`, Workbench).
    /// A first sighting of a task that existed at arm time is absorbed
    /// silently -- its program was already running. Known limits: exec
    /// reusing a task's address can absorb one event, and a reverse
    /// rewind can re-fire an already-pruned module (benign; library
    /// events auto-resume).
    pub fn observe(&mut self, os: &OsMemory) -> Option<&TrackedModule> {
        let base = os.exec_base().ok()?;
        let task = (os.peek32)(base.wrapping_add(THIS_TASK));
        if !plausible_ptr(task) {
            return None;
        }
        let module = os.process_seglist(task).unwrap_or(0);
        let prev = self.task_modules.insert(task, module);
        if prev == Some(module) {
            return None;
        }
        if self.task_modules.len() > TASK_TABLE_CAP {
            self.task_modules.clear();
            self.task_modules.insert(task, module);
        }
        if let Some(old) = prev.filter(|&m| m != 0) {
            // The task's previous program exited or was replaced: forget
            // it, so a later LoadSeg reusing the same BPTR re-fires.
            self.modules
                .retain(|m| !(m.task == task && m.seglist == old));
        }
        if module == 0 || self.modules.iter().any(|m| m.seglist == module) {
            return None;
        }
        let segments = os.walk_seglist(module);
        // Early boot leaves garbage in not-yet-initialized process/CLI
        // fields that can pass the BPTR range checks; only a seglist
        // whose every hunk looks like allocated RAM is a program load.
        if segments.is_empty() || !segments.iter().all(segment_plausible) {
            return None;
        }
        if self.modules.len() >= MODULE_CAP {
            self.modules.remove(0);
        }
        self.modules.push(TrackedModule {
            seglist: module,
            task,
            name: os.process_command_name(task),
            segments,
        });
        let fires = prev.is_some() || !self.known_tasks.contains(&task);
        if fires {
            self.modules.last()
        } else {
            None
        }
    }
}

/// Run `f` against an [`OsMemory`] view built from side-effect-free bus
/// peeks. The callback shape exists because `OsMemory` borrows local
/// peek closures.
pub fn with_bus_memory<R>(bus: &crate::bus::Bus, f: impl FnOnce(&OsMemory) -> R) -> R {
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
    let os = OsMemory {
        peek8: &peek8,
        peek32: &peek32,
    };
    f(&os)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackBounds {
    pub system_lower: u32,
    pub system_upper: u32,
    pub task_lower: u32,
    pub task_upper: u32,
}

/// ExecBase and ThisTask stack bounds, using the same side-effect-free guest
/// structure walk as the console's TASK command.
pub fn stack_bounds_on_bus(bus: &crate::bus::Bus) -> Option<StackBounds> {
    with_bus_memory(bus, |os| {
        let exec = os.exec_info(os.exec_base().ok()?);
        let task = (exec.this_task != 0).then(|| os.task_info(exec.this_task));
        Some(StackBounds {
            system_lower: exec.sys_stk_lower,
            system_upper: exec.sys_stk_upper,
            task_lower: task.as_ref().map_or(0, |task| task.sp_lower),
            task_upper: task.as_ref().map_or(0, |task| task.sp_upper),
        })
    })
}

/// Bus-backed convenience wrapper: the current process's segments, or
/// why exec is not walkable. Shared by the console SEGMENTS command and
/// the GDB stub (monitor segments / qOffsets).
pub fn segments_on_bus(bus: &crate::bus::Bus) -> Result<Vec<Segment>, String> {
    with_bus_memory(bus, |os| {
        let base = os.exec_base()?;
        Ok(os.current_process_segments(base))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A tiny fake guest memory: big-endian bytes in a map.
    struct FakeMem(HashMap<u32, u8>);

    impl FakeMem {
        fn new() -> Self {
            Self(HashMap::new())
        }

        fn put32(&mut self, addr: u32, value: u32) {
            for (i, b) in value.to_be_bytes().iter().enumerate() {
                self.0.insert(addr + i as u32, *b);
            }
        }

        fn put16(&mut self, addr: u32, value: u16) {
            for (i, b) in value.to_be_bytes().iter().enumerate() {
                self.0.insert(addr + i as u32, *b);
            }
        }

        fn put8(&mut self, addr: u32, value: u8) {
            self.0.insert(addr, value);
        }

        fn put_str(&mut self, addr: u32, s: &str) {
            for (i, b) in s.bytes().enumerate() {
                self.0.insert(addr + i as u32, b);
            }
            self.0.insert(addr + s.len() as u32, 0);
        }

        fn peek8(&self, addr: u32) -> u8 {
            *self.0.get(&addr).unwrap_or(&0)
        }

        fn peek32(&self, addr: u32) -> u32 {
            u32::from_be_bytes([
                self.peek8(addr),
                self.peek8(addr + 1),
                self.peek8(addr + 2),
                self.peek8(addr + 3),
            ])
        }
    }

    fn build_exec_world() -> FakeMem {
        let mut mem = FakeMem::new();
        let base = 0x00C0_0676;
        mem.put32(4, base);
        mem.put32(base + CHKBASE, !base);
        // ThisTask: a running task named "input.device".
        let this = 0x0002_1000;
        mem.put32(base + THIS_TASK, this);
        mem.put_str(0x0002_2000, "input.device");
        mem.put32(this + LN_NAME, 0x0002_2000);
        mem.put8(this + LN_TYPE, 13); // NT_PROCESS
        mem.put8(this + LN_PRI, 20i8 as u8);
        mem.put8(this + TC_STATE, 2); // run
                                      // TaskReady: two tasks, then the list's null-succ tail.
        let t1 = 0x0002_3000;
        let t2 = 0x0002_4000;
        mem.put32(base + TASK_READY, t1);
        mem.put32(t1, t2); // ln_Succ
        mem.put_str(0x0002_5000, "trackdisk.device");
        mem.put32(t1 + LN_NAME, 0x0002_5000);
        mem.put8(t1 + LN_PRI, 5);
        mem.put8(t1 + TC_STATE, 3); // ready
        mem.put32(t2, base + TASK_READY + 4); // succ -> lh_Tail pseudo-node (succ reads 0)
        mem.put32(base + TASK_READY + 4, 0);
        mem.put_str(0x0002_6000, "SomeTask");
        mem.put32(t2 + LN_NAME, 0x0002_6000);
        mem.put8(t2 + LN_PRI, -5i8 as u8);
        mem.put8(t2 + TC_STATE, 3);
        // LibList: one library with a version.
        let lib = 0x0002_7000;
        mem.put32(base + LIB_LIST, lib);
        mem.put32(lib, base + LIB_LIST + 4);
        mem.put32(base + LIB_LIST + 4, 0);
        mem.put_str(0x0002_8000, "graphics.library");
        mem.put32(lib + LN_NAME, 0x0002_8000);
        mem.put32(lib + LIB_VERSION, 0x0028_000A); // version 40, revision 10
        mem
    }

    fn os<'a>(peek8: &'a dyn Fn(u32) -> u8, peek32: &'a dyn Fn(u32) -> u32) -> OsMemory<'a> {
        OsMemory { peek8, peek32 }
    }

    #[test]
    fn walks_tasks_and_libraries_from_a_valid_execbase() {
        let mem = build_exec_world();
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        let os = os(&peek8, &peek32);
        let base = os.exec_base().expect("valid ExecBase");
        assert_eq!(base, 0x00C0_0676);

        let this = os.this_task(base).expect("ThisTask");
        assert_eq!(this.name, "input.device");
        assert_eq!(this.state, 2);
        assert_eq!(this.pri, 20);

        let ready = os.walk(base, OsList::TaskReady);
        assert_eq!(ready.len(), 2);
        assert_eq!(ready[0].name, "trackdisk.device");
        assert_eq!(ready[1].name, "SomeTask");
        assert_eq!(ready[1].pri, -5);
        assert_eq!(task_state_name(ready[0].state), "ready");

        let libs = os.walk(base, OsList::Libraries);
        assert_eq!(libs.len(), 1);
        assert_eq!(libs[0].name, "graphics.library");
        assert_eq!(libs[0].version, 40);
        assert_eq!(libs[0].revision, 10);

        // Untouched lists read as empty, not garbage.
        assert!(os.walk(base, OsList::Ports).is_empty());
    }

    #[test]
    fn reads_execbase_scheduler_and_machine_state() {
        let mut mem = build_exec_world();
        let base = 0x00C0_0676;
        mem.put16(base + LIB_VERSION, 40);
        mem.put16(base + LIB_REVISION, 10);
        mem.put16(base + SOFT_VER, 40);
        mem.put32(base + IDLE_COUNT, 0x0001_2345);
        mem.put32(base + DISP_COUNT, 6789);
        mem.put16(base + QUANTUM, 4);
        mem.put16(base + ELAPSED, 3);
        mem.put16(base + SYS_FLAGS, 0xA000); // SAR + SINT
        mem.put8(base + ID_NEST_CNT, 0xFF); // -1: interrupts enabled
        mem.put8(base + TD_NEST_CNT, 1); // Forbid()den, nested twice
        mem.put16(base + ATTN_FLAGS, 0x0027); // 68010/68020/68030/68882
        mem.put16(base + ATTN_RESCHED, 0x0080);
        mem.put8(base + VBLANK_FREQUENCY, 50);
        mem.put8(base + POWER_SUPPLY_FREQUENCY, 50);
        mem.put32(base + ECLOCK_FREQUENCY, 709_379);
        mem.put32(base + LAST_ALERT, 0x8100_0009);
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        let info = {
            let os = os(&peek8, &peek32);
            os.exec_info(os.exec_base().unwrap())
        };

        assert_eq!((info.version, info.revision, info.soft_ver), (40, 10, 40));
        assert_eq!(info.idle_count, 0x0001_2345);
        assert_eq!(info.disp_count, 6789);
        assert_eq!((info.quantum, info.elapsed), (4, 3));
        assert_eq!(sys_flag_names(info.sys_flags), ["SINT", "SAR"]);
        assert_eq!(info.id_nest_cnt, -1);
        assert_eq!(info.td_nest_cnt, 1);
        assert_eq!(
            attn_flag_names(info.attn_flags),
            ["68010", "68020", "68030", "68882"]
        );
        assert_eq!(info.attn_resched, 0x0080);
        assert_eq!(info.this_task_name, "input.device");
        assert_eq!((info.vblank_freq, info.power_freq), (50, 50));
        assert_eq!(info.eclock_freq, Some(709_379));
        assert_eq!(info.last_alert[0], 0x8100_0009);

        // A 1.x ExecBase ends before ex_EClockFrequency: whatever the
        // longword holds there is not reported as a clock rate.
        let mut old = build_exec_world();
        old.put32(base + ECLOCK_FREQUENCY, 0x0002_1000);
        let peek8 = |a: u32| old.peek8(a);
        let peek32 = |a: u32| old.peek32(a);
        assert_eq!(os(&peek8, &peek32).exec_info(base).eclock_freq, None);
    }

    #[test]
    fn reads_a_task_structure_with_stack_use() {
        let mut mem = build_exec_world();
        let this = 0x0002_1000;
        mem.put8(this + TC_FLAGS, 0x50); // STACKCHK | SWITCH
        mem.put8(this + TC_ID_NEST_CNT, 0xFF);
        mem.put8(this + TC_TD_NEST_CNT, 0xFF);
        mem.put32(this + TC_SIG_ALLOC, 0x0000_FFFF);
        mem.put32(this + TC_SIG_WAIT, 0x0000_1000);
        mem.put32(this + TC_SIG_RECVD, 0x0000_0010);
        mem.put32(this + TC_SIG_EXCEPT, 0x0000_0020);
        mem.put16(this + TC_TRAP_ALLOC, 0x0003);
        mem.put16(this + TC_TRAP_ABLE, 0x0001);
        mem.put32(this + TC_TRAP_CODE, 0x00FC_1234);
        mem.put32(this + TC_EXCEPT_CODE, 0x0002_A000);
        mem.put32(this + TC_SP_REG, 0x0007_FF00);
        mem.put32(this + TC_SP_LOWER, 0x0007_F000);
        mem.put32(this + TC_SP_UPPER, 0x0008_0000);
        mem.put32(this + TC_USER_DATA, 0x0002_B000);
        let task = {
            let peek8 = |a: u32| mem.peek8(a);
            let peek32 = |a: u32| mem.peek32(a);
            os(&peek8, &peek32).task_info(this)
        };

        assert_eq!(task.name, "input.device");
        assert_eq!(task_flag_names(task.flags), ["STACKCHK", "SWITCH"]);
        assert_eq!(node_type_name(task.node_type), "process");
        assert_eq!(task_state_name(task.state), "run");
        assert_eq!(task.sig_wait, 0x0000_1000);
        assert_eq!(task.trap_code, 0x00FC_1234);
        assert_eq!(task.except_code, 0x0002_A000);
        assert_eq!(task.user_data, 0x0002_B000);
        // 4 KiB of stack with 256 bytes below the top in use.
        assert_eq!(task.stack_usage(task.sp_reg), Some((0x1000, 0x100)));
        // The live A7 is what counts for the running task.
        assert_eq!(task.stack_usage(0x0007_F800), Some((0x1000, 0x800)));
        // A stack pointer outside the bounds is not a use figure.
        assert_eq!(task.stack_usage(0x0009_0000), None);
        // Without TF_ETASK the trap words are trap masks.
        assert_eq!(task.etask, None);
        assert_eq!((task.trap_alloc, task.trap_able), (3, 1));

        // With TF_ETASK (AROS, and OS 4-era exec) the same two words are
        // the ETask pointer, so they must not read as trap masks.
        mem.put8(this + TC_FLAGS, 0x58);
        mem.put32(this + TC_TRAP_ALLOC, 0x00C3_5978);
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        assert_eq!(os(&peek8, &peek32).task_info(this).etask, Some(0x00C3_5978));
    }

    #[test]
    fn reads_the_process_and_cli_half_of_a_task() {
        let mut mem = build_cli_world();
        let this = 0x0002_1000;
        let cli_addr = 0x0003_0000u32;
        mem.put32(cli_addr + CLI_MODULE, 0x8000 >> 2);
        mem.put32(cli_addr + CLI_RETURN_CODE, 20);
        mem.put32(cli_addr + CLI_FAIL_LEVEL, 10);
        mem.put32(cli_addr + CLI_BACKGROUND, 1);
        mem.put32(cli_addr + CLI_DEFAULT_STACK, 1024);
        mem.put32(cli_addr + CLI_SET_NAME, 0x0003_2000 >> 2);
        mem.put8(0x0003_2000, 5);
        for (i, b) in b"Work:".iter().enumerate() {
            mem.put8(0x0003_2001 + i as u32, *b);
        }
        mem.put32(this + PR_TASK_NUM, 3);
        mem.put32(this + PR_STACK_SIZE, 4096);
        mem.put32(this + PR_RESULT2, 205); // ERROR_OBJECT_NOT_FOUND
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        let os = os(&peek8, &peek32);
        let task = os.task_info(this);
        let proc = task.process.expect("NT_PROCESS gets a process half");

        assert_eq!(proc.task_num, 3);
        assert_eq!(proc.command, "hello");
        assert_eq!(proc.set_name, "Work:");
        assert_eq!((proc.return_code, proc.fail_level), (20, 10));
        assert!(proc.background);
        assert_eq!(proc.default_stack, 1024);
        assert_eq!(proc.stack_size, 4096);
        assert_eq!(proc.result2, 205);
        assert_eq!(proc.seglist, 0x8000 >> 2);
        assert_eq!(proc.segments.len(), 2);
        assert_eq!(proc.segments[0].start, 0x8004);

        // A plain task has no process half at all.
        let t1 = 0x0002_3000; // NT_UNKNOWN in build_exec_world
        assert!(os.task_info(t1).process.is_none());
    }

    #[test]
    fn finds_tasks_by_name_across_the_lists() {
        let mem = build_exec_world();
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        let os = os(&peek8, &peek32);
        let base = os.exec_base().unwrap();
        // Case-insensitive substring, ThisTask and the ready list both.
        assert_eq!(os.find_tasks(base, "INPUT"), [0x0002_1000]);
        assert_eq!(os.find_tasks(base, "trackdisk"), [0x0002_3000]);
        assert_eq!(os.find_tasks(base, ".device").len(), 2);
        assert!(os.find_tasks(base, "workbench").is_empty());
    }

    #[test]
    fn summarizes_the_exec_memory_list() {
        let mut mem = build_exec_world();
        let base = 0x00C0_0676;
        // One MemHeader for chip RAM with two free chunks, and one for
        // fast RAM that is fully allocated (mh_First = 0).
        let chip = 0x0004_0000u32;
        let fast = 0x0004_1000u32;
        mem.put32(base + MEM_LIST, chip);
        mem.put32(chip, fast);
        mem.put32(fast, base + MEM_LIST + 4); // succ -> lh_Tail
        mem.put32(base + MEM_LIST + 4, 0);
        mem.put_str(0x0004_2000, "chip memory");
        mem.put32(chip + LN_NAME, 0x0004_2000);
        mem.put8(chip + LN_PRI, -10i8 as u8);
        mem.put16(chip + MH_ATTRIBUTES, 0x0003); // PUBLIC | CHIP
        mem.put32(chip + MH_LOWER, 0x0000_0400);
        mem.put32(chip + MH_UPPER, 0x0020_0000);
        mem.put32(chip + MH_FREE, 0x0010_0000);
        mem.put32(chip + MH_FIRST, 0x0001_0000);
        mem.put32(0x0001_0000, 0x0018_0000); // mc_Next
        mem.put32(0x0001_0000 + MC_BYTES, 0x0004_0000);
        mem.put32(0x0018_0000, 0); // end of the free list
        mem.put32(0x0018_0000 + MC_BYTES, 0x000C_0000);
        mem.put_str(0x0004_3000, "fast memory");
        mem.put32(fast + LN_NAME, 0x0004_3000);
        mem.put16(fast + MH_ATTRIBUTES, 0x0005); // PUBLIC | FAST
        mem.put32(fast + MH_LOWER, 0x0020_0000);
        mem.put32(fast + MH_UPPER, 0x00A0_0000);
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        let os = os(&peek8, &peek32);
        let regions = os.mem_list(base);

        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].name, "chip memory");
        assert_eq!(regions[0].pri, -10);
        assert_eq!(mem_attr_names(regions[0].attributes), ["PUBLIC", "CHIP"]);
        assert_eq!(regions[0].size(), 0x0020_0000 - 0x400);
        assert_eq!(regions[0].free, 0x0010_0000);
        assert_eq!(regions[0].largest, 0x000C_0000);
        assert_eq!(regions[0].chunks, 2);
        // Fully allocated: no chunks, no largest, but still listed.
        assert_eq!(regions[1].name, "fast memory");
        assert_eq!(regions[1].chunks, 0);
        assert_eq!(regions[1].largest, 0);
    }

    /// A trashed free list must not spin: chunks have to stay inside the
    /// header and ascend, so a self-referential or backwards mc_Next
    /// ends the walk.
    #[test]
    fn a_looping_free_list_terminates() {
        let mut mem = build_exec_world();
        let base = 0x00C0_0676;
        let header = 0x0004_0000u32;
        mem.put32(base + MEM_LIST, header);
        mem.put32(header, base + MEM_LIST + 4);
        mem.put32(base + MEM_LIST + 4, 0);
        mem.put32(header + MH_LOWER, 0x0001_0000);
        mem.put32(header + MH_UPPER, 0x0002_0000);
        mem.put32(header + MH_FIRST, 0x0001_8000);
        mem.put32(0x0001_8000, 0x0001_8000); // points at itself
        mem.put32(0x0001_8000 + MC_BYTES, 0x100);
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        let regions = os(&peek8, &peek32).mem_list(base);
        assert_eq!(regions[0].chunks, 1);
    }

    #[test]
    fn walks_a_cli_process_seglist() {
        let mut mem = build_exec_world();
        // Make ThisTask a process with a CLI whose module is a two-hunk
        // seglist: BPTRs at $8000 and $9000 (headers 8 bytes before the
        // payload).
        let this = 0x0002_1000;
        mem.put8(this + LN_TYPE, NT_PROCESS);
        let cli_addr = 0x0003_0000u32;
        mem.put32(this + PR_CLI, cli_addr >> 2);
        mem.put32(cli_addr + CLI_MODULE, 0x8000 >> 2);
        mem.put32(0x8000 - 4, 0x100); // hunk size incl header
        mem.put32(0x8000, 0x9000 >> 2); // next
        mem.put32(0x9000 - 4, 0x40);
        mem.put32(0x9000, 0); // end of list
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        let os = os(&peek8, &peek32);
        let base = os.exec_base().unwrap();
        let segs = os.current_process_segments(base);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].start, 0x8004);
        assert_eq!(segs[0].size, 0xF8);
        assert_eq!(segs[1].start, 0x9004);
        assert_eq!(segs[1].size, 0x38);
    }

    #[test]
    fn accepts_execbase_in_z3_space_and_rejects_the_gap() {
        // OS 3.2+ SetPatch moves ExecBase to fast RAM, e.g. Zorro III space.
        let base = 0x4000_08C0;
        let mut mem = FakeMem::new();
        mem.put32(4, base);
        mem.put32(base + CHKBASE, !base);
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        assert_eq!(
            os(&peek8, &peek32).exec_base().expect("ExecBase in Z3 RAM"),
            base
        );

        // A pointer in the unmapped gap between Z2 and Z3 is rejected.
        let mut mem = FakeMem::new();
        mem.put32(4, 0x0B00_0000);
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        assert!(os(&peek8, &peek32).exec_base().is_err());
    }

    #[test]
    fn reads_a_bounded_bstr_command_name() {
        let mut mem = FakeMem::new();
        // BSTR at $8000: length 11, "dh0:c/hello".
        mem.put8(0x8000, 11);
        for (i, b) in b"dh0:c/hello".iter().enumerate() {
            mem.put8(0x8001 + i as u32, *b);
        }
        {
            let peek8 = |a: u32| mem.peek8(a);
            let peek32 = |a: u32| mem.peek32(a);
            let os = os(&peek8, &peek32);
            assert_eq!(os.read_bstr(0x8000 >> 2, 255), "dh0:c/hello");
            assert_eq!(os.read_bstr(0x8000 >> 2, 5), "dh0:c");
            assert_eq!(os.read_bstr(0, 255), "");
        }
        // Unprintable bytes come back filtered, not raw.
        let mut mem = FakeMem::new();
        mem.put8(0x8000, 2);
        mem.put8(0x8001, 0x07);
        mem.put8(0x8002, b'x');
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        assert_eq!(os(&peek8, &peek32).read_bstr(0x8000 >> 2, 255), ".x");
    }

    #[test]
    fn command_basename_strips_amigados_paths() {
        assert_eq!(command_basename("dh0:c/hello"), "hello");
        assert_eq!(command_basename("work:hello"), "hello");
        assert_eq!(command_basename("hello"), "hello");
        assert_eq!(command_basename(""), "");
    }

    /// Stage a CLI process world for the tracker: ThisTask is a process
    /// whose CLI has a cli_CommandName but no cli_Module yet.
    fn build_cli_world() -> FakeMem {
        let mut mem = build_exec_world();
        let this = 0x0002_1000;
        mem.put8(this + LN_TYPE, NT_PROCESS);
        let cli_addr = 0x0003_0000u32;
        mem.put32(this + PR_CLI, cli_addr >> 2);
        mem.put32(cli_addr + CLI_COMMAND_NAME, 0x0003_1000 >> 2);
        mem.put8(0x0003_1000, 11);
        for (i, b) in b"dh0:c/hello".iter().enumerate() {
            mem.put8(0x0003_1001 + i as u32, *b);
        }
        // A two-hunk seglist parked at $8000/$9000, not yet installed.
        mem.put32(0x8000 - 4, 0x100);
        mem.put32(0x8000, 0x9000 >> 2);
        mem.put32(0x9000 - 4, 0x40);
        mem.put32(0x9000, 0);
        mem
    }

    #[test]
    fn library_tracker_fires_on_loads_and_absorbs_context_switches() {
        let mut mem = build_cli_world();
        let this = 0x0002_1000;
        let cli_addr = 0x0003_0000u32;
        let mut tracker = LibraryTracker::default();
        {
            let peek8 = |a: u32| mem.peek8(a);
            let peek32 = |a: u32| mem.peek32(a);
            let os = os(&peek8, &peek32);
            tracker.arm(&os);
            assert!(tracker.armed());
            assert!(tracker.observe(&os).is_none(), "nothing loaded yet");
        }

        // The shell LoadSegs a command into cli_Module: fires, named.
        mem.put32(cli_addr + CLI_MODULE, 0x8000 >> 2);
        {
            let peek8 = |a: u32| mem.peek8(a);
            let peek32 = |a: u32| mem.peek32(a);
            let os = os(&peek8, &peek32);
            let event = tracker.observe(&os).expect("LoadSeg fires");
            assert_eq!(event.name, "hello");
            assert_eq!(event.segments.len(), 2);
            assert_eq!(event.segments[0].start, 0x8004);
            assert!(tracker.observe(&os).is_none(), "no re-fire");
        }

        // Context switch to a task that existed at arm time, already
        // running a program: absorbed silently but listed.
        let t1 = 0x0002_3000; // on TaskReady in build_exec_world
        let base = 0x00C0_0676;
        mem.put32(base + THIS_TASK, t1);
        mem.put8(t1 + LN_TYPE, NT_PROCESS);
        let cli2 = 0x0003_4000u32;
        mem.put32(t1 + PR_CLI, cli2 >> 2);
        mem.put32(cli2 + CLI_MODULE, 0xA000 >> 2);
        mem.put32(0xA000 - 4, 0x20);
        mem.put32(0xA000, 0);
        {
            let peek8 = |a: u32| mem.peek8(a);
            let peek32 = |a: u32| mem.peek32(a);
            let os = os(&peek8, &peek32);
            assert!(tracker.observe(&os).is_none(), "old task absorbed");
            assert_eq!(tracker.modules().len(), 2);
        }

        // A process created after arming shows up with a module: fires.
        let t3 = 0x0002_9000;
        mem.put32(base + THIS_TASK, t3);
        mem.put8(t3 + LN_TYPE, NT_PROCESS);
        let cli3 = 0x0003_6000u32;
        mem.put32(t3 + PR_CLI, cli3 >> 2);
        mem.put32(cli3 + CLI_MODULE, 0xB000 >> 2);
        mem.put32(0xB000 - 4, 0x20);
        mem.put32(0xB000, 0);
        {
            let peek8 = |a: u32| mem.peek8(a);
            let peek32 = |a: u32| mem.peek32(a);
            let os = os(&peek8, &peek32);
            assert!(tracker.observe(&os).is_some(), "new process fires");
        }

        // Same task runs the next command: the old module is pruned and
        // the new one fires, even at a reused seglist address.
        mem.put32(base + THIS_TASK, this);
        mem.put32(cli_addr + CLI_MODULE, 0x9000 >> 2);
        {
            let peek8 = |a: u32| mem.peek8(a);
            let peek32 = |a: u32| mem.peek32(a);
            let os = os(&peek8, &peek32);
            let event = tracker.observe(&os).expect("replacement fires");
            assert_eq!(event.seglist, 0x9000 >> 2);
            assert!(!tracker
                .modules()
                .iter()
                .any(|m| m.seglist == 0x8000 >> 2 && m.task == this));
        }
    }

    #[test]
    fn library_tracker_rejects_implausible_boot_garbage_seglists() {
        // Seen on a real KS3.1 boot: the strap process's uninitialized
        // CLI fields yield a "seglist" whose hunk lands in ROM space
        // with a wild size. The tracker must not fire on it.
        let mut mem = build_cli_world();
        let cli_addr = 0x0003_0000u32;
        let mut tracker = LibraryTracker::default();
        {
            let peek8 = |a: u32| mem.peek8(a);
            let peek32 = |a: u32| mem.peek32(a);
            let os = os(&peek8, &peek32);
            tracker.arm(&os);
        }
        // A hunk at $FA8240 (ROM) with a garbage size longword.
        mem.put32(cli_addr + CLI_MODULE, 0x00FA_8240 >> 2);
        mem.put32(0x00FA_8240 - 4, 0x7200_0008);
        mem.put32(0x00FA_8240, 0);
        {
            let peek8 = |a: u32| mem.peek8(a);
            let peek32 = |a: u32| mem.peek32(a);
            let os = os(&peek8, &peek32);
            assert!(tracker.observe(&os).is_none(), "ROM-space hunk rejected");
            assert!(tracker.modules().is_empty());
        }
        // A RAM hunk with an implausible size is rejected too.
        mem.put32(cli_addr + CLI_MODULE, 0x8000 >> 2);
        mem.put32(0x8000 - 4, 0x7200_0008);
        mem.put32(0x8000, 0);
        {
            let peek8 = |a: u32| mem.peek8(a);
            let peek32 = |a: u32| mem.peek32(a);
            let os = os(&peek8, &peek32);
            assert!(tracker.observe(&os).is_none(), "oversized hunk rejected");
        }
        // The real load afterwards still fires.
        mem.put32(0x8000 - 4, 0x100);
        mem.put32(0x8000, 0);
        {
            let peek8 = |a: u32| mem.peek8(a);
            let peek32 = |a: u32| mem.peek32(a);
            let os = os(&peek8, &peek32);
            // Same BPTR as the rejected junk: force re-evaluation via a
            // different module first, as a real shell would.
            assert!(tracker.observe(&os).is_none(), "same BPTR, cached");
        }
        mem.put32(cli_addr + CLI_MODULE, 0x9000 >> 2);
        mem.put32(0x9000 - 4, 0x40);
        mem.put32(0x9000, 0);
        {
            let peek8 = |a: u32| mem.peek8(a);
            let peek32 = |a: u32| mem.peek32(a);
            let os = os(&peek8, &peek32);
            let event = tracker.observe(&os).expect("valid load fires");
            assert_eq!(event.segments[0].start, 0x9004);
        }
    }

    #[test]
    fn rejects_a_missing_or_corrupt_execbase() {
        let mem = FakeMem::new();
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        assert!(os(&peek8, &peek32).exec_base().is_err());

        let mut mem = FakeMem::new();
        mem.put32(4, 0x0000_2000);
        mem.put32(0x2000 + CHKBASE, 0xDEAD_BEEF); // wrong complement
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        let err = os(&peek8, &peek32).exec_base().unwrap_err();
        assert!(err.contains("ChkBase"), "{err}");
    }

    /// Exec structures live wherever exec allocated them. On a big-box
    /// machine that is the Ramsey motherboard bank ($04000000-$07FFFFFF),
    /// the CPU-slot accelerator bank ($08000000-$0FFFFFFF), or a Zorro III
    /// board -- all beyond the 24-bit space the small-box map fits in.
    #[test]
    fn plausible_pointers_cover_the_32_bit_ram_windows() {
        for addr in [
            0x0000_1000, // chip
            0x0020_0000, // Zorro II fast
            0x00C0_0000, // slow
            0x0400_0000, // motherboard (Ramsey), bottom of the window
            0x07FF_FFFE, // motherboard, top of the window
            0x0800_0000, // CPU slot, bottom
            0x0FFF_FFFE, // CPU slot, top
            0x1000_0000, // Zorro III, bottom
            0x4000_0000, // Zorro III
        ] {
            assert!(plausible_ptr(addr), "${addr:08X} should be plausible");
            assert!(
                plausible_bptr(addr >> 2),
                "BPTR ${:08X} should be plausible",
                addr >> 2
            );
        }
        // Still rejected: odd addresses, the zero page, the custom-register
        // and ROM spaces, and anything past the Zorro III window.
        for addr in [
            0x0000_0001,
            0x0000_0080,
            0x00DF_F000,
            0x00F8_0000,
            0x8000_0000u32,
        ] {
            assert!(!plausible_ptr(addr), "${addr:08X} should be rejected");
        }
    }

    /// A seglist walked from a hunk in motherboard RAM: the BPTR bound
    /// must follow the same address windows, not a 16 MiB ceiling.
    #[test]
    fn walks_a_seglist_loaded_above_the_24_bit_space() {
        let hunk = 0x0450_0000u32;
        let mut mem = FakeMem::new();
        mem.put32(hunk - 4, 0x100); // loader size longword
        mem.put32(hunk, 0); // end of list
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        let segs = os(&peek8, &peek32).walk_seglist(hunk >> 2);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].start, hunk + 4);
        assert!(segment_plausible(&segs[0]));
    }

    /// The same for a BSTR: cli_CommandName points into whatever RAM the
    /// shell was loaded into.
    #[test]
    fn reads_a_bstr_above_the_24_bit_space() {
        let bstr = 0x0900_0000u32;
        let mut mem = FakeMem::new();
        mem.put8(bstr, 3);
        mem.put_str(bstr + 1, "Dir");
        let peek8 = |a: u32| mem.peek8(a);
        let peek32 = |a: u32| mem.peek32(a);
        assert_eq!(os(&peek8, &peek32).read_bstr(bstr >> 2, 255), "Dir");
    }
}
