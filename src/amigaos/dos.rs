// SPDX-License-Identifier: GPL-3.0-or-later

//! AmigaDOS ABI mirror (NDK dos/dos.h and dos/dosextens.h): the packet
//! types, error codes, and guest-memory structures a DOS handler works
//! with. Structures are `#[repr(C)]` with big-endian fields, so the Rust
//! definition IS the guest layout (offsets guaranteed, no padding possible)
//! and one `write_bytes(bus, at, x.as_bytes())` serializes it. BCPL strings
//! are fixed in-struct arrays or appended BSTRs.

use m68k::AddressBus;
use zerocopy::byteorder::{BigEndian, U32};
use zerocopy::Immutable;
pub use zerocopy::IntoBytes;

// Packet types (dos/dosextens.h ACTION_*).
pub const ACTION_LOCATE_OBJECT: i32 = 8;
pub const ACTION_FREE_LOCK: i32 = 15;
pub const ACTION_COPY_DIR: i32 = 19; // DupLock()
pub const ACTION_SET_PROTECT: i32 = 21;
pub const ACTION_EXAMINE_OBJECT: i32 = 23;
pub const ACTION_EXAMINE_NEXT: i32 = 24;
pub const ACTION_DISK_INFO: i32 = 25;
pub const ACTION_INFO: i32 = 26;
pub const ACTION_PARENT: i32 = 29;
pub const ACTION_READ: i32 = 82; // 'R'
pub const ACTION_FINDINPUT: i32 = 1005; // Open(..., MODE_OLDFILE)
pub const ACTION_END: i32 = 1007; // Close()
pub const ACTION_SEEK: i32 = 1008;
pub const ACTION_IS_FILESYSTEM: i32 = 1027;

// dos/dos.h.
pub const DOSTRUE: u32 = 0xFFFF_FFFF;
pub const DOSFALSE: u32 = 0;
pub const ERROR_OBJECT_NOT_FOUND: u32 = 205;
pub const ERROR_ACTION_NOT_KNOWN: u32 = 209;
pub const ERROR_INVALID_LOCK: u32 = 211;
pub const ERROR_OBJECT_WRONG_TYPE: u32 = 212;
pub const ERROR_SEEK_ERROR: u32 = 219;
pub const ERROR_NO_MORE_ENTRIES: u32 = 232;
/// ACTION_SEEK Arg3 modes (dos/dos.h OFFSET_*).
pub const OFFSET_BEGINNING: i32 = -1;
pub const OFFSET_CURRENT: i32 = 0;
pub const OFFSET_END: i32 = 1;
/// fl_Access shared ("read") lock (dos/dos.h ACCESS_READ = -2).
pub const ACCESS_READ: u32 = 0xFFFF_FFFE;
/// id_DiskState: validated, read/write.
pub const ID_VALIDATED: u32 = 82;
// fib_DirEntryType values (dos/dosextens.h ST_*).
pub const ST_ROOT: i32 = 1;
pub const ST_USERDIR: i32 = 2;
pub const ST_FILE: i32 = -3;
// fib_Protection bits (dos/dos.h FIBF_*). The rwed group is inverted:
// a SET bit means the operation is DENIED.
pub const FIBF_DELETE: u32 = 1 << 0;
pub const FIBF_WRITE: u32 = 1 << 2;

// The isolated fields poked in DOS structures the guest owns.
/// `dn_Task` in `struct DeviceNode` (dos/dosextens.h).
pub const DEVICENODE_TASK: u32 = 8;
/// `dn_Startup` in `struct DeviceNode`.
pub const DEVICENODE_STARTUP: u32 = 28;
/// `fh_Arg1` in `struct FileHandle` (dos/dosextens.h).
pub const FILEHANDLE_ARG1: u32 = 36;

/// A big-endian LONG as the guest sees it.
pub type Long = U32<BigEndian>;

pub fn long(v: u32) -> Long {
    U32::new(v)
}

