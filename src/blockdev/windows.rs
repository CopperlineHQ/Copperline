// SPDX-License-Identifier: GPL-3.0-or-later

//! Windows block devices, through the storage IOCTLs.
//!
//! Windows keeps no registry of media to read the way macOS's IOKit does next
//! door. The disks come from the configuration manager instead: the device
//! interfaces present in the `GUID_DEVINTERFACE_DISK` class, which is the
//! same list Disk Management is built from. Each is opened with
//! `dwDesiredAccess = 0` and asked which disk it is and what it is.
//!
//! Walking `\\.\PhysicalDrive0` upwards would answer the same question with
//! one call fewer, and it is what the older emulators do, but it has to stop
//! counting somewhere -- and disk numbers are neither contiguous nor bounded
//! by anything a program can know, so any ceiling silently hides a disk on
//! the machine that has more. Asking for the devices that are actually there
//! has no ceiling to pick.
//!
//! Opening for nothing is not a trick to get around anything. The security
//! descriptor on a physical drive grants Everyone `FILE_GENERIC_EXECUTE` --
//! `READ_CONTROL`, `SYNCHRONIZE` and `FILE_READ_ATTRIBUTES`, and no access at
//! all to the data -- so an open asking for no access passes the access
//! check, and the descriptive IOCTLs are answered on the handle that comes
//! back. Nothing is read from the medium, so a listing cannot disturb a disk
//! or need a privilege.
//!
//! # Which IOCTLs that handle can serve
//!
//! An IOCTL's code carries in it the access the I/O manager will demand of
//! the handle, and this is what decides the shape of the enumeration.
//! `IOCTL_DISK_GET_LENGTH_INFO` is declared `FILE_READ_ACCESS`, so it is
//! refused on a handle opened for nothing, however exactly it names the thing
//! we want. `IOCTL_STORAGE_QUERY_PROPERTY`, `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX`
//! and `IOCTL_STORAGE_GET_MEDIA_TYPES_EX` are all `FILE_ANY_ACCESS`, and
//! between them they carry the model, the bus, the sector size, the capacity
//! and whether the medium is write protected.
//!
//! # Privilege
//!
//! Reading a sector is a different matter from describing the disk. Measured
//! on Windows 11 (build 26200), a fixed SATA disk and a removable USB card
//! reader carried the same descriptor:
//!
//! ```text
//! D:P(A;;FA;;;BA)(A;;FA;;;SY)(A;;FX;;;WD)(A;;FX;;;RC)(A;;0x12019f;;;UD)
//! ```
//!
//! Everything to Administrators, to `SYSTEM`, and to the user-mode driver
//! host; to everybody else only the execute-shaped right that carries no data
//! access. **Removable media does not lift this.** What does carry an
//! interactive user's read and write grant is the *volume* object
//! (`\\.\E:`), and a volume is one partition of one disk -- it cannot reach
//! block 0, which is where an Amiga disk keeps the RDB that makes sense of
//! the rest of it. So there is no unprivileged route to a whole disk, and an
//! Amiga disk is exactly the case with no volume to borrow one from.
//!
//! # Being our own broker
//!
//! A process cannot elevate itself, and relaunching Copperline as
//! Administrator would throw away the machine already running -- which is
//! precisely what somebody attaching a disk mid-session does not want. macOS
//! is spared this by `authopen`, a system tool that opens a device on your
//! behalf and passes the descriptor back. Windows ships nothing equivalent,
//! so this module is that tool for itself.
//!
//! On `ERROR_ACCESS_DENIED`, the same binary is run once more through
//! `ShellExecuteEx` with the `runas` verb, which is what raises the consent
//! dialog. That privileged half opens the disk, takes the volumes, copies the
//! handles into the still-running unprivileged process with `DuplicateHandle`,
//! and exits. Access on Windows is decided at open and travels with the
//! handle, so what comes back keeps working long after the process that
//! obtained it is gone -- the same property `authopen` relies on, reached
//! down a different road.
//!
//! The privileged half decides for itself what it is willing to open. It is
//! reached by a command line, and a command line can say anything, so it
//! re-applies the safety rules rather than trusting the half that asked: the
//! disk the host runs from is refused there too.
//!
//! # Taking the disk from the host
//!
//! Since Vista the filesystem owns the sectors of a mounted volume, and a
//! write to them through a disk handle is refused. `FSCTL_LOCK_VOLUME` then
//! `FSCTL_DISMOUNT_VOLUME` on every volume the disk carries is what gives
//! them up, and the lock lasts exactly as long as the handle that took it --
//! so the handles are kept for as long as the machine has the disk, in the
//! [`super::BlockDevice`] itself. An Amiga RDB disk usually has no volume at
//! all, Windows recognising nothing on it, and then there is nothing to take.

use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::ptr;

use anyhow::{Context, Result};

use super::broker_protocol::{self, BROKER_FLAG};
use super::{HostDevice, Safety};

// Only the calls this needs are declared, following `src/net/bridge/windows.rs`
// and `src/midi/winmm.rs`, rather than taking on a binding crate for one
// screen of FFI.
type Handle = *mut c_void;

const INVALID_HANDLE_VALUE: Handle = usize::MAX as Handle;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;
/// Writes reach the medium rather than sitting in the cache. A card can be
/// pulled out of a reader at any moment, which an image file cannot.
const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;

const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_NOT_READY: i32 = 21;
const ERROR_WRITE_PROTECT: i32 = 19;
const ERROR_NO_MORE_FILES: i32 = 18;
const ERROR_MORE_DATA: i32 = 234;

// CTL_CODE(device, function, METHOD_BUFFERED, access): the access field is the
// part that matters here, and the comment on each says which it is.
/// `IOCTL_STORAGE_QUERY_PROPERTY`, `FILE_ANY_ACCESS`.
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
/// `IOCTL_STORAGE_GET_MEDIA_TYPES_EX`, `FILE_ANY_ACCESS`.
const IOCTL_STORAGE_GET_MEDIA_TYPES_EX: u32 = 0x002D_0C04;
/// `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX`, `FILE_ANY_ACCESS`.
const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
/// `IOCTL_DISK_GET_LENGTH_INFO`, `FILE_READ_ACCESS` -- so only once opened.
const IOCTL_DISK_GET_LENGTH_INFO: u32 = 0x0007_405C;
/// `IOCTL_STORAGE_GET_DEVICE_NUMBER`, `FILE_ANY_ACCESS`.
const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
/// `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`, `FILE_ANY_ACCESS`.
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
/// `FSCTL_LOCK_VOLUME`, `FILE_ANY_ACCESS`.
const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
/// `FSCTL_DISMOUNT_VOLUME`, `FILE_ANY_ACCESS`.
const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;

/// `PropertyStandardQuery`.
const PROPERTY_STANDARD_QUERY: u32 = 0;
/// `StorageDeviceProperty`.
const STORAGE_DEVICE_PROPERTY: u32 = 0;
/// `StorageAccessAlignmentProperty`.
const STORAGE_ACCESS_ALIGNMENT_PROPERTY: u32 = 6;

/// `MEDIA_WRITE_PROTECTED` in `DEVICE_MEDIA_INFO::MediaCharacteristics`.
const MEDIA_WRITE_PROTECTED: u32 = 0x0100;

// STORAGE_BUS_TYPE. Only the ones this classifies by are named; the rest fall
// through to "not a bus an Amiga disk arrives on".
const BUS_TYPE_1394: u32 = 0x04;
const BUS_TYPE_USB: u32 = 0x07;
const BUS_TYPE_SD: u32 = 0x0C;
const BUS_TYPE_MMC: u32 = 0x0D;

