// SPDX-License-Identifier: GPL-3.0-or-later

//! Host block devices: a real disk standing in for a hard-drive image.
//!
//! An Amiga's system disk is usually a CF card, an SD card, or an IDE drive
//! that a host can read directly. Attaching one here lets the emulated machine
//! use the very disk a real Amiga boots from, rather than a copy of it.
//!
//! # Why this is the most dangerous thing the emulator does
//!
//! Everything else Copperline writes to is a file. This writes to whole
//! physical media, where a mistake is not a corrupted image but somebody's
//! only copy of their Amiga's system disk -- or, if the wrong device is
//! chosen, the host's own operating system. The whole module is arranged
//! around that:
//!
//! - The disk the host is running from is classified [`Safety::SystemDisk`]
//!   and is never offered, on any platform. Not a warning: it is not in the
//!   list. (WinUAE has no such check at all -- it infers "this is your
//!   Windows disk" from the presence of an NTFS volume -- and Amiberry's
//!   equivalent is dead code that is never called.)
//! - Internal fixed disks are classified [`Safety::Internal`] and hidden
//!   unless deliberately asked for. An Amiga disk reaches a modern host
//!   through a card reader or a USB bridge, so the useful device is nearly
//!   always removable or external.
//! - Enumeration never opens anything, so listing devices cannot disturb
//!   them, cannot spin up a sleeping drive, and needs no privileges.
//!
//! # Privileges
//!
//! Reading raw media is privileged on every supported host, and each one
//! grants it differently, so the escalation lives in the platform backend
//! rather than here. The shape they share: the user is asked once, by the
//! system's own prompt, and what comes back is an already-open handle to one
//! named device -- a capability that cannot be turned on anything else.
//!
//! # Sector sizes
//!
//! The guest reads and writes 512-byte sectors ([`crate::harddrive::SECTOR_SIZE`]),
//! and modern media often does not: 4096 is common, and a host may refuse
//! any transfer that is not a whole number of its own blocks. Translating
//! between the two is the backend's job, so the emulated machine sees the
//! 512-byte device it expects whatever the media underneath is.

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
use macos as platform;

use std::path::PathBuf;

/// How safe a device is to hand to the emulated machine.
///
/// Ordered by how much protection the device needs, not by preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Safety {
    /// Nothing in the way: removable or external media the host is not
    /// relying on.
    Offerable,
    /// An internal fixed disk. Probably the host's own storage, but not
    /// provably so, so it is hidden rather than refused: a genuine Amiga
    /// drive on an internal bus is a real, if rare, setup.
    Internal,
    /// The disk the host is running from. Never offered and never opened,
    /// whatever else asks for it.
    SystemDisk,
}

impl Safety {
    /// Whether a device may be listed to the user by default.
    pub const fn listable(self) -> bool {
        matches!(self, Self::Offerable)
    }

    /// Whether a device may be opened at all. The system disk never may.
    pub const fn openable(self) -> bool {
        !matches!(self, Self::SystemDisk)
    }

    /// Short tag for a picker, saying *why* a device is held back. Naming the
    /// reason lets somebody decide for themselves; a bare absence does not.
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Offerable => "",
            Self::Internal => "internal",
            Self::SystemDisk => "system disk",
        }
    }
}

/// One whole physical device the host can see.
///
/// Everything here is gathered without opening the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDevice {
    /// Stable platform identifier, as a configuration file names it
    /// (`disk4` on macOS, `sdb` on Linux, `PhysicalDrive1` on Windows).
    pub id: String,
    /// The node to open for raw access. Not necessarily `/dev/<id>`: macOS
    /// prefers the unbuffered character device, `/dev/rdisk4`.
    pub path: PathBuf,
    /// Model as the hardware reports it, when it reports one.
    pub model: Option<String>,
    /// Capacity in bytes.
    pub size_bytes: u64,
    /// The media's own block size. Often 512, increasingly 4096. Transfers
    /// may have to be whole multiples of this.
    pub block_size: u32,
    /// The medium can be taken out of the drive.
    pub removable: bool,
    /// Attached to an internal bus rather than a port.
    pub internal: bool,
    /// The hardware says it can be written to (a locked SD card cannot).
    pub writable: bool,
    /// Where the host has volumes from this device mounted. Non-empty means
    /// the host is using it right now, and it must be unmounted before the
    /// emulator can have it.
    pub mounted: Vec<String>,
    /// Whether this device may be offered, and why not.
    pub safety: Safety,
}

