//! Host-filesystem service: mount host directories as AmigaDOS volumes
//! (`HOSTFS0:`, `HOSTFS1:`, ...).
//!
//! The guest side is a tiny handler (see `guest/services/`) mapped into the
//! Copperline services board together with a mount table and a hand-built
//! DiagArea. At expansion init the DiagArea's DiagPoint calls the handler's
//! expansion-init entry with the DiagPoint context; the handler builds one
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

use crate::amigaos::dos::*;

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
/// The DiagArea (`BoardSpec::copperline_services` points er_InitDiagVec
/// here): embedded in the handler ROM at +0x40, like real autoboot boards
/// carry theirs in the device ROM (see `_diag_area` in entry.s).
pub const DIAG_OFFSET: u16 = ROM_OFFSET as u16 + 0x40;
/// Per-unit volume DosList nodes, built by the host at handler startup and
/// AddDosEntry'd by the guest handler (TRAP_RES_ADDVOLUME).
const VOLUMES_OFFSET: u32 = 0x7000;
const VOLUME_SLOT_SIZE: u32 = 128;
/// Per-unit FileSysStartupMsg (dn_Startup points here), written by the host
/// at expansion init so the Early Startup boot menu displays a device name,
/// unit, and dostype instead of dereferencing garbage. The shared display
/// device name (BSTR) and DosEnvec follow the 16 slots.
const FSSM_OFFSET: u32 = 0x7800;
const FSSM_SLOT_SIZE: u32 = 16;
const FSSM_DEVNAME_OFFSET: u32 = 0x7900;
/// Per-unit DosEnvec slots (each mount has its own de_BootPri).
const FSSM_ENVEC_OFFSET: u32 = 0x7940;
const ENVEC_SLOT_SIZE: u32 = 68; // sizeof(DosEnvec)
/// Host-managed pool for guest-visible objects (FileLocks), through the end
/// of the 64K window. The guest never touches it.
const POOL_OFFSET: u32 = 0x8000;
const POOL_END: u32 = 0x1_0000;
/// FileLock is 20 bytes; keep slots longword-aligned.
const LOCK_SLOT_SIZE: u32 = 24;

// trap_packet return values (D0); see guest/services/copperline_board.h.
const TRAP_RES_REPLY: u32 = 0;
const TRAP_RES_ADDVOLUME: u32 = 2;
const TRAP_RES_DIE: u32 = 3;

/// Base of the reserved A-line opcode range for filesys host traps. A-line
/// (LINE 1010, exception vector 10) is unused by AmigaOS, so these never
/// collide with guest code.
pub const TRAP_BASE: u16 = 0xA400;
/// DiagPoint entered: logged, and A0 (the board base) is captured.
const TRAP_DIAG_ENTRY: u16 = 0xA400;
/// DosPacket from the handler: D1 = packet APTR, A1 = handler MsgPort.
const TRAP_PACKET: u16 = 0xA402;

/// 'CLFS' -- our own id_DiskType, honest about not being an FFS at all.
/// (The UAE filesys reports 'DOS\1' here instead; if some tool turns out to
/// insist on a DOS\x dostype, reconsider.)
const ID_CLFS_DISK: u32 = 0x434C_4653;

/// One `[[filesys]]` entry: a host directory exported as an AmigaDOS
/// device `HOSTFS<n>:` with the given volume name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountSpec {
    pub path: PathBuf,
    pub volume: String,
    /// AddBootNode priority: -128 = mounted but never a boot candidate
    /// (the default); higher beats other boot devices in strap's ranking.
    pub boot_pri: i8,
}

/// DOS device name of mount `unit` (`HOSTFS0`, `HOSTFS1`, ...).
pub fn device_name(unit: usize) -> String {
    format!("HOSTFS{unit}")
}

/// Build the 64K board window: fake seglist header, the handler ROM (which
/// embeds the DiagArea), and the mount table.
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

    img
}