/// `MAX_PATH`, which is the buffer size the volume calls are specified in
/// terms of; a volume GUID path is far shorter.
const MAX_PATH: usize = 260;

/// `SEE_MASK_NOCLOSEPROCESS`: keep the child's process handle, which is the
/// only way to know when it has finished and what it decided.
const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
/// `SEE_MASK_NO_CONSOLE`: the helper inherits this console rather than
/// flashing one of its own.
const SEE_MASK_NO_CONSOLE: u32 = 0x0000_8000;
/// `SW_HIDE`: the helper has no window to show.
const SW_HIDE: i32 = 0;
/// `INFINITE`. The wait is over a consent dialog, which lasts as long as the
/// person looking at it does.
const INFINITE: u32 = 0xFFFF_FFFF;
/// `ERROR_CANCELLED`, which is how a refused consent dialog arrives.
const ERROR_CANCELLED: i32 = 1223;
/// `PROCESS_DUP_HANDLE`.
const PROCESS_DUP_HANDLE: u32 = 0x0040;
/// `TOKEN_QUERY`.
const TOKEN_QUERY: u32 = 0x0008;
/// `TokenElevation` in `TOKEN_INFORMATION_CLASS`.
const TOKEN_ELEVATION_CLASS: u32 = 20;
/// `DUPLICATE_SAME_ACCESS`.
const DUPLICATE_SAME_ACCESS: u32 = 0x0002;
/// `DUPLICATE_CLOSE_SOURCE`, which is the only way to close a handle living in
/// another process: duplicate it out and throw the copy away.
const DUPLICATE_CLOSE_SOURCE: u32 = 0x0001;

#[repr(C)]
struct StoragePropertyQuery {
    property_id: u32,
    query_type: u32,
    additional_parameters: [u8; 1],
}

#[repr(C)]
#[allow(dead_code)] // FFI layout mirror: read by byte offset, not by field.
struct StorageDeviceDescriptor {
    version: u32,
    size: u32,
    device_type: u8,
    device_type_modifier: u8,
    removable_media: u8,
    command_queueing: u8,
    vendor_id_offset: u32,
    product_id_offset: u32,
    product_revision_offset: u32,
    serial_number_offset: u32,
    bus_type: u32,
    raw_properties_length: u32,
    raw_device_properties: [u8; 1],
}

#[repr(C)]
#[allow(dead_code)] // FFI layout mirror.
struct StorageAccessAlignmentDescriptor {
    version: u32,
    size: u32,
    bytes_per_cache_line: u32,
    bytes_offset_for_cache_alignment: u32,
    bytes_per_logical_sector: u32,
    bytes_per_physical_sector: u32,
    bytes_offset_for_sector_alignment: u32,
}

#[repr(C)]
#[allow(dead_code)] // FFI layout mirror: only disk_size is read.
struct DiskGeometryEx {
    cylinders: i64,
    media_type: u32,
    tracks_per_cylinder: u32,
    sectors_per_track: u32,
    bytes_per_sector: u32,
    disk_size: i64,
    data: [u8; 1],
}

#[repr(C)]
#[allow(dead_code)] // FFI layout mirror: only media_characteristics is read.
struct DeviceMediaInfo {
    cylinders: i64,
    media_type: u32,
    tracks_per_cylinder: u32,
    sectors_per_track: u32,
    bytes_per_sector: u32,
    number_media_sides: u32,
    media_characteristics: u32,
}

#[repr(C)]
#[allow(dead_code)] // FFI layout mirror.
struct GetMediaTypes {
    device_type: u32,
    media_info_count: u32,
    media_info: [DeviceMediaInfo; 1],
}

#[repr(C)]
#[allow(dead_code)] // FFI layout mirror: only disk_number is read.
struct DiskExtent {
    disk_number: u32,
    starting_offset: i64,
    extent_length: i64,
}

#[repr(C)]
struct VolumeDiskExtents {
    number_of_disk_extents: u32,
    extents: [DiskExtent; 1],
}

#[repr(C)]
#[allow(dead_code)] // FFI layout mirror: only device_number is read.
struct StorageDeviceNumber {
    device_type: u32,
    device_number: u32,
    partition_number: i32,
}

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct ShellExecuteInfoW {
    size: u32,
    mask: u32,
    parent_window: Handle,
    verb: *const u16,
    file: *const u16,
    parameters: *const u16,
    directory: *const u16,
    show: i32,
    instance: Handle,
    id_list: *mut c_void,
    class: *const u16,
    key_class: Handle,
    hot_key: u32,
    icon_or_monitor: Handle,
    process: Handle,
}

/// `GUID_DEVINTERFACE_DISK`: every whole disk the configuration manager knows
/// about, which is what Disk Management shows and what this offers from.
const GUID_DEVINTERFACE_DISK: Guid = Guid {
    data1: 0x53f5_6307,
    data2: 0xb6bf,
    data3: 0x11d0,
    data4: [0x94, 0xf2, 0x00, 0xa0, 0xc9, 0x1e, 0xfb, 0x8b],
};

/// `CR_SUCCESS`.
const CR_SUCCESS: u32 = 0;
/// `CR_BUFFER_SMALL`: the list grew between being measured and being asked
/// for, which happens when a disk is plugged in at that moment.
const CR_BUFFER_SMALL: u32 = 0x1A;
/// `CM_GET_DEVICE_INTERFACE_LIST_PRESENT`: interfaces whose device is here
/// now, rather than every one this machine has ever seen.
const CM_GET_DEVICE_INTERFACE_LIST_PRESENT: u32 = 0;

// Pin the mirrors to the SDK layout. Every one of these is filled in by the
// kernel at a byte offset it decides, so a mismatch would be read as plausible
// nonsense -- a wrong capacity here is a wrong idea of where a disk ends.
const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<StoragePropertyQuery>() == 12);
    assert!(size_of::<StorageDeviceDescriptor>() == 40);
    assert!(offset_of!(StorageDeviceDescriptor, removable_media) == 10);
    assert!(offset_of!(StorageDeviceDescriptor, vendor_id_offset) == 12);
    assert!(offset_of!(StorageDeviceDescriptor, bus_type) == 28);
    assert!(size_of::<StorageAccessAlignmentDescriptor>() == 28);
    assert!(offset_of!(StorageAccessAlignmentDescriptor, bytes_per_logical_sector) == 16);
    assert!(size_of::<DiskGeometryEx>() == 40);
    assert!(offset_of!(DiskGeometryEx, disk_size) == 24);
    assert!(size_of::<DeviceMediaInfo>() == 32);
    assert!(offset_of!(DeviceMediaInfo, media_characteristics) == 28);
    assert!(offset_of!(GetMediaTypes, media_info) == 8);
    assert!(size_of::<DiskExtent>() == 24);
    assert!(offset_of!(VolumeDiskExtents, extents) == 8);
    assert!(size_of::<StorageDeviceNumber>() == 12);
    assert!(offset_of!(StorageDeviceNumber, device_number) == 4);
    assert!(size_of::<Guid>() == 16);
};

// `SHELLEXECUTEINFOW` is passed by size: the shell reads `cbSize` to decide
// which fields it may touch, so a mirror that disagrees with the SDK is read
// off the end of this one.
#[cfg(target_pointer_width = "64")]
const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<ShellExecuteInfoW>() == 112);
    assert!(offset_of!(ShellExecuteInfoW, verb) == 16);
    assert!(offset_of!(ShellExecuteInfoW, show) == 48);
    assert!(offset_of!(ShellExecuteInfoW, process) == 104);
};

