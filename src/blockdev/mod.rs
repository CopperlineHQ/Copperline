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

// A platform backend supplies three things: `list_devices`, `take_disks`
// (get the host to give the named disks up, asking for whatever privilege
// that costs, and return something that holds them), and `lend` (turn one
// held disk into a [`BlockDevice`] for a machine, leaving the hold in
// place). Everything above that -- the safety rules, which disks are held
// and on what terms, when to ask and when a held disk is simply lent again
// -- is decided here, once, so a rule added here reaches every host.
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod platform;
#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod platform;

use std::path::PathBuf;

/// Neither Windows nor Linux has a broker of its own that hands back an opened
/// device -- macOS's `authopen` has no equivalent on either -- so Copperline is
/// one for itself: this is the privileged half, and the flag that reaches it.
/// What is behind the flag differs by host and is private to the two halves
/// that speak it; see the [`platform`] module docs for why it is a separate
/// process and what it will refuse.
#[cfg(any(windows, target_os = "linux"))]
pub use broker_protocol::BROKER_FLAG;
#[cfg(any(windows, target_os = "linux"))]
pub use platform::serve_broker_request;

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
    /// Whether the medium has said it cannot flush. Some USB bridges have no
    /// cache-flush command; the fact is worth one line, not one per write.
    flush_unsupported: bool,
    /// Volumes the host had mounted from this disk, locked and dismounted for
    /// as long as the machine has it.
    ///
    /// Only Windows has this: a lock there lasts exactly as long as the handle
    /// that took it, so something must hold them or the host mounts the disk
    /// back underneath a running emulator. Declared after `file` so the disk
    /// handle closes first, and the host is let back in only once the machine
    /// has finished with it.
    #[cfg(windows)]
    dismounted: Vec<std::fs::File>,
}

/// A machine letting go of its copy. The reservation holds its own, so this
/// is not the disk going back to the host -- that is [`release_device`], and
/// it says so itself. Worth a line only to somebody tracing handle lifetimes.
impl Drop for BlockDevice {
    fn drop(&mut self) {
        log::debug!("blockdev: the machine let go of {}", self.id);
    }
}

impl BlockDevice {
    /// Wrap an already-open device. The handle is expected to have come from
    /// a platform opener, which is where privilege is dealt with.
    ///
    /// A platform opener is the only caller, and not every platform has
    /// written one yet, so on those this is built but never reached.
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux", windows)),
        allow(dead_code)
    )]
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
            flush_unsupported: false,
            #[cfg(windows)]
            dismounted: Vec::new(),
        }
    }

    /// Hold the volumes a backend had to take from the host to get this disk.
    ///
    /// Only Windows calls this. Elsewhere an unmount stands on its own once
    /// done; a Windows lock lives exactly as long as the handle that took it,
    /// so the device has to carry them until it goes back.
    #[cfg(windows)]
    pub(crate) fn holding(mut self, volumes: Vec<std::fs::File>) -> Self {
        self.dismounted = volumes;
        self
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
                     was refused; tick R/W (or drop `read_only`) to let it write",
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
    ///
    /// What that takes differs by host. On Linux and Windows the handle is a
    /// buffered block device, and `sync_data` empties the kernel's cache and
    /// asks the device to empty its own. macOS's raw character device has no
    /// kernel cache to empty and answers `fsync` with `ENOTTY`; what it does
    /// answer is the disk ioctl asking the device itself to flush, so that is
    /// what is used there. A medium that says it cannot flush at all -- some
    /// USB bridges have no such command -- is recorded and not asked again:
    /// the guest flushes after every write command, and one line says what
    /// thousands would.
    pub fn flush(&mut self) -> std::io::Result<()> {
        if !self.writable || self.flush_unsupported {
            return Ok(());
        }
        match flush_medium(&self.file) {
            Ok(()) => Ok(()),
            Err(error) if flush_says_unsupported(&error) => {
                self.flush_unsupported = true;
                log::info!(
                    "blockdev: {} cannot flush its cache ({error}); writes go straight through",
                    self.id
                );
                Ok(())
            }
            Err(error) => Err(error),
        }
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

/// Ask the medium to make everything written to it durable.
///
/// macOS's raw character device has its own way of saying this (`fsync`
/// there is `ENOTTY`); everywhere else `sync_data` is exactly it.
#[cfg(target_os = "macos")]
fn flush_medium(file: &std::fs::File) -> std::io::Result<()> {
    platform::flush_medium(file)
}

#[cfg(not(target_os = "macos"))]
fn flush_medium(file: &std::fs::File) -> std::io::Result<()> {
    file.sync_data()
}

/// Whether a flush failure means "this medium has no such command" rather
/// than "the flush failed". The first is a property of the hardware, said
/// once; the second is an event, reported every time.
#[cfg(unix)]
fn flush_says_unsupported(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENOTTY) | Some(libc::ENOTSUP)
    )
}