/// DosList surgery the guest handler performs after replying a packet
/// (only guest code may take the DosList semaphore); maps to a TRAP_RES_*
/// code with the node address in A0.
enum GuestOp {
    /// AddDosEntry the volume node the host built.
    AddVolume(u32),
    /// ACTION_DIE: RemDosEntry the volume node, then exit the process.
    Die(u32),
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
/// ACTION_* semantics against the host directories in `mounts`.
///
/// "Hle" is the m68k crate's HleHandler trait: High-Level Emulation, the
/// hook that intercepts reserved opcodes on the host side instead of letting
/// the guest take the exception. Installed as the CPU's HLE handler; it
/// reacts only to the reserved [`TRAP_BASE`] range, so leaving it installed
/// with no mounts configured changes nothing.
#[derive(Default)]
pub struct FilesysHle {
    mounts: Vec<MountSpec>,
    /// Board base address, captured from A0 at the DiagPoint trap.
    board_base: Option<u32>,
    /// Handler MsgPort address -> mount unit, learned from startup packets.
    /// All per-boot state is cleared at the TRAP_DIAG_ENTRY of the next
    /// boot (expansion init runs exactly once per boot).
    ports: HashMap<u32, usize>,
    /// Mount unit -> guest address of its volume DosList node.
    volumes: HashMap<usize, u32>,
    /// Mount unit -> guest address of its DeviceNode (from the startup
    /// packet), for clearing dn_Task at ACTION_DIE.
    device_nodes: HashMap<usize, u32>,
    /// Open files by fh_Arg1 cookie (host-side only, no guest structure).
    /// The LockRec remembers what the handle refers to, for EXAMINE_FH.
    files: HashMap<u32, (std::fs::File, LockRec)>,
    next_file_key: u32,
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
        let lock = FileLock {
            link: long(0),
            key: long(addr),
            access: long(access),
            task: long(port),
            volume: long(self.volumes.get(&rec.unit).copied().unwrap_or(0) >> 2),
        };
        write_bytes(bus, addr, lock.as_bytes());
        self.locks.insert(addr, rec);
        Some(addr)
    }

    /// Resolve a DOS path (BPTR lock + name) to a lock record. AmigaDOS path
    /// semantics: an optional `prefix:` is stripped (the supplied lock is
    /// already the base it named), `/` goes to the parent, and names are
    /// case-insensitive.
    fn resolve(&self, unit: usize, lock_bptr: u32, name: &[u8]) -> Option<LockRec> {
        let (unit, mut rel) = if lock_bptr != 0 {
            let rec = self.locks.get(&(lock_bptr << 2))?;
            (rec.unit, rec.rel.clone())
        } else {
            (unit, PathBuf::new())
        };

        let mut rest = name;
        if let Some(colon) = name.iter().position(|&b| b == b':') {
            if !name[..colon].contains(&b'/') {
                // "Volume:" or "Assign:" prefix: DOS routes the packet by
                // the prefix but passes the user's string through unmodified,
                // and the supplied lock is already the right base -- for an
                // assign like LIBS: it IS the target directory. Strip the
                // prefix and stay at the lock (matches UAE's get_aino;
                // restarting at the root instead broke "LIBS:foo" opens).
                rest = &name[colon + 1..];
            }
        }
        if rest.is_empty() {
            // Bare "DEVICE:" or an empty name: the base itself. (split()
            // below would yield one empty component = "parent", wrong.)
            return Some(LockRec { unit, rel });
        }
        // A single trailing '/' does not mean parent: "Sub/" is Sub itself
        // (the "directory part" convention; verified against FFS, where
        // "Prefs/" lists Prefs but "Prefs//" lists its parent).
        let mut comps: Vec<&[u8]> = rest.split(|&b| b == b'/').collect();
        if comps.last() == Some(&&b""[..]) {
            comps.pop();
        }
        for comp in comps {
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

        // Protection, datestamp, and comment come from the UAE `.uaem`
        // sidecar when one exists (written by UAE-family emulators for the
        // attributes a host filesystem cannot hold: script/pure/archive,
        // comments, exact datestamps). Otherwise fall back to what the host
        // can express: read-only denies `w`; `e` stays allowed (mapping
        // Unix x to it would stop most copied binaries from running) --
        // both matching the UAE fsdb.
        let uaem = read_uaem(&path);
        let protection = match &uaem {
            Some(u) => u.protection,
            None if meta.permissions().readonly() => FIBF_WRITE,
            None => 0,
        };
        let (days, mins, ticks) = uaem
            .as_ref()
            .and_then(|u| u.date)
            .unwrap_or_else(|| amiga_datestamp(meta.modified().ok()));
        let comment = uaem.map(|u| u.comment).unwrap_or_default();
        let fib_data = FileInfoBlock {
            disk_key: long(disk_key),
            dir_entry_type: long(entry_type as u32),
            file_name: bcpl::<108>(name.as_bytes()),
            protection: long(protection),
            entry_type: long(entry_type as u32),
            size: long(meta.len().min(u32::MAX as u64) as u32),
            num_blocks: long(meta.len().div_ceil(512).min(u32::MAX as u64) as u32),
            date: [long(days), long(mins), long(ticks)],
            comment: bcpl::<80>(&comment),
        };
        write_bytes(bus, fib, fib_data.as_bytes());
        Ok(())
    }

    /// Sorted directory listing used by EXAMINE_NEXT, hiding the `.uaem`
    /// metadata sidecars (their contents surface as the companion file's
    /// attributes instead). Recomputed per call: simple and correct for
    /// interactive use; cache if it ever shows up.
    fn dir_listing(&self, rec: &LockRec) -> Vec<std::ffi::OsString> {
        let mut names: Vec<_> = std::fs::read_dir(self.lock_path(rec))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| !n.as_encoded_bytes().ends_with(b".uaem"))
            .collect();
        names.sort();
        names
    }