#[cfg(target_pointer_width = "32")]
const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<ShellExecuteInfoW>() == 60);
    assert!(offset_of!(ShellExecuteInfoW, process) == 56);
};

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn DeviceIoControl(
        device: Handle,
        code: u32,
        input: *const c_void,
        input_size: u32,
        output: *mut c_void,
        output_size: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn GetWindowsDirectoryW(buffer: *mut u16, size: u32) -> u32;
    fn GetVolumePathNameW(file: *const u16, path: *mut u16, length: u32) -> i32;
    fn GetVolumeNameForVolumeMountPointW(mount: *const u16, name: *mut u16, length: u32) -> i32;
    fn FindFirstVolumeW(name: *mut u16, length: u32) -> Handle;
    fn FindNextVolumeW(search: Handle, name: *mut u16, length: u32) -> i32;
    fn FindVolumeClose(search: Handle) -> i32;
    fn GetVolumePathNamesForVolumeNameW(
        name: *const u16,
        buffer: *mut u16,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> Handle;
    fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> Handle;
    fn DuplicateHandle(
        source_process: Handle,
        source: Handle,
        target_process: Handle,
        target: *mut Handle,
        access: u32,
        inherit: i32,
        options: u32,
    ) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn CloseHandle(handle: Handle) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(
        token: Handle,
        class: u32,
        information: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}

#[link(name = "cfgmgr32")]
extern "system" {
    fn CM_Get_Device_Interface_List_SizeW(
        length: *mut u32,
        class: *const Guid,
        device: *const u16,
        flags: u32,
    ) -> u32;
    fn CM_Get_Device_Interface_ListW(
        class: *const Guid,
        device: *const u16,
        buffer: *mut u16,
        length: u32,
        flags: u32,
    ) -> u32;
}

/// A NUL-terminated wide string, as every `W` entry point wants.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The first NUL-terminated string in a wide buffer.
fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// Split a multi-string -- several NUL-terminated strings run together, the
/// list ended by an empty one -- which is how both the configuration manager
/// and the volume calls return a list.
fn multi_string(buffer: &[u16]) -> Vec<String> {
    buffer
        .split(|&unit| unit == 0)
        .take_while(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// Open a device node -- a whole disk, or a volume cut from one.
///
/// The handle owns itself, so a path that gives up part-way closes what it
/// had: volumes taken from the host before something failed go back to it on
/// the way out, rather than being left locked against it.
fn open_device_node(path: &str, access: u32, flags: u32) -> std::io::Result<OwnedHandle> {
    let path = wide(path);
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            // Sharing is what the storage stack itself already has open on
            // the disk; refusing to share would fail against the volume
            // manager rather than protect anything.
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null_mut(),
            OPEN_EXISTING,
            flags,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) })
}

/// Issue an IOCTL, returning how many bytes came back.
fn control(
    device: &OwnedHandle,
    code: u32,
    input: &[u8],
    output: &mut [u8],
) -> std::io::Result<u32> {
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            device.as_raw_handle() as Handle,
            code,
            if input.is_empty() {
                ptr::null()
            } else {
                input.as_ptr().cast()
            },
            input.len() as u32,
            if output.is_empty() {
                ptr::null_mut()
            } else {
                output.as_mut_ptr().cast()
            },
            output.len() as u32,
            &mut returned,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(returned)
}

/// Ask the storage stack for one of a device's property descriptors.
fn query_property(
    device: &OwnedHandle,
    property_id: u32,
    output: &mut [u8],
) -> std::io::Result<u32> {
    let query = StoragePropertyQuery {
        property_id,
        query_type: PROPERTY_STANDARD_QUERY,
        additional_parameters: [0],
    };
    let input = unsafe {
        std::slice::from_raw_parts(
            ptr::from_ref(&query).cast::<u8>(),
            std::mem::size_of::<StoragePropertyQuery>(),
        )
    };
    control(device, IOCTL_STORAGE_QUERY_PROPERTY, input, output)
}

/// Read one of the strings a `STORAGE_DEVICE_DESCRIPTOR` points at.
///
/// The strings live past the end of the struct, in the same buffer, named by
/// byte offsets from its start -- an offset of zero means the device did not
/// report that one.
fn descriptor_string(buffer: &[u8], offset: u32) -> Option<String> {
    let offset = offset as usize;
    if offset == 0 || offset >= buffer.len() {
        return None;
    }
    let tail = &buffer[offset..];
    let end = tail
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(tail.len());
    let text = String::from_utf8_lossy(&tail[..end]).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// What to show for a disk: the vendor and product as one name.
///
/// Devices are inconsistent about which of the two carries the useful half --
/// a USB reader answers "Generic" and "MassStorageClass", an NVMe drive puts
/// everything in the product -- so both are joined and the empty one drops
/// out.
fn model_of(vendor: Option<String>, product: Option<String>) -> Option<String> {
    let joined = [vendor, product]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (!joined.is_empty()).then_some(joined)
}

/// Whether a bus is one the host's own storage usually hangs off.
///
/// An Amiga disk most often arrives through a card reader or a USB bridge,
/// so a disk on any other bus is more likely the host's own. This only
/// labels and sorts -- every disk but the system's is offered either way.
const fn bus_is_internal(bus_type: u32) -> bool {
    !matches!(
        bus_type,
        BUS_TYPE_USB | BUS_TYPE_SD | BUS_TYPE_MMC | BUS_TYPE_1394
    )
}

/// Whether a disk may be handed to a machine: everything but the one the
/// host is running from.
const fn classify(is_system: bool) -> Safety {
    if is_system {
        Safety::SystemDisk
    } else {
        Safety::Offerable
    }
}

/// The disk number in an identifier this module made.
fn disk_number_of(id: &str) -> Option<u32> {
    id.strip_prefix("PhysicalDrive")?.parse().ok()
}

/// A volume GUID path as `CreateFileW` wants it.
///
/// The volume calls hand back `\\?\Volume{...}\` and the file calls want it
/// without that trailing separator: with it, the open names the volume's root
/// directory rather than the volume itself, and the FSCTLs below have nothing
/// to act on.
fn volume_node_path(guid_path: &str) -> String {
    guid_path.trim_end_matches('\\').to_string()
}

/// One volume the host has, and where it sits.
struct Volume {
    /// `\\?\Volume{...}\`, as the enumeration gives it.
    guid_path: String,
    /// Every disk the volume's extents land on. More than one means a spanned
    /// or striped volume, and then every one of those disks is in use.
    disks: Vec<u32>,
    /// Drive letters and mount points, which is how a user knows the volume.
    mounted_at: Vec<String>,
}

/// The disks a volume's extents lie on.
///
/// Opened for nothing, like the disks themselves: this asks the volume
/// manager a question about layout and reads no data.
fn disks_behind(guid_path: &str) -> Vec<u32> {
    let Ok(volume) = open_device_node(&volume_node_path(guid_path), 0, 0) else {
        return Vec::new();
    };
    let first = std::mem::offset_of!(VolumeDiskExtents, extents);
    let stride = std::mem::size_of::<DiskExtent>();
    // A plain partition has one extent; enough room for a few, and a volume
    // spanning more says so rather than being cut short.
    let mut buffer = vec![0u8; first + 8 * stride];
    loop {
        match control(
            &volume,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            &[],
            &mut buffer,
        ) {
            Ok(_) => break,
            Err(error) if error.raw_os_error() == Some(ERROR_MORE_DATA) => {
                // The count is filled in even when the extents themselves did
                // not fit, which is how a volume says how much room it wants.
                // Asking again matters more here than anywhere else in the
                // file: this is how the disk Windows runs from is recognised,
                // and a volume left unread is a disk left unprotected.
                let wanted = u32::from_ne_bytes(buffer[..4].try_into().expect("4 bytes")) as usize;
                let needed = first + wanted * stride;
                if wanted == 0 || needed <= buffer.len() {
                    return Vec::new();
                }
                buffer = vec![0u8; needed];
            }
            Err(_) => return Vec::new(),
        }
    }

    let count = u32::from_ne_bytes(buffer[..4].try_into().expect("4 bytes")) as usize;
    let mut disks = Vec::new();
    for index in 0..count {
        let at = first + index * stride;
        if at + stride > buffer.len() {
            break;
        }
        let number = u32::from_ne_bytes(buffer[at..at + 4].try_into().expect("4 bytes"));
        if !disks.contains(&number) {
            disks.push(number);
        }
    }
    disks
}

/// Where the host has volumes mounted, and which disk each one is cut from.
fn volumes() -> Vec<Volume> {
    let mut found = Vec::new();
    let mut name = [0u16; MAX_PATH];
    let search = unsafe { FindFirstVolumeW(name.as_mut_ptr(), name.len() as u32) };
    if search == INVALID_HANDLE_VALUE {
        return found;
    }
    loop {
        let guid_path = from_wide(&name);
        if !guid_path.is_empty() {
            found.push(Volume {
                disks: disks_behind(&guid_path),
                mounted_at: mount_points(&guid_path),
                guid_path,
            });
        }
        if unsafe { FindNextVolumeW(search, name.as_mut_ptr(), name.len() as u32) } == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES) {
                log::debug!("blockdev: walking volumes stopped early: {error}");
            }
            break;
        }
    }
    unsafe { FindVolumeClose(search) };
    found
}

