// SPDX-License-Identifier: GPL-3.0-or-later

//! CHD (MAME "Compressed Hunks of Data") hard-disk image backend.
//!
//! A hard-disk CHD, as `chdman createhd` writes one from an HDF, stores the
//! disk as compressed hunks of whole 512-byte sectors and describes it with
//! one `GDDD` metadata entry (`CYLS:401,HEADS:16,SECS:32,BPS:512.`).
//! chdman derives the CHS geometry from the file size and pads the last
//! cylinder with zero sectors, so the disk can come out slightly larger than
//! the hardfile it was made from; `chdman extracthd` gives the original
//! bytes back exactly. The container is the same CHD v5 the CD backend in
//! `cdrom/chd.rs` parses; the sector arithmetic is simpler (LBA to
//! hunk/offset, no track layout, no audio byte order).
//!
//! The `chd` crate is read-only and nothing rewrites compressed hunks in
//! place, so guest writes go to a copy-on-write overlay sidecar beside the
//! image (`disk.chd.wov`, see [`Overlay`]): a write lands in the sidecar,
//! a read checks the sidecar before decompressing, and the CHD itself is
//! never modified. When the sidecar cannot be created the image attaches
//! write-protected, and the controllers report that to the guest the way a
//! write-protected volume is reported.
//!
//! [`media_kind`] tells hard-disk CHDs from CD ones by their metadata tags
//! without decoding a hunk map, which is what the configuration and the
//! window's drop classification use: a `.chd` on a drive slot is a hard
//! disk or a CD-ROM depending on what chdman put in it, not on its name.

use anyhow::{anyhow, bail, Context, Result};
use chd::metadata::{KnownMetadata, Metadata};
use chd::Chd;
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use super::SECTOR_SIZE;

/// Every CHD version starts with this.
const MAGIC: &[u8; 8] = b"MComprHD";
/// The v5 header, the longest of the versions accepted here.
const HEADER_BYTES: usize = 124;
/// The largest hunk MAME's own header validation accepts (`chd.cpp`:
/// `hunkbytes >= 65536 * 256` is rejected). The `chd` crate applies the
/// same bound to v1-v4 headers but skips v5 validation entirely.
const MAX_HUNK_BYTES: u32 = 65536 * 256;
/// The largest disk a hard-disk CHD may describe: 1 TiB, far past anything
/// an Amiga filesystem addresses and small enough that the hunk count fits
/// the `u32` the crate keeps it in at the smallest hunk size.
const MAX_DISK_BYTES: u64 = 1 << 40;
/// A metadata chain longer than this is not chdman's (it writes a handful
/// of entries); the bound keeps a looping chain from spinning the classifier.
const MAX_METADATA_ENTRIES: usize = 256;
/// One metadata entry header: tag, flags and length, next-entry offset.
const METADATA_HEADER_BYTES: usize = 16;

/// What a CHD holds, as its metadata tags say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChdMedia {
    /// `GDDD` hard-disk metadata (chdman `createhd`), or a v1/v2 image,
    /// which the format only ever used for hard disks.
    HardDisk,
    /// CD-ROM (or GD-ROM) track metadata (chdman `createcd`).
    CdRom,
}

fn be32(raw: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(raw[at..at + 4].try_into().unwrap())
}

fn be64(raw: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(raw[at..at + 8].try_into().unwrap())
}

/// Whether `head` (the first bytes of a file) opens with the CHD magic.
pub fn is_chd(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

/// Read the metadata tags of a CHD and say what kind of media it holds.
///
/// Reads the header and walks the metadata chain only: the hunk map, which
/// the `chd` crate sizes from the header and decodes on open, is never
/// touched, so this is cheap enough for the launcher to call while a path
/// field is being edited. A file that is not a CHD, or carries neither kind
/// of metadata, is an error.
pub fn media_kind(path: &Path) -> Result<ChdMedia> {
    let mut file =
        File::open(path).with_context(|| format!("opening CHD image {}", path.display()))?;
    let mut raw = [0u8; HEADER_BYTES];
    file.read_exact(&mut raw).map_err(|_| {
        anyhow!(
            "{}: not a readable CHD image: header truncated",
            path.display()
        )
    })?;
    if !is_chd(&raw) {
        bail!("{}: not a readable CHD image: bad magic", path.display());
    }
    let length = be32(&raw, 8);
    let version = be32(&raw, 12);
    let meta_offset = match (version, length) {
        // v1 and v2 predate metadata and only ever held hard disks.
        (1 | 2, _) => return Ok(ChdMedia::HardDisk),
        (3, 120) | (4, 108) => be64(&raw, 36),
        (5, 124) => be64(&raw, 48),
        _ => bail!(
            "{}: not a readable CHD image: unknown version {version} (header {length} bytes)",
            path.display()
        ),
    };
    let cd_tags = [
        KnownMetadata::CdRomOld as u32,
        KnownMetadata::CdRomTrack as u32,
        KnownMetadata::CdRomTrack2 as u32,
        KnownMetadata::GdRomOld as u32,
        KnownMetadata::GdRomTrack as u32,
    ];
    let mut offset = meta_offset;
    let mut entry = [0u8; METADATA_HEADER_BYTES];
    for _ in 0..MAX_METADATA_ENTRIES {
        if offset == 0 {
            break;
        }
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut entry))
            .map_err(|_| anyhow!("{}: CHD metadata chain is truncated", path.display()))?;
        let tag = be32(&entry, 0);
        if tag == KnownMetadata::HardDisk as u32 {
            return Ok(ChdMedia::HardDisk);
        }
        if cd_tags.contains(&tag) {
            return Ok(ChdMedia::CdRom);
        }
        offset = be64(&entry, 8);
    }
    bail!(
        "{}: CHD carries neither hard-disk (GDDD) nor CD-ROM track metadata",
        path.display()
    )
}

/// Whether `path` is a CHD holding a hard disk. Anything else -- a CD CHD,
/// a file that cannot be read, a path that does not exist yet -- is `false`,
/// so a `.chd` a caller cannot classify keeps its traditional reading as a
/// CD image and the open that follows reports the real problem.
///
/// The verdict is cached against the file's size and modification time:
/// the launcher and the configuration screen ask about the same path on
/// every redraw, and a stat per call is all that costs.
pub fn is_hard_disk_chd(path: &Path) -> bool {
    type Cache = HashMap<PathBuf, (u64, Option<SystemTime>, ChdMedia)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    let stamp = (meta.len(), meta.modified().ok());
    let cache = CACHE.get_or_init(Mutex::default);
    let cached = cache.lock().unwrap().get(path).copied();
    if let Some((len, modified, kind)) = cached {
        if (len, modified) == stamp {
            return kind == ChdMedia::HardDisk;
        }
    }
    match media_kind(path) {
        Ok(kind) => {
            cache
                .lock()
                .unwrap()
                .insert(path.to_path_buf(), (stamp.0, stamp.1, kind));
            kind == ChdMedia::HardDisk
        }
        Err(_) => false,
    }
}

