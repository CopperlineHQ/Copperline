// SPDX-License-Identifier: GPL-3.0-or-later

//! Linux block devices, through sysfs and a privileged opener.
//!
//! Linux keeps a description of every block device in sysfs, so enumeration is
//! a directory walk over files the kernel has already written: no device is
//! opened, nothing spins up, and no privilege is involved. That makes this the
//! cheapest of the three backends to list from, and it moves all the
//! difficulty to the two questions sysfs does not answer directly -- which
//! disk the host is running from, and how to get a descriptor to a node an
//! ordinary user cannot open.
//!
//! # What was measured, and where
//!
//! Ubuntu 24.04 (GNOME 46, Wayland, udisks2 2.10.1), on an ordinary desktop
//! user account in an active local session, August 2026:
//!
//! ```text
//! $ ls -l /dev/sdb                 brw-rw---- 1 root disk 8, 16 /dev/sdb
//! $ getfacl /dev/sdb               user::rw-  group::rw-  other::---
//! $ open("/dev/sdb", O_RDONLY)     EACCES
//! $ open("/dev/sdb", O_RDWR|O_EXCL) EACCES
//! ```
//!
//! **No udev rule grants the seat's user access to removable media**, and the
//! card reader's node is exactly as closed as the system disk's -- the user is
//! not in `disk`, and there is no ACL on the node. The escalation is therefore
//! the ordinary path on a desktop, not a fallback for unusual setups, and the
//! direct open below is what covers a user in `disk`, a site udev rule, or a
//! run as root.
//!
//! Both escalations the brief proposed were measured before choosing:
//!
//! - **udisks2** does work. `org.freedesktop.UDisks2.Block.OpenDevice` is
//!   present in 2.10.1, its `options` argument takes extra open flags (it
//!   rejects only the access-mode ones: *"Using 'O_RDWR', 'O_RDONLY' and
//!   'O_WRONLY' flags is not permitted. Use 'mode' argument instead"*), so
//!   `O_EXCL` is reachable through it, and the polkit prompt appears.
//! - Its action `org.freedesktop.udisks2.open-device` is `auth_admin_keep`
//!   **even for an active local session**, so it costs an administrator
//!   password exactly as `pkexec` does. It buys no friction back.
//!
//! So udisks2 would have meant hand-rolling a D-Bus client -- the SASL
//! handshake, the type-aligned marshalling, `NEGOTIATE_UNIX_FD` and
//! `SCM_RIGHTS` over the bus socket -- or a new dependency, to arrive at the
//! same prompt. `pkexec` reaches it with the shape this tree already has
//! written twice: the Windows backend is the same privileged-half-of-the-same-
//! binary design, and `src/net/bridge/linux.rs` already passes descriptors
//! both ways. udisks2 is still used for the one thing it is unambiguously
//! best at -- the unmount -- because `filesystem-mount` *is* `yes` for an
//! active session, so unmounting the user's own removable disk costs no
//! prompt at all.
//!
//! # Enumeration
//!
//! `/sys/block` has one entry per whole device. Skipped: `loop*`, `ram*`,
//! `zram*`, `dm-*`, `md*` (none of which are media anybody can hand to an
//! Amiga) and `sr*` (optical). Also skipped are devices reporting zero
//! capacity, which is what an empty card-reader slot looks like, and NVMe
//! namespaces marked `hidden`.
//!
//! - `size` is the capacity **always in 512-byte units**, whatever
//!   `queue/logical_block_size` says. Measured against a 64 MB `scsi_debug`
//!   disk given `sector_size=4096`: it reports `logical_block_size = 4096` and
//!   `size = 131072`, and 131072 x 512 is the 64 MB it is. Multiplying by the
//!   logical block size instead would call it 512 MB -- and a disk wrong by a
//!   factor of eight still enumerates, still opens, and fails only at the far
//!   end of the medium.
//! - `device/vendor` and `device/model` come from the SCSI INQUIRY and are
//!   fixed-width fields, so they arrive padded and are frequently split
//!   oddly across the two: the reader measured here answers `Generic` and
//!   `MassStorageClass`, so they are joined rather than picked between.
//!
//! # Which disk the host is running from
//!
//! This is the dangerous part on Linux, and it is not solved by matching
//! names. `/proc/self/mountinfo` gives the root mount's `major:minor`
//! directly, and `/sys/dev/block/<major>:<minor>` is a symlink to that exact
//! device's directory -- so the starting point needs no parsing at all. From
//! there:
//!
//! - a partition's directory sits inside its whole disk's, so the parent
//!   directory names the disk (`.../block/sdb/sdb1` gives `sdb`);
//! - a device-mapper or MD device has a non-empty `slaves/`, and each entry is
//!   another device to resolve the same way, recursively, because a mapper
//!   device may sit on another mapper device (LUKS over LVM) and because one
//!   logical volume may span several physical disks -- in which case **every**
//!   disk underneath it is the system's.
//!
//! A layout whose root cannot be traced to any whole disk is not guessed at:
//! every disk is classified [`Safety::SystemDisk`](super::Safety::SystemDisk)
//! and the reason is logged. Offering nothing is recoverable and visible;
//! offering the disk the host is running from is neither.
//!
//! # Taking the disk
//!
//! `O_EXCL` on a block device is not advisory. The kernel refuses it while any
//! filesystem on that disk is mounted, and refuses a later mount while it is
//! held, which is the whole interlock -- and it lives exactly as long as the
//! open file description, so it needs nothing to hold it and is dropped by the
//! same close that hands the disk back. It is taken on read-only attachments
//! too: exclusive use is the point of attaching a real disk, and a volume left
//! mounted is the host writing its own metadata to the medium underneath a
//! guest that cannot account for it changing.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::broker_protocol::{self, BROKER_FLAG};
use super::{HostDevice, Safety};

/// Where the kernel's own answers are read from.
///
/// Indirected so a test can point it at a tree it built. The layouts that
/// matter most here -- a root on LVM, a logical volume spanning two disks --
/// are ones no single development machine has, and they are exactly the ones
/// that must not be got wrong.
struct Kernel {
    sys: PathBuf,
    mountinfo: PathBuf,
}

impl Kernel {
    fn live() -> Self {
        Self {
            sys: PathBuf::from("/sys"),
            mountinfo: PathBuf::from("/proc/self/mountinfo"),
        }
    }

    /// A block device's own directory, whether it is a whole disk or a
    /// partition: `/sys/class/block` carries both, while `/sys/block` has only
    /// whole disks.
    fn block(&self, name: &str) -> PathBuf {
        self.sys.join("class/block").join(name)
    }

    fn attribute(&self, name: &str, attribute: &str) -> Option<String> {
        let text = std::fs::read_to_string(self.block(name).join(attribute)).ok()?;
        let text = text.trim().to_string();
        (!text.is_empty()).then_some(text)
    }

    fn number(&self, name: &str, attribute: &str) -> Option<u64> {
        self.attribute(name, attribute)?.parse().ok()
    }

    fn flag(&self, name: &str, attribute: &str) -> Option<bool> {
        Some(self.number(name, attribute)? != 0)
    }
}

/// How deep a stack of mapper devices is followed before it is called a loop.
///
/// LUKS over LVM over MD is three, and each layer is a deliberate act of
/// administration; a tower taller than this is likelier to be a cycle in a
/// tree being read while it changes than a real arrangement.
const MAX_MAPPER_DEPTH: usize = 16;