/// The drive letters and directories a volume is reachable through.
///
/// A volume need not have any: one that Windows can see but not mount, which
/// is what an Amiga disk normally looks like, has none, and so does one
/// deliberately left without a letter.
fn mount_points(guid_path: &str) -> Vec<String> {
    // The trailing separator stays on here -- this call wants the name in the
    // form the enumeration produced it.
    let name = wide(guid_path);
    let mut needed = 0u32;
    let mut buffer = vec![0u16; MAX_PATH];
    loop {
        let ok = unsafe {
            GetVolumePathNamesForVolumeNameW(
                name.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut needed,
            )
        };
        if ok != 0 {
            break;
        }
        // A volume with many mount points needs a bigger buffer, and the call
        // says how much; anything else means it has none to report.
        if needed as usize > buffer.len() {
            buffer = vec![0u16; needed as usize];
            continue;
        }
        return Vec::new();
    }
    multi_string(&buffer)
}

/// Every whole disk the configuration manager has present right now, as
/// device interface paths.
///
/// The list is measured and then fetched, and a disk plugged in between the
/// two makes the second call say so rather than truncate; asking again is the
/// documented answer to that.
fn disk_interface_paths() -> Vec<String> {
    for _ in 0..4 {
        let mut length = 0u32;
        let rc = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut length,
                &GUID_DEVINTERFACE_DISK,
                ptr::null(),
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        if rc != CR_SUCCESS {
            log::warn!("blockdev: the list of disks could not be measured (CONFIGRET {rc})");
            return Vec::new();
        }
        let mut buffer = vec![0u16; length as usize];
        let rc = unsafe {
            CM_Get_Device_Interface_ListW(
                &GUID_DEVINTERFACE_DISK,
                ptr::null(),
                buffer.as_mut_ptr(),
                length,
                CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
            )
        };
        match rc {
            CR_SUCCESS => return multi_string(&buffer),
            CR_BUFFER_SMALL => continue,
            _ => {
                log::warn!("blockdev: the list of disks could not be read (CONFIGRET {rc})");
                return Vec::new();
            }
        }
    }
    log::warn!("blockdev: the set of disks kept changing while it was being listed");
    Vec::new()
}

/// Which physical disk an open device interface is.
///
/// The interface path names the device but not its number, and the number is
/// what a user, a configuration file and `\\.\PhysicalDriveN` all speak in.
fn device_number(device: &OwnedHandle) -> Option<u32> {
    let mut buffer = [0u8; std::mem::size_of::<StorageDeviceNumber>()];
    control(device, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], &mut buffer).ok()?;
    let at = std::mem::offset_of!(StorageDeviceNumber, device_number);
    Some(u32::from_ne_bytes(
        buffer[at..at + 4].try_into().expect("4 bytes"),
    ))
}