/// `ERROR_INVALID_FUNCTION` and `ERROR_NOT_SUPPORTED`, which are how a driver
/// without a flush path answers `FlushFileBuffers`.
#[cfg(windows)]
fn flush_says_unsupported(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(1) | Some(50))
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

/// Refuse a device nothing should be allowed to touch, whatever asks.
///
/// Every route to a real disk goes through here first, so a backend never gets
/// an opinion on the system disk and no later path can skip the question by
/// taking a different door.
fn refuse_if_unusable(device: &HostDevice, write: bool) -> anyhow::Result<()> {
    // The read-modify-write path assumes a guest sector sits wholly inside one
    // media block. Odd block sizes exist (520-byte SCSI media, formatted for a
    // controller's own use), and one would have it straddling two.
    if !(device.block_size as usize).is_multiple_of(crate::harddrive::SECTOR_SIZE) {
        anyhow::bail!(
            "{} has {}-byte blocks, which are not whole {}-byte sectors",
            device.id,
            device.block_size,
            crate::harddrive::SECTOR_SIZE
        );
    }
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
    Ok(())
}

/// One disk taken from the host and not yet given back.
///
/// What "taken" physically is belongs to the backend -- a descriptor whose
/// `O_EXCL` outlives any machine, a Windows handle whose volume locks must be
/// held -- and lives in its [`platform::Held`]. What is uniform is the terms:
/// which disk, and whether it was taken for writing, because a descriptor's
/// access is settled at open and a change of terms means taking it again.
struct Reservation {
    id: String,
    write: bool,
    held: platform::Held,
}

/// Disks taken from the host, held for the whole emulator session.
///
/// Taking happens where somebody asked for the disk -- the launcher's Mount
/// button, or the first machine built from a configuration -- and whatever
/// permission it costs is asked for then, once. A machine is *lent* the disk:
/// powering off drops the machine's copy but not this one, so powering back
/// on finds the disk still in hand rather than raising a second prompt, and
/// the host stays excluded for exactly as long as the launcher says the disk
/// is attached.
static RESERVED: std::sync::Mutex<Vec<Reservation>> = std::sync::Mutex::new(Vec::new());

fn reserved() -> std::sync::MutexGuard<'static, Vec<Reservation>> {
    // A panic elsewhere must not make the disks unreachable: what is behind
    // this lock is a list of open handles, and losing it would strand them --
    // and the exclusion they hold over the host -- until the process ends.
    RESERVED.lock().unwrap_or_else(|held| held.into_inner())
}

/// Lend a reserved disk to a machine, if one is held on exactly these terms.
fn lend_reserved(device: &HostDevice, write: bool) -> Option<BlockDevice> {
    let held = reserved();
    let entry = held
        .iter()
        .find(|entry| entry.id == device.id && entry.write == write)?;
    platform::lend(device, write, &entry.held)
        .map_err(|error| {
            log::warn!(
                "blockdev: {} could not be lent to the machine: {error:#}",
                device.id
            );
        })
        .ok()
}

/// Take disks and keep them, resolving each identifier first.
fn take_and_reserve(wanted: &[(HostDevice, bool)]) -> anyhow::Result<()> {
    let mut needed = Vec::new();
    for (device, write) in wanted {
        // Already held on the same terms is not a second ask: it is somebody
        // pressing Mount again, and the disk is already where they want it.
        if reserved()
            .iter()
            .any(|held| held.id == device.id && held.write == *write)
        {
            continue;
        }
        // Held on other terms has to be taken again, since the access a
        // handle carries was settled when it was opened -- and the old hold
        // goes back first, or the new open meets the exclusion the old one
        // is still enforcing.
        release_device(&device.id);
        needed.push((device.clone(), *write));
    }
    if needed.is_empty() {
        return Ok(());
    }
    let taken = platform::take_disks(&needed)?;
    let mut store = Vec::new();
    for (id, held) in taken {
        let write = needed
            .iter()
            .find(|(device, _)| device.id == id)
            .map(|(_, write)| *write)
            .ok_or_else(|| anyhow::anyhow!("{id} was taken but never asked for"))?;
        store.push(Reservation { id, write, held });
    }
    let names: Vec<&str> = store.iter().map(|entry| entry.id.as_str()).collect();
    log::info!(
        "blockdev: {} taken from the host, ready for the machine",
        names.join(", ")
    );
    reserved().extend(store);
    Ok(())
}