/// Devices in `/sys/block` that are not media anyone can give to an Amiga.
fn is_real_media(name: &str) -> bool {
    const NOT_MEDIA: [&str; 6] = ["loop", "ram", "zram", "dm-", "md", "sr"];
    // `md` must not swallow `mmcblk`, and `ram` must not swallow a future
    // device whose name merely starts the same way, so a prefix match is only
    // trusted where what follows it is the digit these names all continue with.
    !NOT_MEDIA.iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    })
}

/// The whole disks a device is ultimately made of.
///
/// A partition resolves to the disk it is cut from, a mapper or MD device to
/// everything underneath it, and a plain disk to itself. The answer is a set
/// because one logical volume can span several physical disks, and then all of
/// them carry the filesystem.
fn whole_disks_of(kernel: &Kernel, name: &str, depth: usize) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    if depth > MAX_MAPPER_DEPTH {
        log::warn!("blockdev: gave up tracing {name} to a physical disk after {depth} layers");
        return found;
    }

    // A mapper or MD device names what it is built from here. An ordinary disk
    // has the directory too, but empty, which is what tells the two apart.
    let slaves = kernel.block(name).join("slaves");
    if let Ok(entries) = std::fs::read_dir(&slaves) {
        let mut any = false;
        for entry in entries.flatten() {
            if let Some(slave) = entry.file_name().to_str() {
                any = true;
                found.extend(whole_disks_of(kernel, slave, depth + 1));
            }
        }
        if any {
            return found;
        }
    }

    // Otherwise the kernel says which it is outright: only a partition carries
    // a `partition` attribute, and a partition's directory lives inside its
    // whole disk's, so the parent names the disk.
    //
    // Deliberately not decided by the parent directory being called `block`.
    // That glue directory is inserted only where the parent device is not
    // itself a class device, so SCSI and ATA disks have it (`.../0:0:0:0/
    // block/sda`) and an NVMe namespace does not (`.../nvme/nvme0/nvme0n1`,
    // hanging straight off its controller). Reading the name would trace every
    // NVMe root to its controller and then to nothing.
    let Ok(path) = std::fs::canonicalize(kernel.block(name)) else {
        return found;
    };
    if kernel.block(name).join("partition").exists() {
        if let Some(disk) = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|p| p.to_str())
        {
            found.extend(whole_disks_of(kernel, disk, depth + 1));
        }
        return found;
    }

    // The end of the line has to be something with a medium in it: a whole
    // disk, which is what `/sys/block` holds and nothing else. A mapper or MD
    // device that named nothing underneath it is a trace that failed, not a
    // disk -- answering `dm-0` here would leave the physical disk it is built
    // from looking like anybody's to take. Nothing is said about it here: a
    // desktop traces a loop device per installed snap, and only the root's own
    // trace failing is worth a word, which [`enumerate`] says once.
    if is_real_media(name) && kernel.sys.join("block").join(name).exists() {
        found.insert(name.to_string());
    }
    found
}

/// The device `major:minor` names, as sysfs spells it.
fn device_named(kernel: &Kernel, major_minor: &str) -> Option<String> {
    let path = std::fs::canonicalize(kernel.sys.join("dev/block").join(major_minor)).ok()?;
    Some(path.file_name()?.to_str()?.to_string())
}

/// One filesystem the host has mounted, already traced to its disks.
struct Mount {
    /// The whole disks the filesystem ultimately sits on.
    disks: BTreeSet<String>,
    /// Where it is mounted, as somebody would recognise it.
    point: String,
    /// The device node underneath, which is what an unmount has to name.
    source: Option<String>,
}