/// Refuse a CHD whose header describes more than the file can hold, before
/// the `chd` crate acts on it. `Chd::open` sizes the whole hunk map
/// (`hunk_count * 12` bytes, then walks it) and the codecs' hunk-sized
/// buffers straight from the header's words, computes the hunk count with
/// unchecked arithmetic, and performs no bounds checks at all on a v5
/// header, so a 124-byte file claiming a petabyte disk would otherwise take
/// the process down in the allocator (or an overflow panic) instead of
/// being refused. The CD backend guards the same way; the bounds here are a
/// hard disk's.
///
/// Only the fields the bounds need are decoded; everything else is left to
/// the crate, which re-reads the header from offset 0.
fn bound_raw_header(reader: &mut BufReader<File>, path: &Path) -> Result<()> {
    let mut raw = [0u8; HEADER_BYTES];
    reader.read_exact(&mut raw).map_err(|_| {
        anyhow!(
            "{}: not a readable CHD image: header truncated",
            path.display()
        )
    })?;
    if !is_chd(&raw) {
        bail!("{}: not a readable CHD image: bad magic", path.display());
    }
    let length = be32(&raw, 8);
    let version = be32(&raw, 12);
    // (logical bytes, hunk bytes, stated hunk count) as the header has them.
    let (logical_bytes, hunk_bytes, stated_hunks) = match (version, length) {
        (3, 120) => (be64(&raw, 28), be32(&raw, 76), Some(be32(&raw, 24))),
        (4, 108) => (be64(&raw, 28), be32(&raw, 44), Some(be32(&raw, 24))),
        (5, 124) => (be64(&raw, 32), be32(&raw, 56), None),
        (1 | 2, _) => bail!(
            "{}: CHD v{version} images are too old to read; re-create it with a current \
             chdman (createhd)",
            path.display()
        ),
        _ => bail!(
            "{}: not a readable CHD image: unknown version {version} (header {length} bytes)",
            path.display()
        ),
    };
    if hunk_bytes == 0
        || hunk_bytes >= MAX_HUNK_BYTES
        || !(hunk_bytes as usize).is_multiple_of(SECTOR_SIZE)
    {
        bail!(
            "{}: not a hard-disk CHD (hunk {hunk_bytes} bytes; expected hunks of whole \
             {SECTOR_SIZE}-byte sectors)",
            path.display()
        );
    }
    if version == 5 && be32(&raw, 60) as usize != SECTOR_SIZE {
        bail!(
            "{}: not a hard-disk CHD (unit {} bytes; expected {SECTOR_SIZE}-byte sectors)",
            path.display(),
            be32(&raw, 60)
        );
    }
    if logical_bytes == 0 || !logical_bytes.is_multiple_of(SECTOR_SIZE as u64) {
        bail!(
            "{}: CHD describes {logical_bytes} bytes, not a whole number of {SECTOR_SIZE}-byte \
             sectors",
            path.display()
        );
    }
    if logical_bytes > MAX_DISK_BYTES {
        bail!(
            "{}: CHD describes {logical_bytes} bytes, more than the {} GiB a hard-disk image \
             may hold",
            path.display(),
            MAX_DISK_BYTES >> 30
        );
    }
    let hunk_count = logical_bytes.div_ceil(u64::from(hunk_bytes));
    if let Some(stated) = stated_hunks {
        if u64::from(stated) != hunk_count {
            bail!(
                "{}: CHD states {stated} hunks but its size needs {hunk_count}",
                path.display()
            );
        }
    }
    // Every hunk has a map entry in the file, and no map encoding spends
    // less than a bit on one, so a file shorter than an eighth of its hunk
    // count cannot be real. The crate allocates 12 bytes per hunk before it
    // reads any of them.
    let file_len = reader
        .seek(SeekFrom::End(0))
        .map_err(|e| anyhow!("{}: reading CHD image: {e}", path.display()))?;
    if hunk_count > file_len.saturating_mul(8) {
        bail!(
            "{}: CHD claims {hunk_count} hunks in a {file_len}-byte file",
            path.display()
        );
    }
    // A v5 compressed hunk map carries its own byte count as a u32 at the
    // map offset, and the chd crate allocates that many bytes before
    // reading them. The map is stored verbatim, so it must fit inside the
    // file; refuse one that does not before the crate sizes a buffer from it.
    if version == 5 && be32(&raw, 16) != 0 {
        let map_offset = be64(&raw, 40);
        let mut map_head = [0u8; 4];
        let map_bytes = map_offset
            .checked_add(16)
            .filter(|&end| end <= file_len)
            .and_then(|_| {
                reader.seek(SeekFrom::Start(map_offset)).ok()?;
                reader.read_exact(&mut map_head).ok()?;
                Some(u64::from(u32::from_be_bytes(map_head)))
            })
            .ok_or_else(|| {
                anyhow!(
                    "{}: CHD map offset {map_offset} lies past the end of the file",
                    path.display()
                )
            })?;
        if map_offset + 16 + map_bytes > file_len {
            bail!(
                "{}: CHD compressed map claims {map_bytes} bytes, past the end of the file",
                path.display()
            );
        }
    }
    Ok(())
}

/// The bytes-per-sector a `GDDD` entry states, e.g. from
/// `CYLS:401,HEADS:16,SECS:32,BPS:512.`.
fn parse_hard_disk_metadata(text: &str) -> Result<u32> {
    let mut bps = None;
    for field in text.trim_end_matches(['\0', '.']).split(',') {
        if let Some((key, value)) = field.trim().split_once(':') {
            if key == "BPS" {
                bps = Some(value.parse::<u32>().context("bad BPS value")?);
            }
        }
    }
    bps.context("hard-disk metadata without a BPS field")
}

/// A hard-disk CHD open for reading, with its overlay when writes are
/// possible.
pub(super) struct ChdHardDisk {
    chd: Chd<BufReader<File>>,
    path: PathBuf,
    total_sectors: u64,
    sectors_per_hunk: u64,
    /// Decompressed contents of hunk `cached_hunk`.
    hunk_buf: Vec<u8>,
    /// Scratch buffer for compressed hunk bytes, kept between reads.
    comp_buf: Vec<u8>,
    cached_hunk: Option<u32>,
    /// Where guest writes go. `None` attaches the disk write-protected.
    overlay: Option<Overlay>,
}