    /// Write one FileSysStartupMsg per mount into the board window, plus the
    /// shared display device name and the per-unit DosEnvecs they reference.
    /// dn_Startup points at these so the Early Startup boot menu shows
    /// "CLFS hostfs-N" instead of dereferencing garbage, ACTION_STARTUP reads
    /// the unit back from fssm_Unit, and the guest handler passes de_BootPri
    /// to AddBootNode.
    fn write_startup_msgs(&self, bus: &mut dyn AddressBus, base: u32) {
        write_bytes(bus, base + FSSM_DEVNAME_OFFSET, &bcpl::<32>(b"hostfs"));
        for (unit, mount) in self.mounts.iter().enumerate() {
            let unit = unit as u32;
            let envec = DosEnvec {
                table_size: long(16),  // entries after this one, through dos_type
                size_block: long(128), // longwords = 512-byte blocks
                sec_org: long(0),
                surfaces: long(1),
                sectors_per_block: long(1),
                blocks_per_track: long(1),
                reserved: long(2),
                pre_alloc: long(0),
                interleave: long(0),
                low_cyl: long(0),
                high_cyl: long(0),
                num_buffers: long(1),
                buf_mem_type: long(1), // MEMF_PUBLIC
                max_transfer: long(0x7FFF_FFFF),
                mask: long(0xFFFF_FFFE),
                boot_pri: long(mount.boot_pri as i32 as u32),
                dos_type: long(ID_CLFS_DISK),
            };
            let envec_at = base + FSSM_ENVEC_OFFSET + unit * ENVEC_SLOT_SIZE;
            write_bytes(bus, envec_at, envec.as_bytes());
            let fssm = FileSysStartupMsg {
                unit: long(unit),
                device: long((base + FSSM_DEVNAME_OFFSET) >> 2),
                environ: long(envec_at >> 2),
                flags: long(0),
            };
            write_bytes(
                bus,
                base + FSSM_OFFSET + unit * FSSM_SLOT_SIZE,
                fssm.as_bytes(),
            );
        }
    }

    /// Build the volume DosList node for `unit` in the board window; the
    /// guest handler AddDosEntry's it (only guest code may take the DosList
    /// semaphore). Returns the node's guest address.
    fn build_volume_node(&mut self, bus: &mut dyn AddressBus, unit: usize, port: u32) -> u32 {
        let base = self.board_base.expect("startup packet before DiagPoint");
        let vol = base + VOLUMES_OFFSET + unit as u32 * VOLUME_SLOT_SIZE;
        let fixed = std::mem::size_of::<VolumeNode>() as u32;
        let (days, mins, ticks) = amiga_datestamp(Some(std::time::SystemTime::now()));
        let node = VolumeNode {
            next: long(0),
            r#type: long(2), // DLT_VOLUME
            task: long(port),
            lock: long(0),
            volume_date: [long(days), long(mins), long(ticks)],
            lock_list: long(0),
            disk_type: long(ID_CLFS_DISK),
            unused: long(0),
            name: long((vol + fixed) >> 2), // BSTR right after the struct
        };
        write_bytes(bus, vol, node.as_bytes());
        let name: Vec<u8> = self.mounts[unit].volume.bytes().take(30).collect();
        write_bytes(bus, vol + fixed, &bcpl::<32>(&name));
        self.volumes.insert(unit, vol);
        vol
    }

