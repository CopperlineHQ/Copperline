// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared hard-drive image backend: the sector store behind both the Gayle
//! IDE drives and the A2091 SCSI targets.
//!
//! A unit opens from a raw HDF image file or from a host directory (built
//! into an in-memory FFS volume at open time, see `dirfs.rs`). Bare
//! partition hardfiles (a filesystem boot block at sector 0, no RDSK) get a
//! synthesized RDB cylinder prepended so the ROM boot driver can
//! mount them without pre-conversion.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const SECTOR_SIZE: usize = 512;

// HDToolBox-default RDB geometry: 16 surfaces x 32 sectors. Used both for
// the IDE IDENTIFY translation default and for the RDB synthesized in front
// of bare partition hardfiles.
pub const RDB_HEADS: u32 = 16;
pub const RDB_SPT: u32 = 32;
/// Sectors in one cylinder of the RDB geometry (also the size of the
/// synthesized RDB area, which occupies cylinder 0).
pub const CYL_SECTORS: u32 = RDB_HEADS * RDB_SPT;
pub const CYL_BYTES: u64 = CYL_SECTORS as u64 * SECTOR_SIZE as u64;
/// The RDSK block may live in any of the first 16 sectors (RDB_LOCATION_LIMIT).
const RDB_LOCATION_LIMIT: usize = 16;

/// Sector storage behind a drive: a host image file, or an in-memory image
/// (a filesystem built from a host directory) whose writes last only for
/// the session.
enum Backing {
    File(File),
    Memory(Vec<u8>),
    /// A real disk attached to the host. Presents 512-byte sectors like the
    /// others; the block size the media actually uses, and the privilege the
    /// open needed, are the device's own business.
    #[cfg(not(target_arch = "wasm32"))]
    Device(Box<crate::blockdev::BlockDevice>),
}

pub struct HardDriveImage {
    path: PathBuf,
    backing: Backing,
    total_sectors: u64,
    /// Synthesized RDB cylinder prepended to a bare partition hardfile (an
    /// image whose boot block starts with 'DOS\xNN' and that carries no RDSK
    /// of its own). LBAs inside it are served from this buffer and the file
    /// shifts up by one cylinder; writes to it stay in memory for the
    /// session. None for images that contain their own RDB.
    rdb_overlay: Option<Vec<u8>>,
    overlay_write_warned: bool,
    /// Lowercase bus tag ("ide"/"scsi") for log messages.
    bus_name: &'static str,
}

/// Serde shadow of `HardDriveImage`. File-backed images store only the host
/// path (the sectors live in the file, which a save state does not capture);
/// in-memory directory volumes are embedded whole so their session-only
/// writes survive the round trip. Deserialization reopens file backings,
/// which makes a missing/moved image a load-time error instead of a later
/// I/O panic.
#[derive(serde::Serialize, serde::Deserialize)]
struct HardDriveImageState {
    path: PathBuf,
    memory: Option<Vec<u8>>,
    total_sectors: u64,
    rdb_overlay: Option<Vec<u8>>,
    overlay_write_warned: bool,
    scsi_bus: bool,
}

impl serde::Serialize for HardDriveImage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        HardDriveImageState {
            path: self.path.clone(),
            memory: match &self.backing {
                Backing::Memory(image) => Some(image.clone()),
                // A physical disk is not ours to capture: the medium lives
                // outside the emulator and can be swapped while a state is
                // saved. Only the path is recorded, and reopening it asks for
                // permission again.
                _ => None,
            },
            total_sectors: self.total_sectors,
            rdb_overlay: self.rdb_overlay.clone(),
            overlay_write_warned: self.overlay_write_warned,
            scsi_bus: self.bus_name == "scsi",
        }
        .serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for HardDriveImage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let state = HardDriveImageState::deserialize(deserializer)?;
        let backing = match state.memory {
            Some(image) => Backing::Memory(image),
            None => Backing::File(
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&state.path)
                    .map_err(|e| {
                        serde::de::Error::custom(format!(
                            "reopening hard-drive image {}: {e}",
                            state.path.display()
                        ))
                    })?,
            ),
        };
        Ok(Self {
            path: state.path,
            backing,
            total_sectors: state.total_sectors,
            rdb_overlay: state.rdb_overlay,
            overlay_write_warned: state.overlay_write_warned,
            bus_name: if state.scsi_bus { "scsi" } else { "ide" },
        })
    }
}