impl std::fmt::Debug for ChdHardDisk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChdHardDisk")
            .field("path", &self.path)
            .field("total_sectors", &self.total_sectors)
            .field("overlay", &self.overlay.as_ref().map(|o| &o.path))
            .finish_non_exhaustive()
    }
}

impl ChdHardDisk {
    /// Open a hard-disk CHD. With `writable`, the overlay sidecar beside the
    /// image is opened (or created) to take guest writes; when that fails
    /// the disk attaches write-protected with a warning rather than
    /// refusing to open, since reading it is still worth something. Without
    /// `writable` no sidecar is touched at all.
    pub(super) fn open(path: &Path, bus_name: &str, writable: bool) -> Result<Self> {
        let file = File::open(path)
            .map_err(|e| anyhow!("opening {bus_name} image {}: {e}", path.display()))?;
        let mut reader = BufReader::new(file);
        bound_raw_header(&mut reader, path)?;
        let mut chd = Chd::open(reader, None)
            .map_err(|e| anyhow!("{}: not a readable CHD image: {e}", path.display()))?;
        if chd.header().has_parent() {
            bail!(
                "{}: delta CHD with a parent is not supported; flatten it with chdman",
                path.display()
            );
        }
        let unit_bytes = chd.header().unit_bytes();
        let hunk_bytes = chd.header().hunk_size();
        let logical_bytes = chd.header().logical_bytes();
        // `bound_raw_header` already refused such files, but the values used
        // from here on are the crate's, so check those.
        if unit_bytes as usize != SECTOR_SIZE
            || hunk_bytes == 0
            || !(hunk_bytes as usize).is_multiple_of(SECTOR_SIZE)
            || logical_bytes == 0
            || !logical_bytes.is_multiple_of(SECTOR_SIZE as u64)
        {
            bail!(
                "{}: not a hard-disk CHD (unit {unit_bytes} bytes, hunk {hunk_bytes} bytes, \
                 {logical_bytes} bytes; expected hunks of whole {SECTOR_SIZE}-byte sectors)",
                path.display()
            );
        }

        let entries: Vec<Metadata> = chd
            .metadata_refs()
            .try_into()
            .map_err(|e| anyhow!("{}: reading CHD metadata: {e}", path.display()))?;
        let mut geometry = None;
        for entry in &entries {
            if entry.metatag == KnownMetadata::HardDisk as u32 {
                let text = std::str::from_utf8(&entry.value)
                    .map(|s| s.trim_end_matches('\0'))
                    .map_err(|_| {
                        anyhow!("{}: CHD hard-disk metadata is not text", path.display())
                    })?;
                let bps = parse_hard_disk_metadata(text)
                    .with_context(|| format!("{}: {text:?}", path.display()))?;
                if bps as usize != SECTOR_SIZE {
                    bail!(
                        "{}: CHD hard disk has {bps}-byte sectors; only {SECTOR_SIZE}-byte \
                         sectors are supported",
                        path.display()
                    );
                }
                geometry = Some(text.to_string());
            } else if KnownMetadata::is_cdrom(entry.metatag) {
                bail!(
                    "{}: this CHD holds a CD-ROM, not a hard disk; attach it as a CD image",
                    path.display()
                );
            }
        }
        let Some(geometry) = geometry else {
            bail!(
                "{}: CHD carries no hard-disk (GDDD) metadata; re-create it with chdman createhd",
                path.display()
            );
        };

        let total_sectors = logical_bytes / SECTOR_SIZE as u64;
        let overlay = if writable {
            let sha1 = chd.header().sha1().unwrap_or([0; 20]);
            match Overlay::open(path, total_sectors, sha1) {
                Ok(overlay) => Some(overlay),
                Err(error) => {
                    log::warn!(
                        "{bus_name}: {}: cannot open the write overlay ({error:#}); the \
                         disk is attached WRITE-PROTECTED",
                        path.display()
                    );
                    None
                }
            }
        } else {
            None
        };
        log::info!(
            "{bus_name}: {} is a CHD hard disk ({geometry}; {total_sectors} sectors, {} bytes \
             on disk){}",
            path.display(),
            std::fs::metadata(path).map_or(0, |m| m.len()),
            match &overlay {
                Some(o) => format!("; guest writes go to {}", o.path.display()),
                None => "; write-protected".to_string(),
            }
        );
        let hunk_buf = chd.get_hunksized_buffer();
        Ok(Self {
            chd,
            path: path.to_path_buf(),
            total_sectors,
            sectors_per_hunk: u64::from(hunk_bytes) / SECTOR_SIZE as u64,
            hunk_buf,
            comp_buf: Vec::new(),
            cached_hunk: None,
            overlay,
        })
    }

    pub(super) fn total_sectors(&self) -> u64 {
        self.total_sectors
    }

    /// Whether guest writes are refused: no overlay could be opened.
    pub(super) fn write_protected(&self) -> bool {
        self.overlay.is_none()
    }

    fn out_of_range(lba: u64) -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!("sector {lba} beyond end of CHD image"),
        )
    }

    pub(super) fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> std::io::Result<()> {
        if lba >= self.total_sectors {
            return Err(Self::out_of_range(lba));
        }
        if let Some(overlay) = &self.overlay {
            if overlay.read(lba, buf)? {
                return Ok(());
            }
        }
        // The hunk count fits a u32 by header validation.
        self.load_hunk((lba / self.sectors_per_hunk) as u32)?;
        let offset = (lba % self.sectors_per_hunk) as usize * SECTOR_SIZE;
        buf[..SECTOR_SIZE].copy_from_slice(&self.hunk_buf[offset..offset + SECTOR_SIZE]);
        Ok(())
    }

    pub(super) fn write_sector(&mut self, lba: u64, buf: &[u8]) -> std::io::Result<()> {
        if lba >= self.total_sectors {
            return Err(Self::out_of_range(lba));
        }
        match &mut self.overlay {
            Some(overlay) => overlay.write(lba, buf),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "CHD image is write-protected (no write overlay)",
            )),
        }
    }

    pub(super) fn flush(&mut self) -> std::io::Result<()> {
        match &mut self.overlay {
            Some(overlay) => overlay.file.flush(),
            None => Ok(()),
        }
    }

    /// Every overlaid sector, for a save state. `None` when the disk is
    /// write-protected.
    pub(super) fn overlay_contents(&self) -> std::io::Result<Option<BTreeMap<u64, Vec<u8>>>> {
        self.overlay.as_ref().map(Overlay::contents).transpose()
    }

    /// Make the overlay hold exactly `sectors` (a save state's contents),
    /// discarding whatever it held. With no overlay open, a non-empty set
    /// cannot be honoured and is an error: the restored machine would see
    /// a disk other than the one it saved.
    pub(super) fn restore_overlay(
        &mut self,
        sectors: &BTreeMap<u64, Vec<u8>>,
    ) -> std::io::Result<()> {
        self.cached_hunk = None;
        match &mut self.overlay {
            Some(overlay) => overlay.replace(sectors, self.total_sectors),
            None if sectors.is_empty() => Ok(()),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the state carries guest writes but the CHD's write overlay cannot be opened",
            )),
        }
    }

    fn load_hunk(&mut self, hunk: u32) -> std::io::Result<()> {
        if self.cached_hunk == Some(hunk) {
            return Ok(());
        }
        self.cached_hunk = None;
        self.chd
            .hunk(hunk)
            .and_then(|mut h| h.read_hunk_in(&mut self.comp_buf, &mut self.hunk_buf))
            .map_err(|e| {
                std::io::Error::other(format!(
                    "{}: reading CHD hunk {hunk}: {e}",
                    self.path.display()
                ))
            })?;
        self.cached_hunk = Some(hunk);
        Ok(())
    }
}

