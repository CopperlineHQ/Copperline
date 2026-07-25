// SPDX-License-Identifier: GPL-3.0-or-later

//! Read-only AmigaOS (exec.library) structure walking for the debugger:
//! the task, library, device, resource, and port lists, reached from
//! ExecBase via side-effect-free memory peeks. The offsets are exec's
//! public ABI (execbase.h / nodes.h / lists.h), stable from Kickstart
//! 1.x through 3.x and AROS, so no version sniffing is needed -- only
//! pointer plausibility checks, since the OS may simply not be up yet.

pub mod dos;

use std::collections::HashMap;

/// ExecBase field offsets (execbase.h).
const EXECBASE_PTR: u32 = 4;
const CHKBASE: u32 = 0x26;
const THIS_TASK: u32 = 0x114;
const RESOURCE_LIST: u32 = 0x150;
const DEVICE_LIST: u32 = 0x15E;
const INTR_LIST: u32 = 0x16C;
const LIB_LIST: u32 = 0x17A;
const PORT_LIST: u32 = 0x188;
const TASK_READY: u32 = 0x196;
const TASK_WAIT: u32 = 0x1A4;

/// Node field offsets (nodes.h).
const LN_TYPE: u32 = 8;
const LN_PRI: u32 = 9;
const LN_NAME: u32 = 10;
/// Task field offsets (tasks.h).
const TC_STATE: u32 = 15;
/// Library field offsets (libraries.h).
const LIB_VERSION: u32 = 20;
const LIB_REVISION: u32 = 22;

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

/// Heuristic: even and in an address range where exec structures can
/// live -- chip+Z2 RAM, slow RAM, or Zorro III space (OS 3.2+ SetPatch
/// moves ExecBase to fast RAM, which may be a Z3 board).
fn plausible_ptr(addr: u32) -> bool {
    addr & 1 == 0
        && ((0x100..0x00A0_0000).contains(&addr)           // chip + Z2 fast
            || (0x00C0_0000..0x00D8_0000).contains(&addr)  // slow
            || (0x0400_0000..0x0800_0000).contains(&addr)  // A3000/A4000 fast
            || (0x0800_0000..0x1000_0000).contains(&addr)  // CPU-slot RAM
            || (0x1000_0000..0x8000_0000).contains(&addr)) // Zorro III
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

/// One hunk of a loaded DOS segment list.
pub struct Segment {
    /// First byte of the hunk's payload (code/data).
    pub start: u32,
    /// Payload bytes (the loader's 8-byte size/next header excluded).
    pub size: u32,
}

/// Process field offsets (dos/dosextens.h). pr_SegList/pr_CLI are BPTRs.
const PR_SEGLIST: u32 = 0x80;
const PR_CLI: u32 = 0xAC;
/// CommandLineInterface field offsets: cli_Module is the BPTR to the
/// currently running command's segment list, cli_CommandName the BSTR
/// naming the command it belongs to.
const CLI_COMMAND_NAME: u32 = 0x10;
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
        if cli != 0 && cli < 0x0040_0000 {
            let module = (self.peek32)((cli << 2).wrapping_add(CLI_MODULE));
            if module != 0 {
                return Some(module);
            }
        }
        let array = (self.peek32)(task.wrapping_add(PR_SEGLIST));
        if array == 0 || array >= 0x0040_0000 {
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
        while bptr != 0 && bptr < 0x0040_0000 && out.len() < 64 {
            let addr = bptr << 2;
            if !(4..0x0100_0000).contains(&addr) {
                break;
            }
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
        if bptr == 0 || bptr >= 0x0040_0000 {
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
            if cli != 0 && cli < 0x0040_0000 {
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

/// A loaded hunk must sit in RAM exec could have allocated and carry a
/// believable size (the loader's size longword is bounded by the 16MB
/// chip/Z2 space a seglist can live in).
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
}
