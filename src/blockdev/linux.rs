// SPDX-License-Identifier: GPL-3.0-or-later

//! Linux block devices.
//!
//! Not written yet. This file exists so the platform seam in
//! [`super`] is complete and the work has an obvious home; until it is
//! filled in, Linux lists no disks and says so when asked to open one.
//!
//! # What this has to provide
//!
//! Two functions, matching the macOS backend next door:
//!
//! - `list_devices() -> Result<Vec<HostDevice>>` -- every whole physical
//!   disk, gathered **without opening anything**, each carrying its
//!   [`Safety`](super::Safety).
//! - `open_device(&HostDevice, write: bool) -> Result<super::BlockDevice>` --
//!   take the disk from the host and hand back an open handle, built with
//!   `super::BlockDevice::new`.
//!
//! # How Linux answers each part
//!
//! **Enumeration.** All of it is in sysfs, so it costs nothing and needs no
//! privilege: read the directories under `/sys/block`, skipping the ones that
//! are not real media (`loop*`, `ram*`, `zram*`, `dm-*`, `md*`, and `sr*`,
//! which is optical).
//!
//! - `size` is the capacity **always in 512-byte units**, whatever the media's
//!   own block size is. This trips people up: multiply by 512, never by
//!   `logical_block_size`.
//! - `queue/logical_block_size` is this module's `block_size`.
//! - `removable` and `ro` are `0`/`1`.
//! - `device/model` and `device/vendor` name the hardware; NVMe carries only
//!   `device/model`.
//! - "Internal" is best read from the bus: resolve the `device` symlink and
//!   look for `/usb` in the path. A card reader is on USB; the host's own
//!   storage generally is not.
//!
//! **Mounted volumes, and which disk is the host's.** `/proc/self/mountinfo`
//! gives every mount and its source device. A partition maps to its whole disk
//! by name (`sdb1` to `sdb`, `nvme0n1p3` to `nvme0n1`, `mmcblk0p1` to
//! `mmcblk0`) -- note the `p` before the partition number on the last two.
//! Whatever serves `/` is [`Safety::SystemDisk`](super::Safety::SystemDisk)
//! and must never be offered or opened; see the module docs in [`super`] for
//! why that rule is not negotiable. Be careful with device-mapper and LVM: `/`
//! may be served by `/dev/mapper/...`, whose physical disk is reached through
//! `/sys/block/dm-N/slaves/`. A root on LVM whose disk is not identified would
//! leave that disk offerable, which is the one outcome this must not have.
//!
//! **Taking the disk.** `O_EXCL` on a block device is not advisory: the kernel
//! refuses it while any filesystem on that disk is mounted, and refuses a
//! later mount while it is held. That plus `flock(LOCK_EX)` is the whole
//! interlock -- there is no equivalent of macOS's separate unmount step, but a
//! disk the host has mounted must still be unmounted first or the open fails
//! with `EBUSY`. Prefer asking the desktop's own machinery over calling
//! `umount(8)`: `udisksctl unmount -b /dev/sdb1` gives the user the polkit
//! prompt they already know from their file manager.
//!
//! **Privilege.** `/dev/sdX` is `root:disk 0660`, so an ordinary user cannot
//! open it. Try the direct open first regardless -- a user in the `disk`
//! group, a udev rule granting the seat's user access to removable media, or
//! simply running as root all make it work, and prompting when nothing is in
//! the way would be rude. On `EACCES`/`EPERM`, escalate.
//!
//! The escalation to use is a privileged open that hands back the descriptor,
//! the same shape as macOS's `authopen`:
//!
//! - **udisks2** (`org.freedesktop.UDisks2.Block.OpenDevice`) returns a file
//!   descriptor over D-Bus behind a polkit check -- the desktop-native answer,
//!   already running on every desktop distribution, and it needs nothing
//!   installed. It does mean speaking D-Bus, including `UNIX_FD` passing.
//! - **`pkexec`** running this same binary in a small privileged mode, which
//!   opens the device and sends the descriptor back over a unix socket the
//!   parent named on the command line. No new dependency, no install step, and
//!   one prompt can cover both the unmount and the open. `src/net/bridge/linux.rs`
//!   already does `SCM_RIGHTS` receiving and is the closest thing in the tree
//!   to a worked example.
//!
//! Whichever is chosen, the descriptor must be opened with `O_EXCL` by the
//! privileged side, and `FD_CLOEXEC` set on arrival.
//!
//! **The open itself.** `O_RDWR | O_EXCL` when writing, `O_RDONLY` when not.
//! Turn the descriptor into a `std::fs::File` with `File::from_raw_fd`; the
//! positioned reads and writes [`super::BlockDevice`] does are already
//! implemented for unix there, and its block-size translation already handles
//! media whose blocks are larger than a guest sector.

use anyhow::Result;

use super::{BlockDevice, HostDevice};

/// Every whole physical disk the host can see.
pub fn list_devices() -> Result<Vec<HostDevice>> {
    Ok(Vec::new())
}

/// Take a disk from the host and open it for the emulated machine.
pub fn open_device(_device: &HostDevice, _write: bool) -> Result<BlockDevice> {
    anyhow::bail!("physical disks are not supported on Linux yet")
}