/// The overlay's file name: the image's own name with this appended
/// (`disk.chd` -> `disk.chd.wov`), so the pair sorts together and nothing
/// has to guess which image an overlay belongs to.
pub const OVERLAY_SUFFIX: &str = ".wov";
const OVERLAY_MAGIC: &[u8; 8] = b"CLWOV001";
/// Magic, sector size (u32 LE), image sectors (u64 LE), the image's SHA-1
/// from its CHD header, and 8 reserved zero bytes.
const OVERLAY_HEADER_BYTES: u64 = 48;
/// One record: the LBA (u64 LE) followed by the sector's bytes.
const RECORD_BYTES: u64 = 8 + SECTOR_SIZE as u64;

/// The copy-on-write overlay sidecar of a CHD hard disk.
///
/// A log of `(lba, sector)` records behind a header naming the image it
/// belongs to. The first write to a sector appends a record; later writes
/// to the same sector overwrite that record in place, so the file grows
/// only with the number of distinct sectors written. The index from LBA to
/// record is rebuilt by scanning the log at open. A record cut short by a
/// crash is dropped at open, which is the only recovery the format needs:
/// every record is complete or absent, and a sector's latest bytes are
/// either in its record or still in the CHD.
struct Overlay {
    file: File,
    path: PathBuf,
    /// LBA to the file offset of the record's sector bytes.
    index: BTreeMap<u64, u64>,
    /// Length of the file as this handle knows it; the next record goes here.
    len: u64,
}

/// The sidecar path for `image`.
pub fn overlay_path(image: &Path) -> PathBuf {
    let mut name = image.as_os_str().to_owned();
    name.push(OVERLAY_SUFFIX);
    PathBuf::from(name)
}

impl Overlay {
    fn open(image: &Path, total_sectors: u64, sha1: [u8; 20]) -> Result<Self> {
        let path = overlay_path(image);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| anyhow!("opening overlay {}: {e}", path.display()))?;
        let len = file
            .metadata()
            .map_err(|e| anyhow!("stat overlay {}: {e}", path.display()))?
            .len();
        let mut overlay = Self {
            file,
            path,
            index: BTreeMap::new(),
            len,
        };
        if len == 0 {
            overlay.write_header(total_sectors, sha1)?;
            return Ok(overlay);
        }

        let mut header = [0u8; OVERLAY_HEADER_BYTES as usize];
        overlay.file.rewind()?;
        overlay.file.read_exact(&mut header).map_err(|_| {
            anyhow!(
                "overlay {} is too short to carry a header; move it aside if it is not one",
                overlay.path.display()
            )
        })?;
        if &header[..8] != OVERLAY_MAGIC {
            bail!(
                "overlay {} is not a Copperline CHD write overlay; move it aside",
                overlay.path.display()
            );
        }
        let sector_bytes = u32::from_le_bytes(header[8..12].try_into().unwrap());
        let sectors = u64::from_le_bytes(header[12..20].try_into().unwrap());
        if sector_bytes as usize != SECTOR_SIZE || sectors != total_sectors {
            bail!(
                "overlay {} describes a {sectors}-sector disk of {sector_bytes}-byte sectors, \
                 not this image ({total_sectors} sectors of {SECTOR_SIZE}); move it aside",
                overlay.path.display()
            );
        }
        if header[20..40] != sha1 {
            bail!(
                "overlay {} belongs to a different CHD image (its SHA-1 differs); move it \
                 aside or delete it",
                overlay.path.display()
            );
        }