/// The disks the running Windows installation is on.
///
/// Found by asking where Windows itself is and following that back through
/// the volume to its extents, rather than by guessing from a filesystem the
/// way WinUAE does -- a machine can have several NTFS volumes and only one of
/// them is the one that must never be handed to the emulator. A volume that
/// spans disks puts every disk it touches out of reach, because any of them
/// carries part of the running system.
fn system_disks() -> Vec<u32> {
    let mut directory = [0u16; MAX_PATH];
    let length =
        unsafe { GetWindowsDirectoryW(directory.as_mut_ptr(), directory.len() as u32) } as usize;
    if length == 0 || length >= directory.len() {
        log::warn!("blockdev: the Windows directory could not be found, so no disk can be ruled out as the host's own");
        return Vec::new();
    }
    let windows = wide(&from_wide(&directory));

    let mut root = [0u16; MAX_PATH];
    if unsafe { GetVolumePathNameW(windows.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        log::warn!(
            "blockdev: the volume holding Windows could not be named: {}",
            std::io::Error::last_os_error()
        );
        return Vec::new();
    }
    let root = wide(&from_wide(&root));

    let mut guid = [0u16; MAX_PATH];
    if unsafe {
        GetVolumeNameForVolumeMountPointW(root.as_ptr(), guid.as_mut_ptr(), guid.len() as u32)
    } == 0
    {
        log::warn!(
            "blockdev: the volume holding Windows has no GUID path: {}",
            std::io::Error::last_os_error()
        );
        return Vec::new();
    }
    disks_behind(&from_wide(&guid))
}

/// Capacity in bytes, from a handle that may have been opened for nothing.
///
/// The geometry IOCTL is `FILE_ANY_ACCESS`, so this is the one enumeration can
/// ask. `DiskSize` is the true capacity and not the cylinder-rounded figure
/// the old geometry fields imply -- on the card this was written against it
/// agrees with the storage service to the byte, where the rounded figure is
/// nearly a megabyte short.
fn capacity(device: &OwnedHandle) -> Option<u64> {
    let mut geometry = vec![0u8; std::mem::size_of::<DiskGeometryEx>()];
    control(device, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut geometry).ok()?;
    let at = std::mem::offset_of!(DiskGeometryEx, disk_size);
    let size = i64::from_ne_bytes(geometry[at..at + 8].try_into().expect("8 bytes"));
    (size > 0).then_some(size as u64)
}

/// Capacity as the disk itself reports it, which only a handle opened for
/// reading may ask.
///
/// `IOCTL_DISK_GET_LENGTH_INFO` is declared `FILE_READ_ACCESS`, so asking it
/// during enumeration is a syscall that cannot succeed; it is worth asking
/// once the disk is really open, in case a device rounds the geometry.
fn exact_capacity(device: &OwnedHandle) -> Option<u64> {
    let mut length = [0u8; 8];
    control(device, IOCTL_DISK_GET_LENGTH_INFO, &[], &mut length).ok()?;
    let size = i64::from_ne_bytes(length);
    (size > 0).then_some(size as u64)
}

/// The media's own logical sector size.
///
/// The alignment descriptor is the authority, but plenty of USB bridges do not
/// implement it; the geometry's `BytesPerSector` is the same number by another
/// route, and 512 is what a device reporting neither has to be.
fn logical_sector_size(device: &OwnedHandle) -> u32 {
    let mut alignment = vec![0u8; std::mem::size_of::<StorageAccessAlignmentDescriptor>()];
    if query_property(device, STORAGE_ACCESS_ALIGNMENT_PROPERTY, &mut alignment).is_ok() {
        let at = std::mem::offset_of!(StorageAccessAlignmentDescriptor, bytes_per_logical_sector);
        let size = u32::from_ne_bytes(alignment[at..at + 4].try_into().expect("4 bytes"));
        if size > 0 {
            return size;
        }
    }
    let mut geometry = vec![0u8; std::mem::size_of::<DiskGeometryEx>()];
    if control(device, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut geometry).is_ok() {
        let at = std::mem::offset_of!(DiskGeometryEx, bytes_per_sector);
        let size = u32::from_ne_bytes(geometry[at..at + 4].try_into().expect("4 bytes"));
        if size > 0 {
            return size;
        }
    }
    512
}

/// Whether the hardware will accept a write.
///
/// This is the lock switch on an SD card, not a permission: a locked card
/// refuses writes however the disk was opened and whoever asked. A device that
/// does not answer the question is taken at its word as writable, and a write
/// that is then refused says so plainly when it happens.
fn hardware_writable(device: &OwnedHandle) -> bool {
    let mut types = vec![0u8; std::mem::size_of::<GetMediaTypes>()];
    if control(device, IOCTL_STORAGE_GET_MEDIA_TYPES_EX, &[], &mut types).is_err() {
        return true;
    }
    let count = u32::from_ne_bytes(types[4..8].try_into().expect("4 bytes"));
    if count == 0 {
        return true;
    }
    let at = std::mem::offset_of!(GetMediaTypes, media_info)
        + std::mem::offset_of!(DeviceMediaInfo, media_characteristics);
    let characteristics = u32::from_ne_bytes(types[at..at + 4].try_into().expect("4 bytes"));
    characteristics & MEDIA_WRITE_PROTECTED == 0
}

/// Describe one disk, or nothing if it will not say what it is.
fn describe(interface: &str, volumes: &[Volume], system: &[u32]) -> Option<HostDevice> {
    // Zero access: enough for the descriptive IOCTLs, and no access check to
    // fail. A disk unplugged between the listing and here fails at this point
    // and is simply not offered.
    let device = match open_device_node(interface, 0, 0) {
        Ok(device) => device,
        Err(error) => {
            log::debug!("blockdev: {interface} would not open to be described: {error}");
            return None;
        }
    };
    // The interface path names the device; the number is what everything
    // above this module speaks in, so a disk that will not give one is not
    // one this can offer.
    let number = device_number(&device)?;
    let id = format!("PhysicalDrive{number}");
    let path = format!(r"\\.\{id}");

    // An empty slot in a card reader is a disk with no medium in it: it opens,
    // and then has no capacity to report. There is nothing to offer, and
    // nothing wrong either.
    let size_bytes = capacity(&device)?;
    if size_bytes == 0 {
        return None;
    }

    // The strings sit past the end of the struct in the same buffer, so the
    // buffer has to be bigger than the struct.
    let mut descriptor = vec![0u8; 1024];
    let (model, removable, bus_type) =
        match query_property(&device, STORAGE_DEVICE_PROPERTY, &mut descriptor) {
            Ok(_) => {
                let vendor_at = std::mem::offset_of!(StorageDeviceDescriptor, vendor_id_offset);
                let product_at = std::mem::offset_of!(StorageDeviceDescriptor, product_id_offset);
                let removable_at = std::mem::offset_of!(StorageDeviceDescriptor, removable_media);
                let bus_at = std::mem::offset_of!(StorageDeviceDescriptor, bus_type);
                let offset = |at: usize| {
                    u32::from_ne_bytes(descriptor[at..at + 4].try_into().expect("4 bytes"))
                };
                (
                    model_of(
                        descriptor_string(&descriptor, offset(vendor_at)),
                        descriptor_string(&descriptor, offset(product_at)),
                    ),
                    descriptor[removable_at] != 0,
                    u32::from_ne_bytes(descriptor[bus_at..bus_at + 4].try_into().expect("4 bytes")),
                )
            }
            Err(error) => {
                log::debug!("blockdev: {id} did not describe itself: {error}");
                (None, false, 0)
            }
        };

    let mounted: Vec<String> = volumes
        .iter()
        .filter(|volume| volume.disks.contains(&number))
        .flat_map(|volume| volume.mounted_at.iter().cloned())
        .collect();
    let internal = bus_is_internal(bus_type);

    Some(HostDevice {
        id,
        path: PathBuf::from(path),
        model,
        size_bytes,
        block_size: logical_sector_size(&device),
        removable,
        internal,
        writable: hardware_writable(&device),
        mounted,
        safety: classify(system.contains(&number)),
    })
}

/// Every whole physical disk the host can see.
pub fn list_devices() -> Result<Vec<HostDevice>> {
    let volumes = volumes();
    let system = system_disks();
    if system.is_empty() {
        log::warn!(
            "blockdev: no disk could be identified as the one Windows is running from; \
             every disk will be offered, so choose carefully"
        );
    }
    Ok(disk_interface_paths()
        .iter()
        .filter_map(|interface| describe(interface, &volumes, &system))
        .collect())
}

/// Take every volume the host has on this disk, and keep them.
///
/// Locking is what makes a write to those sectors legal at all; dismounting is
/// what stops the filesystem writing its own idea of the disk over the
/// machine's. Both die with the handle, which is why the handles are returned
/// rather than closed here.
fn take_volumes(disk: u32, id: &str) -> Result<Vec<std::fs::File>> {
    let mut held = Vec::new();
    for volume in volumes().into_iter().filter(|v| v.disks.contains(&disk)) {
        let where_it_is = if volume.mounted_at.is_empty() {
            volume.guid_path.clone()
        } else {
            volume.mounted_at.join(", ")
        };
        let handle = open_device_node(
            &volume_node_path(&volume.guid_path),
            GENERIC_READ | GENERIC_WRITE,
            0,
        )
        // These read out to somebody on one line of a launcher, so each says
        // the thing to do about it and leaves the detail to the chain beneath.
        // A volume that will not open at all is usually locked by a machine
        // still running in this very process, which is the one cause nobody
        // guesses from "access denied".
        .with_context(|| {
            format!("{id}: {where_it_is} is still held; a machine running here may have it")
        })?;
        control(&handle, FSCTL_LOCK_VOLUME, &[], &mut [])
            .with_context(|| format!("{id}: {where_it_is} is in use; close anything open on it"))?;
        control(&handle, FSCTL_DISMOUNT_VOLUME, &[], &mut [])
            .with_context(|| format!("{id}: {where_it_is} would not dismount"))?;
        log::info!("blockdev: {id}: {where_it_is} dismounted; the machine has it");
        held.push(std::fs::File::from(handle));
    }
    Ok(held)
}

/// Open the whole disk itself, with no attempt to explain a failure.
fn open_disk_node(device: &HostDevice, write: bool) -> std::io::Result<OwnedHandle> {
    let path = device
        .path
        .to_str()
        .ok_or_else(|| std::io::Error::other("device path is not UTF-8"))?;
    let access = if write {
        GENERIC_READ | GENERIC_WRITE
    } else {
        GENERIC_READ
    };
    // Write-through only when writing: it costs nothing to ask for and means
    // a card pulled out of a reader has less of the machine's work still in
    // flight. Not `FILE_FLAG_NO_BUFFERING`, which would additionally require
    // every transfer buffer to be sector-aligned in memory -- and those
    // buffers belong to the platform-neutral layer above, which has no reason
    // to know what Windows would want of them.
    let flags = if write { FILE_FLAG_WRITE_THROUGH } else { 0 };
    open_device_node(path, access, flags)
}

/// Say what a refused open actually means, in terms of the medium.
fn explain_open_failure(error: &std::io::Error, device: &HostDevice) -> anyhow::Error {
    match error.raw_os_error() {
        Some(ERROR_WRITE_PROTECT) => anyhow::anyhow!(
            "{} is write protected by the medium itself -- an SD card's lock switch does this. \
             Unlock it, or attach the disk read-only.",
            device.id
        ),
        Some(ERROR_NOT_READY) => anyhow::anyhow!(
            "{} has no medium in it: put the card back in the reader",
            device.id
        ),
        _ => anyhow::anyhow!("opening {}: {error}", device.path.display()),
    }
}

/// Where the privileged half leaves its answer.
///
/// The handle numbers in it are meaningless anywhere but this process -- they
/// name entries in *this* process's handle table, put there by the child --
/// so the file carries nothing worth protecting.
fn handoff_path() -> PathBuf {
    let token = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    std::env::temp_dir().join(format!(
        "copperline-hostdisk-{}-{token:x}.handoff",
        std::process::id()
    ))
}

/// Copy an open handle into another process, and say what it is called there.
fn duplicate_into(target: Handle, source: RawHandle) -> Result<usize> {
    let mut duplicated: Handle = ptr::null_mut();
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            source as Handle,
            target,
            &mut duplicated,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).context("handing an open disk back");
    }
    Ok(duplicated as usize)
}