/// Whether attaching a disk will make the host ask for privilege.
///
/// The launcher says so before anybody ticks anything, so the dialog that
/// follows Mount is announced rather than a surprise. Asked of the platform
/// once: what it depends on -- the process's token, its uid -- is settled
/// for the life of the process.
pub fn attaching_needs_privilege() -> bool {
    static ANSWER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ANSWER.get_or_init(platform::taking_needs_privilege)
}

/// Open a device for the emulated machine.
///
/// Refuses the host's own system disk outright, before any platform code
/// runs: that check is not something a backend gets to have an opinion on.
///
/// A disk the launcher already took is lent as it stands -- the permission
/// for it was given at the Mount button, and asking again would raise a
/// dialog for something this process is already holding. One nothing took
/// yet (a machine run straight from a configuration) is taken now and kept,
/// so this is the only time it is asked for: the machine stopping drops its
/// copy, not the hold, and starting again finds the disk still in hand.
pub fn open_device(device: &HostDevice, write: bool) -> anyhow::Result<BlockDevice> {
    refuse_if_unusable(device, write)?;
    if let Some(lent) = lend_reserved(device, write) {
        log::info!("blockdev: {} was already taken from the host", device.id);
        return Ok(lent);
    }
    take_and_reserve(std::slice::from_ref(&(device.clone(), write)))?;
    lend_reserved(device, write).ok_or_else(|| {
        anyhow::anyhow!(
            "{} was taken from the host but could not be used",
            device.id
        )
    })
}

/// Take disks from the host now, ahead of the machine that will use them.
///
/// The launcher settles which disks it wants long before anything runs, and
/// the permission a real disk needs is best asked for there -- where somebody
/// has just asked for them -- rather than minutes later, behind a machine
/// starting up. They are taken together because that permission is a dialog
/// somebody has to read, and one is enough for however many disks were
/// ticked. This is also the whole list: anything held that is not on it was
/// taken for a machine that is no longer being set up, and goes back.
pub fn reserve_devices(disks: &[(String, bool)]) -> anyhow::Result<()> {
    let mut wanted = Vec::new();
    for (id, write) in disks {
        let device = find_device(id)?.ok_or_else(|| anyhow::anyhow!("no host disk called {id}"))?;
        refuse_if_unusable(&device, *write)?;
        wanted.push((device, *write));
    }
    let stale: Vec<String> = reserved()
        .iter()
        .filter(|held| !wanted.iter().any(|(device, _)| device.id == held.id))
        .map(|held| held.id.clone())
        .collect();
    for id in stale {
        release_device(&id);
    }
    take_and_reserve(&wanted)
}

/// Give a disk taken early back to the host, and say whether one was held.
///
/// Nothing held is not a failure: a disk named in a configuration that was
/// never mounted has nothing to hand back. Dropping the hold is what lets
/// the host have the disk again -- though a machine still running keeps its
/// own lent copy until it stops, and the host waits for that too.
pub fn release_device(id: &str) -> bool {
    let mut held = reserved();
    let before = held.len();
    held.retain(|entry| entry.id != id);
    let released = held.len() != before;
    drop(held);
    if released {
        log::info!("blockdev: {id} released back to the host");
    }
    released
}

/// The wire format the Windows and Linux privileged halves share.
///
/// Each of those hosts has no broker of its own that hands back an opened
/// device, so Copperline is one for itself: the same binary run once with
/// privilege, opening what it was asked to and handing the result back. How
/// the handles travel is each host's affair; what is said is not, and lives
/// here so the two halves cannot drift apart. Compiled on every host -- it
/// is a handful of pure string functions -- so its tests run wherever the
/// suite does, not only on the two hosts that speak it.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
mod broker_protocol {
    use anyhow::Result;

    /// The flag that puts this program into its privileged half. Undocumented
    /// in `--help` on purpose: it is one process talking to another, not an
    /// interface, and its arguments only mean anything to the process that
    /// wrote them.
    pub const BROKER_FLAG: &str = "--host-disk-broker";

    /// How one disk is named on the privileged half's command line.
    ///
    /// The identifier is a kernel device name (`sdb`, `PhysicalDrive1`),
    /// which never contains `:` -- and the parse splits on the *last* one,
    /// so even an unexpected identifier round-trips whole.
    pub fn argument(id: &str, write: bool) -> String {
        format!("{id}:{}", if write { "rw" } else { "ro" })
    }

    /// Read one back, refusing anything not in the shape written above.
    pub fn parse_argument(argument: &str) -> Option<(String, bool)> {
        let (id, mode) = argument.rsplit_once(':')?;
        let write = match mode {
            "rw" => true,
            "ro" => false,
            _ => return None,
        };
        (!id.is_empty()).then(|| (id.to_string(), write))
    }