impl HostDevice {
    /// Capacity rounded for display: `2.0 TB`, `3.7 GB`, `512 MB`.
    ///
    /// Decimal units, because that is what the hardware is sold and labelled
    /// in -- a card that says 32 GB should read as about 32 GB here, not 29.
    pub fn size_label(&self) -> String {
        const TB: f64 = 1_000_000_000_000.0;
        const GB: f64 = 1_000_000_000.0;
        const MB: f64 = 1_000_000.0;
        let bytes = self.size_bytes as f64;
        if bytes >= TB {
            format!("{:.1} TB", bytes / TB)
        } else if bytes >= GB {
            format!("{:.1} GB", bytes / GB)
        } else {
            format!("{:.0} MB", bytes / MB)
        }
    }

    /// One line for a picker: what it is, how big, and anything the user
    /// needs to know before choosing it.
    pub fn label(&self) -> String {
        let mut label = match self.model.as_deref() {
            Some(model) if !model.is_empty() => format!("{model} ({})", self.size_label()),
            _ => format!("{} ({})", self.id, self.size_label()),
        };
        let mut notes = Vec::new();
        if !self.safety.tag().is_empty() {
            notes.push(self.safety.tag().to_string());
        }
        if !self.writable {
            notes.push("write protected".to_string());
        }
        if !self.mounted.is_empty() {
            notes.push(format!("mounted: {}", self.mounted.join(", ")));
        }
        if !notes.is_empty() {
            label.push_str(&format!(" [{}]", notes.join(", ")));
        }
        label
    }

    /// Sectors as the guest counts them, whatever the media's own block size.
    pub const fn guest_sectors(&self) -> u64 {
        self.size_bytes / crate::harddrive::SECTOR_SIZE as u64
    }
}

/// An open device, presented to the emulator as 512-byte sectors.
///
/// The media underneath may use a larger block, and a host may refuse any
/// transfer that is not a whole number of those blocks at a multiple of that
/// size (macOS returns `EINVAL`; it does not silently do the right thing).
/// This translates: a guest sector inside a larger block is served by reading
/// the block it sits in, and writing one is a read-modify-write of that block.
///
/// All I/O is positioned (`pread`/`pwrite`). A descriptor that arrived from a
/// privileged opener shares its file offset with whoever opened it, so seeking
/// would be a race; positioned I/O has no offset to race over.
pub struct BlockDevice {
    file: std::fs::File,
    id: String,
    /// The media's block size, which every transfer must be a whole number of.
    block_size: u32,
    size_bytes: u64,
    writable: bool,
    /// Scratch for a partial block, so steady-state I/O allocates nothing.
    block: Vec<u8>,
    /// Whether a refused write has already been reported. The guest will keep
    /// trying, and one explanation is worth more than thousands.
    refusal_reported: bool,
}

impl BlockDevice {
    /// Wrap an already-open device. The handle is expected to have come from
    /// a platform opener, which is where privilege is dealt with.
    pub(crate) fn new(
        file: std::fs::File,
        id: String,
        block_size: u32,
        size_bytes: u64,
        writable: bool,
    ) -> Self {
        let block_size = block_size.max(crate::harddrive::SECTOR_SIZE as u32);
        Self {
            file,
            id,
            block_size,
            size_bytes,
            writable,
            block: vec![0; block_size as usize],
            refusal_reported: false,
        }
    }

    /// Which device this is, for logs and errors.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Capacity in 512-byte guest sectors.
    pub const fn total_sectors(&self) -> u64 {
        self.size_bytes / crate::harddrive::SECTOR_SIZE as u64
    }

    /// Whether writes will be attempted at all.
    pub const fn writable(&self) -> bool {
        self.writable
    }

    /// The media's own block size.
    pub const fn block_size(&self) -> u32 {
        self.block_size
    }