    /// Handle one DosPacket; returns (dp_Res1, dp_Res2). Some packets also
    /// need DosList surgery only the guest may perform (the semaphore);
    /// `guest_op` tells the handler what to do after replying.
    fn handle_packet(
        &mut self,
        bus: &mut dyn AddressBus,
        port: u32,
        pkt: u32,
        guest_op: &mut Option<GuestOp>,
    ) -> (u32, u32) {
        let dp_type = bus.read_long(pkt + 8) as i32;
        let arg = |bus: &mut dyn AddressBus, n: u32| bus.read_long(pkt + 20 + 4 * (n - 1));

        // The first packet on a port is the startup packet DOS sends when it
        // starts the handler process (its dp_Type is not meaningful).
        if !self.ports.contains_key(&port) {
            let dn = arg(bus, 3) << 2; // dp_Arg3: BPTR DeviceNode
                                       // dn_Startup is a BPTR to the unit's FileSysStartupMsg (written
                                       // by write_startup_msgs); fssm_Unit is its first field.
            let fssm = bus.read_long(dn + DEVICENODE_STARTUP) << 2;
            let unit = bus.read_long(fssm) as usize;
            if unit >= self.mounts.len() {
                log::warn!("filesys: startup packet for unknown unit {unit}");
                return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
            }
            bus.write_long(dn + DEVICENODE_TASK, port);
            self.ports.insert(port, unit);
            self.device_nodes.insert(unit, dn);
            *guest_op = Some(GuestOp::AddVolume(self.build_volume_node(bus, unit, port)));
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
                let info = InfoData {
                    num_soft_errors: long(0),
                    unit_number: long(unit as u32),
                    // Read-only for now: shows as "Read Only" in C:Info.
                    disk_state: long(ID_WRITE_PROTECTED),
                    num_blocks: long(numblocks),
                    num_blocks_used: long(inuse),
                    bytes_per_block: long(blocksize),
                    disk_type: long(ID_CLFS_DISK),
                    volume_node: long(self.volumes.get(&unit).copied().unwrap_or(0) >> 2),
                    in_use: long(if locks_open { DOSTRUE } else { 0 }),
                };
                write_bytes(bus, id, info.as_bytes());
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
                log::debug!(
                    "filesys: {}: locate \"{}\" (lock {:#X})",
                    device_name(unit),
                    String::from_utf8_lossy(&name),
                    arg(bus, 1)
                );
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
            ACTION_COPY_DIR => {
                // DupLock(): Arg1 = lock (0 = the root); result is a new
                // shared lock on the same object.
                let bptr = arg(bus, 1);
                let rec = if bptr == 0 {
                    LockRec {
                        unit,
                        rel: PathBuf::new(),
                    }
                } else {
                    match self.locks.get(&(bptr << 2)) {
                        Some(r) => r.clone(),
                        None => return (DOSFALSE, ERROR_INVALID_LOCK),
                    }
                };
                match self.alloc_lock(bus, port, ACCESS_READ, rec) {
                    Some(addr) => (addr >> 2, 0),
                    None => (DOSFALSE, ERROR_OBJECT_NOT_FOUND),
                }
            }
            ACTION_PARENT => {
                // Arg1 = lock; result is a shared lock on its parent, or 0
                // with no error for the root.
                let Some(rec) = self.locks.get(&(arg(bus, 1) << 2)).cloned() else {
                    return (DOSFALSE, ERROR_INVALID_LOCK);
                };
                if rec.rel.as_os_str().is_empty() {
                    return (DOSFALSE, 0); // the root has no parent
                }
                let mut rel = rec.rel;
                rel.pop();
                let parent = LockRec {
                    unit: rec.unit,
                    rel,
                };
                match self.alloc_lock(bus, port, ACCESS_READ, parent) {
                    Some(addr) => (addr >> 2, 0),
                    None => (DOSFALSE, ERROR_OBJECT_NOT_FOUND),
                }
            }
            ACTION_SET_PROTECT => {
                // Arg2 = lock, Arg3 = BSTR name, Arg4 = mask. The host
                // filesystem has nowhere faithful to keep Amiga protection
                // bits; accept and ignore (like a FAT filesystem would).
                // TODO(codewiz): persist to the .uaem sidecar once write
                // support lands, so protection round-trips.
                let name_bptr = arg(bus, 3);
                let name = read_bstr(bus, name_bptr);
                match self.resolve(unit, arg(bus, 2), &name) {
                    Some(rec) if self.lock_path(&rec).exists() => (DOSTRUE, 0),
                    _ => (DOSFALSE, ERROR_OBJECT_NOT_FOUND),
                }
            }
            ACTION_FINDINPUT => {
                // Open(MODE_OLDFILE): Arg1 = BPTR FileHandle, Arg2 = lock,
                // Arg3 = BSTR name. On success fh_Arg1 carries our cookie.
                let fh = arg(bus, 1) << 2;
                let name_bptr = arg(bus, 3);
                let name = read_bstr(bus, name_bptr);
                log::debug!(
                    "filesys: {}: open \"{}\" (lock {:#X})",
                    device_name(unit),
                    String::from_utf8_lossy(&name),
                    arg(bus, 2)
                );
                let Some(rec) = self.resolve(unit, arg(bus, 2), &name) else {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                };
                let path = self.lock_path(&rec);
                if path.is_dir() {
                    return (DOSFALSE, ERROR_OBJECT_WRONG_TYPE);
                }
                match std::fs::File::open(&path) {
                    Ok(f) => {
                        self.next_file_key += 1;
                        let key = self.next_file_key;
                        self.files.insert(key, (f, rec));
                        bus.write_long(fh + FILEHANDLE_ARG1, key);
                        (DOSTRUE, 0)
                    }
                    Err(_) => (DOSFALSE, ERROR_OBJECT_NOT_FOUND),
                }
            }
            ACTION_SAME_LOCK => {
                // SameLock(): Arg1/Arg2 = locks (0 = the root). DOSTRUE if
                // they reference the same object.
                let rec_of = |hle: &Self, bptr: u32| -> Option<LockRec> {
                    if bptr == 0 {
                        Some(LockRec {
                            unit,
                            rel: PathBuf::new(),
                        })
                    } else {
                        hle.locks.get(&(bptr << 2)).cloned()
                    }
                };
                let (Some(a), Some(b)) = (rec_of(self, arg(bus, 1)), rec_of(self, arg(bus, 2)))
                else {
                    return (DOSFALSE, ERROR_INVALID_LOCK);
                };
                if a.unit == b.unit && a.rel == b.rel {
                    (DOSTRUE, 0)
                } else {
                    (DOSFALSE, 0)
                }
            }
            ACTION_FH_FROM_LOCK => {
                // OpenFromLock(): Arg1 = BPTR FileHandle, Arg2 = lock. On
                // success the handle absorbs the lock (the caller must not
                // free it); on failure the lock stays valid.
                let fh = arg(bus, 1) << 2;
                let lock_addr = arg(bus, 2) << 2;
                let Some(rec) = self.locks.get(&lock_addr).cloned() else {
                    return (DOSFALSE, ERROR_INVALID_LOCK);
                };
                let path = self.lock_path(&rec);
                if path.is_dir() {
                    return (DOSFALSE, ERROR_OBJECT_WRONG_TYPE);
                }
                match std::fs::File::open(&path) {
                    Ok(f) => {
                        self.next_file_key += 1;
                        let key = self.next_file_key;
                        self.files.insert(key, (f, rec));
                        bus.write_long(fh + FILEHANDLE_ARG1, key);
                        self.locks.remove(&lock_addr);
                        self.free_slots.push(lock_addr);
                        (DOSTRUE, 0)
                    }
                    Err(_) => (DOSFALSE, ERROR_OBJECT_NOT_FOUND),
                }
            }
            ACTION_PARENT_FH => {
                // ParentOfFH(): Arg1 = fh_Arg1 cookie; result is a shared
                // lock on the directory containing the open file.
                let rec = match self.files.get(&arg(bus, 1)) {
                    Some((_, rec)) => rec.clone(),
                    None => return (DOSFALSE, ERROR_INVALID_LOCK),
                };
                let mut rel = rec.rel;
                rel.pop();
                let parent = LockRec {
                    unit: rec.unit,
                    rel,
                };
                match self.alloc_lock(bus, port, ACCESS_READ, parent) {
                    Some(addr) => (addr >> 2, 0),
                    None => (DOSFALSE, ERROR_OBJECT_NOT_FOUND),
                }
            }
            ACTION_EXAMINE_FH => {
                // ExamineFH(): Arg1 = fh_Arg1 cookie, Arg2 = BPTR FIB.
                let rec = match self.files.get(&arg(bus, 1)) {
                    Some((_, rec)) => rec.clone(),
                    None => return (DOSFALSE, ERROR_INVALID_LOCK),
                };
                let fib = arg(bus, 2) << 2;
                match self.fill_fib(bus, fib, &rec, 0) {
                    Ok(()) => (DOSTRUE, 0),
                    Err(e) => (DOSFALSE, e),
                }
            }
            ACTION_FLUSH => {
                // Nothing buffered on the host side; success by definition.
                (DOSTRUE, 0)
            }
            ACTION_READ => {
                // Arg1 = fh_Arg1 cookie, Arg2 = buffer APTR, Arg3 = length.
                // Res1 = bytes read (0 = EOF), or -1 with Res2 = error.
                use std::io::Read;
                let key = arg(bus, 1);
                let buf = arg(bus, 2);
                let len = arg(bus, 3) as usize;
                let Some((f, _)) = self.files.get_mut(&key) else {
                    return (DOSTRUE, ERROR_INVALID_LOCK); // res1 = -1
                };
                // Transfer in bounded chunks: the length is guest-supplied, so
                // allocating it up front would let a bogus multi-GB read force
                // an unbounded host allocation (OOM/DoS). DOS reads a regular
                // file fully, so loop until len bytes or EOF -- same result,
                // capped memory.
                const READ_CHUNK: usize = 64 * 1024;
                let mut chunk = vec![0u8; len.min(READ_CHUNK)];
                let mut done = 0usize;
                while done < len {
                    let want = (len - done).min(READ_CHUNK);
                    match f.read(&mut chunk[..want]) {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            for (i, &b) in chunk[..n].iter().enumerate() {
                                bus.write_byte(buf + (done + i) as u32, b);
                            }
                            done += n;
                        }
                        Err(_) => return (DOSTRUE, ERROR_SEEK_ERROR), // res1 = -1
                    }
                }
                (done as u32, 0)
            }
            ACTION_SEEK => {
                // Arg1 = fh_Arg1, Arg2 = position, Arg3 = OFFSET_* mode.
                // Res1 = previous position, or -1 with Res2 = error.
                use std::io::{Seek, SeekFrom};
                let key = arg(bus, 1);
                let pos = arg(bus, 2) as i64;
                let mode = arg(bus, 3) as i32;
                let Some((f, _)) = self.files.get_mut(&key) else {
                    return (DOSTRUE, ERROR_INVALID_LOCK); // res1 = -1
                };
                let (old, end) = match (f.stream_position(), f.metadata()) {
                    (Ok(o), Ok(m)) => (o, m.len() as i64),
                    _ => return (DOSTRUE, ERROR_SEEK_ERROR),
                };
                let target = match mode {
                    OFFSET_BEGINNING => pos,
                    OFFSET_CURRENT => old as i64 + pos,
                    OFFSET_END => end + pos,
                    _ => -1,
                };
                if target < 0 || target > end {
                    return (DOSTRUE, ERROR_SEEK_ERROR); // res1 = -1
                }
                match f.seek(SeekFrom::Start(target as u64)) {
                    Ok(_) => (old as u32, 0),
                    Err(_) => (DOSTRUE, ERROR_SEEK_ERROR),
                }
            }
            ACTION_END => {
                self.files.remove(&arg(bus, 1));
                (DOSTRUE, 0)
            }
            ACTION_DIE => {
                // Shut the handler down (dismount tools send this; stock
                // Assign DISMOUNT only unlinks the DeviceNode). Refuse
                // while anything is held open, like a real handler.
                let in_use = self.locks.values().any(|l| l.unit == unit)
                    || self.files.values().any(|(_, r)| r.unit == unit);
                if in_use {
                    return (DOSFALSE, ERROR_OBJECT_IN_USE);
                }
                // Clear dn_Task so the next reference to the device simply
                // restarts the handler (and re-adds the volume).
                if let Some(dn) = self.device_nodes.remove(&unit) {
                    bus.write_long(dn + DEVICENODE_TASK, 0);
                }
                self.ports.remove(&port);
                let vol = self.volumes.remove(&unit).unwrap_or(0);
                *guest_op = Some(GuestOp::Die(vol));
                log::info!("filesys: {}: ACTION_DIE, handler exits", device_name(unit));
                (DOSTRUE, 0)
            }
            // Write-family actions: mounts are read-only for now, so the
            // proper refusal is "write protected", not "unknown packet".
            // Write() and SetFileSize() signal failure with Res1 = -1.
            ACTION_WRITE | ACTION_SET_FILE_SIZE => (DOSTRUE, ERROR_DISK_WRITE_PROTECTED),
            ACTION_FINDOUTPUT | ACTION_FINDUPDATE | ACTION_CREATE_DIR | ACTION_DELETE_OBJECT
            | ACTION_RENAME_OBJECT | ACTION_SET_COMMENT | ACTION_SET_DATE | ACTION_RENAME_DISK => {
                (DOSFALSE, ERROR_DISK_WRITE_PROTECTED)
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
                // Expansion init runs exactly once per boot: (re)capture the
                // board base and drop every per-boot structure -- after a
                // warm reboot the old ports, locks, and open files are
                // stale, and exec tends to reallocate the new handler ports
                // at the same addresses, which would misroute the startup
                // packets of the new boot. Rebuild from a fresh default,
                // keeping only the configured mounts, so a newly added
                // per-boot field can never be left un-reset (this is how
                // next_file_key came to be missed).
                let mounts = std::mem::take(&mut self.mounts);
                *self = FilesysHle {
                    mounts,
                    board_base: Some(cpu.dar[8]),
                    ..Self::default()
                };
                self.write_startup_msgs(bus, cpu.dar[8]);
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
                let mut guest_op = None;
                let (res1, res2) = self.handle_packet(bus, port, pkt, &mut guest_op);
                let unit = self.ports.get(&port).copied();
                log::debug!(
                    "filesys: {}: packet type {dp_type} at {pkt:#010X} -> \
                     res1={res1:#X} res2={res2}",
                    unit.map_or("?".into(), device_name),
                );
                bus.write_long(pkt + 12, res1); // dp_Res1
                bus.write_long(pkt + 16, res2); // dp_Res2
                                                // D0 tells the handler what to do next (reply the packet,
                                                // then Add/RemDosEntry the node passed in A0).
                cpu.dar[0] = match guest_op {
                    Some(GuestOp::AddVolume(vol)) => {
                        cpu.dar[8] = vol; // A0
                        TRAP_RES_ADDVOLUME
                    }
                    Some(GuestOp::Die(vol)) => {
                        cpu.dar[8] = vol; // A0
                        TRAP_RES_DIE
                    }
                    None => TRAP_RES_REPLY,
                };
                true
            }
            _ => {
                // Not one of our two traps (0xA400/0xA402): hand the opcode
                // back so the CPU takes the real A-line exception, as on
                // hardware. Any other 0xA4xx is the guest's own trap, not
                // ours to swallow.
                log::trace!(
                    "filesys: passing through A-line {opcode:#06X} at PC={:#010X}",
                    cpu.pc
                );
                false
            }
        }
    }
}

