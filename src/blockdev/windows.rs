// SPDX-License-Identifier: GPL-3.0-or-later

//! Windows block devices.
//!
//! Not written yet. This file exists so the platform seam in
//! [`super`] is complete and the work has an obvious home; until it is
//! filled in, Windows lists no disks and says so when asked to open one.
//!
//! # What this has to provide
//!
//! Two functions, matching the macOS backend next door:
//!
//! - `list_devices() -> Result<Vec<HostDevice>>` -- every whole physical
//!   disk, gathered **without opening anything for I/O**, each carrying its
//!   [`Safety`](super::Safety).
//! - `open_device(&HostDevice, write: bool) -> Result<super::BlockDevice>` --
//!   take the disk from the host and hand back an open handle, built with
//!   `super::BlockDevice::new`.
//!
//! # How Windows answers each part
//!
//! **Enumeration without privilege.** `CreateFileW(r"\\.\PhysicalDriveN")`
//! with `dwDesiredAccess = 0` succeeds for an ordinary user: no access means
//! no access check, and the handle still serves the descriptive IOCTLs. Walk
//! `PhysicalDrive0..=31` and keep the ones that open.
//!
//! - `IOCTL_STORAGE_QUERY_PROPERTY` / `StorageDeviceProperty` fills a
//!   `STORAGE_DEVICE_DESCRIPTOR`: vendor and product strings (byte offsets
//!   into the same buffer), `RemovableMedia`, and `BusType`.
//! - `IOCTL_STORAGE_QUERY_PROPERTY` / `StorageAccessAlignmentProperty` gives
//!   `BytesPerLogicalSector`, which is this module's `block_size`.
//! - `IOCTL_DISK_GET_LENGTH_INFO` gives the capacity in bytes.
//! - `BusType` is what "internal" means here: `BusTypeUsb`, `BusTypeSd` and
//!   `BusTypeMmc` are the ways an Amiga disk actually arrives, and
//!   `BusTypeNvme`/`BusTypeSata`/`BusTypeRAID` are the host's own storage.
//!
//! **Which disk is the host's.** `GetWindowsDirectoryW` names a path;
//! `GetVolumePathNameW` reduces it to a volume root;
//! `GetVolumeNameForVolumeMountPointW` turns that into a volume GUID path;
//! opening that volume with zero access and issuing
//! `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` names the `DiskNumber` behind it.
//! That disk is [`Safety::SystemDisk`](super::Safety::SystemDisk) and must
//! never be offered or opened -- see the module docs in [`super`] for why
//! this rule is not negotiable.
//!
//! **Mounted volumes.** `FindFirstVolumeW`/`FindNextVolumeW` walks every
//! volume; each one's disk extents say which disk it sits on, and
//! `GetVolumePathNamesForVolumeNameW` gives the drive letters or mount points
//! to show in `HostDevice::mounted`.
//!
//! **Taking the disk.** An Amiga RDB disk carries no filesystem Windows
//! recognises, so usually nothing is mounted and there is nothing to take.
//! When something is: open each volume on the disk with write access,
//! `FSCTL_LOCK_VOLUME`, then `FSCTL_DISMOUNT_VOLUME`, and **keep those volume
//! handles open** -- the lock lasts only as long as the handle, and without it
//! Windows may remount underneath the emulator. Since Vista, a write to a
//! sector belonging to a mounted volume is refused unless the volume is
//! locked, so this is what makes writing work at all.
//!
//! **Privilege.** Windows has no broker that hands back an opened handle, so
//! there is nothing here like macOS's `authopen`. What there is: a disk whose
//! `RemovableMedia` bit is set grants the interactive user full access, which
//! covers the case this feature exists for -- an SD or CF card in a reader.
//! A fixed disk needs Administrator, and a process cannot elevate itself;
//! relaunching elevated would throw away the session. So: try the open, and on
//! `ERROR_ACCESS_DENIED` return an error saying plainly that this disk needs
//! Copperline started as Administrator. Do not silently relaunch.
//!
//! **The open itself.** `CreateFileW(path, GENERIC_READ | GENERIC_WRITE,
//! FILE_SHARE_READ | FILE_SHARE_WRITE, .., OPEN_EXISTING, 0, ..)`, or
//! `GENERIC_READ` alone when `write` is false. Turn the `HANDLE` into a
//! `std::fs::File` with `File::from_raw_handle`; the positioned reads and
//! writes [`super::BlockDevice`] does are already implemented for Windows
//! there, and its block-size translation already handles the requirement that
//! every transfer be a whole number of sectors at a sector-aligned offset.

use anyhow::Result;

use super::{BlockDevice, HostDevice};

/// Every whole physical disk the host can see.
pub fn list_devices() -> Result<Vec<HostDevice>> {
    Ok(Vec::new())
}

/// Take a disk from the host and open it for the emulated machine.
pub fn open_device(_device: &HostDevice, _write: bool) -> Result<BlockDevice> {
    anyhow::bail!("physical disks are not supported on Windows yet")
}