    fn out_of_range(&self, lba: u64) -> std::io::Result<()> {
        if lba >= self.total_sectors() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "{}: sector {lba} is past the end of the device ({} sectors)",
                    self.id,
                    self.total_sectors()
                ),
            ));
        }
        Ok(())
    }

    /// Read one 512-byte guest sector.
    pub fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.out_of_range(lba)?;
        let sector = crate::harddrive::SECTOR_SIZE;
        let offset = lba * sector as u64;
        if self.block_size as usize == sector {
            return read_exact_at(&self.file, &mut buf[..sector], offset);
        }
        let block_start = offset - offset % u64::from(self.block_size);
        let within = (offset - block_start) as usize;
        read_exact_at(&self.file, &mut self.block, block_start)?;
        buf[..sector].copy_from_slice(&self.block[within..within + sector]);
        Ok(())
    }

    /// Write one 512-byte guest sector.
    ///
    /// On media whose blocks are larger than a sector this is a
    /// read-modify-write: the surrounding block is read, the sector patched
    /// into it, and the whole block written back. The read is what keeps the
    /// neighbouring sectors intact, so it is not optional.
    pub fn write_sector(&mut self, lba: u64, buf: &[u8]) -> std::io::Result<()> {
        if !self.writable {
            // Say it once, plainly: a guest that meets this shows its own
            // error, and "why did my disk fail to write" is answered here
            // rather than left to be guessed at. Amiga filesystems commonly
            // write on mount -- PFS marks the volume in use -- so a
            // read-only disk raises this during boot, not only when
            // something is saved.
            if !self.refusal_reported {
                self.refusal_reported = true;
                log::warn!(
                    "blockdev: {} is attached read-only, so the guest's write to sector {lba} \
                     was refused; untick R/O (or set read_only = false) to let it write",
                    self.id
                );
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{}: opened read-only", self.id),
            ));
        }
        self.out_of_range(lba)?;
        let sector = crate::harddrive::SECTOR_SIZE;
        let offset = lba * sector as u64;
        if self.block_size as usize == sector {
            return write_all_at(&self.file, &buf[..sector], offset);
        }
        let block_start = offset - offset % u64::from(self.block_size);
        let within = (offset - block_start) as usize;
        read_exact_at(&self.file, &mut self.block, block_start)?;
        self.block[within..within + sector].copy_from_slice(&buf[..sector]);
        write_all_at(&self.file, &self.block, block_start)
    }

    /// Push everything to the medium. A physical disk can be pulled out, so
    /// buffered writes are a hazard an image file does not have.
    pub fn flush(&mut self) -> std::io::Result<()> {
        use std::io::Write;
        self.file.flush()
    }
}

impl std::fmt::Debug for BlockDevice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BlockDevice")
            .field("id", &self.id)
            .field("block_size", &self.block_size)
            .field("sectors", &self.total_sectors())
            .field("writable", &self.writable)
            .finish()
    }
}

#[cfg(unix)]
fn read_exact_at(file: &std::fs::File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(unix)]
fn write_all_at(file: &std::fs::File, buf: &[u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &std::fs::File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0;
    while done < buf.len() {
        let n = file.seek_read(&mut buf[done..], offset + done as u64)?;
        if n == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        done += n;
    }
    Ok(())
}

#[cfg(windows)]
fn write_all_at(file: &std::fs::File, buf: &[u8], offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0;
    while done < buf.len() {
        let n = file.seek_write(&buf[done..], offset + done as u64)?;
        if n == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::WriteZero));
        }
        done += n;
    }
    Ok(())
}

/// Open a device for the emulated machine.
///
/// Refuses the host's own system disk outright, before any platform code
/// runs: that check is not something a backend gets to have an opinion on.
pub fn open_device(device: &HostDevice, write: bool) -> anyhow::Result<BlockDevice> {
    if !device.safety.openable() {
        anyhow::bail!(
            "{} is the disk this computer is running from and cannot be used as an Amiga drive",
            device.id
        );
    }
    if write && !device.writable {
        anyhow::bail!(
            "{} is write protected by the hardware; mount it read-only or unlock the media",
            device.id
        );
    }
    platform_open(device, write)
}

#[cfg(target_os = "macos")]
fn platform_open(device: &HostDevice, write: bool) -> anyhow::Result<BlockDevice> {
    platform::open_device(device, write)
}

#[cfg(not(target_os = "macos"))]
fn platform_open(_device: &HostDevice, _write: bool) -> anyhow::Result<BlockDevice> {
    anyhow::bail!("physical disks are not supported on this platform yet")
}

/// Every whole device the host can see, including ones held back for safety
/// (each carries its [`Safety`], so a caller can show or hide them).
///
/// Opens nothing. Sorted with the devices most likely to be wanted first:
/// offerable before internal, and larger media before smaller, since an Amiga
/// disk is usually the odd small one on a modern host.
pub fn list_devices() -> anyhow::Result<Vec<HostDevice>> {
    let mut devices = platform_list()?;
    devices.sort_by(|a, b| {
        a.safety
            .cmp(&b.safety)
            .then(b.size_bytes.cmp(&a.size_bytes))
            .then(a.id.cmp(&b.id))
    });
    Ok(devices)
}

/// The device a configuration names, if the host still has it.
///
/// Matched on the stable identifier rather than the node path, because a node
/// path is a property of this boot, not of the hardware.
pub fn find_device(id: &str) -> anyhow::Result<Option<HostDevice>> {
    Ok(list_devices()?.into_iter().find(|device| device.id == id))
}

#[cfg(target_os = "macos")]
fn platform_list() -> anyhow::Result<Vec<HostDevice>> {
    platform::list_devices()
}