/// Close a handle this process put into another one's table.
///
/// Best effort by nature: if it fails there is nothing further to try, and the
/// caller is already reporting a failure of its own.
fn close_in(target: Handle, handle: usize) {
    unsafe {
        DuplicateHandle(
            target,
            handle as Handle,
            ptr::null_mut(),
            ptr::null_mut(),
            0,
            0,
            DUPLICATE_CLOSE_SOURCE,
        )
    };
}

/// Read what the privileged half said: `ok` and then a line per disk, naming
/// it and the handles it opened for it, or the one reason it did none of it.
///
/// ```text
/// ok
/// PhysicalDrive1 1a4 2b8
/// PhysicalDrive3 3c0
/// ```
///
/// Every failure here means something is answering that is not the half this
/// asked for, so none of them produce a handle: a number that is not one, an
/// answer in no known shape, and an empty list are all refusals.
/// Read each disk's line of the privileged half's answer: the identifier and
/// then its handles, the disk's own first, as hexadecimal handle numbers in
/// this process's table.
fn parse_answer(answer: &str) -> Result<Vec<(String, Vec<usize>)>> {
    let mut opened = Vec::new();
    for line in broker_protocol::parse_answer(answer)? {
        let mut parts = line.split_whitespace();
        let id = parts
            .next()
            .context("a line of handles with no disk to belong to")?
            .to_string();
        let handles: Vec<usize> = parts
            .map(|value| {
                usize::from_str_radix(value, 16)
                    .map_err(|_| anyhow::anyhow!("{value} is not a handle"))
            })
            .collect::<Result<_>>()?;
        // The disk itself is always the first handle, so a disk named with
        // none of them is an answer that cannot be acted on.
        if handles.is_empty() {
            anyhow::bail!("{id} came back with nothing open");
        }
        opened.push((id, handles));
    }
    Ok(opened)
}

/// Ask for every disk at once, through a single moment of Administrator.
///
/// Windows has no broker of its own that hands back an opened device, so this
/// is one: the same binary, run once with consent, which opens the disks,
/// takes their volumes, and copies the handles into this still-running process
/// before exiting. Access on Windows is decided at open and travels with the
/// handle, so what comes back keeps working after the privileged process is
/// gone -- the same property that makes macOS's `authopen` work, reached
/// differently.
///
/// Every disk goes in one request because consent is a dialog somebody has to
/// read: asking five times for five disks is four interruptions that say
/// nothing the first did not.
///
/// The emulator is not restarted. A process cannot elevate itself, and the
/// obvious alternative -- relaunching Copperline as Administrator -- would
/// throw away the machine that is already running, which is exactly what
/// somebody attaching a disk mid-session does not want.
fn open_through_broker(wanted: &[(HostDevice, bool)]) -> Result<Vec<(String, Held)>> {
    let exe = std::env::current_exe().context("finding this program to run again with consent")?;
    let reply = handoff_path();
    let asked: Vec<String> = wanted
        .iter()
        .map(|(device, write)| broker_protocol::argument(&device.id, *write))
        .collect();
    let parameters = format!(
        "{BROKER_FLAG} {} \"{}\" {}",
        std::process::id(),
        reply.display(),
        asked.join(" ")
    );

    // Nothing of an earlier attempt may be left where the answer goes: a
    // privileged half that dies before writing must read as silence, not as
    // whatever was there before.
    let _ = std::fs::remove_file(&reply);

    let verb = wide("runas");
    let file = wide(&exe.to_string_lossy());
    let parameters = wide(&parameters);
    let mut info: ShellExecuteInfoW = unsafe { std::mem::zeroed() };
    info.size = std::mem::size_of::<ShellExecuteInfoW>() as u32;
    info.mask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE;
    info.verb = verb.as_ptr();
    info.file = file.as_ptr();
    info.parameters = parameters.as_ptr();
    info.show = SW_HIDE;

    let names: Vec<&str> = wanted
        .iter()
        .map(|(device, _)| device.id.as_str())
        .collect();
    log::info!(
        "blockdev: {} needs Administrator; asking Windows for consent",
        names.join(", ")
    );
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_CANCELLED) {
            anyhow::bail!("Administrator is needed to use a real disk, and that was declined");
        }
        return Err(error).context("asking for consent to open a real disk");
    }

    // The consent dialog lasts as long as the person looking at it, so there
    // is no useful timeout to impose here.
    unsafe { WaitForSingleObject(info.process, INFINITE) };
    unsafe { CloseHandle(info.process) };

    let answer = std::fs::read_to_string(&reply).map_err(|error| {
        anyhow::anyhow!(
            "the privileged half left no answer ({error}); it may have been stopped before it \
             could open the disk"
        )
    })?;
    let _ = std::fs::remove_file(&reply);
    let opened = parse_answer(&answer)?;

    let mut taken = Vec::new();
    for (id, handles) in opened {
        let mut handles = handles.into_iter().map(|raw| {
            // Safe only because the numbers were put in this process's handle
            // table by the child; what the first one names is checked below.
            unsafe { OwnedHandle::from_raw_handle(raw as RawHandle) }
        });
        let disk = handles.next().expect("parse_answer refuses an empty list");
        // What came back is trusted only as far as it can be checked: these
        // are handle numbers read out of a file, and the first should name the
        // disk it was asked for.
        if device_number(&disk) != disk_number_of(&id) {
            anyhow::bail!("{id}: what came back is not that disk, so it will not be used");
        }
        if !wanted.iter().any(|(device, _)| device.id == id) {
            anyhow::bail!("{id} was opened but never asked for");
        }
        taken.push((
            id,
            Held {
                disk,
                dismounted: handles.map(std::fs::File::from).collect(),
            },
        ));
    }
    Ok(taken)
}