/// `struct InfoData` (dos/dos.h).
#[derive(IntoBytes, Immutable)]
#[repr(C)]
pub struct InfoData {
    pub num_soft_errors: Long,
    pub unit_number: Long,
    pub disk_state: Long,
    pub num_blocks: Long,
    pub num_blocks_used: Long,
    pub bytes_per_block: Long,
    pub disk_type: Long,
    pub volume_node: Long, // BPTR
    pub in_use: Long,
}

/// `struct FileLock` (dos/dosextens.h).
#[derive(IntoBytes, Immutable)]
#[repr(C)]
pub struct FileLock {
    pub link: Long,   // BPTR
    pub key: Long,    // opaque to DOS
    pub access: Long, // ACCESS_READ / ACCESS_WRITE
    pub task: Long,   // APTR: the handler's MsgPort
    pub volume: Long, // BPTR to the volume DosList node
}

/// `struct DosList`, DLT_VOLUME flavor (dos/dosextens.h struct DeviceList).
#[derive(IntoBytes, Immutable)]
#[repr(C)]
pub struct VolumeNode {
    pub next: Long, // BPTR
    pub r#type: Long,
    pub task: Long, // APTR: the handler's MsgPort
    pub lock: Long, // BPTR
    pub volume_date: [Long; 3],
    pub lock_list: Long, // BPTR
    pub disk_type: Long,
    pub unused: Long,
    pub name: Long, // BSTR
}

/// `struct FileInfoBlock` (dos/dos.h), through fib_Comment. The file name
/// and comment are BCPL strings embedded in the block.
#[derive(IntoBytes, Immutable)]
#[repr(C)]
pub struct FileInfoBlock {
    pub disk_key: Long,
    pub dir_entry_type: Long,
    pub file_name: [u8; 108],
    pub protection: Long,
    pub entry_type: Long,
    pub size: Long,
    pub num_blocks: Long,
    pub date: [Long; 3],
    pub comment: [u8; 80],
}

/// A BCPL string field: length byte + bytes + NUL (the NUL for 2.0+ tools).
pub fn bcpl<const N: usize>(s: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    let len = s.len().min(N - 2);
    out[0] = len as u8;
    out[1..1 + len].copy_from_slice(&s[..len]);
    out
}

/// Copy a host byte slice into guest memory.
pub fn write_bytes(bus: &mut dyn AddressBus, at: u32, bytes: &[u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        bus.write_byte(at + i as u32, b);
    }
}

/// Read a BSTR (BPTR to length-prefixed string) from guest memory.
pub fn read_bstr(bus: &mut dyn AddressBus, bptr: u32) -> Vec<u8> {
    let addr = bptr << 2;
    let len = bus.read_byte(addr) as u32;
    (0..len).map(|i| bus.read_byte(addr + 1 + i)).collect()
}

/// An AmigaDOS DateStamp: days/minutes/ticks since 1978-01-01.
pub type DateStamp = (u32, u32, u32);

/// Host mtime -> AmigaDOS DateStamp.
pub fn amiga_datestamp(time: Option<std::time::SystemTime>) -> DateStamp {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structs_have_the_ndk_layout() {
        assert_eq!(std::mem::size_of::<InfoData>(), 36);
        assert_eq!(std::mem::size_of::<FileLock>(), 20);
        assert_eq!(std::mem::size_of::<VolumeNode>(), 44);
        assert_eq!(std::mem::size_of::<FileInfoBlock>(), 224);
        // Spot-check a serialized field: id_DiskType at offset 24, BE.
        let mut info: InfoData = unsafe { std::mem::zeroed() };
        info.disk_type = long(0x444F_5301);
        assert_eq!(&info.as_bytes()[24..28], &[0x44, 0x4F, 0x53, 0x01]);
    }

    #[test]
    fn datestamp_of_known_moment() {
        // 1978-01-01 00:01:30 UTC = day 0, minute 1, 1500 ticks.
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(252_460_800 + 90);
        assert_eq!(amiga_datestamp(Some(t)), (0, 1, 30 * 50));
    }
}