/// Hosts without a backend yet report nothing rather than failing, so every
/// caller (config validation, the launcher) works everywhere and simply finds
/// no devices to offer.
#[cfg(not(target_os = "macos"))]
fn platform_list() -> anyhow::Result<Vec<HostDevice>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, size: u64, safety: Safety) -> HostDevice {
        HostDevice {
            id: id.to_string(),
            path: PathBuf::from(format!("/dev/r{id}")),
            model: None,
            size_bytes: size,
            block_size: 512,
            removable: true,
            internal: false,
            writable: true,
            mounted: Vec::new(),
            safety,
        }
    }

    /// The system disk is never openable, however it is reached. This is the
    /// one guarantee the whole module exists to make.
    #[test]
    fn the_system_disk_can_never_be_opened() {
        assert!(!Safety::SystemDisk.openable());
        assert!(!Safety::SystemDisk.listable());
        // And an internal disk is merely hidden, not forbidden.
        assert!(Safety::Internal.openable());
        assert!(!Safety::Internal.listable());
        assert!(Safety::Offerable.listable());
    }

    /// A label has to say enough to choose by, including the reasons a device
    /// is held back -- an unexplained omission is worse than a warning.
    #[test]
    fn a_label_names_what_matters_before_choosing() {
        let mut d = device("disk4", 4_000_000_000, Safety::Offerable);
        d.model = Some("SanDisk Cruzer".to_string());
        assert_eq!(d.label(), "SanDisk Cruzer (4.0 GB)");

        d.safety = Safety::Internal;
        d.writable = false;
        d.mounted = vec!["/Volumes/UNTITLED".to_string()];
        assert_eq!(
            d.label(),
            "SanDisk Cruzer (4.0 GB) [internal, write protected, mounted: /Volumes/UNTITLED]"
        );

        // With no model, the identifier is what the user has to go on.
        let bare = device("disk9", 512_000_000, Safety::Offerable);
        assert_eq!(bare.label(), "disk9 (512 MB)");
    }

    /// Read and write a real device end to end.
    ///
    /// Ignored because it needs a device to point at, and writes to it. Use a
    /// disposable one -- an attached disk image is ideal and needs no
    /// hardware:
    ///
    /// ```sh
    /// hdiutil create -size 64m -layout NONE -type UDIF /tmp/testdisk.dmg
    /// hdiutil attach -nomount /tmp/testdisk.dmg          # prints /dev/diskN
    /// COPPERLINE_TEST_DISK=diskN cargo test --release \
    ///     blockdev::tests::device_round_trip -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes to a real device named by COPPERLINE_TEST_DISK"]
    fn device_round_trip() {
        let Ok(id) = std::env::var("COPPERLINE_TEST_DISK") else {
            panic!("set COPPERLINE_TEST_DISK to a disposable device (e.g. disk4)");
        };
        let device = find_device(&id)
            .expect("enumerate")
            .unwrap_or_else(|| panic!("no device {id}"));
        println!("device: {}", device.label());
        println!(
            "  block size {}, {} guest sectors",
            device.block_size,
            device.guest_sectors()
        );
        assert!(
            device.safety.openable(),
            "refusing to test against {id}: {}",
            device.safety.tag()
        );

        let mut disk = open_device(&device, true).expect("open for writing");
        assert_eq!(disk.total_sectors(), device.guest_sectors());

        let sector = crate::harddrive::SECTOR_SIZE;
        // Sector 1, so a botched test does not land on block zero, where an
        // RDB would live.
        let lba = 1;
        let mut original = vec![0u8; sector];
        disk.read_sector(lba, &mut original).expect("read");

        let mut written = vec![0u8; sector];
        for (i, byte) in written.iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        disk.write_sector(lba, &written).expect("write");
        disk.flush().expect("flush");

        let mut read_back = vec![0u8; sector];
        disk.read_sector(lba, &mut read_back).expect("read back");
        assert_eq!(read_back, written, "what was written must read back");
        println!("  wrote and read back {sector} bytes at sector {lba}");

        // A neighbour inside the same media block must be untouched, which is
        // what proves the read-modify-write is not clobbering the block.
        let neighbour = lba + 1;
        if neighbour < disk.total_sectors() {
            let mut before = vec![0u8; sector];
            disk.read_sector(neighbour, &mut before).expect("neighbour");
            disk.write_sector(lba, &original).expect("restore");
            let mut after = vec![0u8; sector];
            disk.read_sector(neighbour, &mut after).expect("neighbour");
            assert_eq!(before, after, "a write must not disturb its neighbours");
            println!("  neighbouring sector untouched by the write");
        }

        // Past the end is refused rather than wrapping or corrupting.
        let mut buf = vec![0u8; sector];
        assert!(disk.read_sector(disk.total_sectors(), &mut buf).is_err());
        println!("  reads past the end are refused");
    }

    /// Guest sectors are 512 bytes whatever the media's own block size is.
    #[test]
    fn guest_sectors_count_in_512_byte_units() {
        let mut d = device("disk4", 4096 * 1000, Safety::Offerable);
        d.block_size = 4096;
        assert_eq!(d.guest_sectors(), 8000);
    }
}
