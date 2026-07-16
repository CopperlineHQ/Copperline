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
/// Maximum host mounts (units), and the divisor for each unit's fixed
/// board-window lock-pool slice.
pub const MOUNT_MAX_COUNT: usize = 8;
/// Longest AmigaDOS volume label: a DosList BSTR holds 30 bytes.
pub const VOLUME_NAME_MAX: usize = 30;

/// Why `name` would not work as an AmigaDOS volume label, or `None` if it
/// would. Shared by the config validator and the launcher's name editor so
/// the GUI cannot save a name the config would reject.
pub fn volume_name_error(name: &str) -> Option<String> {
    if name.is_empty() {
        Some("volume name must not be empty".to_string())
    } else if name.len() > VOLUME_NAME_MAX {
        Some(format!(
            "volume name {name:?} is too long ({} bytes; max {VOLUME_NAME_MAX})",
            name.len()
        ))
    } else if name.contains([':', '/', '\0']) {
        Some(format!(
            "volume name {name:?} contains an invalid character (no ':' '/' or NUL)"
        ))
    } else {
        None
    }
}
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
    /// Refuse every write, answering like a write-protected disk. Off by
    /// default: the mount is the host directory, and the guest can change it.
    pub readonly: bool,
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

/// A handed-out FileLock: the path relative to its unit's mount root (empty =
/// the root itself). Keyed by the guest address of the FileLock structure in
/// the board-window pool, and always stored in its owning unit.
#[derive(Debug, Clone)]
struct LockRec {
    rel: PathBuf,
}

/// One unit's slice of the board-window FileLock pool: a bump cursor over the
/// board-relative range up to `end`, plus a free list of recycled absolute
/// addresses. Slices are fixed and non-overlapping (see `set_mounts`), so units
/// never share allocator state.
struct LockPool {
    /// Board-relative bump cursor.
    next: u32,
    /// Board-relative end of this unit's slice.
    end: u32,
    /// Recycled slot addresses (absolute), reused before bumping.
    free: Vec<u32>,
}

impl LockPool {
    fn new(base: u32, end: u32) -> Self {
        Self {
            next: base,
            end,
            free: Vec::new(),
        }
    }

    /// Hand out one `LOCK_SLOT_SIZE` slot as an absolute guest address, or None
    /// once this unit's slice is exhausted.
    fn alloc(&mut self, board_base: u32) -> Option<u32> {
        self.free.pop().or_else(|| {
            (self.next + LOCK_SLOT_SIZE <= self.end).then(|| {
                let addr = board_base + self.next;
                self.next += LOCK_SLOT_SIZE;
                addr
            })
        })
    }

    /// Return a freed slot (absolute address) for reuse.
    fn release(&mut self, addr: u32) {
        self.free.push(addr);
    }
}

/// Per-mount state: the immutable [`MountSpec`] from config plus everything the
/// handler learns or hands out at run time, including this unit's slice of the
/// board-window lock pool. `FilesysHle` owns one per unit, indexed by unit
/// number, so all per-unit state lives in one place and the packet handler is
/// a method on the unit.
struct FilesysUnit {
    /// Unit number: this mount's `HOSTFS<index>` device and board-window slot.
    index: usize,
    mount: MountSpec,
    /// Handler MsgPort, captured from the startup packet; stamped into the
    /// dn_Task/dol_Task/fl_Task fields DOS uses to reach the handler.
    port: Option<u32>,
    /// Guest address of the DeviceNode (from the startup packet); also the
    /// "handler started" marker, cleared at ACTION_DIE.
    device_node: Option<u32>,
    /// Guest address of the volume DosList node the host built.
    volume: Option<u32>,
    /// Open files by fh_Arg1 cookie (host-side only, no guest structure).
    /// The LockRec remembers what the handle refers to, for EXAMINE_FH.
    files: HashMap<u32, (std::fs::File, LockRec)>,
    /// Cookie counter for this unit's open files (unique within the unit).
    next_file_key: u32,
    /// Guest FileLock address -> what it locks.
    locks: HashMap<u32, LockRec>,
    /// EXAMINE_NEXT cursor per directory lock: the last child name handed out.
    /// Positioning by name (not a list index) keeps enumeration stable when the
    /// caller deletes entries as it goes, as `Delete ALL` does.
    examine: HashMap<u32, std::ffi::OsString>,
    /// This unit's fixed slice of the board-window FileLock pool.
    pool: LockPool,
}

impl FilesysUnit {
    fn new(index: usize, mount: MountSpec, pool_base: u32, pool_end: u32) -> Self {
        Self {
            index,
            mount,
            port: None,
            device_node: None,
            volume: None,
            files: HashMap::new(),
            next_file_key: 0,
            locks: HashMap::new(),
            examine: HashMap::new(),
            pool: LockPool::new(pool_base, pool_end),
        }
    }
}

/// Host side of the filesys trap gateway: implements the AmigaDOS packet
/// ACTION_* semantics against the host directories in `units`.
///
/// "Hle" is the m68k crate's HleHandler trait: High-Level Emulation, the
/// hook that intercepts reserved opcodes on the host side instead of letting
/// the guest take the exception. Installed as the CPU's HLE handler; it
/// reacts only to the reserved [`TRAP_BASE`] range, so leaving it installed
/// with no mounts configured changes nothing.
#[derive(Default)]
pub struct FilesysHle {
    /// Per-mount state, indexed by unit number (built from the config mounts
    /// in `set_mounts`).
    units: Vec<FilesysUnit>,
    /// Board base address, captured from A0 at the DiagPoint trap.
    board_base: Option<u32>,
}

impl FilesysHle {
    pub fn set_mounts(&mut self, mounts: Vec<MountSpec>) {
        // Give each unit a fixed, non-overlapping slice of the board-window
        // lock pool. Size the slices by the board's *maximum* unit count, not
        // the current mount count, so a unit's slice stays at the same offset
        // no matter how many are active -- outstanding lock addresses must not
        // move when units are added or removed at runtime (the eventual
        // mount/eject-on-the-fly goal).
        let chunk = (POOL_END - POOL_OFFSET) / MOUNT_MAX_COUNT as u32;
        self.units = mounts
            .into_iter()
            .enumerate()
            .map(|(i, mount)| {
                let base = POOL_OFFSET + i as u32 * chunk;
                FilesysUnit::new(i, mount, base, base + chunk)
            })
            .collect();
    }

    /// Dispatch a DosPacket to its unit; returns (dp_Res1, dp_Res2). `unit`
    /// comes from the handler (D2); `board_base` is stamped in from here so
    /// the units need not each carry it.
    fn handle_packet(
        &mut self,
        bus: &mut dyn AddressBus,
        unit: usize,
        port: u32,
        pkt: u32,
        guest_op: &mut Option<GuestOp>,
    ) -> (u32, u32) {
        if unit >= self.units.len() {
            log::warn!("filesys: packet for unknown unit {unit}");
            return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
        }
        let base = self.board_base.expect("packet before DiagPoint");
        self.units[unit].handle_packet(bus, base, port, pkt, guest_op)
    }
}

impl FilesysUnit {
    /// Host path a lock refers to.
    fn lock_path(&self, rec: &LockRec) -> PathBuf {
        self.mount.path.join(&rec.rel)
    }