/// Metadata from a UAE `.uaem` sidecar file: the attributes a host
/// filesystem cannot hold (script/pure/archive bits, file comment, exact
/// datestamp), written by UAE-family emulators next to the real file.
struct UaemInfo {
    /// fib_Protection value (deny-style rwed like the FIB wants).
    protection: u32,
    /// DateStamp, when the sidecar's timestamp parses.
    date: Option<DateStamp>,
    comment: Vec<u8>,
}

/// Read and parse the `.uaem` sidecar of `path`, if any.
fn read_uaem(path: &Path) -> Option<UaemInfo> {
    let mut side = path.as_os_str().to_owned();
    side.push(".uaem");
    parse_uaem(&std::fs::read(Path::new(&side)).ok()?)
}

/// Parse a `.uaem` sidecar: one line, eight flag letters ("hsparwed", a
/// letter means the bit is on), a "YYYY-MM-DD HH:MM:SS.CC" timestamp, and
/// an optional comment. Same grammar as the UAE fsdb, including the
/// `^ 0xF` flip of the rwed group into the FIB's deny convention.
fn parse_uaem(data: &[u8]) -> Option<UaemInfo> {
    let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data); // BOM
    let flags = data.get(..8)?;
    let mut mode = 0u32;
    for (i, (&c, &l)) in flags.iter().zip(b"hsparwed").enumerate() {
        if c == l {
            mode |= 1 << (7 - i);
        }
    }
    let mut rest = &data[8..];
    while let Some(r) = rest.strip_prefix(b" ") {
        rest = r;
    }
    let (date, after) = match parse_uaem_date(rest) {
        Some((d, a)) => (Some(d), a),
        None => (None, rest),
    };
    let comment = after
        .strip_prefix(b" ")
        .and_then(|c| c.split(|&b| b == b'\n' || b == b'\r').next())
        .unwrap_or_default()
        .to_vec();
    Some(UaemInfo {
        protection: mode ^ 0xF,
        date,
        comment,
    })
}

