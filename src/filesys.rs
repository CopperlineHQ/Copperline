//! Host-filesystem service: mount host directories as AmigaDOS volumes
//! (`HOSTFS0:`, `HOSTFS1:`, ...).
//!
//! The guest side is a tiny handler (see `guest/services/`) mapped into the
//! Copperline services board together with a mount table and a hand-built
//! DiagArea. At expansion init the DiagArea's DiagPoint calls the handler's
//! mount entry with the documented DiagPoint context; the handler builds one
//! DeviceNode per mount table entry and `AddBootNode`s it, so DOS mounts the
//! devices at boot and starts the handler process on first reference.
//! The handler forwards every DosPacket to [`FilesysHle`] through a reserved
//! A-line trap; all ACTION_* semantics are implemented here against the host
//! filesystem, with results written straight into guest memory.
//!
//! Guest-visible objects the handler must hand out (FileLocks) are allocated
//! from a pool inside the board window, so the host never has to call
//! AllocMem in the guest.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use m68k::{AddressBus, CpuCore, HleHandler};

/// The guest-side handler ROM (see `guest/services/README.md`).
pub const FILESYS_HANDLER: &[u8] = include_bytes!("../assets/services/services_rom.bin");

// Board-window layout. Keep in sync with `guest/services/copperline_board.h`; the unit
// tests lock it.
/// Handler code offset; the two longwords before it are a fake seglist header
/// (length, next = 0) so `dn_SegList = (base + 4) >> 2`.
pub const ROM_OFFSET: usize = 0x0008;
/// Mount table: u16 count, then fixed-size NUL-terminated device names.
pub const MOUNTS_OFFSET: usize = 0x3800;
pub const MOUNT_ENTRY_SIZE: usize = 32;
pub const MOUNT_MAX_COUNT: usize = 16;
/// The DiagArea (`BoardSpec::copperline_services` points er_InitDiagVec here).
pub const DIAG_OFFSET: u16 = 0x4000;
/// Per-unit volume DosList nodes, built by the host at handler startup and
/// AddDosEntry'd by the guest handler (TRAP_RES_ADDVOLUME).
const VOLUMES_OFFSET: u32 = 0x7000;
const VOLUME_SLOT_SIZE: u32 = 128;
/// Host-managed pool for guest-visible objects (FileLocks), through the end
/// of the 64K window. The guest never touches it.
const POOL_OFFSET: u32 = 0x8000;
const POOL_END: u32 = 0x1_0000;
/// FileLock is 20 bytes; keep slots longword-aligned.
const LOCK_SLOT_SIZE: u32 = 24;

// trap_packet return values (D0); see guest/services/copperline_board.h.
const TRAP_RES_REPLY: u32 = 0;
const TRAP_RES_ADDVOLUME: u32 = 2;

/// Base of the reserved A-line opcode range for filesys host traps. A-line
/// (LINE 1010, exception vector 10) is unused by AmigaOS, so these never
/// collide with guest code.
pub const TRAP_BASE: u16 = 0xA400;
/// DiagPoint entered: logged, and A0 (the board base) is captured.
const TRAP_DIAG_ENTRY: u16 = 0xA400;
/// DosPacket from the handler: D1 = packet APTR, A1 = handler MsgPort.
const TRAP_PACKET: u16 = 0xA402;

// AmigaDOS packet types (dos/dosextens.h).
const ACTION_LOCATE_OBJECT: i32 = 8;
const ACTION_FREE_LOCK: i32 = 15;
const ACTION_EXAMINE_OBJECT: i32 = 23;
const ACTION_EXAMINE_NEXT: i32 = 24;
const ACTION_DISK_INFO: i32 = 25;
const ACTION_INFO: i32 = 26;
const ACTION_IS_FILESYSTEM: i32 = 1027;