/// Undo the octal escapes `mountinfo` writes for the characters that would
/// otherwise end a field. A mount point containing a space is unusual but
/// entirely legal, and one silently cut in half is worse than one not shown.
fn unescape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut rest = field;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        let escape = rest.get(at + 1..at + 4).and_then(|digits| {
            u8::from_str_radix(digits, 8)
                .ok()
                .filter(|_| digits.bytes().all(|b| b.is_ascii_digit()))
        });
        match escape {
            Some(byte) => {
                out.push(byte as char);
                rest = &rest[at + 4..];
            }
            None => {
                out.push('\\');
                rest = &rest[at + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Every mount the host has, each traced to the disks underneath it.
///
/// `mountinfo`'s third field is the device's `major:minor`, which is an exact
/// handle on the device -- no name to parse and no ambiguity about which of
/// several similarly named things is meant.
fn mounts(kernel: &Kernel) -> Vec<Mount> {
    // Read as bytes, not as text. The kernel escapes only the four characters
    // that would end a field, so a volume labelled in some other encoding
    // reaches here as raw bytes -- and one plugged-in stick making the whole
    // file unreadable would take the system disk's identity down with it,
    // which is how a machine ends up refusing every disk it has.
    let text = match std::fs::read(&kernel.mountinfo) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => {
            log::warn!(
                "blockdev: {} could not be read ({error}), so no mount is known",
                kernel.mountinfo.display()
            );
            return Vec::new();
        }
    };
    // Tracing a device is several `readlink`s, and a desktop has dozens of
    // mounts sharing a handful of devices between them.
    let mut traced: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut found = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(major_minor) = fields.nth(2) else {
            continue;
        };
        let Some(point) = fields.nth(1) else {
            continue;
        };
        let source = device_named(kernel, major_minor);
        let disks = traced
            .entry(major_minor.to_string())
            .or_insert_with(|| match &source {
                Some(name) => whole_disks_of(kernel, name, 0),
                // Major zero is a filesystem with no block device under it at
                // all -- a container's overlay, tmpfs, an NFS root -- and
                // there is nothing to trace.
                None => BTreeSet::new(),
            })
            .clone();
        found.push(Mount {
            disks,
            point: unescape(point),
            source,
        });
    }
    found
}

/// Mount points the host cannot be running without.
///
/// `/` is the obvious one and on most machines the only one that matters, since
/// the rest are usually partitions of the same disk. It is not enough on its
/// own, though: a root on ZFS, and a live session's overlay root, are served by
/// no block device at all -- `mountinfo` gives them an anonymous `0:NN` -- and
/// asking only about `/` on those would conclude that no disk here is the
/// host's and offer it the very medium it booted from. The others are where
/// the physical disk shows through on exactly those layouts: an EFI system
/// partition is a real partition even under ZFS, and a live USB stick is
/// mounted whole at one of the media points.
const ESSENTIAL_MOUNTS: [&str; 9] = [
    "/",
    "/boot",
    "/boot/efi",
    "/usr",
    "/var",
    // Where the installers of the major distributions mount the medium a live
    // session is running from.
    "/cdrom",
    "/isodevice",
    "/run/live/medium",
    "/lib/live/mount/medium",
];

/// What the disks the host is running from turned out to be.
enum Root {
    /// The disks it is served from. Never empty.
    Disks(BTreeSet<String>),
    /// Nothing the host cannot do without could be traced to a whole disk, so
    /// which disk is the host's is unknown and none of them can be safe.
    Untraceable,
}

/// Which disks the host is running from.
///
/// Every essential mount contributes, not just `/`: a volume group spanning
/// two disks puts the running system on both, and a layout whose root is not
/// block-backed is only caught by the others.
fn root_of(mounts: &[Mount]) -> Root {
    let mut disks = BTreeSet::new();
    let mut block_backed = false;
    for mount in mounts {
        if !ESSENTIAL_MOUNTS.contains(&mount.point.as_str()) {
            continue;
        }
        block_backed |= mount.source.is_some();
        disks.extend(mount.disks.iter().cloned());
    }
    if disks.is_empty() {
        // Said at the level that knows what it means for the caller.
        log::warn!(
            "blockdev: nothing the host runs from could be traced to a disk{}",
            if block_backed {
                ""
            } else {
                " (it is not on a block device: a container, a network root, or ZFS)"
            }
        );
        return Root::Untraceable;
    }
    Root::Disks(disks)
}

/// Whether a device hangs off a bus that media arrives on rather than one the
/// host's own storage is fixed to. Labels and sorts only -- every disk but
/// the system's is offered either way.
///
/// Read from the device's place in the tree: a card reader is reached through
/// a USB or MMC host controller, and a laptop's built-in SD slot -- which is
/// the likeliest way an Amiga's card is plugged in at all -- is on MMC while
/// reporting itself as not removable.
fn on_a_port(kernel: &Kernel, name: &str) -> bool {
    let Ok(path) = std::fs::canonicalize(kernel.block(name).join("device")) else {
        return false;
    };
    path.components().any(|component| {
        let part = component.as_os_str().to_string_lossy();
        part.starts_with("usb")
            || part.starts_with("mmc_host")
            || part.starts_with("fw-host")
            || part.starts_with("firewire")
    })
}

/// What to show for a disk: the vendor and the model as one name.
///
/// Both are fixed-width INQUIRY fields and devices split the useful half
/// between them inconsistently -- a USB reader answers "Generic" and
/// "MassStorageClass", an NVMe drive fills in only the model -- so they are
/// joined rather than chosen between.
fn model_of(kernel: &Kernel, name: &str) -> Option<String> {
    let vendor = kernel.attribute(name, "device/vendor");
    let model = kernel.attribute(name, "device/model");
    let joined = match (vendor, model) {
        (Some(vendor), Some(model)) => format!("{vendor} {model}"),
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => return None,
    };
    let joined = joined.trim().to_string();
    (!joined.is_empty()).then_some(joined)
}

/// Every whole physical disk the host can see.
pub fn list_devices() -> Result<Vec<HostDevice>> {
    enumerate(&Kernel::live())
}

fn enumerate(kernel: &Kernel) -> Result<Vec<HostDevice>> {
    let mounts = mounts(kernel);
    let root = root_of(&mounts);
    if matches!(root, Root::Untraceable) {
        log::warn!("blockdev: no disk will be offered, since one of them may be the host's own");
    }

    let directory = kernel.sys.join("block");
    let entries = std::fs::read_dir(&directory)
        .with_context(|| format!("reading {}", directory.display()))?;

    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !is_real_media(&id) {
            continue;
        }
        // An NVMe namespace the controller has masked is not media either, and
        // it has no node to open.
        if kernel.flag(&id, "hidden").unwrap_or(false) {
            continue;
        }
        // `size` counts 512-byte units whatever the medium's own block is.
        let size_bytes = kernel.number(&id, "size").unwrap_or(0) * 512;
        // No capacity is an empty slot in a card reader: the drive is there,
        // the medium is not.
        if size_bytes == 0 {
            continue;
        }

        let removable = kernel.flag(&id, "removable").unwrap_or(false);
        let internal = !removable && !on_a_port(kernel, &id);
        let mounted: Vec<String> = mounts
            .iter()
            .filter(|mount| mount.disks.contains(&id))
            .map(|mount| mount.point.clone())
            .collect();

        let safety = match &root {
            Root::Untraceable => Safety::SystemDisk,
            Root::Disks(disks) if disks.contains(&id) => Safety::SystemDisk,
            _ => Safety::Offerable,
        };

        devices.push(HostDevice {
            path: PathBuf::from("/dev").join(&id),
            model: model_of(kernel, &id),
            size_bytes,
            block_size: kernel
                .number(&id, "queue/logical_block_size")
                .unwrap_or(512) as u32,
            removable,
            internal,
            // The hardware's own answer: a locked SD card, or a bridge that
            // has decided the medium is read-only, reports it here.
            writable: !kernel.flag(&id, "ro").unwrap_or(false),
            mounted,
            safety,
            id,
        });
    }
    Ok(devices)
}

/// The `major:minor` sysfs gives a whole disk, as the kernel numbers it.
///
/// This is what a descriptor that came back from the privileged half is
/// checked against: a number in a message is a claim, and `fstat` is the only
/// thing that can confirm it names the disk that was asked for.
fn device_number(kernel: &Kernel, id: &str) -> Option<libc::dev_t> {
    let text = kernel.attribute(id, "dev")?;
    let (major, minor) = text.split_once(':')?;
    Some(libc::makedev(major.parse().ok()?, minor.parse().ok()?))
}

/// Give the host's volumes on a disk back before taking it.
///
/// `O_EXCL` on a block device is refused outright while any filesystem on that
/// disk is mounted, so this is not a courtesy -- the open below cannot succeed
/// until it has happened. It is done on read-only attachments too: the host
/// writing its own metadata to a medium underneath a guest that cannot account
/// for it changing is a hazard whichever way the emulator opened it.
///
/// `udisksctl` is asked rather than `umount(8)` because it is the same
/// machinery the file manager's eject button uses: unmounting the user's own
/// removable disk is `allow_active: yes` in the shipped policy and so costs no
/// prompt at all, and where a prompt is needed it is the one they already know.
fn unmount_volumes(kernel: &Kernel, id: &str) -> Result<()> {
    // One entry per node, not per mount: a filesystem can be mounted at
    // several points at once -- btrfs subvolumes and bind mounts both do it --
    // and unmounting the node once takes all of them.
    let nodes: BTreeSet<String> = mounts(kernel)
        .into_iter()
        .filter(|mount| mount.disks.contains(id))
        .filter_map(|mount| mount.source)
        .collect();
    if nodes.is_empty() {
        return Ok(());
    }
    for node in &nodes {
        let node = format!("/dev/{node}");
        let output = std::process::Command::new("udisksctl")
            .arg("unmount")
            .arg("-b")
            .arg(&node)
            .output();
        let output = match output {
            Ok(output) => output,
            // Not every Linux has udisks2 -- a minimal container, a server
            // install, a distribution that ships something else. The open is
            // still attempted rather than refused here: root can hold a disk
            // whose volumes it unmounted by hand, and where the mount really
            // is in the way the kernel says so itself, with `EBUSY`.
            Err(error) => {
                log::warn!(
                    "blockdev: {id} is mounted and udisksctl could not be run ({error}); \
                     unmount it by hand if taking the disk fails"
                );
                return Ok(());
            }
        };
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // Already unmounted is the state this was asked to reach, however it
        // got there -- another process may have done it a moment ago.
        if output.status.success() || said.contains("NotMounted") {
            continue;
        }
        anyhow::bail!("{node} could not be unmounted: {}", said.trim());
    }
    log::info!("blockdev: {id} is no longer mounted by the host");
    Ok(())
}