fn put_be32(block: &mut [u8], offset: usize, value: u32) {
    block[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

/// Set the longword at offset 8 so the first 64 big-endian longs sum to 0,
/// the checksum rule shared by RDSK and PART blocks.
fn rdb_checksum(block: &mut [u8]) {
    put_be32(block, 8, 0);
    let mut sum = 0u32;
    for i in 0..64 {
        sum = sum.wrapping_add(u32::from_be_bytes(
            block[i * 4..i * 4 + 4].try_into().unwrap(),
        ));
    }
    put_be32(block, 8, 0u32.wrapping_sub(sum));
}

/// Build the RDSK block for a synthesized RDB: `total_cyls` cylinders of
/// 16x32 geometry, RDB occupying cylinder 0, partitionable space from
/// cylinder 1. `disk_label` fills the vendor/product identity fields.
fn build_rdsk_block(total_cyls: u32, disk_label: &str) -> Vec<u8> {
    let mut b = vec![0u8; SECTOR_SIZE];
    b[0..4].copy_from_slice(b"RDSK");
    put_be32(&mut b, 4, 64); // size in longs
    put_be32(&mut b, 12, 7); // host id
    put_be32(&mut b, 16, SECTOR_SIZE as u32);
    put_be32(&mut b, 20, 0x17); // flags: last disk/LUN/ID
    put_be32(&mut b, 24, !0); // bad-block list: none
    put_be32(&mut b, 28, 1); // partition list at sector 1
    put_be32(&mut b, 32, !0); // filesystem-header list: none
    put_be32(&mut b, 36, !0); // drive init
    put_be32(&mut b, 40, !0);
    for off in (44..64).step_by(4) {
        put_be32(&mut b, off, !0);
    }
    put_be32(&mut b, 64, total_cyls);
    put_be32(&mut b, 68, RDB_SPT);
    put_be32(&mut b, 72, RDB_HEADS);
    put_be32(&mut b, 76, 1); // interleave
    put_be32(&mut b, 80, total_cyls); // park cylinder
    put_be32(&mut b, 96, !0); // write precomp
    put_be32(&mut b, 100, !0); // reduced write
    put_be32(&mut b, 104, 3); // step rate
    put_be32(&mut b, 128, 0); // rdb blocks low
    put_be32(&mut b, 132, CYL_SECTORS - 1); // rdb blocks high
    put_be32(&mut b, 136, 1); // lo cylinder
    put_be32(&mut b, 140, total_cyls - 1); // hi cylinder
    put_be32(&mut b, 144, CYL_SECTORS); // blocks per cylinder
                                        // Vendor (8) + product (16) identity, then the revision.
    let mut label = disk_label.as_bytes().to_vec();
    label.resize(24, b' ');
    b[160..184].copy_from_slice(&label);
    b[184..188].copy_from_slice(b"1.0 ");
    rdb_checksum(&mut b);
    b
}

/// Build the PART block: one partition (device `name`, e.g. DH0) spanning
/// cylinders 1..total_cyls-1 with the dostype found in the image's boot block.
/// `boot_pri` becomes `de_BootPri`, which is what strap ranks boot nodes by;
/// the `PBFB_BOOTABLE` flag is cleared for the `BOOT_PRI_NEVER` sentinel so
/// such a partition mounts without ever being a boot candidate.
fn build_part_block(total_cyls: u32, dostype: u32, name: &[u8], boot_pri: i8) -> Vec<u8> {
    let mut b = vec![0u8; SECTOR_SIZE];
    b[0..4].copy_from_slice(b"PART");
    put_be32(&mut b, 4, 64);
    put_be32(&mut b, 12, 7); // host id
    put_be32(&mut b, 16, !0); // next partition: none
    let bootable = u32::from(boot_pri != crate::config::BOOT_PRI_NEVER);
    put_be32(&mut b, 20, bootable); // flags: PBFB_BOOTABLE
    let name = &name[..name.len().min(31)];
    b[36] = name.len() as u8;
    b[37..37 + name.len()].copy_from_slice(name);
    let env: [u32; 17] = [
        16,                       // table size
        (SECTOR_SIZE / 4) as u32, // longs per block
        0,                        // sec org
        RDB_HEADS,
        1, // sectors per block
        RDB_SPT,
        2, // DOS reserved blocks
        0, // prealloc
        0, // interleave
        1, // low cylinder
        total_cyls - 1,
        30,                         // buffers
        0,                          // buffer memory type
        0x00FF_FFFF,                // max transfer
        0x7FFF_FFFE,                // mask
        i32::from(boot_pri) as u32, // boot priority (de_BootPri, signed)
        dostype,
    ];
    for (i, v) in env.iter().enumerate() {
        put_be32(&mut b, 128 + i * 4, *v);
    }
    rdb_checksum(&mut b);
    b
}

impl HardDriveImage {
    /// Attach a real host disk in place of an image.
    ///
    /// The device is identified the way a configuration names it (`disk4`,
    /// `sdb`, `PhysicalDrive1`) rather than by node path, because a node path
    /// belongs to this boot and the hardware does not. Opening may ask the
    /// user for permission; the disk the host is running from is refused
    /// before that point, by [`crate::blockdev::open_device`].
    ///
    /// Unlike an image, nothing here synthesizes an RDB. A disk out of an
    /// Amiga carries its own partition table, and inventing one over the top
    /// of unfamiliar bytes is exactly how real media gets damaged: if it is
    /// not an Amiga disk, the guest should be told so, not handed a fiction.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_device(id: &str, bus_name: &'static str, writable: bool) -> anyhow::Result<Self> {
        let device = crate::blockdev::find_device(id)?
            .ok_or_else(|| anyhow::anyhow!("no host disk called {id}"))?;
        // A disk the host has mounted is not refused: taking it is the point,
        // and the platform layer asks the host to let go before opening it.
        // Say which volumes are going, because the user may have something
        // open on one of them.
        if !device.mounted.is_empty() {
            log::info!(
                "{bus_name}: {id} is mounted ({}); taking it from the host",
                device.mounted.join(", ")
            );
        }
        let opened = crate::blockdev::open_device(&device, writable)?;
        let total_sectors = opened.total_sectors();
        if total_sectors == 0 {
            anyhow::bail!("{id} reports no capacity");
        }
        log::info!(
            "{bus_name}: {} attached as a physical disk: {}, {} sectors, {}",
            id,
            device.label(),
            total_sectors,
            if writable {
                "WRITABLE -- the guest can change this disk"
            } else {
                "read-only"
            }
        );
        Ok(Self {
            path: PathBuf::from(&device.path),
            backing: Backing::Device(Box::new(opened)),
            total_sectors,
            rdb_overlay: None,
            overlay_write_warned: false,
            bus_name,
        })
    }

    /// Open a drive image. `device_name` is the DOS device name (e.g.
    /// "DH0") a synthesized RDB advertises; `bus_name` ("ide"/"scsi") tags
    /// log messages; `disk_label` fills the RDSK vendor/product identity.
    /// The path may be a raw HDF image file, or a host directory, which is
    /// built into an in-memory FFS volume at open time. `volume_override`
    /// names that FFS volume; when `None` the directory name is used. It has
    /// no effect on a raw HDF, which carries its own label inside the image.
    /// `boot_pri` is the `de_BootPri` written into a synthesized RDB; it too is
    /// ignored by an image that carries its own RDB.
    pub fn open(
        path: &Path,
        device_name: &str,
        bus_name: &'static str,
        disk_label: &str,
        volume_override: Option<&str>,
        boot_pri: i8,
    ) -> anyhow::Result<Self> {
        let mut backing = if path.is_dir() {
            let volume = volume_override.map(str::to_string).unwrap_or_else(|| {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            let image = crate::dirfs::build_ffs_image(path, &volume)?;
            log::warn!(
                "{bus_name}: {} mounted as an in-memory volume \"{volume}\" built from the \
                 directory; guest writes to it are NOT written back to the host and are lost \
                 at exit",
                path.display()
            );
            Backing::Memory(image)
        } else {
            if volume_override.is_some() {
                log::warn!(
                    "{bus_name}: {} is a raw HDF image; the configured drive name is ignored \
                     (the volume label lives inside the image)",
                    path.display()
                );
            }
            Backing::File(
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(path)
                    .map_err(|e| {
                        anyhow::anyhow!("opening {bus_name} image {}: {e}", path.display())
                    })?,
            )
        };
        let len = match &backing {
            Backing::File(file) => file
                .metadata()
                .map_err(|e| anyhow::anyhow!("stat {bus_name} image {}: {e}", path.display()))?
                .len(),
            Backing::Memory(image) => image.len() as u64,
            #[cfg(not(target_arch = "wasm32"))]
            Backing::Device(device) => device.total_sectors() * SECTOR_SIZE as u64,
        };
        if len == 0 || len % SECTOR_SIZE as u64 != 0 {
            anyhow::bail!(
                "{bus_name} image {} is {} bytes; expected a non-empty multiple of {} (raw HDF)",
                path.display(),
                len,
                SECTOR_SIZE
            );
        }

        // Sniff the first sectors: a bare partition hardfile (filesystem boot
        // block at sector 0, no RDSK block in the first 16 sectors) gets a
        // synthesized RDB cylinder in front so the driver can mount it
        // without any pre-conversion step.
        let sniff_len = (len as usize).min(RDB_LOCATION_LIMIT * SECTOR_SIZE);
        let mut head = vec![0u8; sniff_len];
        match &mut backing {
            Backing::File(file) => file
                .read_exact(&mut head)
                .map_err(|e| anyhow::anyhow!("reading {bus_name} image {}: {e}", path.display()))?,
            Backing::Memory(image) => head.copy_from_slice(&image[..sniff_len]),
            #[cfg(not(target_arch = "wasm32"))]
            Backing::Device(device) => {
                for (lba, sector) in head.chunks_mut(SECTOR_SIZE).enumerate() {
                    device.read_sector(lba as u64, sector).map_err(|e| {
                        anyhow::anyhow!("reading {bus_name} device {}: {e}", path.display())
                    })?;
                }
            }
        }
        let has_rdsk = head
            .chunks(SECTOR_SIZE)
            .any(|sector| sector.get(..4) == Some(b"RDSK"));
        let bare_partition = !has_rdsk && head.get(..3) == Some(b"DOS");

        let rdb_overlay = if bare_partition {
            if len % CYL_BYTES != 0 {
                anyhow::bail!(
                    "{bus_name} image {} looks like a bare partition hardfile ({} bytes) but is \
                     not a multiple of {} (16 surfaces x 32 sectors); cannot wrap it in \
                     an RDB without guessing geometry",
                    path.display(),
                    len,
                    CYL_BYTES
                );
            }
            let part_cyls = (len / CYL_BYTES) as u32;
            let total_cyls = 1 + part_cyls;
            let dostype = u32::from_be_bytes(head[..4].try_into().unwrap());
            let mut overlay = vec![0u8; CYL_BYTES as usize];
            overlay[..SECTOR_SIZE].copy_from_slice(&build_rdsk_block(total_cyls, disk_label));
            overlay[SECTOR_SIZE..2 * SECTOR_SIZE].copy_from_slice(&build_part_block(
                total_cyls,
                dostype,
                device_name.as_bytes(),
                boot_pri,
            ));
            log::info!(
                "{bus_name}: {} is a bare partition hardfile (dostype {:08X}); wrapping it in \
                 a synthesized RDB ({} cylinders, partition {} on 1-{}, bootpri {})",
                path.display(),
                dostype,
                total_cyls,
                device_name,
                total_cyls - 1,
                boot_pri
            );
            Some(overlay)
        } else {
            if boot_pri != crate::config::HARDFILE_DEFAULT_BOOT_PRI {
                log::warn!(
                    "{bus_name}: {} carries its own RDB; the configured bootpri ({boot_pri}) is \
                     ignored (the partition priorities live inside the image)",
                    path.display()
                );
            }
            None
        };

        let overlay_sectors = rdb_overlay.as_ref().map_or(0, |_| u64::from(CYL_SECTORS));
        let total_sectors = len / SECTOR_SIZE as u64 + overlay_sectors;
        Ok(Self {
            path: path.to_path_buf(),
            backing,
            total_sectors,
            rdb_overlay,
            overlay_write_warned: false,
            bus_name,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether this drive is a real disk of the host's rather than an image.
    ///
    /// Only a real disk cares: an image file can be opened again by anyone at
    /// any time, but a physical disk is held exclusively, and while it is held
    /// the host cannot have it back and nothing else can open it.
    pub fn is_host_disk(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            matches!(self.backing, Backing::Device(_))
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }

    pub fn total_sectors(&self) -> u64 {
        self.total_sectors
    }

    /// Sectors served from the synthesized RDB overlay; file LBAs shift down
    /// by this much.
    fn overlay_sectors(&self) -> u64 {
        self.rdb_overlay
            .as_ref()
            .map_or(0, |_| u64::from(CYL_SECTORS))
    }

    pub fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> std::io::Result<()> {
        if let Some(overlay) = &self.rdb_overlay {
            if lba < u64::from(CYL_SECTORS) {
                let off = lba as usize * SECTOR_SIZE;
                buf[..SECTOR_SIZE].copy_from_slice(&overlay[off..off + SECTOR_SIZE]);
                return Ok(());
            }
        }
        let file_lba = lba - self.overlay_sectors();
        match &mut self.backing {
            Backing::File(file) => {
                file.seek(SeekFrom::Start(file_lba * SECTOR_SIZE as u64))?;
                file.read_exact(&mut buf[..SECTOR_SIZE])
            }
            Backing::Memory(image) => {
                let off = file_lba as usize * SECTOR_SIZE;
                buf[..SECTOR_SIZE].copy_from_slice(&image[off..off + SECTOR_SIZE]);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            Backing::Device(device) => device.read_sector(file_lba, buf),
        }
    }

    pub fn write_sector(&mut self, lba: u64, buf: &[u8]) -> std::io::Result<()> {
        if let Some(overlay) = &mut self.rdb_overlay {
            if lba < u64::from(CYL_SECTORS) {
                if !self.overlay_write_warned {
                    self.overlay_write_warned = true;
                    log::warn!(
                        "{}: {}: write to the synthesized RDB area; partition-table \
                         changes are kept in memory and will be lost at exit",
                        self.bus_name,
                        self.path.display()
                    );
                }
                let off = lba as usize * SECTOR_SIZE;
                overlay[off..off + SECTOR_SIZE].copy_from_slice(&buf[..SECTOR_SIZE]);
                return Ok(());
            }
        }
        let file_lba = lba - self.overlay_sectors();
        match &mut self.backing {
            Backing::File(file) => {
                file.seek(SeekFrom::Start(file_lba * SECTOR_SIZE as u64))?;
                file.write_all(&buf[..SECTOR_SIZE])
            }
            Backing::Memory(image) => {
                let off = file_lba as usize * SECTOR_SIZE;
                image[off..off + SECTOR_SIZE].copy_from_slice(&buf[..SECTOR_SIZE]);
                Ok(())
            }
            #[cfg(not(target_arch = "wasm32"))]
            Backing::Device(device) => device.write_sector(file_lba, buf),
        }
    }

    pub fn flush(&mut self) {
        // A real disk can be pulled out of its reader, so anything still held
        // back is a hazard an image file does not have.
        #[cfg(not(target_arch = "wasm32"))]
        if let Backing::Device(device) = &mut self.backing {
            if let Err(e) = device.flush() {
                log::warn!("{}: {}: flush failed: {e}", self.bus_name, device.id());
            }
            return;
        }
        if let Backing::File(file) = &mut self.backing {
            if let Err(e) = file.flush() {
                log::warn!(
                    "{} image {}: flush failed: {e}",
                    self.bus_name,
                    self.path.display()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_image(name: &str, bytes: &[u8]) -> PathBuf {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "copperline-harddrive-{}-{unique}-{name}",
            std::process::id()
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn serde_reopens_file_backed_image_and_serves_same_sectors() {
        let mut bytes = vec![0u8; 64 * SECTOR_SIZE];
        bytes[5 * SECTOR_SIZE..5 * SECTOR_SIZE + 4].copy_from_slice(b"MARK");
        let path = temp_image("serde.hdf", &bytes);
        let mut original = HardDriveImage::open(&path, "DH0", "ide", "TEST DISK", None, 0).unwrap();

        let encoded = bincode::serialize(&original).unwrap();
        let mut restored: HardDriveImage = bincode::deserialize(&encoded).unwrap();
        assert_eq!(restored.total_sectors(), original.total_sectors());
        assert_eq!(restored.path(), original.path());
        let mut a = vec![0u8; SECTOR_SIZE];
        let mut b = vec![0u8; SECTOR_SIZE];
        for lba in [0u64, 5, 63, original.total_sectors() - 1] {
            original.read_sector(lba, &mut a).unwrap();
            restored.read_sector(lba, &mut b).unwrap();
            assert_eq!(a, b, "lba {lba}");
        }

        // Writes through the restored handle land in the same backing file.
        let sector = vec![0x5A; SECTOR_SIZE];
        restored.write_sector(7, &sector).unwrap();
        restored.flush();
        original.read_sector(7, &mut a).unwrap();
        assert_eq!(a, sector);

        let _ = std::fs::remove_file(&path);
    }

    /// Read every sector of an image and return the FFS volume label found in
    /// its root block (block type 2 / secondary type 1). The directory mount
    /// prepends a synthesized RDB, so the root block is not at a fixed LBA;
    /// scan for it instead.
    fn volume_label(disk: &mut HardDriveImage) -> String {
        let mut sector = vec![0u8; SECTOR_SIZE];
        for lba in 0..disk.total_sectors() {
            disk.read_sector(lba, &mut sector).unwrap();
            let be32 = |off: usize| u32::from_be_bytes(sector[off..off + 4].try_into().unwrap());
            if be32(0) == 2 && be32(SECTOR_SIZE - 4) == 1 {
                let name_base = SECTOR_SIZE - 80;
                let len = sector[name_base] as usize;
                return String::from_utf8_lossy(&sector[name_base + 1..name_base + 1 + len])
                    .into_owned();
            }
        }
        panic!("no FFS root block found");
    }

    /// A uniquely-named parent temp dir holding a short-named child directory
    /// (returned) with one file. The child's name is short enough that the
    /// 30-character FFS volume-label limit does not truncate it.
    fn temp_mount_dir(child: &str) -> (PathBuf, PathBuf) {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "copperline-harddrive-dir-{}-{unique}",
            std::process::id()
        ));
        let mount = parent.join(child);
        std::fs::create_dir_all(&mount).unwrap();
        std::fs::write(mount.join("hello.txt"), b"hi\n").unwrap();
        (parent, mount)
    }

    #[test]
    fn directory_mount_uses_the_directory_name_as_the_volume_label() {
        let (parent, mount) = temp_mount_dir("Games");
        let mut disk = HardDriveImage::open(&mount, "DH0", "ide", "TEST DISK", None, 0).unwrap();
        assert_eq!(volume_label(&mut disk), "Games");
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn directory_mount_volume_override_sets_the_label() {
        let (parent, mount) = temp_mount_dir("Games");
        let mut disk =
            HardDriveImage::open(&mount, "DH0", "ide", "TEST DISK", Some("Workbench"), 0).unwrap();
        assert_eq!(volume_label(&mut disk), "Workbench");
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn serde_errors_when_backing_file_is_missing() {
        let path = temp_image("gone.hdf", &vec![0u8; 8 * SECTOR_SIZE]);
        let original = HardDriveImage::open(&path, "DH0", "ide", "TEST DISK", None, 0).unwrap();
        let encoded = bincode::serialize(&original).unwrap();
        let _ = std::fs::remove_file(&path);
        let Err(err) = bincode::deserialize::<HardDriveImage>(&encoded) else {
            panic!("deserializing with the image file gone must fail");
        };
        assert!(err.to_string().contains("reopening hard-drive image"));
    }

    /// A bare partition hardfile: an FFS boot block and no RDSK, sized to a
    /// whole number of 16x32 cylinders so the RDB wrapper can be synthesized.
    fn bare_partition_image(name: &str) -> PathBuf {
        let mut bytes = vec![0u8; CYL_BYTES as usize];
        bytes[..4].copy_from_slice(b"DOS\x01");
        temp_image(name, &bytes)
    }

    /// The PART block of a synthesized RDB (LBA 1), read back through the
    /// overlay exactly as the guest driver sees it.
    fn part_block(disk: &mut HardDriveImage) -> Vec<u8> {
        let mut sector = vec![0u8; SECTOR_SIZE];
        disk.read_sector(1, &mut sector).unwrap();
        assert_eq!(&sector[..4], b"PART");
        sector
    }

    fn be32(block: &[u8], off: usize) -> u32 {
        u32::from_be_bytes(block[off..off + 4].try_into().unwrap())
    }

    /// `de_BootPri` is env word 15, at PART offset 128 + 15*4.
    const DE_BOOT_PRI_OFF: usize = 128 + 15 * 4;
    const PART_FLAGS_OFF: usize = 20;

    #[test]
    fn synthesized_partition_carries_the_configured_boot_priority() {
        let path = bare_partition_image("bootpri.hdf");
        for pri in [0i8, 6, 20, -5, 127] {
            let mut disk =
                HardDriveImage::open(&path, "DH0", "ide", "TEST DISK", None, pri).unwrap();
            let part = part_block(&mut disk);
            assert_eq!(
                be32(&part, DE_BOOT_PRI_OFF) as i32,
                i32::from(pri),
                "de_BootPri for {pri}"
            );
            assert_eq!(be32(&part, PART_FLAGS_OFF), 1, "PBFB_BOOTABLE for {pri}");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn boot_pri_never_mounts_the_partition_without_making_it_bootable() {
        let path = bare_partition_image("neverboot.hdf");
        let mut disk = HardDriveImage::open(
            &path,
            "DH1",
            "scsi",
            "TEST DISK",
            None,
            crate::config::BOOT_PRI_NEVER,
        )
        .unwrap();
        let part = part_block(&mut disk);
        assert_eq!(
            be32(&part, DE_BOOT_PRI_OFF) as i32,
            i32::from(crate::config::BOOT_PRI_NEVER)
        );
        assert_eq!(
            be32(&part, PART_FLAGS_OFF),
            0,
            "PBFB_BOOTABLE must be clear so strap never offers the partition"
        );
        // The partition still mounts: the device name is intact.
        assert_eq!(part[36], 3);
        assert_eq!(&part[37..40], b"DH1");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn part_block_checksum_stays_valid_for_every_boot_priority() {
        let path = bare_partition_image("checksum.hdf");
        for pri in [i8::MIN, -1, 0, 1, i8::MAX] {
            let mut disk =
                HardDriveImage::open(&path, "DH0", "ide", "TEST DISK", None, pri).unwrap();
            let part = part_block(&mut disk);
            let sum = (0..64).fold(0u32, |acc, i| acc.wrapping_add(be32(&part, i * 4)));
            assert_eq!(sum, 0, "RDB checksum rule broken for bootpri {pri}");
        }
        let _ = std::fs::remove_file(&path);
    }
}