/// Open disks on behalf of a Copperline that was refused, and hand them back.
///
/// This is the privileged half, and it is reached by a command line, which can
/// say anything -- so it decides for itself what it is willing to open rather
/// than trusting what it was told to. The answer is written where the asking
/// process said, whether it succeeded or not, because a failure it cannot
/// report would look to the other side like a crash.
///
/// One disk it will not open fails the whole request. They were asked for
/// together, and half a machine's disks is not what anybody pressed the button
/// for.
pub fn serve_broker_request(asked: &[String], parent_process_id: u32, reply: &Path) -> Result<()> {
    let outcome = asked
        .iter()
        .map(|argument| {
            broker_protocol::parse_argument(argument)
                .ok_or_else(|| anyhow::anyhow!("{argument} does not name a disk"))
        })
        .collect::<Result<Vec<_>>>()
        .and_then(|wanted| broker_open(&wanted, parent_process_id));
    let answer = match &outcome {
        Ok(opened) => {
            let mut answer = String::from("ok\n");
            for (id, handles) in opened {
                let named: Vec<String> =
                    handles.iter().map(|handle| format!("{handle:x}")).collect();
                answer.push_str(&format!("{id} {}\n", named.join(" ")));
            }
            answer
        }
        Err(error) => format!("error {error:#}"),
    };
    std::fs::write(reply, answer)
        .with_context(|| format!("writing the answer to {}", reply.display()))?;
    outcome.map(|_| ())
}

fn broker_open(
    asked: &[(String, bool)],
    parent_process_id: u32,
) -> Result<Vec<(String, Vec<usize>)>> {
    // Every disk is opened before any of them is handed over, so a request
    // that cannot be met in full is refused having taken nothing: the volumes
    // go back to the host as these handles drop on the way out.
    let mut opened = Vec::new();
    for (id, write) in asked {
        // Re-applied here rather than inherited: this half runs as
        // Administrator, so it is the last place that can refuse the disk the
        // host is running from, and it must not depend on the unprivileged
        // half having asked nicely. It asks the same question that half does
        // rather than a hand-copied likeness of it -- a rule added there has
        // to reach the half that can actually damage the medium.
        let device = super::find_device(id)?
            .ok_or_else(|| anyhow::anyhow!("no host disk called {id} is attached"))?;
        super::refuse_if_unusable(&device, *write)?;
        let disk = open_disk_node(&device, *write)
            .map_err(|error| explain_open_failure(&error, &device))?;
        let number = disk_number_of(&device.id)
            .with_context(|| format!("{} is not a physical disk name", device.id))?;
        let volumes = take_volumes(number, &device.id)?;
        opened.push((device.id.clone(), disk, volumes));
    }

    let parent = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, parent_process_id) };
    if parent.is_null() {
        return Err(std::io::Error::last_os_error())
            .context("reaching the Copperline that asked for these disks");
    }
    let mut handed: Vec<(String, Vec<usize>)> = Vec::new();
    let mut failed = None;
    'disks: for (id, disk, volumes) in &opened {
        let mut mine = Vec::new();
        for source in std::iter::once(disk.as_raw_handle()).chain(
            volumes
                .iter()
                .map(std::os::windows::io::AsRawHandle::as_raw_handle),
        ) {
            match duplicate_into(parent, source) {
                Ok(handle) => mine.push(handle),
                Err(error) => {
                    handed.push((id.clone(), mine));
                    failed = Some(error);
                    break 'disks;
                }
            }
        }
        handed.push((id.clone(), mine));
    }
    if let Some(error) = failed {
        // A half-finished handover must leave nothing behind. The asking
        // process is about to be told this failed, so it will never close what
        // it was already given -- and a volume it holds without knowing stays
        // dismounted from the host for as long as it runs.
        for (_, handles) in handed {
            for handle in handles {
                close_in(parent, handle);
            }
        }
        unsafe { CloseHandle(parent) };
        return Err(error);
    }
    unsafe { CloseHandle(parent) };
    // This process's own handles close as it returns. The file objects behind
    // them -- and so the volume locks -- stay alive on the copies now in the
    // asking process, which is the whole point.
    Ok(handed)
}

/// What a reservation keeps on Windows: the open disk, and the volume locks
/// that must live exactly as long as it.
///
/// Dropping it closes every handle, which is what hands the disk back.
pub(super) struct Held {
    disk: OwnedHandle,
    dismounted: Vec<std::fs::File>,
}

/// Whether taking a disk will need the host to raise a consent dialog.
///
/// Raw disk access needs Administrator whatever the medium, so an
/// unelevated process always ends up at the `runas` consent prompt; one
/// already elevated opens directly. Read from this process's token.
pub(super) fn taking_needs_privilege() -> bool {
    let mut token: Handle = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        // Not knowing reads as needing it: the warning this feeds errs
        // toward telling somebody a dialog may appear.
        return true;
    }
    let mut elevation: u32 = 0;
    let mut returned: u32 = 0;
    let asked = unsafe {
        GetTokenInformation(
            token,
            TOKEN_ELEVATION_CLASS,
            (&raw mut elevation).cast(),
            std::mem::size_of::<u32>() as u32,
            &mut returned,
        )
    };
    unsafe { CloseHandle(token) };
    asked == 0 || elevation == 0
}

/// Take a disk from the host: directly if this process may, and otherwise by
/// asking for consent.
///
/// The disk is opened before anything is taken from the host, so an open that
/// is refused does not leave somebody's volumes dismounted for a disk the
/// machine never got. The host then lets go whether or not the machine means
/// to write, because exclusive use is the point of attaching a disk at all: a
/// volume left mounted is Windows still writing its own metadata to the
/// medium, under a guest that cannot account for it changing. A write
/// additionally *needs* this -- since Vista the filesystem owns the sectors of
/// a mounted volume and refuses one.
pub(super) fn take_disks(wanted: &[(HostDevice, bool)]) -> Result<Vec<(String, Held)>> {
    let mut taken = Vec::new();
    let mut ask = Vec::new();
    for (device, write) in wanted {
        match open_disk_node(device, *write) {
            Ok(disk) => {
                let number = disk_number_of(&device.id)
                    .with_context(|| format!("{} is not a physical disk name", device.id))?;
                let dismounted = take_volumes(number, &device.id)?;
                taken.push((device.id.clone(), Held { disk, dismounted }));
            }
            // Gathered rather than asked for one at a time, so however many
            // disks need Administrator they cost one dialog between them.
            Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                ask.push((device.clone(), *write));
            }
            Err(error) => return Err(explain_open_failure(&error, device)),
        }
    }
    if !ask.is_empty() {
        taken.extend(open_through_broker(&ask)?);
    }
    Ok(taken)
}

/// Copy an open handle within this process, so two owners can hold the one
/// open thing. Both name the same file object, so the volume lock behind it
/// lasts until the last of them closes.
fn duplicate_here(source: RawHandle) -> Result<OwnedHandle> {
    let mut copy: Handle = ptr::null_mut();
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            source as Handle,
            GetCurrentProcess(),
            &mut copy,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).context("copying an open disk");
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(copy as RawHandle) })
}