/// Open the device node directly, exclusively.
///
/// `O_EXCL` is what takes the disk: the kernel refuses a later mount of
/// anything on it while this is held, and hands it back when the last
/// descriptor sharing this open closes. `O_CLOEXEC` keeps raw media out of
/// anything this process goes on to run.
fn direct_open(path: &Path, write: bool) -> std::io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    let node = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
    let flags = libc::O_CLOEXEC | libc::O_EXCL | if write { libc::O_RDWR } else { libc::O_RDONLY };
    let fd = unsafe { libc::open(node.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Say what a refused open actually means, in terms of the disk rather than
/// the errno.
fn explain(error: &std::io::Error, device: &HostDevice) -> anyhow::Error {
    match error.raw_os_error() {
        // Careful not to blame the host for a claim this process may be
        // holding itself: the same disk named twice in a configuration, or
        // taken read-only and then asked for read-write, meets Copperline's
        // own `O_EXCL` here, and sending somebody hunting for a mount that
        // does not exist is worse than saying it might be either.
        Some(libc::EBUSY) => anyhow::anyhow!(
            "{} is already held exclusively -- by the host, or by another drive of this \
             machine already using it",
            device.id
        ),
        Some(libc::EROFS) => anyhow::anyhow!(
            "{} is write protected by the hardware; mount it read-only or unlock the media",
            device.id
        ),
        // Anything else keeps the kernel's own words: they are more specific
        // than a guess at what it meant.
        _ => anyhow::anyhow!("opening {}: {error}", device.path.display()),
    }
}

/// Whether taking a disk will need the host to raise a password prompt.
///
/// `/dev/sdX` is `root:disk 0660` with no seat ACL (see the module docs), so
/// short of running as root the open ends at polkit. A user in `disk` is the
/// rare exception this cannot see without a device to try; the warning this
/// feeds errs toward telling somebody a prompt may appear.
pub(super) fn taking_needs_privilege() -> bool {
    let euid = unsafe { libc::geteuid() };
    euid != 0
}

/// What a reservation keeps on Linux: the open descriptor itself.
///
/// Its `O_EXCL` is the claim -- the kernel refuses a mount while it is held
/// -- and it lives exactly as long as the open file description, so it needs
/// nothing else to hold it and is dropped by the same close that hands the
/// disk back.
pub(super) type Held = std::fs::File;

/// Take disks from the host, asking for privilege only where it is needed.
///
/// A user in `disk`, a site udev rule, or a run as root all get the direct
/// open without a prompt; a desktop user does not, and the denials are
/// gathered so however many disks need privilege they cost one prompt
/// between them.
pub(super) fn take_disks(wanted: &[(HostDevice, bool)]) -> Result<Vec<(String, Held)>> {
    let kernel = Kernel::live();
    let mut taken = Vec::new();
    let mut ask = Vec::new();
    for (device, write) in wanted {
        unmount_volumes(&kernel, &device.id)?;
        match direct_open(&device.path, *write) {
            Ok(file) => taken.push((device.id.clone(), file)),
            Err(error) if is_denial(&error) => ask.push((device.clone(), *write)),
            Err(error) => return Err(explain(&error, device)),
        }
    }
    if !ask.is_empty() {
        taken.extend(open_through_broker(&ask)?);
    }
    Ok(taken)
}

/// Give a held disk to a machine.
///
/// The copy and the reservation share one open file description, so the
/// `O_EXCL` claim lasts until the last of them closes -- and they share a
/// file offset, which is why everything [`super::BlockDevice`] does is
/// positioned rather than seeking.
pub(super) fn lend(device: &HostDevice, write: bool, held: &Held) -> Result<super::BlockDevice> {
    let file = held
        .try_clone()
        .context("lending the disk to the machine")?;
    Ok(super::BlockDevice::new(
        file,
        device.id.clone(),
        device.block_size,
        device.size_bytes,
        write,
    ))
}

fn is_denial(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EACCES) | Some(libc::EPERM))
}

/// Where the two halves meet: a socket in a directory only this user can enter.
///
/// What actually keeps a descriptor from going to the wrong process is the
/// check that whoever connected is root, since no ordinary user can pass that.
/// The directory is the belt to that pair of braces: `XDG_RUNTIME_DIR` is
/// already the user's alone, but the fallback is the shared temporary
/// directory, where somebody else could have got there first -- so a directory
/// that already exists is used only if it turns out to be ours and private.
fn socket_path() -> Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    // Named per request, not per process: the launcher taking disks and a
    // machine starting are different threads, and one unlinking the other's
    // socket mid-prompt would leave both waiting for a child that connected
    // to the wrong listener.
    static REQUEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let request = REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(
            "copperline-blockdev-{}-{request}",
            std::process::id()
        ));
    match std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(|error| error.kind())
    {
        Ok(()) => {}
        // Ours from an earlier attempt in this same process is expected;
        // anybody else's is not, and is left well alone.
        Err(std::io::ErrorKind::AlreadyExists) => {
            let about = std::fs::symlink_metadata(&directory)
                .with_context(|| format!("inspecting {}", directory.display()))?;
            if !about.is_dir() || about.uid() != unsafe { libc::getuid() } {
                anyhow::bail!(
                    "{} is not this user's directory, so it will not be used",
                    directory.display()
                );
            }
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("securing {}", directory.display()))?;
        }
        Err(error) => {
            return Err(std::io::Error::from(error))
                .with_context(|| format!("creating {}", directory.display()));
        }
    }
    Ok(directory.join("open.sock"))
}

/// Ask for every disk at once, through a single moment of privilege.
///
/// `/dev/sdX` is `root:disk 0660` and a desktop user is in neither, so this is
/// the ordinary path rather than a fallback. `pkexec` raises the polkit prompt
/// and runs this same binary as root; that privileged half opens the disks and
/// passes the descriptors back over a unix socket before exiting. A descriptor
/// keeps the access it was opened with for as long as it is open, so what
/// comes back keeps working long after the process that obtained it is gone --
/// the property macOS's `authopen` relies on, reached down a different road.
///
/// Every disk goes in one request because the prompt is a dialog somebody has
/// to read: asking five times for five disks is four interruptions that say
/// nothing the first did not.
///
/// The emulator is not restarted. A process cannot gain privilege in place,
/// and relaunching Copperline through `pkexec` would both throw away a machine
/// that may already be running and leave the whole emulator running as root,
/// which is a far larger thing to be than one open descriptor.
fn open_through_broker(wanted: &[(HostDevice, bool)]) -> Result<Vec<(String, Held)>> {
    use std::os::unix::net::UnixListener;

    let exe =
        std::env::current_exe().context("finding this program to run again with privilege")?;
    let path = socket_path()?;
    // Nothing of an earlier attempt may be left where the answer arrives.
    let _ = std::fs::remove_file(&path);
    let listener =
        UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;

    let asked: Vec<String> = wanted
        .iter()
        .map(|(device, write)| broker_protocol::argument(&device.id, *write))
        .collect();
    let names: Vec<&str> = wanted
        .iter()
        .map(|(device, _)| device.id.as_str())
        .collect();
    log::info!(
        "blockdev: {} needs privilege to open; asking through pkexec",
        names.join(", ")
    );

    let mut child = std::process::Command::new("pkexec")
        .arg(&exe)
        .arg(BROKER_FLAG)
        .arg(&path)
        .args(&asked)
        .spawn()
        .context("running pkexec; a polkit agent is needed to ask for permission")?;

    let accepted = accept_broker(&listener, &mut child);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(path.parent().unwrap_or(&path));
    let stream = accepted?;

    // Whoever connected has to be root. The directory keeps other users out,
    // but this user's own processes could reach the socket, and a descriptor
    // taken from the wrong sender is a descriptor to something else entirely.
    let uid = peer_uid(&stream).context("checking who answered")?;
    if uid != 0 {
        anyhow::bail!("the answer came from uid {uid}, not from root");
    }

    let received = receive_disks(&stream, wanted.len());
    // It has said everything it was run to say, so collect it rather than
    // leaving it for the process table: a launcher session is one of these per
    // Mount pressed, and none of them are ever waited for anywhere else.
    let _ = child.wait();
    let (answer, files) = received?;
    let opened = broker_protocol::parse_answer(&answer)?;
    if opened.len() != files.len() {
        anyhow::bail!(
            "{} disks were named but {} descriptors came back",
            opened.len(),
            files.len()
        );
    }
    // The privileged half refuses a request it cannot meet in full, so a short
    // answer is not a partial success to work with -- it means this is not
    // talking to the half it thinks it is.
    if opened.len() != wanted.len() {
        anyhow::bail!(
            "{} disks were asked for but {} came back",
            wanted.len(),
            opened.len()
        );
    }

    let kernel = Kernel::live();
    let mut taken = Vec::new();
    for (id, file) in opened.into_iter().zip(files) {
        if !wanted.iter().any(|(device, _)| device.id == id) {
            anyhow::bail!("{id} was opened but never asked for");
        }
        // A descriptor arriving over a socket is a claim about what it is.
        // The kernel's own answer for what it points at is the only check
        // worth making, and it is cheap.
        let expected = device_number(&kernel, &id)
            .ok_or_else(|| anyhow::anyhow!("{id} has no device number"))?;
        if stat_rdev(&file)? != expected {
            anyhow::bail!("{id}: what came back is not that disk, so it will not be used");
        }
        taken.push((id, file));
    }
    Ok(taken)
}