    /// Read the privileged half's answer: `ok` and then one line per disk it
    /// opened, or the one reason it did none of it. What a disk's line says
    /// after its identifier belongs to the host's own half.
    pub fn parse_answer(answer: &str) -> Result<Vec<String>> {
        let mut lines = answer.lines();
        match lines.next().map(str::trim) {
            Some("ok") => {}
            // A refusal it explained is passed on as it wrote it -- it knows
            // why, and this side does not.
            Some(line) => match line.split_once(' ') {
                Some(("error", message)) => anyhow::bail!("{}", message.trim()),
                _ => anyhow::bail!("the answer was in no shape this understands"),
            },
            None => anyhow::bail!("the answer was empty"),
        }
        let opened: Vec<String> = lines
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        if opened.is_empty() {
            anyhow::bail!("it opened nothing");
        }
        Ok(opened)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Both halves of the protocol live in this one place, so agreeing
        /// with itself here is agreeing across the privilege boundary -- on
        /// every host, not only the two that speak it.
        #[test]
        fn the_halves_agree_on_how_a_disk_is_named() {
            for (id, write) in [("sdb", true), ("PhysicalDrive12", false), ("mmcblk0", true)] {
                assert_eq!(
                    parse_argument(&argument(id, write)),
                    Some((id.to_string(), write))
                );
            }
            assert_eq!(parse_argument("sdb:rx"), None);
            assert_eq!(parse_argument(":rw"), None);
            assert_eq!(parse_argument("sdb"), None);
        }

        /// A refusal comes back with the privileged half's own words, and a
        /// malformed answer is refused rather than half-believed.
        #[test]
        fn an_answer_is_believed_only_in_shape() {
            assert_eq!(parse_answer("ok\nsdb\nsdc\n").unwrap(), ["sdb", "sdc"]);
            let error = parse_answer("error sdb is the disk this computer is running from")
                .unwrap_err()
                .to_string();
            assert!(error.contains("running from"), "{error}");
            assert!(parse_answer("").is_err());
            assert!(parse_answer("ok\n").is_err());
            assert!(parse_answer("gibberish").is_err());
        }
    }
}

/// Every whole device the host can see, including ones held back for safety
/// (each carries its [`Safety`], so a caller can show or hide them).
///
/// Opens nothing. Sorted with the devices most likely to be wanted first:
/// offerable before internal, and larger media before smaller, since an Amiga
/// disk is usually the odd small one on a modern host.
pub fn list_devices() -> anyhow::Result<Vec<HostDevice>> {
    let mut devices = platform::list_devices()?;
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
    ///
    /// On Windows the disposable disk is a VHD, which attaches as a physical
    /// disk of its own. Both the attach and the test want Administrator:
    /// Windows gives raw access to a whole disk to nobody else, so an
    /// ordinary shell fails this at the open with that as the reason.
    ///
    /// ```text
    /// diskpart
    ///   create vdisk file=C:\testdisk.vhd maximum=64 type=fixed
    ///   attach vdisk
    ///   list disk                                       # says which number it took
    ///   exit
    /// $env:COPPERLINE_TEST_DISK = 'PhysicalDriveN'
    /// cargo test --release blockdev::tests::device_round_trip -- --ignored --nocapture
    /// ```
    ///
    /// A VHD is not on a bus anything removable arrives on, so it lists as
    /// `internal`: hidden from the launcher, still reachable by name here,
    /// which is what this wants.
    ///
    /// On Linux the disposable disk is a `scsi_debug` one, which appears as a
    /// whole SCSI disk of its own and lists as `internal` for the same reason.
    /// A loop device is *not* the equivalent: loop devices are deliberately
    /// left out of enumeration, so one cannot be reached by name here.
    /// `sector_size` is the useful knob -- it is the only way most machines
    /// have of exercising the read-modify-write path on media whose blocks are
    /// larger than a guest sector.
    ///
    /// ```sh
    /// sudo modprobe scsi_debug dev_size_mb=64            # add sector_size=4096 for 4Kn
    /// copperline --list-disks                            # says which sdN it took
    /// cargo test --release --lib --no-run                # then run that binary as root:
    /// sudo COPPERLINE_TEST_DISK=sdN target/release/deps/copperline-XXXX \
    ///     blockdev::tests::device_round_trip --ignored --nocapture
    /// sudo rmmod scsi_debug
    /// ```
    ///
    /// Run as root deliberately: the test is about the sector translation, and
    /// going through the privileged opener would put a password prompt in the
    /// middle of a test run.
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