// dos/dos.h.
const DOSTRUE: u32 = 0xFFFF_FFFF;
const DOSFALSE: u32 = 0;
const ERROR_OBJECT_NOT_FOUND: u32 = 205;
const ERROR_ACTION_NOT_KNOWN: u32 = 209;
const ERROR_NO_MORE_ENTRIES: u32 = 232;
/// 'CLFS' -- our own id_DiskType, honest about not being an FFS at all.
/// (The UAE filesys reports 'DOS\1' here instead; if some tool turns out to
/// insist on a DOS\x dostype, reconsider.)
const ID_CLFS_DISK: u32 = 0x434C_4653;
const ID_VALIDATED: u32 = 82; // id_DiskState: validated, read/write
                              // fib_DirEntryType values (dos/dosextens.h ST_*).
const ST_ROOT: i32 = 1;
const ST_USERDIR: i32 = 2;
const ST_FILE: i32 = -3;

/// One `[[filesys]]` entry: a host directory exported as an AmigaDOS
/// device `HOSTFS<n>:` with the given volume name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountSpec {
    pub path: PathBuf,
    pub volume: String,
}

/// DOS device name of mount `unit` (`HOSTFS0`, `HOSTFS1`, ...).
pub fn device_name(unit: usize) -> String {
    format!("HOSTFS{unit}")
}

/// Build the 64K board window: fake seglist header + handler ROM, the mount
/// table, and the DiagArea whose DiagPoint calls the handler's mount entry.
pub fn board_image(mounts: &[MountSpec]) -> Vec<u8> {
    assert!(ROM_OFFSET + FILESYS_HANDLER.len() <= MOUNTS_OFFSET);
    assert!(mounts.len() <= MOUNT_MAX_COUNT);
    let mut img = vec![0u8; 0x1_0000];

    // Fake seglist: length in longwords at +0 (unused by DOS for handlers,
    // but kept sane), next-segment BPTR = 0 at +4, code at +8.
    let seg_longs = ((ROM_OFFSET + FILESYS_HANDLER.len()) / 4) as u32;
    img[0..4].copy_from_slice(&seg_longs.to_be_bytes());
    img[ROM_OFFSET..ROM_OFFSET + FILESYS_HANDLER.len()].copy_from_slice(FILESYS_HANDLER);

    // Mount table: u16 count, then MOUNT_ENTRY_SIZE-byte NUL-terminated
    // device names.
    let m = MOUNTS_OFFSET;
    img[m..m + 2].copy_from_slice(&(mounts.len() as u16).to_be_bytes());
    for (i, _) in mounts.iter().enumerate() {
        let name = device_name(i);
        let at = m + 2 + i * MOUNT_ENTRY_SIZE;
        img[at..at + name.len()].copy_from_slice(name.as_bytes());
    }

    // struct DiagArea (libraries/configregs.h). Hard-won Kickstart 3.x
    // gotchas: da_Config needs a DAC_BOOTTIME bit or the area is abandoned
    // after one read, and DAC_CONFIGTIME requires a non-zero da_BootPoint.
    let d = DIAG_OFFSET as usize;
    img[d] = 0x90; // da_Config = DAC_WORDWIDE | DAC_CONFIGTIME
    img[d + 1] = 0x00; // da_Flags
    img[d + 2..d + 4].copy_from_slice(&0x0040u16.to_be_bytes()); // da_Size
    img[d + 4..d + 6].copy_from_slice(&0x0010u16.to_be_bytes()); // da_DiagPoint
    img[d + 6..d + 8].copy_from_slice(&0x0024u16.to_be_bytes()); // da_BootPoint
    img[d + 8..d + 10].copy_from_slice(&0x0030u16.to_be_bytes()); // da_Name

    // DiagPoint routine at +0x10, run from the RAM copy with the documented
    // context (A0 = board base, A3 = ConfigDev, A5 = ExpansionBase). It traps
    // to the host (which captures the board base), calls the handler's mount
    // entry as a C function -- mount_boards(board, ExpansionBase, ConfigDev),
    // args pushed right to left -- and returns D0 = 0 so Kickstart frees the
    // copy: nothing references it afterwards.
    #[rustfmt::skip]
    let diag_point: [u8; 20] = [
        (TRAP_DIAG_ENTRY >> 8) as u8, TRAP_DIAG_ENTRY as u8,
        0x2F, 0x0B,             // move.l a3,-(sp)   ConfigDev
        0x2F, 0x0D,             // move.l a5,-(sp)   ExpansionBase
        0x2F, 0x08,             // move.l a0,-(sp)   board base
        0x4E, 0xA8, 0x00, 0x0C, // jsr 12(a0)        handler mount entry
        0x4F, 0xEF, 0x00, 0x0C, // lea 12(sp),sp
        0x70, 0x00,             // moveq #0,d0       free the diag copy
        0x4E, 0x75,             // rts
    ];
    img[d + 0x10..d + 0x10 + diag_point.len()].copy_from_slice(&diag_point);
    // da_BootPoint must be non-zero (see above) but is never usefully
    // called: point it at a harmless "return 0".
    img[d + 0x24..d + 0x28].copy_from_slice(&[0x70, 0x00, 0x4E, 0x75]); // moveq #0,d0 ; rts
    let name = b"Copperline\0";
    img[d + 0x30..d + 0x30 + name.len()].copy_from_slice(name);

    img
}