/// Give a held disk to a machine.
///
/// The machine gets handles of its own -- the disk, and the volume locks
/// that must outlive any mounting Windows might otherwise do -- so the disk
/// stays taken from the host across the machine being stopped and started.
pub(super) fn lend(device: &HostDevice, write: bool, held: &Held) -> Result<super::BlockDevice> {
    let disk = duplicate_here(held.disk.as_raw_handle())?;
    let dismounted = held
        .dismounted
        .iter()
        .map(|volume| duplicate_here(volume.as_raw_handle()).map(std::fs::File::from))
        .collect::<Result<Vec<_>>>()
        .context("lending a dismounted volume to the machine")?;

    // Now that there is a handle opened for reading, the disk can be asked
    // its length outright; the listing had to infer it from the geometry, and
    // a disk that reads as longer than it is would fail at the very end.
    let size_bytes = exact_capacity(&disk).unwrap_or(device.size_bytes);
    if size_bytes != device.size_bytes {
        log::debug!(
            "blockdev: {} is {size_bytes} bytes, not the {} the listing reported",
            device.id,
            device.size_bytes
        );
    }

    Ok(super::BlockDevice::new(
        std::fs::File::from(disk),
        device.id.clone(),
        device.block_size,
        size_bytes,
        write,
    )
    .holding(dismounted))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The identifier is what a configuration file names, and the disk number
    /// has to come back out of it to reach that disk's volumes.
    #[test]
    fn an_identifier_names_a_disk_number() {
        assert_eq!(disk_number_of("PhysicalDrive0"), Some(0));
        assert_eq!(disk_number_of("PhysicalDrive12"), Some(12));
        assert_eq!(disk_number_of("disk4"), None);
        assert_eq!(disk_number_of("PhysicalDrive"), None);
        assert_eq!(disk_number_of("PhysicalDriveX"), None);
    }

    /// A volume is opened by a path that must not end in a separator, or the
    /// open names the volume's root directory instead and the FSCTLs have
    /// nothing to lock.
    #[test]
    fn a_volume_is_opened_without_its_trailing_separator() {
        assert_eq!(
            volume_node_path(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\"),
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}"
        );
        // Already trimmed, and left alone.
        assert_eq!(volume_node_path(r"\\.\E:"), r"\\.\E:");
    }

    /// The buses an Amiga disk actually arrives on, against the ones the host
    /// keeps its own storage on.
    #[test]
    fn a_card_reader_is_not_internal_storage() {
        assert!(!bus_is_internal(BUS_TYPE_USB));
        assert!(!bus_is_internal(BUS_TYPE_SD));
        assert!(!bus_is_internal(BUS_TYPE_MMC));
        assert!(!bus_is_internal(BUS_TYPE_1394));
        // SATA, NVMe, SAS, RAID and an unknown bus: the host's own.
        for bus in [0x00, 0x01, 0x03, 0x08, 0x0A, 0x0B, 0x11] {
            assert!(
                bus_is_internal(bus),
                "bus {bus:#x} should count as internal"
            );
        }
    }

    /// The disk the host runs from is the one refusal, and it is the only
    /// one: a drive on a SATA port is as usable as one in a card reader.
    #[test]
    fn only_the_running_system_s_disk_is_refused() {
        assert_eq!(classify(true), Safety::SystemDisk);
        assert_eq!(classify(false), Safety::Offerable);
    }

    /// Both halves of a name are used, because devices disagree about which
    /// one carries anything worth reading.
    #[test]
    fn a_model_joins_what_the_device_reported() {
        assert_eq!(
            model_of(Some("Generic".into()), Some("MassStorageClass".into())),
            Some("Generic MassStorageClass".into())
        );
        assert_eq!(
            model_of(None, Some("Samsung SSD 990".into())),
            Some("Samsung SSD 990".into())
        );
        assert_eq!(
            model_of(Some("SanDisk".into()), None),
            Some("SanDisk".into())
        );
        assert_eq!(model_of(None, None), None);
        assert_eq!(model_of(Some(String::new()), None), None);
    }

    /// The descriptor's strings are named by byte offsets into the buffer the
    /// kernel filled, and an offset of zero means the device said nothing.
    #[test]
    fn descriptor_strings_are_read_at_their_offsets() {
        let mut buffer = vec![0u8; 64];
        buffer[40..47].copy_from_slice(b"SanDisk");
        buffer[48..54].copy_from_slice(b"Cruzer");
        assert_eq!(descriptor_string(&buffer, 40), Some("SanDisk".into()));
        assert_eq!(descriptor_string(&buffer, 48), Some("Cruzer".into()));
        assert_eq!(descriptor_string(&buffer, 0), None);
        // Past the end of what came back, rather than into somebody else's
        // memory.
        assert_eq!(descriptor_string(&buffer, 4096), None);
    }

    /// Both the configuration manager and the volume calls answer with
    /// strings run together and the list ended by an empty one, so a reader
    /// that stopped at the first NUL would see one disk on a machine with
    /// several.
    #[test]
    fn a_multi_string_is_read_to_its_empty_terminator() {
        let buffer: Vec<u16> = "\\\\?\\a\0\\\\?\\b\0\0".encode_utf16().collect();
        assert_eq!(multi_string(&buffer), vec![r"\\?\a", r"\\?\b"]);
        // Trailing slack in an over-sized buffer is not a third entry.
        let mut padded = buffer.clone();
        padded.extend(std::iter::repeat_n(0u16, 16));
        assert_eq!(multi_string(&padded), vec![r"\\?\a", r"\\?\b"]);
        assert!(multi_string(&[0u16, 0]).is_empty());
        assert!(multi_string(&[]).is_empty());
    }

    /// The answer crosses a privilege boundary in a file, so nothing about it
    /// is assumed: only the shape the privileged half writes turns into
    /// handles, and everything else is a refusal rather than a guess at what
    /// was meant.
    #[test]
    fn only_a_well_formed_answer_becomes_handles() {
        // One disk, and its volumes after it.
        assert_eq!(
            parse_answer("ok\nPhysicalDrive1 1a4 2b8\n").unwrap(),
            vec![("PhysicalDrive1".to_string(), vec![0x1a4, 0x2b8])]
        );
        // Several disks from the one consent, each on its own line.
        assert_eq!(
            parse_answer("ok\nPhysicalDrive1 40\nPhysicalDrive3 50 60\n").unwrap(),
            vec![
                ("PhysicalDrive1".to_string(), vec![0x40]),
                ("PhysicalDrive3".to_string(), vec![0x50, 0x60]),
            ]
        );

        // A refusal it explained reaches the user in its own words.
        let refused = parse_answer("error PhysicalDrive0 is the disk this computer runs from")
            .expect_err("a refusal is not a success");
        assert!(refused.to_string().contains("runs from"), "{refused}");

        for nonsense in [
            "",
            "ok",
            "ok\n",
            "okay\nPhysicalDrive1 1a4",
            "ok\nPhysicalDrive1 zz",
            // A disk named with nothing open is not something to act on: the
            // first handle is always the disk itself.
            "ok\nPhysicalDrive1",
            "PhysicalDrive1 1a4",
        ] {
            assert!(
                parse_answer(nonsense).is_err(),
                "{nonsense:?} must not become a handle"
            );
        }
    }

    /// Enumeration must work unprivileged and with nothing plugged in: it is
    /// reached from config validation and from the launcher, on machines that
    /// have never seen an Amiga disk.
    #[test]
    fn enumeration_is_safe_with_nothing_attached() {
        let devices = list_devices().expect("enumeration works unprivileged");
        // Every host has at least the disk it booted from.
        assert!(
            devices.iter().any(|d| d.safety == Safety::SystemDisk),
            "the disk Windows is running from must be identified: {devices:#?}"
        );
        for device in &devices {
            assert!(device.id.starts_with("PhysicalDrive"));
            assert!(disk_number_of(&device.id).is_some());
            assert!(device.size_bytes > 0);
            assert!(device.block_size >= 512);
            assert_eq!(device.path, PathBuf::from(format!(r"\\.\{}", device.id)));
        }
    }
}