    /// The error a mutating packet must fail with, or None when the mount takes
    /// writes. A `[[filesys]] readonly` mount answers exactly like a
    /// write-protected disk.
    fn write_refusal(&self) -> Option<u32> {
        self.mount.readonly.then_some(ERROR_DISK_WRITE_PROTECTED)
    }

    /// Allocate a FileLock in this unit's board-window sub-pool and register
    /// it. The handler port and volume node come from the unit.
    fn alloc_lock(
        &mut self,
        bus: &mut dyn AddressBus,
        board_base: u32,
        access: u32,
        rec: LockRec,
    ) -> Option<u32> {
        let addr = self.pool.alloc(board_base)?;
        let lock = FileLock {
            link: long(0),
            key: long(addr),
            access: long(access),
            task: long(self.port.unwrap_or(0)),
            volume: long(self.volume.unwrap_or(0) >> 2),
        };
        write_bytes(bus, addr, lock.as_bytes());
        self.locks.insert(addr, rec);
        Some(addr)
    }

    /// Resolve a DOS path (BPTR lock + name) to a lock record. AmigaDOS path
    /// semantics: an optional `prefix:` is stripped (the supplied lock is
    /// already the base it named), `/` goes to the parent, and names are
    /// case-insensitive.
    /// Resolve a name that is about to be created: every component but the last
    /// must exist (and is matched case-insensitively, like `resolve`), while a
    /// missing last component is taken literally so it can be created under the
    /// spelling the guest asked for.
    fn resolve_for_create(&self, lock_bptr: u32, name: &[u8]) -> Option<LockRec> {
        self.resolve_inner(lock_bptr, name, true)
    }

    fn resolve(&self, lock_bptr: u32, name: &[u8]) -> Option<LockRec> {
        self.resolve_inner(lock_bptr, name, false)
    }