/// A handed-out FileLock: which mount it belongs to and the path relative to
/// the mount root (empty = the root itself). Keyed by the guest address of
/// the FileLock structure in the board-window pool.
#[derive(Debug, Clone)]
struct LockRec {
    unit: usize,
    rel: PathBuf,
}

/// Host side of the filesys trap gateway: implements the AmigaDOS packet
/// ACTION_* semantics against the host directories in `mounts`. Installed as
/// the CPU's HLE handler; it reacts only to the reserved [`TRAP_BASE`] range,
/// so leaving it installed with no mounts configured changes nothing.
#[derive(Default)]
pub struct FilesysHle {
    mounts: Vec<MountSpec>,
    /// Board base address, captured from A0 at the DiagPoint trap.
    board_base: Option<u32>,
    /// Handler MsgPort address -> mount unit, learned from startup packets.
    ports: HashMap<u32, usize>,
    /// Mount unit -> guest address of its volume DosList node.
    volumes: HashMap<usize, u32>,
    /// Guest FileLock address -> what it locks.
    locks: HashMap<u32, LockRec>,
    /// Free slots in the board-window lock pool (guest addresses).
    free_slots: Vec<u32>,
    /// Bump allocator behind `free_slots`, board-relative.
    pool_next: u32,
}

impl FilesysHle {
    pub fn set_mounts(&mut self, mounts: Vec<MountSpec>) {
        self.mounts = mounts;
    }

    /// Host path a lock refers to.
    fn lock_path(&self, rec: &LockRec) -> PathBuf {
        self.mounts[rec.unit].path.join(&rec.rel)
    }

    /// Allocate a FileLock in the board-window pool and register it.
    fn alloc_lock(
        &mut self,
        bus: &mut dyn AddressBus,
        port: u32,
        access: u32,
        rec: LockRec,
    ) -> Option<u32> {
        let base = self.board_base?;
        let addr = self.free_slots.pop().or_else(|| {
            let next = POOL_OFFSET + self.pool_next;
            (next + LOCK_SLOT_SIZE <= POOL_END).then(|| {
                self.pool_next += LOCK_SLOT_SIZE;
                base + next
            })
        })?;
        let volume = self.volumes.get(&rec.unit).copied().unwrap_or(0);
        bus.write_long(addr, 0); // fl_Link
        bus.write_long(addr + 4, addr); // fl_Key: unique, opaque to DOS
        bus.write_long(addr + 8, access); // fl_Access
        bus.write_long(addr + 12, port); // fl_Task: the handler port
        bus.write_long(addr + 16, volume >> 2); // fl_Volume (BPTR)
        self.locks.insert(addr, rec);
        Some(addr)
    }