/// Wait for the privileged half, which cannot be hurried: the prompt in front
/// of it lasts as long as the person looking at it. A child that exits without
/// connecting is the end of the wait, and is how a declined prompt arrives.
fn accept_broker(
    listener: &std::os::unix::net::UnixListener,
    child: &mut std::process::Child,
) -> Result<std::os::unix::net::UnixStream> {
    use std::os::fd::AsRawFd;
    listener
        .set_nonblocking(true)
        .context("watching for the privileged half")?;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .context("reading from the privileged half")?;
                return Ok(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error).context("waiting for the privileged half"),
        }
        if let Some(status) = child.try_wait().context("waiting for pkexec")? {
            // 126 is pkexec's own code for a dialog that was dismissed or an
            // authorization that was not given.
            if status.code() == Some(126) {
                anyhow::bail!("permission to open the disk was not given");
            }
            anyhow::bail!("the privileged half exited ({status}) without opening anything");
        }
        let mut wait = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // Long enough not to spin, short enough that a child which died is
        // noticed promptly.
        unsafe { libc::poll(&mut wait, 1, 200) };
    }
}

/// Read the one message the privileged half sends: what it opened, and the
/// descriptors themselves as ancillary data.
///
/// Everything arrives together, so there is no second message to synchronise
/// with. A truncated control buffer is fatal rather than partial, but the
/// descriptors that did arrive are collected before saying so: this kernel
/// installs every descriptor it can fit before reporting the truncation, and
/// they are `O_EXCL` claims on raw disks -- leaking one would hold a disk away
/// from the host, and from any later attempt to take it, until the emulator
/// quit.
fn receive_disks(
    stream: &std::os::unix::net::UnixStream,
    most: usize,
) -> Result<(String, Vec<std::fs::File>)> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let mut text = [0u8; 4096];
    let mut iov = libc::iovec {
        iov_base: text.as_mut_ptr().cast(),
        iov_len: text.len(),
    };
    let space =
        unsafe { libc::CMSG_SPACE((most * std::mem::size_of::<libc::c_int>()) as u32) } as usize;
    let mut control = vec![0u8; space];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len();

    let received =
        unsafe { libc::recvmsg(stream.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
    if received < 0 {
        return Err(std::io::Error::last_os_error())
            .context("reading the privileged half's answer");
    }
    let truncated = message.msg_flags & libc::MSG_CTRUNC != 0;

    // Every descriptor is taken ownership of first and judged afterwards, so
    // that no path out of here leaves one installed but unowned.
    let mut raw = Vec::new();
    let mut header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    while !header.is_null() {
        let (level, kind, len) = unsafe {
            (
                (*header).cmsg_level,
                (*header).cmsg_type,
                (*header).cmsg_len as usize,
            )
        };
        if level == libc::SOL_SOCKET && kind == libc::SCM_RIGHTS {
            let count =
                (len - unsafe { libc::CMSG_LEN(0) } as usize) / std::mem::size_of::<libc::c_int>();
            let data = unsafe { libc::CMSG_DATA(header) };
            for index in 0..count {
                raw.push(unsafe {
                    std::ptr::read_unaligned(data.cast::<libc::c_int>().add(index))
                });
            }
        }
        header = unsafe { libc::CMSG_NXTHDR(&message, header) };
    }
    let files: Vec<std::fs::File> = raw
        .iter()
        .filter(|fd| **fd >= 0)
        .map(|fd| unsafe { std::fs::File::from_raw_fd(*fd) })
        .collect();

    if truncated {
        anyhow::bail!("the descriptors did not fit in the control buffer");
    }
    if raw.iter().any(|fd| *fd < 0) {
        anyhow::bail!("a descriptor that came back is not valid");
    }
    Ok((
        String::from_utf8_lossy(&text[..received as usize]).into_owned(),
        files,
    ))
}

/// Who is on the other end of a unix socket, as the kernel saw them when they
/// connected -- so a process that has since exited, or changed identity, is
/// still reported as what it was. `std`'s equivalent is still unstable.
fn peer_uid(stream: &std::os::unix::net::UnixStream) -> Result<libc::uid_t> {
    use std::os::fd::AsRawFd;
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let asked = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &mut length,
        )
    };
    if asked != 0 {
        return Err(std::io::Error::last_os_error()).context("reading the peer's credentials");
    }
    Ok(credentials.uid)
}

/// What the file descriptor actually points at, as the kernel numbers it.
fn stat_rdev(file: &std::fs::File) -> Result<u64> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let about = file.metadata().context("checking what came back")?;
    if !about.file_type().is_block_device() {
        anyhow::bail!("what came back is not a block device");
    }
    Ok(about.rdev())
}

/// Open disks on behalf of a Copperline that was refused, and hand them back.
///
/// This is the privileged half. It is reached by a command line, and a command
/// line can say anything, so it decides for itself what it is willing to open
/// rather than trusting what it was told: the disk the host runs from is
/// refused here too. Running as root is exactly when that check matters most,
/// since it is the last thing between a mistyped identifier and the medium.
///
/// One disk it will not open fails the whole request. They were asked for
/// together, and half a machine's disks is not what anybody pressed the button
/// for.
pub fn serve_broker_request(arguments: &[String]) -> Result<()> {
    use std::os::unix::net::UnixStream;

    let (path, asked) = arguments
        .split_first()
        .context("the privileged half takes a socket path and then DEVICE:rw|ro...")?;
    if asked.is_empty() {
        anyhow::bail!("the privileged half was given no disk to open");
    }
    let stream = UnixStream::connect(path).with_context(|| format!("connecting to {path}"))?;

    let outcome = asked
        .iter()
        .map(|argument| {
            broker_protocol::parse_argument(argument)
                .ok_or_else(|| anyhow::anyhow!("{argument} does not name a disk"))
        })
        .collect::<Result<Vec<_>>>()
        .and_then(|wanted| broker_open(&wanted));

    match outcome {
        Ok(opened) => {
            let mut answer = String::from("ok\n");
            for (id, _) in &opened {
                answer.push_str(id);
                answer.push('\n');
            }
            let files: Vec<&std::fs::File> = opened.iter().map(|(_, file)| file).collect();
            send_disks(&stream, &answer, &files)
        }
        // A failure it cannot report would look to the other side like a
        // crash, so the reason goes back before this exits.
        Err(error) => {
            let _ = send_disks(&stream, &format!("error {error:#}"), &[]);
            Err(error)
        }
    }
}