    fn resolve_inner(&self, lock_bptr: u32, name: &[u8], create_leaf: bool) -> Option<LockRec> {
        // A lock handed to this unit's handler always belongs to this unit.
        let mut rel = if lock_bptr != 0 {
            self.locks.get(&(lock_bptr << 2))?.rel.clone()
        } else {
            PathBuf::new()
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
            return Some(LockRec { rel });
        }
        // A single trailing '/' does not mean parent: "Sub/" is Sub itself
        // (the "directory part" convention; verified against FFS, where
        // "Prefs/" lists Prefs but "Prefs//" lists its parent).
        let mut comps: Vec<&[u8]> = rest.split(|&b| b == b'/').collect();
        if comps.last() == Some(&&b""[..]) {
            comps.pop();
        }
        let last = comps.len().saturating_sub(1);
        for (i, comp) in comps.iter().enumerate() {
            if comp.is_empty() {
                // Leading or doubled '/': up to the parent.
                if !rel.pop() {
                    return None;
                }
                continue;
            }
            let comp = latin1_to_utf8(comp);
            let dir = self.mount.path.join(&rel);
            // Host symlinks are followed, wherever they point: the guest has no
            // packet that creates one, so a symlink inside the mount was placed
            // there by the host user, and grafting an outside directory into a
            // mount that way is a feature (same trust model as the UAE family).
            // The escapes we do block ("..", separators) are the ones a guest
            // program could construct on its own.
            match match_component(&dir, &comp) {
                Some(existing) => rel.push(existing),
                // The leaf may legitimately not exist yet, but a name the host
                // would read as a path (or as "here"/"up") never becomes one.
                None if create_leaf && i == last && is_creatable_name(&comp) => {
                    if !dir.is_dir() {
                        return None;
                    }
                    rel.push(&comp);
                }
                None => return None,
            }
        }
        Some(LockRec { rel })
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
        // The volume label is config-supplied ASCII; a leaf name comes from the
        // host and is mapped back to Latin-1. Anything the guest could have
        // reached is representable (resolve only matches names it could name),
        // and dir_listing already hides the rest, so this never loses data here.
        let name: Vec<u8> = if rec.rel.as_os_str().is_empty() {
            self.mount.volume.clone().into_bytes()
        } else {
            rec.rel
                .file_name()
                .and_then(utf8_to_latin1)
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
            file_name: bcpl::<108>(&name),
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
    /// attributes instead). Recomputed per call, which makes a full
    /// EXAMINE_NEXT walk quadratic in the directory size -- deliberately so:
    /// the fresh listing is what keeps a walk correct while the caller
    /// mutates the directory (Delete ALL), and EXAMINE_NEXT is the legacy
    /// slow path anyway, superseded by ACTION_EXAMINE_ALL for software that
    /// cares about speed. Revisit only if a real workload hurts.
    fn dir_listing(&self, rec: &LockRec) -> Vec<std::ffi::OsString> {
        let mut names: Vec<_> = std::fs::read_dir(self.lock_path(rec))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| !n.as_encoded_bytes().ends_with(b".uaem"))
            // Hide names that have no Latin-1 spelling: the guest could neither
            // display nor reopen them. amiberry's my_readdir skips them too.
            .filter(|n| utf8_to_latin1(n).is_some())
            .collect();
        names.sort();
        names
    }
}

impl FilesysHle {
    /// Write one FileSysStartupMsg per mount into the board window, plus the
    /// shared display device name and the per-unit DosEnvecs they reference.
    /// dn_Startup points at these so the Early Startup boot menu shows
    /// "CLFS hostfs-N" instead of dereferencing garbage, ACTION_STARTUP reads
    /// the unit back from fssm_Unit, and the guest handler passes de_BootPri
    /// to AddBootNode.
    fn write_startup_msgs(&self, bus: &mut dyn AddressBus, base: u32) {
        write_bytes(bus, base + FSSM_DEVNAME_OFFSET, &bcpl::<32>(b"hostfs"));
        for (unit, u) in self.units.iter().enumerate() {
            let mount = &u.mount;
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
}

impl FilesysUnit {
    /// Build this unit's volume DosList node in the board window; the guest
    /// handler AddDosEntry's it (only guest code may take the DosList
    /// semaphore). Returns the node's guest address.
    fn build_volume_node(&mut self, bus: &mut dyn AddressBus, board_base: u32) -> u32 {
        let vol = board_base + VOLUMES_OFFSET + self.index as u32 * VOLUME_SLOT_SIZE;
        let fixed = std::mem::size_of::<VolumeNode>() as u32;
        let (days, mins, ticks) = amiga_datestamp(Some(std::time::SystemTime::now()));
        let node = VolumeNode {
            next: long(0),
            r#type: long(2), // DLT_VOLUME
            task: long(self.port.unwrap_or(0)),
            lock: long(0),
            volume_date: [long(days), long(mins), long(ticks)],
            lock_list: long(0),
            disk_type: long(ID_CLFS_DISK),
            unused: long(0),
            name: long((vol + fixed) >> 2), // BSTR right after the struct
        };
        write_bytes(bus, vol, node.as_bytes());
        let name: Vec<u8> = self.mount.volume.bytes().take(30).collect();
        write_bytes(bus, vol + fixed, &bcpl::<32>(&name));
        self.volume = Some(vol);
        vol
    }

    /// Handle one DosPacket for this unit; returns (dp_Res1, dp_Res2). Some
    /// packets also need DosList surgery only the guest may perform (the
    /// semaphore); `guest_op` tells the handler what to do after replying.
    /// `port` is the handler's MsgPort, captured at the startup packet and
    /// stamped into the dn_Task/dol_Task/fl_Task fields DOS uses to reach the
    /// handler; `board_base` locates this unit's board-window structures.
    fn handle_packet(
        &mut self,
        bus: &mut dyn AddressBus,
        board_base: u32,
        port: u32,
        pkt: u32,
        guest_op: &mut Option<GuestOp>,
    ) -> (u32, u32) {
        let dp_type = bus.read_long(pkt + 8) as i32;
        let arg = |bus: &mut dyn AddressBus, n: u32| bus.read_long(pkt + 20 + 4 * (n - 1));

        // The first packet is the startup packet (ACTION_STARTUP, synonymous
        // with ACTION_NIL == 0): the handler passes its unit in on every
        // packet, so the first one we see is the one DOS sent to start this
        // unit's process. dp_Arg3 is the DeviceNode; capture the handler
        // MsgPort, wire dn_Task to it, and hand the guest the volume node to
        // AddDosEntry.
        if self.device_node.is_none() {
            let dn = arg(bus, 3) << 2; // dp_Arg3: BPTR DeviceNode
            bus.write_long(dn + DEVICENODE_TASK, port);
            self.device_node = Some(dn);
            self.port = Some(port);
            let vol = self.build_volume_node(bus, board_base);
            *guest_op = Some(GuestOp::AddVolume(vol));
            log::info!(
                "filesys: {}: handler started ({}: -> {})",
                device_name(self.index),
                self.mount.volume,
                self.mount.path.display()
            );
            return (DOSTRUE, 0);
        }

        match dp_type {
            ACTION_IS_FILESYSTEM => (DOSTRUE, 0),
            ACTION_DISK_INFO | ACTION_INFO => {
                // DISK_INFO: Arg1 = BPTR InfoData; INFO: Arg2 (Arg1 is a lock).
                let n = if dp_type == ACTION_DISK_INFO { 1 } else { 2 };
                let id = arg(bus, n) << 2;
                // Like the UAE filesys, report the size and free space of the
                // host filesystem holding the mount (statvfs), scaled so the
                // block counts survive AmigaDOS's 32-bit arithmetic.
                let (total, avail) = host_fs_usage(&self.mount.path).unwrap_or((1 << 30, 1 << 29));
                let (blocksize, numblocks, inuse) = scale_blocks(total, avail);
                let locks_open = !self.locks.is_empty();
                let info = InfoData {
                    num_soft_errors: long(0),
                    unit_number: long(self.index as u32),
                    // C:Info prints this as "Read Only" or "Read/Write".
                    disk_state: long(if self.mount.readonly {
                        ID_WRITE_PROTECTED
                    } else {
                        ID_VALIDATED
                    }),
                    num_blocks: long(numblocks),
                    num_blocks_used: long(inuse),
                    bytes_per_block: long(blocksize),
                    disk_type: long(ID_CLFS_DISK),
                    volume_node: long(self.volume.unwrap_or(0) >> 2),
                    in_use: long(if locks_open { DOSTRUE } else { 0 }),
                };
                write_bytes(bus, id, info.as_bytes());
                log::debug!(
                    "filesys: {}: InfoData at {id:#010X}: blocks={numblocks} \
                     used={inuse} bs={blocksize} (host total={total} avail={avail})",
                    device_name(self.index)
                );
                (DOSTRUE, 0)
            }
            ACTION_LOCATE_OBJECT => {
                let name_bptr = arg(bus, 2);
                let name = read_bstr(bus, name_bptr);
                log::debug!(
                    "filesys: {}: locate \"{}\" (lock {:#X})",
                    device_name(self.index),
                    latin1_to_utf8(&name),
                    arg(bus, 1)
                );
                let Some(rec) = self.resolve(arg(bus, 1), &name) else {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                };
                if !self.lock_path(&rec).exists() {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                }
                let access = arg(bus, 3);
                match self.alloc_lock(bus, board_base, access, rec) {
                    Some(addr) => (addr >> 2, 0),
                    None => (DOSFALSE, ERROR_OBJECT_NOT_FOUND),
                }
            }
            ACTION_FREE_LOCK => {
                let addr = arg(bus, 1) << 2;
                if addr != 0 && self.locks.remove(&addr).is_some() {
                    self.examine.remove(&addr);
                    self.pool.release(addr);
                }
                (DOSTRUE, 0)
            }
            ACTION_EXAMINE_OBJECT => {
                let lock = arg(bus, 1) << 2;
                let Some(rec) = self.locks.get(&lock).cloned() else {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                };
                // Examine restarts any enumeration on this lock.
                self.examine.remove(&lock);
                let fib = arg(bus, 2) << 2;
                match self.fill_fib(bus, fib, &rec, 0) {
                    Ok(()) => (DOSTRUE, 0),
                    Err(e) => (DOSFALSE, e),
                }
            }
            ACTION_EXAMINE_NEXT => {
                let lock = arg(bus, 1) << 2;
                let Some(rec) = self.locks.get(&lock).cloned() else {
                    return (DOSFALSE, ERROR_NO_MORE_ENTRIES);
                };
                let fib = arg(bus, 2) << 2;
                // Position by the last name handed out, not a list index: a
                // caller that deletes each entry as it examines (Delete ALL)
                // shrinks the host directory under us, so an index into the
                // re-read, re-sorted listing would skip whatever slid into the
                // used slot. The listing is sorted, so "the first name after the
                // last one" is a cursor that survives the entries vanishing.
                let names = self.dir_listing(&rec);
                let last = self.examine.get(&lock).cloned();
                let Some(name) = examine_after(&names, last.as_deref()).cloned() else {
                    self.examine.remove(&lock);
                    return (DOSFALSE, ERROR_NO_MORE_ENTRIES);
                };
                let child = LockRec {
                    rel: rec.rel.join(&name),
                };
                self.examine.insert(lock, name);
                match self.fill_fib(bus, fib, &child, 0) {
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
                        rel: PathBuf::new(),
                    }
                } else {
                    match self.locks.get(&(bptr << 2)) {
                        Some(r) => r.clone(),
                        None => return (DOSFALSE, ERROR_INVALID_LOCK),
                    }
                };
                match self.alloc_lock(bus, board_base, ACCESS_READ, rec) {
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
                let parent = LockRec { rel };
                match self.alloc_lock(bus, board_base, ACCESS_READ, parent) {
                    Some(addr) => (addr >> 2, 0),
                    None => (DOSFALSE, ERROR_OBJECT_NOT_FOUND),
                }
            }
            ACTION_SET_PROTECT => {
                // Arg2 = lock, Arg3 = BSTR name, Arg4 = mask (fib_Protection).
                // The host mode cannot hold the h/s/p/a bits or a non-default
                // rwed set, so anything but the default lands in a .uaem sidecar.
                if let Some(err) = self.write_refusal() {
                    return (DOSFALSE, err);
                }
                let name_bptr = arg(bus, 3);
                let name = read_bstr(bus, name_bptr);
                let Some(rec) = self.resolve(arg(bus, 2), &name) else {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                };
                let path = self.lock_path(&rec);
                if !path.exists() {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                }
                match set_protection(&path, arg(bus, 4) & 0xFF) {
                    Ok(()) => (DOSTRUE, 0),
                    Err(e) => (DOSFALSE, host_error(&e)),
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
                    device_name(self.index),
                    latin1_to_utf8(&name),
                    arg(bus, 2)
                );
                let Some(rec) = self.resolve(arg(bus, 2), &name) else {
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
                // Both locks belong to this unit, so equal paths mean the same
                // object.
                if a.rel == b.rel {
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
                        self.pool.release(lock_addr);
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
                let parent = LockRec { rel };
                match self.alloc_lock(bus, board_base, ACCESS_READ, parent) {
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
                if !self.locks.is_empty() || !self.files.is_empty() {
                    return (DOSFALSE, ERROR_OBJECT_IN_USE);
                }
                // Clear dn_Task so the next reference to the device simply
                // restarts the handler (and re-adds the volume). Dropping
                // device_node/port marks the unit un-started again.
                if let Some(dn) = self.device_node.take() {
                    bus.write_long(dn + DEVICENODE_TASK, 0);
                }
                self.port = None;
                let vol = self.volume.take().unwrap_or(0);
                *guest_op = Some(GuestOp::Die(vol));
                log::info!(
                    "filesys: {}: ACTION_DIE, handler exits",
                    device_name(self.index)
                );
                (DOSTRUE, 0)
            }
            ACTION_FINDOUTPUT | ACTION_FINDUPDATE => {
                // Open(MODE_NEWFILE) truncates or creates; Open(MODE_READWRITE)
                // opens for update, creating the file if it is not there.
                // Arg1 = BPTR FileHandle, Arg2 = lock, Arg3 = BSTR name.
                if let Some(err) = self.write_refusal() {
                    return (DOSFALSE, err);
                }
                let fh = arg(bus, 1) << 2;
                let name_bptr = arg(bus, 3);
                let name = read_bstr(bus, name_bptr);
                let Some(rec) = self.resolve_for_create(arg(bus, 2), &name) else {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                };
                let path = self.lock_path(&rec);
                if path.is_dir() {
                    return (DOSFALSE, ERROR_OBJECT_WRONG_TYPE);
                }
                let mut opts = std::fs::OpenOptions::new();
                opts.read(true).write(true).create(true);
                if dp_type == ACTION_FINDOUTPUT {
                    opts.truncate(true);
                }
                match opts.open(&path) {
                    Ok(f) => {
                        self.next_file_key += 1;
                        let key = self.next_file_key;
                        self.files.insert(key, (f, rec));
                        bus.write_long(fh + FILEHANDLE_ARG1, key);
                        (DOSTRUE, 0)
                    }
                    Err(e) => (DOSFALSE, host_error(&e)),
                }
            }
            ACTION_WRITE => {
                // Arg1 = fh_Arg1 cookie, Arg2 = buffer APTR, Arg3 = length.
                // Res1 = bytes written, or -1 with Res2 = error.
                use std::io::Write;
                let key = arg(bus, 1);
                let buf = arg(bus, 2);
                let len = arg(bus, 3) as usize;
                if let Some(err) = self.write_refusal() {
                    return (DOSTRUE, err); // res1 = -1
                }
                let Some((f, _)) = self.files.get_mut(&key) else {
                    return (DOSTRUE, ERROR_INVALID_LOCK);
                };
                // Chunked for the same reason as ACTION_READ: the length comes
                // from the guest, so a bogus one must not size a host buffer.
                const WRITE_CHUNK: usize = 64 * 1024;
                let mut chunk = vec![0u8; len.min(WRITE_CHUNK)];
                let mut done = 0usize;
                while done < len {
                    let want = (len - done).min(WRITE_CHUNK);
                    for (i, b) in chunk[..want].iter_mut().enumerate() {
                        *b = bus.read_byte(buf + (done + i) as u32);
                    }
                    match f.write_all(&chunk[..want]) {
                        Ok(()) => done += want,
                        Err(e) => return (DOSTRUE, host_error(&e)), // res1 = -1
                    }
                }
                (done as u32, 0)
            }
            ACTION_SET_FILE_SIZE => {
                // Arg1 = fh_Arg1, Arg2 = offset, Arg3 = OFFSET_* mode.
                // Res1 = the new size, or -1 with Res2 = error.
                use std::io::{Seek, SeekFrom};
                let key = arg(bus, 1);
                let offset = arg(bus, 2) as i32 as i64;
                let mode = arg(bus, 3) as i32;
                if let Some(err) = self.write_refusal() {
                    return (DOSTRUE, err); // res1 = -1
                }
                let Some((f, _)) = self.files.get_mut(&key) else {
                    return (DOSTRUE, ERROR_INVALID_LOCK);
                };
                let (pos, end) = match (f.stream_position(), f.metadata()) {
                    (Ok(p), Ok(m)) => (p as i64, m.len() as i64),
                    _ => return (DOSTRUE, ERROR_SEEK_ERROR),
                };
                let size = match mode {
                    OFFSET_BEGINNING => offset,
                    OFFSET_CURRENT => pos + offset,
                    OFFSET_END => end + offset,
                    _ => -1,
                };
                if size < 0 {
                    return (DOSTRUE, ERROR_SEEK_ERROR);
                }
                if let Err(e) = f.set_len(size as u64) {
                    return (DOSTRUE, host_error(&e));
                }
                // Truncating below the file position leaves it past the end;
                // DOS expects the position clamped to the new size.
                if pos > size && f.seek(SeekFrom::Start(size as u64)).is_err() {
                    return (DOSTRUE, ERROR_SEEK_ERROR);
                }
                (size as u32, 0)
            }
            ACTION_CREATE_DIR => {
                // Arg1 = lock, Arg2 = BSTR name. Result is a lock on the new
                // directory (the caller frees it).
                if let Some(err) = self.write_refusal() {
                    return (DOSFALSE, err);
                }
                let name_bptr = arg(bus, 2);
                let name = read_bstr(bus, name_bptr);
                let Some(rec) = self.resolve_for_create(arg(bus, 1), &name) else {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                };
                let path = self.lock_path(&rec);
                if path.exists() {
                    return (DOSFALSE, ERROR_OBJECT_EXISTS);
                }
                if let Err(e) = std::fs::create_dir(&path) {
                    return (DOSFALSE, host_error(&e));
                }
                match self.alloc_lock(bus, board_base, ACCESS_READ, rec) {
                    Some(addr) => (addr >> 2, 0),
                    None => (DOSFALSE, ERROR_OBJECT_NOT_FOUND),
                }
            }
            ACTION_DELETE_OBJECT => {
                // Arg1 = lock, Arg2 = BSTR name.
                if let Some(err) = self.write_refusal() {
                    return (DOSFALSE, err);
                }
                let name_bptr = arg(bus, 2);
                let name = read_bstr(bus, name_bptr);
                let Some(rec) = self.resolve(arg(bus, 1), &name) else {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                };
                let path = self.lock_path(&rec);
                let meta = match std::fs::symlink_metadata(&path) {
                    Ok(m) => m,
                    Err(e) => return (DOSFALSE, host_error(&e)),
                };
                // The object's own delete-protection bit (kept in the .uaem
                // sidecar) refuses the delete, per the ACTION_DELETE_OBJECT
                // autodoc. A residual host permission failure stays
                // ERROR_WRITE_PROTECTED via host_error, which the same autodoc
                // sanctions ("a delete operation on a file also implies a
                // write"); the whole-volume case returns
                // ERROR_DISK_WRITE_PROTECTED through write_refusal above.
                if read_uaem(&path).is_some_and(|u| u.protection & FIBF_DELETE != 0) {
                    return (DOSFALSE, ERROR_DELETE_PROTECTED);
                }
                let res = if meta.is_dir() {
                    std::fs::remove_dir(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                match res {
                    Ok(()) => {
                        // The attribute sidecar belongs to the file, not to the
                        // guest: it goes with it.
                        let _ = std::fs::remove_file(uaem_path(&path));
                        (DOSTRUE, 0)
                    }
                    Err(e) => (DOSFALSE, host_error(&e)),
                }
            }
            ACTION_RENAME_OBJECT => {
                // Arg1 = source lock, Arg2 = BSTR source name,
                // Arg3 = target dir lock, Arg4 = BSTR target name. Both locks
                // reached this handler, so both are on this unit's volume.
                if let Some(err) = self.write_refusal() {
                    return (DOSFALSE, err);
                }
                let (from_bptr, to_bptr) = (arg(bus, 2), arg(bus, 4));
                let from_name = read_bstr(bus, from_bptr);
                let to_name = read_bstr(bus, to_bptr);
                let Some(from) = self.resolve(arg(bus, 1), &from_name) else {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                };
                let Some(to) = self.resolve_for_create(arg(bus, 3), &to_name) else {
                    return (DOSFALSE, ERROR_DIRECTORY_NOT_FOUND);
                };
                let (from_path, to_path) = (self.lock_path(&from), self.lock_path(&to));
                // A rename that only changes case is not an overwrite: on a
                // case-insensitive host the two paths are the same file.
                let renaming_in_place = from_path == to_path;
                if to_path.exists() && !renaming_in_place {
                    return (DOSFALSE, ERROR_OBJECT_EXISTS);
                }
                match std::fs::rename(&from_path, &to_path) {
                    Ok(()) => {
                        let (from_uaem, to_uaem) = (uaem_path(&from_path), uaem_path(&to_path));
                        if from_uaem.exists() {
                            let _ = std::fs::rename(&from_uaem, &to_uaem);
                        }
                        (DOSTRUE, 0)
                    }
                    Err(e) => (DOSFALSE, host_error(&e)),
                }
            }
            ACTION_SET_DATE => {
                // Arg2 = lock, Arg3 = BSTR name, Arg4 = ptr to DateStamp.
                if let Some(err) = self.write_refusal() {
                    return (DOSFALSE, err);
                }
                let name_bptr = arg(bus, 3);
                let name = read_bstr(bus, name_bptr);
                let Some(rec) = self.resolve(arg(bus, 2), &name) else {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                };
                let ds = arg(bus, 4);
                let (days, mins, ticks) = (
                    bus.read_long(ds),
                    bus.read_long(ds + 4),
                    bus.read_long(ds + 8),
                );
                let path = self.lock_path(&rec);
                if let Err(e) = set_host_mtime(&path, days, mins, ticks) {
                    return (DOSFALSE, host_error(&e));
                }
                // If a sidecar already exists (for protection or a comment), keep
                // its stored date in step with the new mtime. A file with only a
                // date and no other attributes needs no sidecar -- the mtime holds
                // it -- so we never create one here.
                if let Some(mut info) = read_uaem(&path) {
                    info.date = Some((days, mins, ticks));
                    if let Err(e) = write_uaem(&path, &info) {
                        return (DOSFALSE, host_error(&e));
                    }
                }
                (DOSTRUE, 0)
            }
            ACTION_SET_COMMENT => {
                // Arg2 = lock, Arg3 = BSTR name, Arg4 = BSTR comment. The host
                // cannot hold a file comment, so it goes in the .uaem sidecar.
                if let Some(err) = self.write_refusal() {
                    return (DOSFALSE, err);
                }
                let name_bptr = arg(bus, 3);
                let name = read_bstr(bus, name_bptr);
                let comment_bptr = arg(bus, 4);
                let comment = read_bstr(bus, comment_bptr);
                let Some(rec) = self.resolve(arg(bus, 2), &name) else {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                };
                let path = self.lock_path(&rec);
                if !path.exists() {
                    return (DOSFALSE, ERROR_OBJECT_NOT_FOUND);
                }
                // Keep the existing protection and date -- including a
                // denied `w` held only in the host mode -- and change just
                // the comment.
                let mut info = read_attributes(&path);
                info.comment = comment;
                match write_uaem(&path, &info) {
                    Ok(()) => (DOSTRUE, 0),
                    Err(e) => (DOSFALSE, host_error(&e)),
                }
            }
            // Relabel: the volume name comes from the config, not the guest.
            ACTION_RENAME_DISK => (DOSFALSE, ERROR_ACTION_NOT_KNOWN),
            _ => {
                log::debug!(
                    "filesys: {}: unhandled action {dp_type}",
                    device_name(self.index)
                );
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
                let mounts: Vec<MountSpec> = std::mem::take(&mut self.units)
                    .into_iter()
                    .map(|u| u.mount)
                    .collect();
                *self = FilesysHle::default();
                self.set_mounts(mounts);
                self.board_base = Some(cpu.dar[8]);
                self.write_startup_msgs(bus, cpu.dar[8]);
                log::info!(
                    "filesys: expansion init at board {:#010X}, {} mount(s)",
                    cpu.dar[8],
                    self.units.len()
                );
                true
            }
            TRAP_PACKET => {
                let pkt = cpu.dar[1]; // D1
                let unit = cpu.dar[2] as usize; // D2: mount unit (the handler
                                                // passes its own; see handler.c)
                let port = cpu.dar[9]; // A1
                let dp_type = bus.read_long(pkt + 8) as i32;
                let mut guest_op = None;
                let (res1, res2) = self.handle_packet(bus, unit, port, pkt, &mut guest_op);
                log::debug!(
                    "filesys: {}: packet type {dp_type} at {pkt:#010X} -> \
                     res1={res1:#X} res2={res2}",
                    device_name(unit),
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
#[derive(Default)]
struct UaemInfo {
    /// fib_Protection value (deny-style rwed like the FIB wants). 0 is the
    /// default (rwed all allowed, no h/s/p/a), which needs no sidecar.
    protection: u32,
    /// DateStamp, when the sidecar's timestamp parses.
    date: Option<DateStamp>,
    comment: Vec<u8>,
}

/// Read and parse the `.uaem` sidecar of `path`, if any.
fn read_uaem(path: &Path) -> Option<UaemInfo> {
    parse_uaem(&std::fs::read(uaem_path(path)).ok()?)
}

/// The attributes of `path` as Examine would report them: the sidecar when
/// one exists, otherwise what the host mode implies (fill_fib's fallback --
/// read-only denies `w`). A read-modify-write of one attribute must start
/// from this, not from a bare `read_uaem`, or a host-read-only file with no
/// sidecar would lose its denied `w` in the rewrite.
fn read_attributes(path: &Path) -> UaemInfo {
    read_uaem(path).unwrap_or_else(|| UaemInfo {
        protection: match std::fs::metadata(path) {
            Ok(m) if m.permissions().readonly() => FIBF_WRITE,
            _ => 0,
        },
        ..UaemInfo::default()
    })
}

/// Mirror a protection mask's FIBF_WRITE bit onto the host mode, the one
/// protection the host holds natively: a denied `w` becomes the read-only
/// flag (fill_fib maps it back), so it needs no `.uaem` sidecar and
/// host-side tools see it too.
#[cfg(unix)]
fn apply_host_write_bit(path: &Path, protection: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::metadata(path)?.permissions();
    let mode = perms.mode();
    // Deny: clear every write bit (that is what `readonly()` reports on).
    // Allow: the owner bit is enough, leave group/other to the file.
    let new_mode = if protection & FIBF_WRITE != 0 {
        mode & !0o222
    } else {
        mode | 0o200
    };
    if new_mode != mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(new_mode))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_host_write_bit(path: &Path, protection: u32) -> std::io::Result<()> {
    let mut perms = std::fs::metadata(path)?.permissions();
    let deny = protection & FIBF_WRITE != 0;
    if perms.readonly() != deny {
        // Not the world-writable hazard clippy fears: this mirrors the
        // guest's own protection choice onto its own file.
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(deny);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// ACTION_SET_PROTECT's semantics: the `w` bit lands in the host mode, the
/// rest of the mask (and any existing comment) in the `.uaem` sidecar --
/// which write_uaem removes again when the mode alone says it all.
fn set_protection(path: &Path, protection: u32) -> std::io::Result<()> {
    let mut info = read_uaem(path).unwrap_or_default();
    info.protection = protection;
    write_uaem(path, &info)
}
/// The attribute sidecar that belongs to a host path.
fn uaem_path(path: &Path) -> PathBuf {
    let mut side = path.as_os_str().to_owned();
    side.push(".uaem");
    PathBuf::from(side)
}

/// Persist `info` to `path`'s `.uaem` sidecar, or delete the sidecar when the
/// host object itself can carry everything: the mtime holds the date, the
/// host read-only flag holds a denied `w`, and default attributes need
/// nothing at all. Only the bits with no host representation (h/s/p/a,
/// denied r/e/d, a comment) force a sidecar. The line format matches
/// amiberry's fsdb_host.cpp, so sidecars stay interoperable between the two
/// emulators.
fn write_uaem(path: &Path, info: &UaemInfo) -> std::io::Result<()> {
    // The `w` bit lives in the host mode; sync it here rather than in the
    // callers, or deleting a sidecar whose only content was a denied `w`
    // (the fast path below) could leave the mode stale and lose the bit.
    apply_host_write_bit(path, info.protection)?;
    let side = uaem_path(path);
    if info.protection & 0xFF & !FIBF_WRITE == 0 && info.comment.is_empty() {
        return match std::fs::remove_file(&side) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        };
    }

    // The rwed group is stored deny-style in fib_Protection; `^ 0xF` turns it
    // back into the "allowed" letters the sidecar shows (the inverse of
    // parse_uaem's flip). h/s/p/a are stored directly.
    let mode = info.protection ^ 0xF;
    let mut line: Vec<u8> = (0..8)
        .zip(b"hsparwed")
        .map(|(i, &l)| if mode & (1 << (7 - i)) != 0 { l } else { b'-' })
        .collect();

    let (days, mins, ticks) = info.date.unwrap_or_else(|| {
        amiga_datestamp(std::fs::metadata(path).ok().and_then(|m| m.modified().ok()))
    });
    let (y, m, d) = civil_from_days(days_from_civil(1978, 1, 1) + i64::from(days));
    let text = format!(
        " {y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:02}",
        mins / 60,
        mins % 60,
        ticks / 50,
        (ticks % 50) * 2,
    );
    line.extend_from_slice(text.as_bytes());
    if !info.comment.is_empty() {
        line.push(b' ');
        // The sidecar body is UTF-8 like the host filenames (amiberry writes
        // the comment through the same host-name encoding).
        line.extend_from_slice(latin1_to_utf8(&info.comment).as_bytes());
    }
    line.push(b'\n');

    // Write through a temp file and rename so a crash never leaves a truncated
    // sidecar in place of the old one (matching amiberry).
    let tmp = {
        let mut t = side.clone().into_os_string();
        t.push(".tmp");
        PathBuf::from(t)
    };
    std::fs::write(&tmp, &line)?;
    std::fs::rename(&tmp, &side)
}

/// The civil date of `z` days since 1970-01-01 (Howard Hinnant's algorithm,
/// the inverse of [`days_from_civil`]).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Map a host I/O error onto the AmigaDOS error the guest expects, so a full
/// disk says "disk full" rather than a generic failure.
fn host_error(e: &std::io::Error) -> u32 {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::NotFound => ERROR_OBJECT_NOT_FOUND,
        K::PermissionDenied => ERROR_WRITE_PROTECTED,
        K::AlreadyExists => ERROR_OBJECT_EXISTS,
        K::DirectoryNotEmpty => ERROR_DIRECTORY_NOT_EMPTY,
        K::StorageFull => ERROR_DISK_FULL,
        K::CrossesDevices => ERROR_RENAME_ACROSS_DEVICES,
        K::InvalidFilename => ERROR_INVALID_COMPONENT_NAME,
        _ => ERROR_SEEK_ERROR,
    }
}

/// Stamp a host file with an AmigaDOS DateStamp (days/minutes/ticks since
/// 1978-01-01). The host keeps only the modification time, which is what
/// Examine() reports back.
fn set_host_mtime(path: &Path, days: u32, mins: u32, ticks: u32) -> std::io::Result<()> {
    /// Seconds between the Unix epoch and the AmigaDOS epoch.
    const AMIGA_EPOCH_OFFSET: u64 = 252_460_800;
    let secs = AMIGA_EPOCH_OFFSET
        + u64::from(days) * 86_400
        + u64::from(mins) * 60
        + u64::from(ticks) / 50;
    // A tick is 1/50 s = 20 ms: keep the sub-second part, so a DateStamp
    // round-trips through the host mtime at its native resolution.
    let nanos = (ticks % 50) * 20_000_000;
    // checked_add: `+` panics when the sum exceeds the platform's time
    // representation (Windows FILETIME tops out around year 30828, and a
    // garbage guest DateStamp reaches far beyond), and a bad date from the
    // guest must fail the packet, not the emulator.
    let time = std::time::UNIX_EPOCH
        .checked_add(std::time::Duration::new(secs, nanos))
        .ok_or(std::io::ErrorKind::InvalidInput)?;
    std::fs::File::options()
        .write(true)
        .open(path)
        .or_else(|_| std::fs::File::open(path))?
        .set_modified(time)
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

// AmigaDOS filenames are ISO-8859-1 (Latin-1); the host filesystem is UTF-8.
// Latin-1 is exactly the first 256 Unicode code points, so both directions are
// a trivial per-character map and need no iconv. This mirrors amiberry's
// osdep/amiberry_filesys.cpp (utf8_to_latin1_string / iso_8859_1_to_utf8) so
// host directory mounts stay interoperable between the two emulators -- names
// that amiberry drops, we drop the same way.

/// A guest-supplied Latin-1 name -> the host UTF-8 string. Total: every byte is
/// a valid Latin-1 code point.
fn latin1_to_utf8(name: &[u8]) -> String {
    name.iter().map(|&b| b as char).collect()
}

/// A host filename -> AmigaDOS Latin-1 bytes, or None if it contains any
/// character outside Latin-1 (including invalid UTF-8). amiberry hides such
/// entries from the guest rather than mangling them; so do we.
fn utf8_to_latin1(name: &std::ffi::OsStr) -> Option<Vec<u8>> {
    name.to_str()?
        .chars()
        .map(|c| (u32::from(c) <= 0xff).then_some(c as u8))
        .collect()
}

/// The next EXAMINE_NEXT entry from a sorted listing given the last name handed
/// out (`None` to start). Positioning by name rather than index keeps a walk
/// stable when the caller deletes each entry as it goes: the entry that slides
/// into a vacated slot is never skipped, and the just-deleted name still orders
/// the search correctly even though it is gone from `names`.
fn examine_after<'a>(
    names: &'a [std::ffi::OsString],
    last: Option<&std::ffi::OsStr>,
) -> Option<&'a std::ffi::OsString> {
    match last {
        None => names.first(),
        Some(l) => names.iter().find(|n| n.as_os_str() > l),
    }
}

/// Case-insensitive component match: prefer the exact host name, else scan
/// the directory for a case-insensitive match (AmigaDOS names are
/// case-insensitive but case-preserving).
/// Whether a name the guest asked us to create is safe to create verbatim on
/// the host. "." and ".." are ordinary names on AmigaDOS but path operators to
/// the host, and a separator or NUL would let one component become several --
/// either way the write could land outside the mount.
fn is_creatable_name(comp: &str) -> bool {
    !comp.is_empty()
        && comp != "."
        && comp != ".."
        && !comp.contains('/')
        && !comp.contains('\\')
        && !comp.contains('\0')
        // The .uaem sidecars are ours: a guest file by that name would shadow
        // another file's attributes and vanish from directory listings.
        && !comp.to_ascii_lowercase().ends_with(".uaem")
}

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
            readonly: false,
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
            readonly: false,
        }]);
        // A lock on Libs, as DOS supplies with opens through the LIBS:
        // assign. The name still carries the user's "LIBS:" prefix; it
        // must be stripped without resetting to the root.
        hle.units[0]
            .locks
            .insert(0x1000, LockRec { rel: "Libs".into() });
        let unit = &hle.units[0];
        let rec = unit.resolve(0x1000 >> 2, b"LIBS:68040.library").unwrap();
        assert_eq!(rec.rel, PathBuf::from("Libs/68040.library"));
        // A volume prefix with no lock starts at the root as before.
        let rec = unit.resolve(0, b"Test:Libs/68040.library").unwrap();
        assert_eq!(rec.rel, PathBuf::from("Libs/68040.library"));

        // Host dot-dirs must not act as path components: ".." would
        // escape the mount root ("." and ".." are legal-ish AmigaDOS
        // names with no special meaning; "/" is the parent).
        assert!(unit.resolve(0, b"..").is_none());
        assert!(unit.resolve(0, b"Libs/../../etc").is_none());
        assert!(unit.resolve(0, b"Libs/.").is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Creating a file resolves its parent like any other lookup, but takes the
    /// leaf literally so it lands under the spelling the guest asked for. What
    /// it must never do is let that leaf become a path: the guest picks the
    /// name, and "..", a separator, or a `.uaem` suffix would write outside the
    /// mount or shadow another file's attributes.
    #[test]
    fn a_created_name_stays_inside_the_mount() {
        let root = std::env::temp_dir().join(format!("clfs-create-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Sub")).unwrap();

        let mut hle = FilesysHle::default();
        hle.set_mounts(vec![MountSpec {
            path: root.clone(),
            volume: "Test".into(),
            boot_pri: -128,
            readonly: false,
        }]);
        let unit = &hle.units[0];

        // A missing leaf under an existing directory is the whole point.
        let rec = unit.resolve_for_create(0, b"Sub/brand-new.txt").unwrap();
        assert_eq!(rec.rel, PathBuf::from("Sub/brand-new.txt"));
        // The parent still has to exist, and is still matched case-insensitively.
        // The resolved parent keeps whatever spelling the host reports, which
        // differs between case-sensitive and case-insensitive filesystems, so
        // only the case-folded path is portable to assert.
        let rec = unit.resolve_for_create(0, b"SUB/other.txt").unwrap();
        assert!(rec.rel.file_name().unwrap() == "other.txt");
        assert!(rec
            .rel
            .to_string_lossy()
            .eq_ignore_ascii_case("Sub/other.txt"));
        assert!(unit.resolve_for_create(0, b"Nope/file.txt").is_none());

        // None of these may resolve: each would escape the mount or collide
        // with our own sidecars.
        for escape in [
            &b".."[..],
            b"Sub/..",
            b"../outside.txt",
            b"Sub/../../outside.txt",
            b".",
            b"ReadMe.txt.uaem",
        ] {
            assert!(
                unit.resolve_for_create(0, escape).is_none(),
                "resolved {:?}",
                String::from_utf8_lossy(escape)
            );
        }

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Latin-1 <-> UTF-8 mapping matches amiberry: a guest name with high bytes
    /// finds its UTF-8 host file, the host name maps back to those same bytes,
    /// and a host name outside Latin-1 is hidden from directory listings.
    #[test]
    fn utf8_host_names_map_to_latin1() {
        // "francais" with a c-cedilla (U+00E7): Latin-1 byte 0xE7, UTF-8 0xC3 0xA7.
        assert_eq!(latin1_to_utf8(b"fran\xe7ais"), "fran\u{e7}ais");
        assert_eq!(
            utf8_to_latin1(std::ffi::OsStr::new("fran\u{e7}ais")),
            Some(b"fran\xe7ais".to_vec())
        );
        // Above Latin-1 (U+2603 SNOWMAN): no mapping.
        assert_eq!(utf8_to_latin1(std::ffi::OsStr::new("sn\u{2603}w")), None);

        let root = std::env::temp_dir().join(format!("clfs-latin1-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("fran\u{e7}ais"), b"x").unwrap();
        std::fs::write(root.join("sn\u{2603}w"), b"x").unwrap();

        let mut hle = FilesysHle::default();
        hle.set_mounts(vec![MountSpec {
            path: root.clone(),
            volume: "Test".into(),
            boot_pri: -128,
            readonly: false,
        }]);
        let unit = &hle.units[0];

        // The guest names it in Latin-1 and reaches the UTF-8 host file.
        let rec = unit.resolve(0, b"fran\xe7ais").unwrap();
        assert_eq!(rec.rel, PathBuf::from("fran\u{e7}ais"));
        // The listing shows the mappable name and hides the snowman.
        let listing = unit.dir_listing(&LockRec {
            rel: PathBuf::new(),
        });
        assert!(listing.iter().any(|n| n == "fran\u{e7}ais"));
        assert!(listing.iter().all(|n| n != "sn\u{2603}w"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// EXAMINE_NEXT must visit every entry exactly once even when the caller
    /// deletes each one as it is examined (what `Delete ALL` does). Walking a
    /// re-read listing by index skips whatever slides into the vacated slot;
    /// walking by last-name-returned does not.
    #[test]
    fn examine_next_survives_deletion_mid_walk() {
        let mut remaining: Vec<std::ffi::OsString> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(Into::into)
            .collect();
        let mut visited = Vec::new();
        let mut last: Option<std::ffi::OsString> = None;
        // Re-read the (shrinking) listing each step, exactly as the handler does.
        while let Some(name) = examine_after(&remaining, last.as_deref()).cloned() {
            visited.push(name.to_string_lossy().into_owned());
            last = Some(name.clone());
            // The caller deletes the entry it just examined.
            remaining.retain(|n| *n != name);
        }
        assert_eq!(visited, ["a", "b", "c", "d", "e", "f"]);
        assert!(remaining.is_empty());
    }

    /// A `readonly` mount answers every mutating packet like a write-protected
    /// disk, and says so in the volume's InfoData.
    #[test]
    fn a_readonly_mount_refuses_every_write() {
        let mut hle = FilesysHle::default();
        hle.set_mounts(vec![
            MountSpec {
                path: "/nonexistent".into(),
                volume: "Locked".into(),
                boot_pri: -128,
                readonly: true,
            },
            MountSpec {
                path: "/nonexistent".into(),
                volume: "Open".into(),
                boot_pri: -128,
                readonly: false,
            },
        ]);
        assert_eq!(
            hle.units[0].write_refusal(),
            Some(ERROR_DISK_WRITE_PROTECTED)
        );
        assert_eq!(hle.units[1].write_refusal(), None);
    }

    /// SetProtection with only `w` denied lands in the host read-only flag,
    /// not a sidecar -- so cloning a read-only file stays sidecar-free (the
    /// spurious .uaem regression from a WB Copy CLONE). Bits the host cannot
    /// hold still get the sidecar with the mode kept in sync, and returning
    /// to default clears both.
    #[test]
    fn w_only_protection_lands_in_the_host_mode() {
        let root = std::env::temp_dir().join(format!("clfs-prot-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("locked");
        std::fs::write(&file, b"x").unwrap();
        // What Examine reports: the sidecar, or the fill_fib host fallback.
        let examine_prot = |file: &Path| read_attributes(file).protection;

        set_protection(&file, FIBF_WRITE).unwrap();
        assert!(!uaem_path(&file).exists());
        assert!(std::fs::metadata(&file).unwrap().permissions().readonly());
        assert_eq!(examine_prot(&file), FIBF_WRITE);

        // FIBF_SCRIPT has no host representation: sidecar appears with the
        // full mask, and the mode still mirrors the denied w.
        set_protection(&file, 0x40 | FIBF_WRITE).unwrap();
        assert!(uaem_path(&file).exists());
        assert!(std::fs::metadata(&file).unwrap().permissions().readonly());
        assert_eq!(examine_prot(&file), 0x40 | FIBF_WRITE);

        set_protection(&file, 0).unwrap();
        assert!(!uaem_path(&file).exists());
        assert!(!std::fs::metadata(&file).unwrap().permissions().readonly());
        assert_eq!(examine_prot(&file), 0);

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// SetComment on a host-read-only file with no sidecar keeps the file
    /// read-only: the implicit denied `w` is carried into the sidecar's mask
    /// instead of being reset to 0 by the read-modify-write.
    #[test]
    fn a_comment_keeps_the_implicit_denied_w() {
        let root = std::env::temp_dir().join(format!("clfs-cmt-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("guarded");
        std::fs::write(&file, b"x").unwrap();
        let mut perms = std::fs::metadata(&file).unwrap().permissions();
        perms.set_readonly(true); // as a host tool would
        std::fs::set_permissions(&file, perms).unwrap();

        // What ACTION_SET_COMMENT does.
        let mut info = read_attributes(&file);
        assert_eq!(info.protection, FIBF_WRITE);
        info.comment = b"do not touch".to_vec();
        write_uaem(&file, &info).unwrap();

        assert!(std::fs::metadata(&file).unwrap().permissions().readonly());
        let back = read_uaem(&file).unwrap();
        assert_eq!(back.protection, FIBF_WRITE);
        assert_eq!(back.comment, b"do not touch");

        // Clearing the comment removes the sidecar; the denied `w` stays
        // behind in the host mode.
        info.comment.clear();
        write_uaem(&file, &info).unwrap();
        assert!(!uaem_path(&file).exists());
        assert!(std::fs::metadata(&file).unwrap().permissions().readonly());

        // remove_dir_all cannot delete read-only files on Windows.
        set_protection(&file, 0).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A legacy sidecar whose only content is a denied `w` (written by
    /// amiberry, host file left writable) can be rewritten by actions that
    /// never touch protection, like SET_DATE. Deleting it as redundant must
    /// push the bit into the host mode, not drop it.
    #[test]
    fn deleting_a_w_only_legacy_sidecar_keeps_the_denied_w() {
        let root = std::env::temp_dir().join(format!("clfs-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("inherited");
        std::fs::write(&file, b"x").unwrap();
        std::fs::write(uaem_path(&file), b"----r-ed 2026-07-10 00:00:00.00\n").unwrap();

        // What ACTION_SET_DATE does after set_host_mtime.
        let mut info = read_uaem(&file).unwrap();
        assert_eq!(info.protection, FIBF_WRITE);
        info.date = Some((17722, 0, 0));
        write_uaem(&file, &info).unwrap();

        assert!(!uaem_path(&file).exists());
        assert!(std::fs::metadata(&file).unwrap().permissions().readonly());
        assert_eq!(read_attributes(&file).protection, FIBF_WRITE);

        // remove_dir_all cannot delete read-only files on Windows.
        set_protection(&file, 0).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A DateStamp survives the trip through the host mtime at its native
    /// 1/50 s resolution: SetFileDate then Examine must return the same
    /// ticks, not the value truncated to whole seconds.
    #[test]
    fn datestamp_round_trips_subsecond_through_host_mtime() {
        let root = std::env::temp_dir().join(format!("clfs-ticks-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("stamped");
        std::fs::write(&file, b"x").unwrap();

        // 2026-07-16 12:34:56.84: 42 ticks past the second.
        let stamp = (17743, 12 * 60 + 34, 56 * 50 + 42);
        set_host_mtime(&file, stamp.0, stamp.1, stamp.2).unwrap();
        let mtime = std::fs::metadata(&file).unwrap().modified().ok();
        assert_eq!(amiga_datestamp(mtime), stamp);

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

    /// write_uaem produces exactly the byte format amiberry reads, round-trips
    /// through parse_uaem, and deletes the sidecar when attributes go default.
    #[test]
    fn write_uaem_matches_amiberry_and_removes_default() {
        let root = std::env::temp_dir().join(format!("clfs-uaem-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("Shell-Startup");
        std::fs::write(&file, b"; script").unwrap();

        // Script bit + execute denied, a comment, and a fixed datestamp.
        let info = UaemInfo {
            protection: 0x42, // FIBF_SCRIPT | FIBF_EXECUTE
            date: Some((17722, 16 * 60 + 16, 51 * 50 + 32 / 2)),
            comment: b"a comment".to_vec(),
        };
        write_uaem(&file, &info).unwrap();

        // Byte-for-byte what amiberry writes.
        let raw = std::fs::read(uaem_path(&file)).unwrap();
        assert_eq!(raw, b"-s--rw-d 2026-07-10 16:16:51.32 a comment\n");

        // ...and it parses back to the same attributes.
        let back = read_uaem(&file).unwrap();
        assert_eq!(back.protection, 0x42);
        assert_eq!(back.date, info.date);
        assert_eq!(back.comment, b"a comment");

        // Default attributes (rwed, no bits, no comment) delete the sidecar.
        write_uaem(&file, &UaemInfo::default()).unwrap();
        assert!(!uaem_path(&file).exists());
        // Removing an already-absent sidecar is not an error.
        write_uaem(&file, &UaemInfo::default()).unwrap();

        std::fs::remove_dir_all(&root).unwrap();
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