/// Parse "YYYY-MM-DD HH:MM:SS.CC" into a DateStamp, returning the rest.
/// Converted directly from the civil date (no timezone round trip): the
/// sidecar records the guest's original DateStamp rendered as text.
fn parse_uaem_date(s: &[u8]) -> Option<(DateStamp, &[u8])> {
    let t = std::str::from_utf8(s.get(..22)?).ok()?;
    let b = t.as_bytes();
    if !(b[4] == b'-' && b[7] == b'-' && b[10] == b' ' && b[13] == b':') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| t[r].parse::<i64>().ok();
    let days = days_from_civil(num(0..4)?, num(5..7)?, num(8..10)?) - days_from_civil(1978, 1, 1);
    let mins = num(11..13)? * 60 + num(14..16)?;
    let ticks = num(17..19)? * 50 + num(20..22)? / 2;
    if days < 0 {
        return None; // before the AmigaDOS epoch
    }
    Some(((days as u32, mins as u32, ticks as u32), &s[22..]))
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719_468
}

/// Case-insensitive component match: prefer the exact host name, else scan
/// the directory for a case-insensitive match (AmigaDOS names are
/// case-insensitive but case-preserving).
fn match_component(dir: &Path, comp: &str) -> Option<std::ffi::OsString> {
    // "." and ".." are not directory shortcuts in AmigaDOS ("/" is the
    // parent), but the host would honor them and ".." escapes the mount.
    if comp == "." || comp == ".." {
        return None;
    }
    if dir.join(comp).exists() {
        return Some(comp.into());
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name())
        .find(|n| n.to_string_lossy().eq_ignore_ascii_case(comp))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_mounts() -> Vec<MountSpec> {
        vec![MountSpec {
            path: "/nonexistent".into(),
            volume: "Test".into(),
            boot_pri: -128,
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
        // The handler ROM's entry table: process entry (bra.w) at +0, the
        // expansion-init entry at +4 starting with the TRAP_DIAG_ENTRY
        // opcode (see guest/services/entry.s).
        assert_eq!(img[ROM_OFFSET], 0x60);
        assert_eq!(img[ROM_OFFSET + 1], 0x00);
        assert_eq!(
            u16::from_be_bytes([img[ROM_OFFSET + 4], img[ROM_OFFSET + 5]]),
            TRAP_DIAG_ENTRY
        );

        // Mount table.
        let m = MOUNTS_OFFSET;
        assert_eq!(u16::from_be_bytes([img[m], img[m + 1]]), 1);
        assert_eq!(&img[m + 2..m + 9], b"HOSTFS0");

        // DiagArea header, embedded in the ROM at DIAG_OFFSET (see
        // `_diag_area` in guest/services/entry.s).
        let d = DIAG_OFFSET as usize;
        assert_eq!(img[d], 0x90); // DAC_WORDWIDE | DAC_CONFIGTIME
        let da_size = u16::from_be_bytes([img[d + 2], img[d + 3]]) as usize;
        let da_diag = u16::from_be_bytes([img[d + 4], img[d + 5]]) as usize;
        let da_boot = u16::from_be_bytes([img[d + 6], img[d + 7]]) as usize;
        let da_name = u16::from_be_bytes([img[d + 8], img[d + 9]]) as usize;
        // DAC_CONFIGTIME requires a non-zero da_BootPoint (Kickstart 3.x
        // rejects the whole DiagArea otherwise), and everything referenced
        // must lie inside the copied da_Size bytes.
        assert!(da_diag != 0 && da_diag < da_size);
        assert!(da_boot != 0 && da_boot < da_size);
        assert!(da_name != 0 && da_name < da_size);
        assert!(d + da_size <= ROM_OFFSET + FILESYS_HANDLER.len());
        // The DiagPoint stub reaches the ROM's expansion-init entry through
        // the board base: jsr 12(a0) (12 = ROM_OFFSET + 4), then rts.
        assert_eq!(
            &img[d + da_diag..d + da_diag + 6],
            &[0x4E, 0xA8, 0x00, 0x0C, 0x4E, 0x75]
        );
        assert_eq!((ROM_OFFSET + 4) as u16, 0x000C);
        assert_eq!(&img[d + da_name..d + da_name + 11], b"Copperline\0");
        // The trap opcode must be A-line (group 0xA) to reach handle_aline.
        assert_eq!(TRAP_BASE >> 12, 0xA);
        // The per-unit DosEnvec array must not run into the lock pool.
        assert!(FSSM_ENVEC_OFFSET + MOUNT_MAX_COUNT as u32 * ENVEC_SLOT_SIZE <= POOL_OFFSET);
        assert_eq!(ENVEC_SLOT_SIZE as usize, std::mem::size_of::<DosEnvec>());
    }

    #[test]
    fn resolve_strips_the_prefix_and_keeps_the_lock_base() {
        let root = std::env::temp_dir().join(format!("clfs-resolve-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Libs")).unwrap();
        std::fs::write(root.join("Libs/68040.library"), b"x").unwrap();

        let mut hle = FilesysHle::default();
        hle.set_mounts(vec![MountSpec {
            path: root.clone(),
            volume: "Test".into(),
            boot_pri: -128,
        }]);
        // A lock on Libs, as DOS supplies with opens through the LIBS:
        // assign. The name still carries the user's "LIBS:" prefix; it
        // must be stripped without resetting to the root.
        hle.locks.insert(
            0x1000,
            LockRec {
                unit: 0,
                rel: "Libs".into(),
            },
        );
        let rec = hle.resolve(0, 0x1000 >> 2, b"LIBS:68040.library").unwrap();
        assert_eq!(rec.rel, PathBuf::from("Libs/68040.library"));
        // A volume prefix with no lock starts at the root as before.
        let rec = hle.resolve(0, 0, b"Test:Libs/68040.library").unwrap();
        assert_eq!(rec.rel, PathBuf::from("Libs/68040.library"));

        // Host dot-dirs must not act as path components: ".." would
        // escape the mount root ("." and ".." are legal-ish AmigaDOS
        // names with no special meaning; "/" is the parent).
        assert!(hle.resolve(0, 0, b"..").is_none());
        assert!(hle.resolve(0, 0, b"Libs/../../etc").is_none());
        assert!(hle.resolve(0, 0, b"Libs/.").is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn uaem_sidecar_parses_flags_date_and_comment() {
        // A real line written by Amiberry for S/Shell-Startup: script bit
        // set, execute denied (rwed stored as "allowed" letters, flipped
        // into the FIB's deny convention by ^ 0xF).
        let info = parse_uaem(b"-s--rw-d 2026-07-10 16:16:51.32").unwrap();
        assert_eq!(info.protection, 0x42); // FIBF_SCRIPT | FIBF_EXECUTE
                                           // 2026-07-10 is 17722 days after 1978-01-01.
        assert_eq!(info.date, Some((17722, 16 * 60 + 16, 51 * 50 + 32 / 2)));
        assert!(info.comment.is_empty());

        let info = parse_uaem(b"----rwed 1985-07-23 12:00:00.00 hello world\n").unwrap();
        assert_eq!(info.protection, 0);
        assert_eq!(info.comment, b"hello world");

        // Pure (resident-able) tool: p bit, all of rwed allowed.
        let info = parse_uaem(b"--p-rwed 1992-05-01 00:00:00.00").unwrap();
        assert_eq!(info.protection, 0x20); // FIBF_PURE

        // Flags but no parsable date: attributes still apply.
        let info = parse_uaem(b"---arwed").unwrap();
        assert_eq!(info.protection, 0x10); // FIBF_ARCHIVE
        assert_eq!(info.date, None);
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
}
