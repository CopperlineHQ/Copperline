// SPDX-License-Identifier: GPL-3.0-or-later

//! Read-only AmigaOS (exec.library) structure walking for the debugger:
//! the task, library, device, resource, and port lists, reached from
//! ExecBase via side-effect-free memory peeks. The offsets are exec's
//! public ABI (execbase.h / nodes.h / lists.h), stable from Kickstart
//! 1.x through 3.x and AROS, so no version sniffing is needed -- only
//! pointer plausibility checks, since the OS may simply not be up yet.

pub mod dos;

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
        && ((0x100..0x00A0_0000).contains(&addr)          // chip + Z2 fast
            || (0x00C0_0000..0x00D8_0000).contains(&addr) // slow
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
/// currently running command's segment list.
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
}

/// Bus-backed convenience wrapper: the current process's segments, or
/// why exec is not walkable. Shared by the console SEGMENTS command and
/// the GDB stub (monitor segments / qOffsets).
pub fn segments_on_bus(bus: &crate::bus::Bus) -> Result<Vec<Segment>, String> {
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
    let base = os.exec_base()?;
    Ok(os.current_process_segments(base))
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