fn broker_open(asked: &[(String, bool)]) -> Result<Vec<(String, std::fs::File)>> {
    let mut opened = Vec::new();
    for (id, write) in asked {
        // Re-applied here rather than inherited: this half runs as root, so it
        // is the last place that can refuse the disk the host is running from,
        // and it must not depend on the other half having asked nicely. It
        // asks the same question the unprivileged path does rather than a
        // hand-copied likeness of it -- a rule added to that one has to reach
        // the half that can actually damage the medium.
        let device = super::find_device(id)?
            .ok_or_else(|| anyhow::anyhow!("no host disk called {id} is attached"))?;
        super::refuse_if_unusable(&device, *write)?;
        let file = direct_open(&device.path, *write).map_err(|error| explain(&error, &device))?;
        opened.push((device.id.clone(), file));
    }
    Ok(opened)
}

/// Send every descriptor with the answer that names them, in one message.
fn send_disks(
    stream: &std::os::unix::net::UnixStream,
    answer: &str,
    files: &[&std::fs::File],
) -> Result<()> {
    use std::os::fd::{AsRawFd, RawFd};

    let mut text = answer.as_bytes().to_vec();
    let mut iov = libc::iovec {
        iov_base: text.as_mut_ptr().cast(),
        iov_len: text.len(),
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;

    // What travels is one descriptor number per file, not the references this
    // was handed: the kernel reads the count back out of the length, so
    // overstating it makes it look for descriptors that were never written.
    let bytes = files.len() * std::mem::size_of::<RawFd>();
    let mut control = vec![0u8; unsafe { libc::CMSG_SPACE(bytes as u32) } as usize];
    if !files.is_empty() {
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control.len();
        let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
        unsafe {
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(bytes as u32) as usize;
            let data = libc::CMSG_DATA(header).cast::<RawFd>();
            for (index, file) in files.iter().enumerate() {
                std::ptr::write_unaligned(data.add(index), file.as_raw_fd());
            }
        }
    }
    if unsafe { libc::sendmsg(stream.as_raw_fd(), &message, libc::MSG_NOSIGNAL) } < 0 {
        return Err(std::io::Error::last_os_error()).context("handing the disks back");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sysfs tree built to order, so layouts this machine does not have can
    /// still be tested -- which for the root-on-LVM case is the whole point.
    struct FakeSys {
        root: PathBuf,
    }

    impl FakeSys {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("copperline-blockdev-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("class/block")).expect("fake sysfs");
            std::fs::create_dir_all(root.join("dev/block")).expect("fake sysfs");
            std::fs::create_dir_all(root.join("block")).expect("fake sysfs");
            Self { root }
        }

        /// A fixed disk on an internal bus, the shape of a host's own storage.
        fn disk(&self, name: &str, major_minor: &str) -> &Self {
            self.media(name, major_minor, "devices/pci0000:00/ata1/host0", false)
        }

        /// A card in a reader: on USB, and saying so.
        fn card(&self, name: &str, major_minor: &str) -> &Self {
            self.media(name, major_minor, "devices/pci0000:00/usb3/3-3/host6", true)
        }

        fn media(&self, name: &str, major_minor: &str, bus: &str, removable: bool) -> &Self {
            let target = self.root.join(bus).join("target0");
            let real = target.join("block").join(name);
            std::fs::create_dir_all(real.join("slaves")).expect("disk");
            std::fs::create_dir_all(real.join("queue")).expect("queue");
            std::fs::create_dir_all(&target).expect("target");
            std::fs::write(real.join("size"), "1024\n").expect("size");
            std::fs::write(real.join("queue/logical_block_size"), "512\n").expect("block size");
            std::fs::write(
                real.join("removable"),
                if removable { "1\n" } else { "0\n" },
            )
            .expect("removable");
            // The bus a device hangs off is read by resolving this link, which
            // is how a card reader is told from the disk the host boots from.
            std::os::unix::fs::symlink(&target, real.join("device")).expect("device link");
            self.link(name, &real, major_minor);
            self
        }

        /// An NVMe namespace, which hangs straight off its controller with no
        /// `block` directory between them -- the layout that exists because
        /// the controller is itself a class device, and the one a whole-disk
        /// test that only models SCSI would never catch.
        fn namespace(&self, controller: &str, name: &str, major_minor: &str) -> &Self {
            let real = self
                .root
                .join("devices/pci0000:00/nvme")
                .join(controller)
                .join(name);
            std::fs::create_dir_all(real.join("slaves")).expect("namespace");
            std::fs::create_dir_all(real.join("queue")).expect("queue");
            std::fs::write(real.join("size"), "1024\n").expect("size");
            std::fs::write(real.join("queue/logical_block_size"), "512\n").expect("block size");
            std::fs::write(real.join("removable"), "0\n").expect("removable");
            self.link(name, &real, major_minor);
            self
        }

        /// A partition, which lives inside its disk's directory and says so
        /// with the attribute only a partition carries.
        fn partition(&self, disk: &str, name: &str, major_minor: &str) -> &Self {
            let real = std::fs::canonicalize(self.root.join("class/block").join(disk))
                .expect("the disk must exist first")
                .join(name);
            std::fs::create_dir_all(&real).expect("partition");
            std::fs::write(real.join("partition"), "1\n").expect("partition marker");
            std::os::unix::fs::symlink(&real, self.root.join("class/block").join(name))
                .expect("class link");
            std::os::unix::fs::symlink(&real, self.root.join("dev/block").join(major_minor))
                .expect("dev link");
            self
        }

        /// A device-mapper or MD device, named by what it is built from.
        fn mapper(&self, name: &str, major_minor: &str, slaves: &[&str]) -> &Self {
            let real = self.root.join("devices/virtual/block").join(name);
            std::fs::create_dir_all(real.join("slaves")).expect("mapper");
            for slave in slaves {
                std::fs::write(real.join("slaves").join(slave), "").expect("slave");
            }
            self.link(name, &real, major_minor);
            self
        }

        fn link(&self, name: &str, real: &Path, major_minor: &str) {
            std::os::unix::fs::symlink(real, self.root.join("class/block").join(name))
                .expect("class link");
            std::os::unix::fs::symlink(real, self.root.join("dev/block").join(major_minor))
                .expect("dev link");
            std::os::unix::fs::symlink(real, self.root.join("block").join(name))
                .expect("block link");
        }

        fn kernel(&self, mountinfo: &str) -> Kernel {
            let path = self.root.join("mountinfo");
            std::fs::write(&path, mountinfo).expect("mountinfo");
            Kernel {
                sys: self.root.clone(),
                mountinfo: path,
            }
        }
    }

    impl Drop for FakeSys {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// One mountinfo line, with the fields this reads in the right places.
    fn mount_line(major_minor: &str, point: &str, source: &str) -> String {
        format!("29 1 {major_minor} / {point} rw,relatime shared:1 - ext4 {source} rw\n")
    }

    /// A partition belongs to the disk it is cut from, whatever the naming
    /// scheme -- and the kernel says so without any name to parse, which is
    /// why `nvme0n1p3` and `mmcblk0p1` need no special case here.
    ///
    /// The NVMe namespace is built the way the kernel really builds one, with
    /// no `block` directory between it and its controller. An earlier version
    /// of this decided a whole disk by that directory's name and so traced
    /// every NVMe root to nothing -- which, since a root that cannot be traced
    /// makes every disk unusable, took the whole feature down on the machines
    /// most people have. The test that missed it modelled NVMe as if it were
    /// SCSI.
    #[test]
    fn a_partition_resolves_to_its_whole_disk() {
        let sys = FakeSys::new("partitions");
        sys.disk("sdb", "8:16").partition("sdb", "sdb1", "8:17");
        sys.namespace("nvme0", "nvme0n1", "259:0")
            .partition("nvme0n1", "nvme0n1p3", "259:3");
        sys.disk("mmcblk0", "179:0")
            .partition("mmcblk0", "mmcblk0p1", "179:1");
        let kernel = sys.kernel("");

        for (partition, disk) in [
            ("sdb1", "sdb"),
            ("nvme0n1p3", "nvme0n1"),
            ("mmcblk0p1", "mmcblk0"),
        ] {
            assert_eq!(
                whole_disks_of(&kernel, partition, 0),
                BTreeSet::from([disk.to_string()]),
                "{partition} belongs to {disk}"
            );
        }
        // A whole disk is already the answer to its own question.
        assert_eq!(
            whole_disks_of(&kernel, "sdb", 0),
            BTreeSet::from(["sdb".to_string()])
        );
    }

    /// A root on LVM must still name its physical disk. This is the layout the
    /// module exists to get right: an unidentified disk here would be offered
    /// to the emulator as an Amiga drive.
    #[test]
    fn a_root_on_lvm_still_names_its_physical_disk() {
        let sys = FakeSys::new("lvm");
        sys.disk("sda", "8:0").partition("sda", "sda2", "8:2");
        sys.mapper("dm-0", "253:0", &["sda2"]);
        let kernel = sys.kernel(&mount_line("253:0", "/", "/dev/mapper/vg-root"));

        let mounts = mounts(&kernel);
        match root_of(&mounts) {
            Root::Disks(disks) => assert_eq!(disks, BTreeSet::from(["sda".to_string()])),
            _ => panic!("the root's physical disk must be identified"),
        }
    }

    /// LUKS on top of LVM is two mapper layers, and a volume group can span
    /// several disks -- every one of which carries the running system.
    #[test]
    fn every_disk_under_a_stacked_root_is_the_system_disk() {
        let sys = FakeSys::new("stacked");
        sys.disk("sda", "8:0").partition("sda", "sda2", "8:2");
        sys.disk("sdb", "8:16").partition("sdb", "sdb1", "8:17");
        // A volume group over both disks, with the filesystem inside LUKS on
        // top of the logical volume.
        sys.mapper("dm-0", "253:0", &["sda2", "sdb1"]);
        sys.mapper("dm-1", "253:1", &["dm-0"]);
        let kernel = sys.kernel(&mount_line("253:1", "/", "/dev/mapper/crypt"));

        let mounts = mounts(&kernel);
        match root_of(&mounts) {
            Root::Disks(disks) => assert_eq!(
                disks,
                BTreeSet::from(["sda".to_string(), "sdb".to_string()]),
                "a volume group spanning two disks makes both of them the system's"
            ),
            _ => panic!("a stacked root must still be traced"),
        }
    }

    /// A root that cannot be traced to a disk must not leave disks offerable.
    /// Refusing everything is visible and recoverable; offering the host's own
    /// disk is neither.
    #[test]
    fn an_untraceable_root_makes_every_disk_unusable() {
        let sys = FakeSys::new("untraceable");
        sys.disk("sda", "8:0");
        // A mapper device whose slaves the kernel does not show -- the shape
        // of any layout this cannot follow.
        sys.mapper("dm-0", "253:0", &[]);
        let kernel = sys.kernel(&mount_line("253:0", "/", "/dev/mapper/mystery"));

        let mounts = mounts(&kernel);
        assert!(matches!(root_of(&mounts), Root::Untraceable));
        for device in enumerate(&kernel).expect("enumerate") {
            assert_eq!(
                device.safety,
                Safety::SystemDisk,
                "{} must not be offered when the system disk is unknown",
                device.id
            );
        }
    }

    /// A root on ZFS is served by no block device -- `mountinfo` gives it an
    /// anonymous `0:NN` -- but the machine is still running from a disk, and
    /// the EFI partition is where that disk shows through.
    ///
    /// Reading only `/` here would conclude that no disk is the host's and
    /// hand the emulator the very medium it booted from.
    #[test]
    fn a_root_on_zfs_is_still_found_through_the_disk_it_boots_from() {
        let sys = FakeSys::new("zfs");
        sys.disk("sda", "8:0")
            .partition("sda", "sda1", "8:1")
            .partition("sda", "sda2", "8:2");
        sys.card("sdb", "8:16");
        let kernel = sys.kernel(&format!(
            "29 1 0:76 / / rw,relatime shared:1 - zfs rpool/ROOT/ubuntu rw\n{}",
            mount_line("8:1", "/boot/efi", "/dev/sda1"),
        ));

        let devices = enumerate(&kernel).expect("enumerate");
        let system = devices.iter().find(|d| d.id == "sda").expect("sda");
        assert_eq!(
            system.safety,
            Safety::SystemDisk,
            "the disk a ZFS root boots from must not be offerable"
        );
        // And the card in the reader is still perfectly usable.
        let card = devices.iter().find(|d| d.id == "sdb").expect("sdb");
        assert_eq!(card.safety, Safety::Offerable);
    }

    /// A live session runs from an overlay whose medium is the stick it
    /// booted from -- which is removable and on USB, so it would otherwise be
    /// offered as an ordinary choice, and writing to it would take the running
    /// system out from underneath itself.
    #[test]
    fn a_live_session_does_not_offer_the_stick_it_booted_from() {
        let sys = FakeSys::new("live");
        sys.card("sdb", "8:16").partition("sdb", "sdb1", "8:17");
        let kernel = sys.kernel(&format!(
            "29 1 0:52 / / rw,relatime shared:1 - overlay overlay rw\n{}",
            mount_line("8:17", "/cdrom", "/dev/sdb1"),
        ));

        let devices = enumerate(&kernel).expect("enumerate");
        let stick = devices.iter().find(|d| d.id == "sdb").expect("sdb");
        assert_eq!(stick.safety, Safety::SystemDisk);
    }

    /// A root on nothing at all -- a container, an NFS root -- leaves no way
    /// to tell which disk is the host's, so none of them can be offered.
    #[test]
    fn a_root_that_touches_no_disk_leaves_nothing_offerable() {
        let sys = FakeSys::new("container");
        sys.card("sdb", "8:16");
        let kernel = sys.kernel("29 1 0:52 / / rw,relatime shared:1 - overlay overlay rw\n");

        let mounts = mounts(&kernel);
        assert!(matches!(root_of(&mounts), Root::Untraceable));
        for device in enumerate(&kernel).expect("enumerate") {
            assert_eq!(device.safety, Safety::SystemDisk, "{}", device.id);
        }
    }

    /// Mounts are reported against the disk underneath them, through however
    /// many mapper layers, because that is the disk that has to be given up.
    #[test]
    fn a_mount_is_reported_against_the_disk_it_sits_on() {
        let sys = FakeSys::new("mounts");
        sys.disk("sda", "8:0").partition("sda", "sda2", "8:2");
        sys.card("sdb", "8:16").partition("sdb", "sdb1", "8:17");
        let kernel = sys.kernel(&format!(
            "{}{}",
            mount_line("8:2", "/", "/dev/sda2"),
            mount_line("8:17", "/media/My\\040Card", "/dev/sdb1"),
        ));

        let devices = enumerate(&kernel).expect("enumerate");
        let card = devices.iter().find(|d| d.id == "sdb").expect("sdb");
        assert_eq!(card.mounted, vec!["/media/My Card".to_string()]);
        assert_eq!(card.safety, Safety::Offerable);
        let system = devices.iter().find(|d| d.id == "sda").expect("sda");
        assert_eq!(system.safety, Safety::SystemDisk);
    }

    /// The things in `/sys/block` that are not media are told apart from the
    /// ones that are by more than a prefix: `md0` is not media, `mmcblk0` is.
    #[test]
    fn what_is_not_media_is_left_out() {
        for name in ["loop0", "ram3", "zram0", "dm-0", "md127", "sr0"] {
            assert!(!is_real_media(name), "{name} is not media");
        }
        for name in ["sda", "sdb", "nvme0n1", "mmcblk0", "mmcblk0boot0", "vda"] {
            assert!(is_real_media(name), "{name} is media");
        }
    }

    /// The two halves have to agree about the wire between them, and the part
    /// that cannot be checked by reading it is the ancillary data: descriptors
    /// arrive as numbers that mean nothing except in the process that received
    /// them. So they are sent for real here, over a socket pair, and read back
    /// through.
    #[test]
    fn descriptors_survive_the_journey_between_the_halves() {
        use std::io::{Read, Seek, Write};

        let (theirs, ours) = std::os::unix::net::UnixStream::pair().expect("socket pair");
        let mut files = Vec::new();
        for (index, content) in ["first disk", "second disk"].iter().enumerate() {
            let path =
                std::env::temp_dir().join(format!("copperline-fd-{}-{index}", std::process::id()));
            let mut file = std::fs::File::options()
                .create(true)
                .truncate(true)
                .read(true)
                .write(true)
                .open(&path)
                .expect("scratch file");
            file.write_all(content.as_bytes()).expect("write");
            std::fs::remove_file(&path).expect("unlink");
            files.push(file);
        }

        let borrowed: Vec<&std::fs::File> = files.iter().collect();
        send_disks(&theirs, "ok\nsdb\nsdc\n", &borrowed).expect("send");
        let (answer, mut received) = receive_disks(&ours, 2).expect("receive");
        assert_eq!(
            broker_protocol::parse_answer(&answer).expect("parse"),
            ["sdb", "sdc"]
        );
        assert_eq!(received.len(), 2, "one descriptor per disk named");

        // What came back must be the same open files, not merely two numbers.
        for (file, expected) in received.iter_mut().zip(["first disk", "second disk"]) {
            let mut read = String::new();
            file.rewind().expect("rewind");
            file.read_to_string(&mut read).expect("read back");
            assert_eq!(read, expected);
        }
    }

    /// A refusal has to arrive as a refusal, carrying the reason, rather than
    /// as an empty success that would read as "opened nothing".
    #[test]
    fn a_refusal_comes_back_with_its_reason() {
        let (theirs, ours) = std::os::unix::net::UnixStream::pair().expect("socket pair");
        send_disks(&theirs, "error sdb is write protected", &[]).expect("send");
        let (answer, received) = receive_disks(&ours, 1).expect("receive");
        assert!(received.is_empty(), "a refusal carries no descriptor");
        let error = broker_protocol::parse_answer(&answer).expect_err("a refusal is not a success");
        assert!(error.to_string().contains("write protected"), "{error}");
    }

    /// A disk taken at the Mount button has to still be gettable when the
    /// machine starts, and gettable *again* after the machine stops.
    ///
    /// This is the interaction `O_EXCL` makes sharp: the reservation is itself
    /// an exclusive claim, so a machine that opened the node afresh would meet
    /// it and fail with `EBUSY`. It has to be lent instead. Ignored because it
    /// needs a device to point at; it only reads. Set it up as
    /// [`crate::blockdev::tests::device_round_trip`] describes -- and run the
    /// ignored tests with `--test-threads=1`, since both take the one device
    /// `COPPERLINE_TEST_DISK` names, and taking it exclusively is the point.
    #[test]
    #[ignore = "needs a real device named by COPPERLINE_TEST_DISK"]
    fn a_reserved_disk_is_lent_to_the_machine_rather_than_reopened() {
        let Ok(id) = std::env::var("COPPERLINE_TEST_DISK") else {
            panic!("set COPPERLINE_TEST_DISK to a disposable device (e.g. sdd)");
        };
        let device = crate::blockdev::find_device(&id)
            .expect("enumerate")
            .unwrap_or_else(|| panic!("no device {id}"));

        crate::blockdev::reserve_devices(&[(id.clone(), false)])
            .expect("the launcher takes the disk");
        // Proof the reservation really is exclusive: opening the node again is
        // what the machine must not do, and this is what it would meet.
        assert_eq!(
            direct_open(&device.path, false)
                .expect_err("a second exclusive open must be refused")
                .raw_os_error(),
            Some(libc::EBUSY),
            "the reservation must hold the disk exclusively"
        );
        // ...and the machine gets it anyway, because it is lent the one that
        // is already open rather than opening its own.
        let disk =
            crate::blockdev::open_device(&device, false).expect("the machine is lent the disk");
        assert_eq!(disk.total_sectors(), device.guest_sectors());
        // Stopping the machine must not hand the disk back on its own: the
        // launcher still shows it as attached, and the consent was given once.
        drop(disk);
        assert!(
            direct_open(&device.path, false).is_err(),
            "a machine stopping must not release a disk the launcher still holds"
        );

        assert!(
            crate::blockdev::release_device(&id),
            "the launcher hands it back"
        );
        // And now, and only now, the host can have it.
        drop(direct_open(&device.path, false).expect("released back to the host"));
        assert!(
            !crate::blockdev::release_device(&id),
            "releasing twice is not a second release"
        );
    }

    /// Enumeration must work unprivileged with nothing plugged in: it is
    /// reached from config validation and from the launcher on machines that
    /// have never seen an Amiga disk. This runs against the real host.
    #[test]
    fn enumeration_is_safe_with_nothing_attached() {
        let kernel = Kernel::live();
        let devices = list_devices().expect("sysfs enumeration works unprivileged");
        // Whatever the host's layout, the disk it runs from has to end up
        // marked -- unless the root is not on a block device at all, which is
        // what a container's overlay looks like and rules out no disk here.
        if let Root::Disks(disks) = root_of(&mounts(&kernel)) {
            for disk in &disks {
                assert!(
                    devices
                        .iter()
                        .any(|d| d.id == *disk && d.safety == Safety::SystemDisk),
                    "{disk} serves / and must be marked as the system disk: {devices:#?}"
                );
            }
        }
        for device in &devices {
            assert!(!device.id.is_empty());
            assert!(device.size_bytes > 0);
            assert!(device.block_size >= 512);
            assert_eq!(device.path, PathBuf::from(format!("/dev/{}", device.id)));
        }
    }
}