        // Rebuild the index from the log, reading only each record's LBA.
        let mut reader = BufReader::new(&overlay.file);
        let mut pos = OVERLAY_HEADER_BYTES;
        let mut lba_bytes = [0u8; 8];
        while pos + RECORD_BYTES <= len {
            reader.seek(SeekFrom::Start(pos))?;
            reader.read_exact(&mut lba_bytes)?;
            let lba = u64::from_le_bytes(lba_bytes);
            if lba >= total_sectors {
                bail!(
                    "overlay {} records sector {lba} past the end of the {total_sectors}-sector \
                     image; it is corrupt",
                    overlay.path.display()
                );
            }
            overlay.index.insert(lba, pos + 8);
            pos += RECORD_BYTES;
        }
        drop(reader);
        if pos < len {
            log::warn!(
                "overlay {}: dropping {} trailing bytes of an incomplete record",
                overlay.path.display(),
                len - pos
            );
            overlay.file.set_len(pos)?;
            overlay.len = pos;
        }
        Ok(overlay)
    }

    fn write_header(&mut self, total_sectors: u64, sha1: [u8; 20]) -> Result<()> {
        let mut header = [0u8; OVERLAY_HEADER_BYTES as usize];
        header[..8].copy_from_slice(OVERLAY_MAGIC);
        header[8..12].copy_from_slice(&(SECTOR_SIZE as u32).to_le_bytes());
        header[12..20].copy_from_slice(&total_sectors.to_le_bytes());
        header[20..40].copy_from_slice(&sha1);
        self.file.rewind()?;
        self.file
            .write_all(&header)
            .map_err(|e| anyhow!("writing overlay {}: {e}", self.path.display()))?;
        self.len = OVERLAY_HEADER_BYTES;
        Ok(())
    }

    /// Copy the overlaid bytes of `lba` into `buf`, if it has any.
    fn read(&self, lba: u64, buf: &mut [u8]) -> std::io::Result<bool> {
        let Some(&offset) = self.index.get(&lba) else {
            return Ok(false);
        };
        let mut file = &self.file;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut buf[..SECTOR_SIZE])?;
        Ok(true)
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> std::io::Result<()> {
        match self.index.get(&lba) {
            Some(&offset) => {
                self.file.seek(SeekFrom::Start(offset))?;
                self.file.write_all(&buf[..SECTOR_SIZE])
            }
            None => {
                let record = self.len;
                self.file.seek(SeekFrom::Start(record))?;
                self.file.write_all(&lba.to_le_bytes())?;
                self.file.write_all(&buf[..SECTOR_SIZE])?;
                self.index.insert(lba, record + 8);
                self.len = record + RECORD_BYTES;
                Ok(())
            }
        }
    }

    fn contents(&self) -> std::io::Result<BTreeMap<u64, Vec<u8>>> {
        let mut file = &self.file;
        let mut out = BTreeMap::new();
        for (&lba, &offset) in &self.index {
            let mut sector = vec![0u8; SECTOR_SIZE];
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(&mut sector)?;
            out.insert(lba, sector);
        }
        Ok(out)
    }

    /// Truncate the log and rewrite it from `sectors`.
    fn replace(
        &mut self,
        sectors: &BTreeMap<u64, Vec<u8>>,
        total_sectors: u64,
    ) -> std::io::Result<()> {
        if let Some((&lba, _)) = sectors
            .iter()
            .find(|(&lba, data)| lba >= total_sectors || data.len() != SECTOR_SIZE)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("state carries an invalid overlay record for sector {lba}"),
            ));
        }
        self.file.set_len(OVERLAY_HEADER_BYTES)?;
        self.len = OVERLAY_HEADER_BYTES;
        self.index.clear();
        for (&lba, data) in sectors {
            self.write(lba, data)?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::harddrive::{HardDriveImage, CYL_BYTES, CYL_SECTORS};

    pub(crate) fn temp_path(name: &str) -> PathBuf {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "copperline-chdhd-{}-{unique}-{name}",
            std::process::id()
        ))
    }

    /// Four sectors per hunk, so a small image spans several hunks.
    pub(crate) const HUNK_BYTES: u32 = 4 * SECTOR_SIZE as u32;

    /// Write a minimal uncompressed CHD v5: 124-byte header, raw hunk map
    /// (one big-endian u32 per hunk, the hunk's file offset in hunk-size
    /// units), a metadata chain, and hunk-aligned data. `sha1` lands where
    /// the crate reads the image's SHA-1 from.
    pub(crate) fn write_chd_v5(
        path: &Path,
        hunk_bytes: u32,
        unit_bytes: u32,
        data: &[u8],
        metas: &[([u8; 4], Vec<u8>)],
        sha1: [u8; 20],
    ) {
        let hunk_count = match hunk_bytes {
            0 => 0,
            _ => data.len().div_ceil(hunk_bytes as usize),
        };
        let map_offset = 124u64;
        let meta_offset = map_offset + 4 * hunk_count as u64;
        let metas_len: u64 = metas.iter().map(|(_, v)| 16 + v.len() as u64).sum();
        let data_start = match hunk_bytes {
            0 => meta_offset + metas_len,
            _ => (meta_offset + metas_len).div_ceil(u64::from(hunk_bytes)) * u64::from(hunk_bytes),
        };

        let mut out = Vec::new();
        out.extend_from_slice(b"MComprHD");
        out.extend_from_slice(&124u32.to_be_bytes());
        out.extend_from_slice(&5u32.to_be_bytes());
        out.extend_from_slice(&[0u8; 16]); // four codecs: none (uncompressed)
        out.extend_from_slice(&(data.len() as u64).to_be_bytes());
        out.extend_from_slice(&map_offset.to_be_bytes());
        out.extend_from_slice(&meta_offset.to_be_bytes());
        out.extend_from_slice(&hunk_bytes.to_be_bytes());
        out.extend_from_slice(&unit_bytes.to_be_bytes());
        out.extend_from_slice(&[0u8; 20]); // raw SHA-1
        out.extend_from_slice(&sha1); // SHA-1
        out.extend_from_slice(&[0u8; 20]); // parent SHA-1
        assert_eq!(out.len(), 124);

        for hunk in 0..hunk_count as u64 {
            let entry = data_start / u64::from(hunk_bytes) + hunk;
            out.extend_from_slice(&(entry as u32).to_be_bytes());
        }
        let mut next = meta_offset;
        for (i, (tag, value)) in metas.iter().enumerate() {
            next += 16 + value.len() as u64;
            out.extend_from_slice(tag);
            out.extend_from_slice(&(0x01u32 << 24 | value.len() as u32).to_be_bytes());
            let next_field = if i + 1 == metas.len() { 0 } else { next };
            out.extend_from_slice(&next_field.to_be_bytes());
            out.extend_from_slice(value);
        }
        out.resize(data_start as usize, 0);
        out.extend_from_slice(data);
        out.resize(data_start as usize + hunk_count * hunk_bytes as usize, 0);
        std::fs::write(path, out).unwrap();
    }

    pub(crate) fn gddd(cyls: u32, heads: u32, secs: u32, bps: u32) -> ([u8; 4], Vec<u8>) {
        let mut value = format!("CYLS:{cyls},HEADS:{heads},SECS:{secs},BPS:{bps}.").into_bytes();
        value.push(0);
        (*b"GDDD", value)
    }

    /// A hard-disk CHD of `sectors` sectors, each filled with its own LBA
    /// (low byte) so any addressing slip shows.
    pub(crate) fn write_hard_disk(path: &Path, sectors: usize, sha1: [u8; 20]) -> Vec<u8> {
        let mut data = vec![0u8; sectors * SECTOR_SIZE];
        for (lba, sector) in data.chunks_mut(SECTOR_SIZE).enumerate() {
            sector.fill(lba as u8);
            sector[..4].copy_from_slice(&(lba as u32).to_be_bytes());
        }
        write_chd_v5(
            path,
            HUNK_BYTES,
            SECTOR_SIZE as u32,
            &data,
            &[gddd(1, 1, sectors as u32, SECTOR_SIZE as u32)],
            sha1,
        );
        data
    }

    fn open(path: &Path) -> HardDriveImage {
        HardDriveImage::open(
            path,
            "DH0",
            "ide",
            None,
            0,
            crate::diskimage::FileSystem::FFS,
        )
        .unwrap()
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(overlay_path(path));
        let _ = std::fs::remove_dir(overlay_path(path));
    }

    /// A write-protected drive for the controller tests: a hard-disk CHD
    /// whose overlay cannot be created because a directory sits where the
    /// sidecar would go. The image has no `DOS`/`RDSK` signature, so every
    /// LBA is a direct image sector. Pass the path back to
    /// [`remove_write_protected_image`] when done.
    pub(crate) fn write_protected_image(name: &str, sectors: usize) -> (HardDriveImage, PathBuf) {
        let path = temp_path(&format!("{name}.chd"));
        let mut sha1 = [0u8; 20];
        sha1[..name.len().min(20)].copy_from_slice(&name.as_bytes()[..name.len().min(20)]);
        write_hard_disk(&path, sectors, sha1);
        std::fs::create_dir(overlay_path(&path)).unwrap();
        let drive = open(&path);
        assert!(drive.write_protected());
        (drive, path)
    }

    pub(crate) fn remove_write_protected_image(path: &Path) {
        cleanup(path);
    }

    #[test]
    fn media_kind_reads_the_metadata_tags() {
        let hd = temp_path("kind-hd.chd");
        write_hard_disk(&hd, 4, [1; 20]);
        assert_eq!(media_kind(&hd).unwrap(), ChdMedia::HardDisk);
        assert!(is_hard_disk_chd(&hd));

        let cd = temp_path("kind-cd.chd");
        write_chd_v5(
            &cd,
            2448 * 2,
            2448,
            &[0u8; 2448 * 4],
            &[(
                *b"CHT2",
                b"TRACK:1 TYPE:MODE1 SUBTYPE:NONE FRAMES:4 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE \
                  POSTGAP:0\0"
                    .to_vec(),
            )],
            [2; 20],
        );
        assert_eq!(media_kind(&cd).unwrap(), ChdMedia::CdRom);
        assert!(!is_hard_disk_chd(&cd));

        // Neither kind of metadata, a truncated header, a non-CHD, and a
        // missing file all classify as "not a hard disk" without an error
        // escaping to the caller.
        let bare = temp_path("kind-none.chd");
        write_chd_v5(
            &bare,
            HUNK_BYTES,
            SECTOR_SIZE as u32,
            &[0u8; 2048],
            &[],
            [3; 20],
        );
        assert!(media_kind(&bare).is_err());
        assert!(!is_hard_disk_chd(&bare));
        let short = temp_path("kind-short.chd");
        std::fs::write(&short, b"MComprHD").unwrap();
        assert!(media_kind(&short).is_err());
        assert!(!is_hard_disk_chd(&short));
        let hdf = temp_path("kind-plain.chd");
        std::fs::write(&hdf, vec![0u8; 1024]).unwrap();
        assert!(!is_hard_disk_chd(&hdf));
        assert!(!is_hard_disk_chd(&temp_path("kind-missing.chd")));

        // A rewritten file is re-read, not answered from the cache.
        std::fs::remove_file(&hd).unwrap();
        std::fs::copy(&cd, &hd).unwrap();
        assert!(!is_hard_disk_chd(&hd));

        for path in [&hd, &cd, &bare, &short, &hdf] {
            cleanup(path);
        }
    }

    #[test]
    fn hard_disk_chd_serves_sectors_across_hunks() {
        let path = temp_path("read.chd");
        let data = write_hard_disk(&path, 11, [4; 20]);
        let mut drive = open(&path);
        assert_eq!(drive.total_sectors(), 11);
        assert!(drive.has_own_rdb(), "no DOS boot block, so nothing to wrap");
        assert!(!drive.write_protected());
        let mut sector = vec![0u8; SECTOR_SIZE];
        // Backwards, so the hunk cache is exercised in both directions.
        for lba in (0..11u64).rev() {
            drive.read_sector(lba, &mut sector).unwrap();
            let want = &data[lba as usize * SECTOR_SIZE..(lba as usize + 1) * SECTOR_SIZE];
            assert_eq!(sector.as_slice(), want, "lba {lba}");
        }
        assert!(drive.read_sector(11, &mut sector).is_err());
        cleanup(&path);
    }

    #[test]
    fn bare_partition_chd_gets_a_synthesized_rdb() {
        let path = temp_path("bare.chd");
        let mut data = vec![0u8; CYL_BYTES as usize];
        data[..4].copy_from_slice(b"DOS\x01");
        data[5 * SECTOR_SIZE..5 * SECTOR_SIZE + 4].copy_from_slice(b"MARK");
        write_chd_v5(
            &path,
            HUNK_BYTES,
            SECTOR_SIZE as u32,
            &data,
            &[gddd(1, 16, 32, 512)],
            [5; 20],
        );
        let mut drive = open(&path);
        assert!(!drive.has_own_rdb());
        assert_eq!(drive.total_sectors(), 2 * u64::from(CYL_SECTORS));
        let mut sector = vec![0u8; SECTOR_SIZE];
        drive.read_sector(0, &mut sector).unwrap();
        assert_eq!(&sector[..4], b"RDSK");
        drive
            .read_sector(u64::from(CYL_SECTORS) + 5, &mut sector)
            .unwrap();
        assert_eq!(&sector[..4], b"MARK");
        cleanup(&path);
    }

    #[test]
    fn rdb_chd_is_left_alone() {
        let path = temp_path("rdb.chd");
        let mut data = vec![0u8; 16 * SECTOR_SIZE];
        data[..4].copy_from_slice(b"RDSK");
        write_chd_v5(
            &path,
            HUNK_BYTES,
            SECTOR_SIZE as u32,
            &data,
            &[gddd(1, 1, 16, 512)],
            [6; 20],
        );
        let mut drive = open(&path);
        assert!(drive.has_own_rdb());
        assert_eq!(drive.total_sectors(), 16);
        let mut sector = vec![0u8; SECTOR_SIZE];
        drive.read_sector(0, &mut sector).unwrap();
        assert_eq!(&sector[..4], b"RDSK");
        cleanup(&path);
    }

    #[test]
    fn writes_land_in_the_overlay_and_persist_across_reopen() {
        let path = temp_path("overlay.chd");
        let data = write_hard_disk(&path, 9, [7; 20]);
        let pristine = std::fs::read(&path).unwrap();
        {
            let mut drive = open(&path);
            let sector = vec![0xA5; SECTOR_SIZE];
            drive.write_sector(3, &sector).unwrap();
            drive.write_sector(8, &sector).unwrap();
            // A second write to the same sector overwrites its record.
            let again = vec![0x5A; SECTOR_SIZE];
            drive.write_sector(3, &again).unwrap();
            drive.flush().unwrap();
            let mut back = vec![0u8; SECTOR_SIZE];
            drive.read_sector(3, &mut back).unwrap();
            assert_eq!(back, again);
            drive.read_sector(4, &mut back).unwrap();
            assert_eq!(&back[..], &data[4 * SECTOR_SIZE..5 * SECTOR_SIZE]);
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            pristine,
            "the CHD is untouched"
        );
        let overlay = std::fs::metadata(overlay_path(&path)).unwrap().len();
        assert_eq!(overlay, OVERLAY_HEADER_BYTES + 2 * RECORD_BYTES);

        let mut drive = open(&path);
        let mut back = vec![0u8; SECTOR_SIZE];
        drive.read_sector(3, &mut back).unwrap();
        assert!(back.iter().all(|&b| b == 0x5A));
        drive.read_sector(8, &mut back).unwrap();
        assert!(back.iter().all(|&b| b == 0xA5));
        drive.read_sector(2, &mut back).unwrap();
        assert_eq!(&back[..], &data[2 * SECTOR_SIZE..3 * SECTOR_SIZE]);
        cleanup(&path);
    }

    #[test]
    fn overlay_of_another_image_is_refused_and_the_disk_goes_write_protected() {
        let a = temp_path("mine.chd");
        let b = temp_path("theirs.chd");
        write_hard_disk(&a, 8, [8; 20]);
        write_hard_disk(&b, 8, [9; 20]);
        {
            let mut drive = open(&a);
            drive.write_sector(1, &[1; SECTOR_SIZE]).unwrap();
        }
        std::fs::rename(overlay_path(&a), overlay_path(&b)).unwrap();
        let mut drive = open(&b);
        assert!(drive.write_protected());
        let err = drive.write_sector(1, &[2; SECTOR_SIZE]).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        // The foreign overlay is not read from either.
        let mut sector = vec![0u8; SECTOR_SIZE];
        drive.read_sector(1, &mut sector).unwrap();
        assert_eq!(&sector[..4], &1u32.to_be_bytes());
        // And it was not touched.
        let foreign = std::fs::read(overlay_path(&b)).unwrap();
        assert_eq!(foreign.len() as u64, OVERLAY_HEADER_BYTES + RECORD_BYTES);
        cleanup(&a);
        cleanup(&b);
    }

    #[test]
    fn torn_trailing_record_is_dropped_at_open() {
        let path = temp_path("torn.chd");
        write_hard_disk(&path, 8, [10; 20]);
        {
            let mut drive = open(&path);
            drive.write_sector(5, &[0xEE; SECTOR_SIZE]).unwrap();
            drive.write_sector(6, &[0xDD; SECTOR_SIZE]).unwrap();
        }
        let overlay = overlay_path(&path);
        let full = std::fs::read(&overlay).unwrap();
        std::fs::write(&overlay, &full[..full.len() - 100]).unwrap();
        let mut drive = open(&path);
        assert!(!drive.write_protected());
        let mut sector = vec![0u8; SECTOR_SIZE];
        drive.read_sector(5, &mut sector).unwrap();
        assert!(sector.iter().all(|&b| b == 0xEE));
        drive.read_sector(6, &mut sector).unwrap();
        assert_eq!(
            &sector[..4],
            &6u32.to_be_bytes(),
            "torn record reads from the CHD"
        );
        assert_eq!(
            std::fs::metadata(&overlay).unwrap().len(),
            OVERLAY_HEADER_BYTES + RECORD_BYTES
        );
        // A new write appends cleanly after the truncation point.
        drive.write_sector(6, &[0xCC; SECTOR_SIZE]).unwrap();
        drop(drive);
        let mut drive = open(&path);
        drive.read_sector(6, &mut sector).unwrap();
        assert!(sector.iter().all(|&b| b == 0xCC));
        cleanup(&path);
    }

    #[test]
    fn unwritable_overlay_location_attaches_the_disk_write_protected() {
        let path = temp_path("noverlay.chd");
        write_hard_disk(&path, 4, [11; 20]);
        // A directory where the sidecar would go cannot be opened as a file.
        std::fs::create_dir(overlay_path(&path)).unwrap();
        let mut drive = open(&path);
        assert!(drive.write_protected());
        let err = drive.write_sector(0, &[0; SECTOR_SIZE]).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        let mut sector = vec![0u8; SECTOR_SIZE];
        drive.read_sector(2, &mut sector).unwrap();
        assert_eq!(&sector[..4], &2u32.to_be_bytes());
        let _ = std::fs::remove_dir(overlay_path(&path));
        cleanup(&path);
    }

    #[test]
    fn save_state_carries_the_overlay_and_restores_it() {
        let path = temp_path("state.chd");
        write_hard_disk(&path, 8, [12; 20]);
        let mut drive = open(&path);
        drive.write_sector(2, &[0x11; SECTOR_SIZE]).unwrap();
        let encoded = bincode::serialize(&drive).unwrap();
        // Writes after the snapshot...
        drive.write_sector(2, &[0x22; SECTOR_SIZE]).unwrap();
        drive.write_sector(7, &[0x33; SECTOR_SIZE]).unwrap();
        drop(drive);

        // ...are undone by restoring it: the overlay is part of the state.
        let mut restored: HardDriveImage = bincode::deserialize(&encoded).unwrap();
        let mut sector = vec![0u8; SECTOR_SIZE];
        restored.read_sector(2, &mut sector).unwrap();
        assert!(sector.iter().all(|&b| b == 0x11));
        restored.read_sector(7, &mut sector).unwrap();
        assert_eq!(&sector[..4], &7u32.to_be_bytes());
        drop(restored);
        assert_eq!(
            std::fs::metadata(overlay_path(&path)).unwrap().len(),
            OVERLAY_HEADER_BYTES + RECORD_BYTES
        );

        // A state saved with no writes leaves an empty overlay behind.
        let clean = open(&path);
        let mut fresh = open(&path);
        fresh.write_sector(0, &[0x44; SECTOR_SIZE]).unwrap();
        drop(fresh);
        let encoded = bincode::serialize(&clean).unwrap();
        drop(clean);
        let mut restored: HardDriveImage = bincode::deserialize(&encoded).unwrap();
        restored.read_sector(0, &mut sector).unwrap();
        assert_eq!(&sector[..4], &0u32.to_be_bytes());

        std::fs::remove_file(&path).unwrap();
        let Err(err) = bincode::deserialize::<HardDriveImage>(&encoded) else {
            panic!("deserializing with the CHD gone must fail");
        };
        assert!(
            err.to_string().contains("reopening hard-drive image"),
            "{err}"
        );
        cleanup(&path);
    }

    #[test]
    fn session_copy_decompresses_the_whole_image_and_never_writes_a_sidecar() {
        let path = temp_path("session.chd");
        let data = write_hard_disk(&path, 6, [13; 20]);
        let mut drive = HardDriveImage::open_session(
            &path,
            "DH0",
            "ide",
            None,
            0,
            crate::diskimage::FileSystem::FFS,
        )
        .unwrap();
        drive.write_sector(1, &[0x99; SECTOR_SIZE]).unwrap();
        let mut sector = vec![0u8; SECTOR_SIZE];
        drive.read_sector(1, &mut sector).unwrap();
        assert!(sector.iter().all(|&b| b == 0x99));
        drive.read_sector(5, &mut sector).unwrap();
        assert_eq!(&sector[..], &data[5 * SECTOR_SIZE..]);
        assert!(!overlay_path(&path).exists());
        cleanup(&path);
    }

    #[test]
    fn cd_chd_is_refused_by_the_hard_disk_opener() {
        let path = temp_path("cd.chd");
        write_chd_v5(
            &path,
            2448 * 2,
            2448,
            &[0u8; 2448 * 4],
            &[(
                *b"CHT2",
                b"TRACK:1 TYPE:MODE1 SUBTYPE:NONE FRAMES:4\0".to_vec(),
            )],
            [14; 20],
        );
        let Err(err) = HardDriveImage::open(
            &path,
            "DH0",
            "ide",
            None,
            0,
            crate::diskimage::FileSystem::FFS,
        ) else {
            panic!("a CD CHD is not a hard disk");
        };
        assert!(err.to_string().contains("not a hard-disk CHD"), "{err:#}");
        // A CD-shaped image whose hunks happen to be sector multiples is
        // told apart by its metadata instead.
        write_chd_v5(
            &path,
            HUNK_BYTES,
            SECTOR_SIZE as u32,
            &[0u8; 4096],
            &[(
                *b"CHT2",
                b"TRACK:1 TYPE:MODE1 SUBTYPE:NONE FRAMES:4\0".to_vec(),
            )],
            [14; 20],
        );
        let Err(err) = HardDriveImage::open(
            &path,
            "DH0",
            "ide",
            None,
            0,
            crate::diskimage::FileSystem::FFS,
        ) else {
            panic!("CD track metadata marks a CD, whatever the hunk size");
        };
        assert!(err.to_string().contains("holds a CD-ROM"), "{err:#}");
        assert!(!overlay_path(&path).exists());
        cleanup(&path);
    }

    #[test]
    fn missing_metadata_and_odd_sector_sizes_are_refused() {
        let path = temp_path("nometa.chd");
        write_chd_v5(
            &path,
            HUNK_BYTES,
            SECTOR_SIZE as u32,
            &[0u8; 4096],
            &[],
            [15; 20],
        );
        let err = ChdHardDisk::open(&path, "ide", false).unwrap_err();
        assert!(
            err.to_string().contains("no hard-disk (GDDD) metadata"),
            "{err:#}"
        );
        write_chd_v5(
            &path,
            HUNK_BYTES,
            SECTOR_SIZE as u32,
            &[0u8; 4096],
            &[gddd(1, 1, 2, 2048)],
            [15; 20],
        );
        let err = ChdHardDisk::open(&path, "ide", false).unwrap_err();
        assert!(err.to_string().contains("2048-byte sectors"), "{err:#}");
        cleanup(&path);
    }

    /// Rewrite one big-endian field of an on-disk CHD v5 header in place.
    fn patch_header(path: &Path, offset: usize, value: &[u8]) {
        let mut bytes = std::fs::read(path).unwrap();
        bytes[offset..offset + value.len()].copy_from_slice(value);
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn hostile_headers_are_refused_before_the_map_is_allocated() {
        let path = temp_path("hostile.chd");
        write_hard_disk(&path, 4, [16; 20]);
        let open = |what: &str| {
            let err = ChdHardDisk::open(&path, "ide", false).unwrap_err();
            assert!(err.to_string().contains(what), "{err:#}");
        };
        // logical_bytes is the u64 at offset 32 of the v5 header: a petabyte
        // disk, then a size that overflows the crate's own hunk arithmetic.
        patch_header(&path, 32, &(1u64 << 50).to_be_bytes());
        open("more than the");
        patch_header(&path, 32, &u64::MAX.to_be_bytes());
        open("not a whole number");
        // Inside the size cap but far more hunks than the file could map.
        patch_header(&path, 32, &(1u64 << 36).to_be_bytes());
        open("claims");
        patch_header(&path, 32, &(4 * SECTOR_SIZE as u64).to_be_bytes());

        // Hunk size (u32 at 56): zero, huge, and not a sector multiple.
        patch_header(&path, 56, &0u32.to_be_bytes());
        open("not a hard-disk CHD");
        patch_header(&path, 56, &MAX_HUNK_BYTES.to_be_bytes());
        open("not a hard-disk CHD");
        patch_header(&path, 56, &(SECTOR_SIZE as u32 + 1).to_be_bytes());
        open("not a hard-disk CHD");
        patch_header(&path, 56, &HUNK_BYTES.to_be_bytes());

        // Unit size (u32 at 60) other than a sector.
        patch_header(&path, 60, &2448u32.to_be_bytes());
        open("not a hard-disk CHD");
        patch_header(&path, 60, &(SECTOR_SIZE as u32).to_be_bytes());

        // A compressed map claiming 2 GiB in a tiny file: the codec tag at
        // offset 16 marks the image compressed, the map sits at 124.
        patch_header(&path, 16, b"lzma");
        patch_header(&path, 124, &0x8000_0000u32.to_be_bytes());
        open("past the end of the file");
        patch_header(&path, 16, &[0u8; 4]);

        // A parent SHA-1 (offset 104) makes it a delta.
        patch_header(&path, 104, &[1u8; 20]);
        open("delta CHD");
        patch_header(&path, 104, &[0u8; 20]);

        // Old container versions.
        patch_header(&path, 12, &2u32.to_be_bytes());
        open("too old");
        patch_header(&path, 12, &9u32.to_be_bytes());
        open("unknown version");
        cleanup(&path);
    }

    #[test]
    fn overlay_path_appends_the_suffix_to_the_image_name() {
        assert_eq!(
            overlay_path(Path::new("/disks/work.chd")),
            PathBuf::from("/disks/work.chd.wov")
        );
    }

    #[test]
    fn hard_disk_metadata_parser_reads_bps() {
        assert_eq!(
            parse_hard_disk_metadata("CYLS:401,HEADS:16,SECS:32,BPS:512.").unwrap(),
            512
        );
        assert_eq!(parse_hard_disk_metadata("BPS:2048\0").unwrap(), 2048);
        assert!(parse_hard_disk_metadata("CYLS:1,HEADS:1,SECS:1").is_err());
        assert!(parse_hard_disk_metadata("BPS:lots").is_err());
    }
}