    /// Resolve a DOS path (BPTR lock + name) to a lock record. AmigaDOS path
    /// semantics: an optional `device:` prefix restarts at the root, `/`
    /// goes to the parent, and names are case-insensitive.
    fn resolve(&self, unit: usize, lock_bptr: u32, name: &[u8]) -> Option<LockRec> {
        let (unit, mut rel) = if lock_bptr != 0 {
            let rec = self.locks.get(&(lock_bptr << 2))?;
            (rec.unit, rec.rel.clone())
        } else {
            (unit, PathBuf::new())
        };

        let mut rest = name;
        if let Some(colon) = name.iter().position(|&b| b == b':') {
            // "DEVICE:" or "Volume:" prefix: absolute from the root. The
            // packet reached this handler, so the prefix already named it.
            rest = &name[colon + 1..];
            rel = PathBuf::new();
        }
        if rest.is_empty() {
            // Bare "DEVICE:" or an empty name: the base itself. (split()
            // below would yield one empty component = "parent", wrong.)
            return Some(LockRec { unit, rel });
        }
        for comp in rest.split(|&b| b == b'/') {
            if comp.is_empty() {
                // Leading or doubled '/': up to the parent.
                if !rel.pop() {
                    return None;
                }
                continue;
            }
            let comp = String::from_utf8_lossy(comp).into_owned();
            let dir = self.mounts.get(unit)?.path.join(&rel);
            rel.push(match_component(&dir, &comp)?);
        }
        Some(LockRec { unit, rel })
    }

    /// Fill a FileInfoBlock from a host path.
    fn fill_fib(
        &self,
        bus: &mut dyn AddressBus,
        fib: u32,
        rec: &LockRec,
        disk_key: u32,
    ) -> Result<(), u32> {
        let path = self.lock_path(rec);
        let meta = std::fs::metadata(&path).map_err(|_| ERROR_OBJECT_NOT_FOUND)?;
        let name: String = if rec.rel.as_os_str().is_empty() {
            self.mounts[rec.unit].volume.clone()
        } else {
            rec.rel
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let entry_type: i32 = if !meta.is_dir() {
            ST_FILE
        } else if rec.rel.as_os_str().is_empty() {
            ST_ROOT
        } else {
            ST_USERDIR
        };

        bus.write_long(fib, disk_key); // fib_DiskKey: enumeration cursor
        bus.write_long(fib + 4, entry_type as u32); // fib_DirEntryType
                                                    // fib_FileName: BCPL-style, length byte + chars (max 107).
        let bytes: Vec<u8> = name.bytes().take(107).collect();
        bus.write_byte(fib + 8, bytes.len() as u8);
        for (i, &b) in bytes.iter().enumerate() {
            bus.write_byte(fib + 9 + i as u32, b);
        }
        bus.write_byte(fib + 9 + bytes.len() as u32, 0);
        bus.write_long(fib + 116, 0); // fib_Protection: rwed
        bus.write_long(fib + 120, entry_type as u32); // fib_EntryType
        bus.write_long(fib + 124, meta.len().min(u32::MAX as u64) as u32); // fib_Size
        bus.write_long(
            fib + 128,
            meta.len().div_ceil(512).min(u32::MAX as u64) as u32, // fib_NumBlocks
        );
        let (days, mins, ticks) = amiga_datestamp(meta.modified().ok());
        bus.write_long(fib + 132, days); // fib_Date
        bus.write_long(fib + 136, mins);
        bus.write_long(fib + 140, ticks);
        bus.write_byte(fib + 144, 0); // fib_Comment: empty
        Ok(())
    }

    /// Sorted directory listing used by EXAMINE_NEXT. Recomputed per call:
    /// simple and correct for interactive use; cache if it ever shows up.
    fn dir_listing(&self, rec: &LockRec) -> Vec<std::ffi::OsString> {
        let mut names: Vec<_> = std::fs::read_dir(self.lock_path(rec))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        names.sort();
        names
    }

    /// Build the volume DosList node for `unit` in the board window; the
    /// guest handler AddDosEntry's it (only guest code may take the DosList
    /// semaphore). Returns the node's guest address.
    fn build_volume_node(&mut self, bus: &mut dyn AddressBus, unit: usize, port: u32) -> u32 {
        let base = self.board_base.expect("startup packet before DiagPoint");
        let vol = base + VOLUMES_OFFSET + unit as u32 * VOLUME_SLOT_SIZE;
        let (days, mins, ticks) = amiga_datestamp(Some(std::time::SystemTime::now()));
        bus.write_long(vol, 0); // dol_Next
        bus.write_long(vol + 4, 2); // dol_Type = DLT_VOLUME
        bus.write_long(vol + 8, port); // dol_Task: the handler port
        bus.write_long(vol + 12, 0); // dol_Lock
        bus.write_long(vol + 16, days); // dol_VolumeDate
        bus.write_long(vol + 20, mins);
        bus.write_long(vol + 24, ticks);
        bus.write_long(vol + 28, 0); // dol_LockList
        bus.write_long(vol + 32, ID_CLFS_DISK); // dol_DiskType
        bus.write_long(vol + 36, 0); // dol_unused
        bus.write_long(vol + 40, (vol + 44) >> 2); // dol_Name (BSTR)
        let name: Vec<u8> = self.mounts[unit].volume.bytes().take(30).collect();
        bus.write_byte(vol + 44, name.len() as u8);
        for (i, &b) in name.iter().enumerate() {
            bus.write_byte(vol + 45 + i as u32, b);
        }
        bus.write_byte(vol + 45 + name.len() as u32, 0);
        self.volumes.insert(unit, vol);
        vol
    }

    /// Handle one DosPacket; returns (dp_Res1, dp_Res2). When the packet
    /// creates a volume node the guest must AddDosEntry, its address is
    /// stored in `add_volume`.
    fn handle_packet(
        &mut self,
        bus: &mut dyn AddressBus,
        port: u32,
        pkt: u32,
        add_volume: &mut Option<u32>,
    ) -> (u32, u32) {
        let dp_type = bus.read_long(pkt + 8) as i32;
        let arg = |bus: &mut dyn AddressBus, n: u32| bus.read_long(pkt + 20 + 4 * (n - 1));

        // The first packet on a port is the startup packet DOS sends when it
        // starts the handler process (its dp_Type is not meaningful).
        if !self.ports.contains_key(&port) {
            let dn = arg(bus, 3) << 2; // dp_Arg3: BPTR DeviceNode
            let unit = bus.read_long(dn + 28) as usize; // dn_Startup: mount index
            if unit >= self.mounts.len() {
                log::warn!("filesys: startup packet for unknown unit {unit}");
                return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
            }
            bus.write_long(dn + 8, port); // dn_Task = handler port
            self.ports.insert(port, unit);
            *add_volume = Some(self.build_volume_node(bus, unit, port));
            log::info!(
                "filesys: {}: handler started ({}: -> {})",
                device_name(unit),
                self.mounts[unit].volume,
                self.mounts[unit].path.display()
            );
            return (DOSTRUE, 0);
        }
        let unit = self.ports[&port];

        match dp_type {
            ACTION_IS_FILESYSTEM => (DOSTRUE, 0),
            ACTION_DISK_INFO | ACTION_INFO => {
                // DISK_INFO: Arg1 = BPTR InfoData; INFO: Arg2 (Arg1 is a lock).
                let n = if dp_type == ACTION_DISK_INFO { 1 } else { 2 };
                let id = arg(bus, n) << 2;
                // Like the UAE filesys, report the size and free space of the
                // host filesystem holding the mount (statvfs), scaled so the
                // block counts survive AmigaDOS's 32-bit arithmetic.
                let (total, avail) =
                    host_fs_usage(&self.mounts[unit].path).unwrap_or((1 << 30, 1 << 29));
                let (blocksize, numblocks, inuse) = scale_blocks(total, avail);
                let locks_open = self.locks.values().any(|l| l.unit == unit);
                bus.write_long(id, 0); // id_NumSoftErrors
                bus.write_long(id + 4, unit as u32); // id_UnitNumber
                bus.write_long(id + 8, ID_VALIDATED); // id_DiskState
                bus.write_long(id + 12, numblocks); // id_NumBlocks
                bus.write_long(id + 16, inuse); // id_NumBlocksUsed
                bus.write_long(id + 20, blocksize); // id_BytesPerBlock
                bus.write_long(id + 24, ID_CLFS_DISK); // id_DiskType
                let volume = self.volumes.get(&unit).copied().unwrap_or(0);
                bus.write_long(id + 28, volume >> 2); // id_VolumeNode (BPTR)
                bus.write_long(id + 32, if locks_open { DOSTRUE } else { 0 }); // id_InUse
                log::debug!(
                    "filesys: {}: InfoData at {id:#010X}: blocks={numblocks} \
                     used={inuse} bs={blocksize} (host total={total} avail={avail})",
                    device_name(unit)
                );
                (DOSTRUE, 0)
            }
            ACTION_LOCATE_OBJECT => {
                let name_bptr = arg(bus, 2);
                let name = read_bstr(bus, name_bptr);
                let Some(rec) = self.resolve(unit, arg(bus, 1), &name) else {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                };
                if !self.lock_path(&rec).exists() {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                }
                let access = arg(bus, 3);
                match self.alloc_lock(bus, port, access, rec) {
                    Some(addr) => (addr >> 2, 0),
                    None => (DOSFALSE, ERROR_OBJECT_NOT_FOUND),
                }
            }
            ACTION_FREE_LOCK => {
                let addr = arg(bus, 1) << 2;
                if addr != 0 && self.locks.remove(&addr).is_some() {
                    self.free_slots.push(addr);
                }
                (DOSTRUE, 0)
            }
            ACTION_EXAMINE_OBJECT => {
                let Some(rec) = self.locks.get(&(arg(bus, 1) << 2)).cloned() else {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                };
                let fib = arg(bus, 2) << 2;
                match self.fill_fib(bus, fib, &rec, 0) {
                    Ok(()) => (DOSTRUE, 0),
                    Err(e) => (DOSFALSE, e),
                }
            }
            ACTION_EXAMINE_NEXT => {
                let Some(rec) = self.locks.get(&(arg(bus, 1) << 2)).cloned() else {
                    return (DOSFALSE, ERROR_NO_MORE_ENTRIES);
                };
                let fib = arg(bus, 2) << 2;
                // The enumeration cursor lives in fib_DiskKey, where the
                // previous EXAMINE_OBJECT/EXAMINE_NEXT left it.
                let index = bus.read_long(fib) as usize;
                let names = self.dir_listing(&rec);
                let Some(name) = names.get(index) else {
                    return (DOSFALSE, ERROR_NO_MORE_ENTRIES);
                };
                let child = LockRec {
                    unit: rec.unit,
                    rel: rec.rel.join(name),
                };
                match self.fill_fib(bus, fib, &child, index as u32 + 1) {
                    Ok(()) => (DOSTRUE, 0),
                    Err(e) => (DOSFALSE, e),
                }
            }
            _ => {
                log::debug!("filesys: {}: unhandled action {dp_type}", device_name(unit));
                (DOSFALSE, ERROR_ACTION_NOT_KNOWN)
            }
        }
    }
}

impl HleHandler for FilesysHle {
    fn handle_aline(&mut self, cpu: &mut CpuCore, bus: &mut dyn AddressBus, opcode: u16) -> bool {
        if opcode & 0xFF00 != TRAP_BASE {
            return false; // not ours: fall back to the real A-line exception
        }
        // dar[0..8] = D0-D7, dar[8..16] = A0-A7.
        match opcode {
            TRAP_DIAG_ENTRY => {
                self.board_base = Some(cpu.dar[8]);
                log::info!(
                    "filesys: expansion init at board {:#010X}, {} mount(s)",
                    cpu.dar[8],
                    self.mounts.len()
                );
                true
            }
            TRAP_PACKET => {
                let pkt = cpu.dar[1]; // D1
                let port = cpu.dar[9]; // A1
                let dp_type = bus.read_long(pkt + 8) as i32;
                let mut add_volume = None;
                let (res1, res2) = self.handle_packet(bus, port, pkt, &mut add_volume);
                log::debug!(
                    "filesys: packet type {dp_type} at {pkt:#010X} -> \
                     res1={res1:#X} res2={res2}"
                );
                bus.write_long(pkt + 12, res1); // dp_Res1
                bus.write_long(pkt + 16, res2); // dp_Res2
                                                // D0 tells the handler what to do next (reply the packet,
                                                // and for ADDVOLUME also AddDosEntry the node passed in A0).
                cpu.dar[0] = match add_volume {
                    Some(vol) => {
                        cpu.dar[8] = vol; // A0
                        TRAP_RES_ADDVOLUME
                    }
                    None => TRAP_RES_REPLY,
                };
                true
            }
            _ => {
                log::warn!(
                    "filesys: unexpected trap opcode {opcode:#06X} at PC={:#010X}",
                    cpu.pc
                );
                true
            }
        }
    }
}

/// Case-insensitive component match: prefer the exact host name, else scan
/// the directory for a case-insensitive match (AmigaDOS names are
/// case-insensitive but case-preserving).
fn match_component(dir: &Path, comp: &str) -> Option<std::ffi::OsString> {
    if dir.join(comp).exists() {
        return Some(comp.into());
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name())
        .find(|n| n.to_string_lossy().eq_ignore_ascii_case(comp))
}

/// Host mtime -> AmigaDOS DateStamp (days/minutes/ticks since 1978-01-01).
fn amiga_datestamp(time: Option<std::time::SystemTime>) -> (u32, u32, u32) {
    /// Seconds between the Unix epoch and the AmigaDOS epoch.
    const AMIGA_EPOCH_OFFSET: u64 = 252_460_800;
    let secs = time
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .saturating_sub(AMIGA_EPOCH_OFFSET);
    (
        (secs / 86_400) as u32,
        (secs % 86_400 / 60) as u32,
        (secs % 60) as u32 * 50,
    )
}

/// Total and available bytes of the host filesystem containing `path`.
#[cfg(unix)]
fn host_fs_usage(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let frsize = st.f_frsize as u64;
    Some((st.f_blocks as u64 * frsize, st.f_bavail as u64 * frsize))
}

#[cfg(not(unix))]
fn host_fs_usage(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// Fit a host filesystem's size into InfoData block counts. AmigaDOS does
/// 32-bit arithmetic on id_NumBlocks (e.g. multiplies by the block size, and
/// C:Info by 100), so double id_BytesPerBlock until the count is comfortable
/// -- the same trick as the UAE filesys, which also caps the doubling at
/// 32K blocks (beyond ~64 TiB the reported size saturates; nothing Amiga
/// cares).
fn scale_blocks(total: u64, avail: u64) -> (u32, u32, u32) {
    let mut blocksize: u64 = 512;
    while blocksize < 32768 && total / blocksize >= 0x0200_0000 {
        blocksize *= 2;
    }
    let numblocks = (total / blocksize).min(u32::MAX as u64).max(10);
    let free = (avail / blocksize).min(numblocks);
    (
        blocksize as u32,
        numblocks as u32,
        (numblocks - free) as u32,
    )
}

/// Read a BSTR (BPTR to length-prefixed string) from guest memory.
fn read_bstr(bus: &mut dyn AddressBus, bptr: u32) -> Vec<u8> {
    let addr = bptr << 2;
    let len = bus.read_byte(addr) as u32;
    (0..len).map(|i| bus.read_byte(addr + 1 + i)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_mounts() -> Vec<MountSpec> {
        vec![MountSpec {
            path: "/nonexistent".into(),
            volume: "Test".into(),
        }]
    }

    #[test]
    fn board_image_lays_out_rom_mounts_and_diagarea() {
        let img = board_image(&test_mounts());
        assert_eq!(img.len(), 0x1_0000);
        // Fake seglist header: next pointer zero, ROM code at ROM_OFFSET.
        assert_eq!(&img[4..8], &[0, 0, 0, 0]);
        assert_eq!(
            &img[ROM_OFFSET..ROM_OFFSET + FILESYS_HANDLER.len()],
            FILESYS_HANDLER
        );
        // The handler ROM's entry table: two bra.w instructions (process entry at
        // +0, mount entry at +4 -- the DiagArea jsr's board+12).
        assert_eq!(img[ROM_OFFSET], 0x60);
        assert_eq!(img[ROM_OFFSET + 1], 0x00);
        assert_eq!(img[ROM_OFFSET + 4], 0x60);
        assert_eq!(img[ROM_OFFSET + 5], 0x00);

        // Mount table.
        let m = MOUNTS_OFFSET;
        assert_eq!(u16::from_be_bytes([img[m], img[m + 1]]), 1);
        assert_eq!(&img[m + 2..m + 9], b"HOSTFS0");

        // DiagArea header (libraries/configregs.h layout).
        let d = DIAG_OFFSET as usize;
        assert_eq!(img[d], 0x90); // DAC_WORDWIDE | DAC_CONFIGTIME
        assert_eq!(u16::from_be_bytes([img[d + 2], img[d + 3]]), 0x0040); // da_Size
        assert_eq!(u16::from_be_bytes([img[d + 4], img[d + 5]]), 0x0010); // da_DiagPoint
                                                                          // DAC_CONFIGTIME requires a non-zero da_BootPoint (Kickstart 3.x
                                                                          // rejects the whole DiagArea otherwise).
        assert_ne!(u16::from_be_bytes([img[d + 6], img[d + 7]]), 0);

        // DiagPoint code: the trap opcode first, and the jsr into the handler's
        // mount entry at board+12 (= ROM_OFFSET + 4).
        assert_eq!(
            u16::from_be_bytes([img[d + 0x10], img[d + 0x11]]),
            TRAP_DIAG_ENTRY
        );
        assert_eq!(&img[d + 0x18..d + 0x1C], &[0x4E, 0xA8, 0x00, 0x0C]);
        assert_eq!((ROM_OFFSET + 4) as u16, 0x000C);
        // The trap opcode must be A-line (group 0xA) to reach handle_aline.
        assert_eq!(TRAP_BASE >> 12, 0xA);
    }

    #[test]
    fn block_scaling_keeps_counts_32bit_sane() {
        // A small filesystem keeps 512-byte blocks.
        let (bs, blocks, used) = scale_blocks(64 << 20, 16 << 20);
        assert_eq!((bs, blocks), (512, 131072));
        assert_eq!(used, 131072 - 32768);
        // A 512 GB partition scales the block size up so the count stays
        // below the UAE overflow guard (0x02000000 blocks)...
        let (bs, blocks, _) = scale_blocks(1 << 39, 1 << 38);
        assert!(bs > 512 && blocks < 0x0200_0000, "bs={bs} blocks={blocks}");
        assert_eq!(bs as u64 * blocks as u64, 1 << 39);
        // ...and past that the block size caps at 32K, exactly like UAE.
        let (bs, blocks, _) = scale_blocks(8 << 40, 1 << 40);
        assert_eq!(bs, 32768);
        assert_eq!(bs as u64 * blocks as u64, 8 << 40);
        // Free space never exceeds the total.
        let (_, blocks, used) = scale_blocks(1 << 20, u64::MAX);
        assert_eq!(used, 0);
        assert!(blocks >= 10);
    }

    #[test]
    fn host_fs_usage_of_root_is_sane() {
        let (total, avail) = host_fs_usage(Path::new("/")).expect("statvfs /");
        assert!(total > 0 && avail <= total);
    }

    #[test]
    fn datestamp_of_known_moment() {
        // 1978-01-01 00:01:30 UTC = day 0, minute 1, 1500 ticks.
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(252_460_800 + 90);
        assert_eq!(amiga_datestamp(Some(t)), (0, 1, 30 * 50));
    }
}
