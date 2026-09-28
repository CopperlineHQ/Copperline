// SPDX-License-Identifier: GPL-3.0-or-later

//! Microsoft Virtual Hard Disk (VHD) image backend.
//!
//! WinUAE creates and attaches hardfiles in this container, as do Windows
//! Disk Management, Virtual PC, VirtualBox and `qemu-img` (its `vpc`
//! format). Every VHD ends with a 512-byte footer (cookie `conectix`, all
//! fields big-endian) whose disk type says how the sectors are laid out:
//!
//! - *Fixed*: the disk's sectors in order, then the footer. The data is an
//!   ordinary raw hardfile; the footer is not part of the disk, so the guest
//!   neither sees it nor can write over it.
//! - *Dynamic*: a copy of the footer at offset 0, a `cxsparse` header, and a
//!   block allocation table (BAT) giving, for each block of the disk (2 MiB
//!   unless the header says otherwise), the sector where the block sits in
//!   the file, or that it has none yet. A block with none reads as zeros;
//!   the first write of anything else to it appends the block to the file
//!   -- a sector bitmap, then the block's data -- and moves the footer back
//!   to the end.
//! - *Differencing*: a dynamic disk that holds only the changes to a parent
//!   image, which it names by host path. Refused: merge it into its parent
//!   first.
//!
//! Like the gzip and CHD sniffs this goes by content: a VHD called `.hdf`
//! is still a VHD, and a raw hardfile called `.vhd` is still raw. Guest
//! writes go straight into the file, exactly as they do for a raw hardfile,
//! so a save state reopens a VHD by path.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::SECTOR_SIZE;

const SECTOR: u64 = SECTOR_SIZE as u64;
/// The cookie every footer, and a dynamic disk's copy of it, starts with.
const FOOTER_COOKIE: &[u8; 8] = b"conectix";
/// The cookie a dynamic disk's header starts with.
const DYNAMIC_COOKIE: &[u8; 8] = b"cxsparse";
const FOOTER_BYTES: usize = 512;
const DYNAMIC_HEADER_BYTES: usize = 1024;

// Footer fields.
const FOOTER_DATA_OFFSET: usize = 16;
const FOOTER_CURRENT_SIZE: usize = 48;
const FOOTER_DISK_TYPE: usize = 60;
const FOOTER_CHECKSUM: usize = 64;

// Dynamic disk header fields.
const HEADER_TABLE_OFFSET: usize = 16;
const HEADER_MAX_TABLE_ENTRIES: usize = 28;
const HEADER_BLOCK_SIZE: usize = 32;
const HEADER_CHECKSUM: usize = 36;

const DISK_FIXED: u32 = 2;
const DISK_DYNAMIC: u32 = 3;
const DISK_DIFFERENCING: u32 = 4;

/// A BAT entry for a block that has no place in the file yet.
const UNALLOCATED: u32 = u32::MAX;
/// The largest disk accepted. The format's own limit is 2040 GiB: a BAT
/// entry is a 32-bit sector number, so no block can start past 2 TiB.
const MAX_DISK_BYTES: u64 = 1 << 41;
/// The largest block accepted. Writers use 2 MiB (the default) or 512 KiB,
/// and giving a block its place in the file means writing all of it.
const MAX_BLOCK_BYTES: u32 = 1 << 28;
/// The BAT is held in memory, four bytes a block; this bounds it at 16 MiB,
/// which is 2 TiB of the smallest blocks any writer uses.
const MAX_BAT_ENTRIES: u64 = 1 << 22;

fn be32(raw: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(raw[at..at + 4].try_into().unwrap())
}

fn be64(raw: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(raw[at..at + 8].try_into().unwrap())
}

/// The one's complement of the byte sum of `block`, leaving out the four
/// checksum bytes at `at`: the rule for the footer and the dynamic header
/// alike.
fn checksum(block: &[u8], at: usize) -> u32 {
    let sum = block
        .iter()
        .enumerate()
        .filter(|(i, _)| !(at..at + 4).contains(i))
        .fold(0u32, |sum, (_, &byte)| sum.wrapping_add(u32::from(byte)));
    !sum
}

fn read_at(file: &mut File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(buf)
}

/// The footer that describes the disk, if `file` is a VHD.
///
/// The end is where every VHD keeps it. A dynamic disk also keeps a copy at
/// offset 0, which is what is left to go by when the last block appended to
/// it was cut off before the footer went back on the end. A fixed disk has
/// no copy -- offset 0 is its first sector -- so only a dynamic or
/// differencing footer is taken from there.
fn find_footer(file: &mut File, len: u64) -> io::Result<Option<[u8; FOOTER_BYTES]>> {
    if len < FOOTER_BYTES as u64 {
        return Ok(None);
    }
    let mut footer = [0u8; FOOTER_BYTES];
    read_at(file, len - FOOTER_BYTES as u64, &mut footer)?;
    if footer.starts_with(FOOTER_COOKIE) {
        return Ok(Some(footer));
    }
    read_at(file, 0, &mut footer)?;
    if footer.starts_with(FOOTER_COOKIE)
        && matches!(
            be32(&footer, FOOTER_DISK_TYPE),
            DISK_DYNAMIC | DISK_DIFFERENCING
        )
    {
        return Ok(Some(footer));
    }
    Ok(None)
}

/// Whether the file at `path` is a VHD, going by its footer.
pub(super) fn is_vhd_file(path: &Path) -> io::Result<bool> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    Ok(find_footer(&mut file, len)?.is_some())
}

/// A VHD open for reading, and for writing unless opened read-only.
pub(super) struct VhdHardDisk {
    file: File,
    total_sectors: u64,
    /// `None` for a fixed disk, whose sectors run from offset 0 up to the
    /// footer.
    dynamic: Option<Dynamic>,
}

/// Where a dynamic disk's blocks are, and what appending one takes.
///
/// The file is the only authority, as it is for a raw hardfile: a second
/// handle on the same image (a save state reopened beside the machine that
/// took it) may append blocks this one has not seen. A block's place never
/// changes once it has one, so what is cached here can only be out of date
/// by missing a block or the file's new end, and both are read again from
/// the file before this handle acts on them.
struct Dynamic {
    /// Per block, the file sector its bitmap starts at, or [`UNALLOCATED`]
    /// as last read.
    bat: Vec<u32>,
    /// File offset of the BAT, for reading and writing entries.
    table_offset: u64,
    sectors_per_block: u64,
    /// The sector bitmap in front of each block's data: a bit per sector,
    /// padded to a whole sector.
    bitmap_bytes: u64,
    /// The file's own structures -- the footer copy, the header, the BAT --
    /// which no block may overlap: a block written over one of them would
    /// take the disk's layout with it.
    metadata: [(u64, u64); 3],
    /// The end of the furthest structure or block this handle knows of. A
    /// block is appended here or at the file's end, whichever is later.
    known_end: u64,
    /// The footer, written again after each block appended.
    footer: [u8; FOOTER_BYTES],
    /// Blocks whose bitmap marks every sector written (the ones appended
    /// here), which a write needs no bitmap update for. Bits are only ever
    /// set, so this cannot go out of date.
    full: HashSet<usize>,
}

impl VhdHardDisk {
    /// Open the VHD at `path`: read-write, or read-only when `writable` is
    /// false (a netplay session copy reads the disk whole and never writes
    /// back).
    pub(super) fn open(path: &Path, bus_name: &str, writable: bool) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(path)
            .with_context(|| format!("opening {bus_name} image {}", path.display()))?;
        let read_error = |e: io::Error| anyhow!("reading {bus_name} image {}: {e}", path.display());
        let len = file.metadata().map_err(read_error)?.len();
        let footer = find_footer(&mut file, len)
            .map_err(read_error)?
            .ok_or_else(|| anyhow!("{}: not a VHD image (no footer)", path.display()))?;
        if be32(&footer, FOOTER_CHECKSUM) != checksum(&footer, FOOTER_CHECKSUM) {
            // Nothing written to the disk depends on it, and refusing the
            // user's disk over one tool's arithmetic helps nobody.
            log::warn!(
                "{bus_name}: {}: VHD footer checksum does not match; using it anyway",
                path.display()
            );
        }
        let disk = match be32(&footer, FOOTER_DISK_TYPE) {
            DISK_FIXED => {
                let data = len - FOOTER_BYTES as u64;
                if data == 0 || !data.is_multiple_of(SECTOR) {
                    bail!(
                        "{}: fixed VHD holds {data} bytes in front of its footer; expected a \
                         non-empty multiple of {SECTOR_SIZE}",
                        path.display()
                    );
                }
                Self {
                    file,
                    total_sectors: data / SECTOR,
                    dynamic: None,
                }
            }
            DISK_DYNAMIC => {
                let dynamic = Dynamic::open(&mut file, path, len, footer)?;
                Self {
                    file,
                    total_sectors: be64(&footer, FOOTER_CURRENT_SIZE) / SECTOR,
                    dynamic: Some(dynamic),
                }
            }
            DISK_DIFFERENCING => bail!(
                "{}: a differencing VHD holds only the changes to a parent image; merge it \
                 into its parent (Hyper-V Manager's Edit Disk, or `VBoxManage clonemedium \
                 child.vhd merged.vhd --format VHD`) and attach the result",
                path.display()
            ),
            other => bail!(
                "{}: VHD disk type {other} is not one this reads (fixed 2, dynamic 3)",
                path.display()
            ),
        };
        log::info!(
            "{bus_name}: {} is a {} VHD of {} sectors",
            path.display(),
            if disk.dynamic.is_some() {
                "dynamic"
            } else {
                "fixed"
            },
            disk.total_sectors
        );
        Ok(disk)
    }

    pub(super) fn total_sectors(&self) -> u64 {
        self.total_sectors
    }

    /// Refuse a sector the disk does not have. For a fixed disk the next
    /// sector along is the footer.
    fn check(&self, lba: u64) -> io::Result<()> {
        if lba < self.total_sectors {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "sector {lba} is past the end of the VHD ({} sectors)",
                    self.total_sectors
                ),
            ))
        }
    }

    pub(super) fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> io::Result<()> {
        self.check(lba)?;
        let buf = &mut buf[..SECTOR_SIZE];
        let at = match &mut self.dynamic {
            None => Some(lba * SECTOR),
            Some(dynamic) => dynamic.locate(&mut self.file, lba)?,
        };
        match at {
            Some(at) => read_at(&mut self.file, at, buf),
            None => {
                buf.fill(0);
                Ok(())
            }
        }
    }

    pub(super) fn write_sector(&mut self, lba: u64, buf: &[u8]) -> io::Result<()> {
        self.check(lba)?;
        let data = &buf[..SECTOR_SIZE];
        match &mut self.dynamic {
            None => {
                self.file.seek(SeekFrom::Start(lba * SECTOR))?;
                self.file.write_all(data)
            }
            Some(dynamic) => dynamic.write(&mut self.file, lba, data),
        }
    }

    pub(super) fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Dynamic {
    fn open(file: &mut File, path: &Path, len: u64, footer: [u8; FOOTER_BYTES]) -> Result<Self> {
        let size = be64(&footer, FOOTER_CURRENT_SIZE);
        if size == 0 || !size.is_multiple_of(SECTOR) || size > MAX_DISK_BYTES {
            bail!(
                "{}: dynamic VHD describes {size} bytes; expected a non-empty multiple of \
                 {SECTOR_SIZE} no larger than {} GiB",
                path.display(),
                MAX_DISK_BYTES >> 30
            );
        }

        let header_offset = be64(&footer, FOOTER_DATA_OFFSET);
        let header_end = header_offset
            .checked_add(DYNAMIC_HEADER_BYTES as u64)
            .filter(|&end| end <= len)
            .ok_or_else(|| {
                anyhow!(
                    "{}: dynamic VHD header at {header_offset} lies past the end of the file",
                    path.display()
                )
            })?;
        let mut header = [0u8; DYNAMIC_HEADER_BYTES];
        read_at(file, header_offset, &mut header)
            .with_context(|| format!("reading dynamic VHD header of {}", path.display()))?;
        if !header.starts_with(DYNAMIC_COOKIE) {
            bail!(
                "{}: dynamic VHD has no `cxsparse` header at {header_offset}",
                path.display()
            );
        }
        if be32(&header, HEADER_CHECKSUM) != checksum(&header, HEADER_CHECKSUM) {
            log::warn!(
                "{}: dynamic VHD header checksum does not match; using it anyway",
                path.display()
            );
        }

        let block_size = be32(&header, HEADER_BLOCK_SIZE);
        if !block_size.is_power_of_two()
            || (block_size as usize) < SECTOR_SIZE
            || block_size > MAX_BLOCK_BYTES
        {
            bail!(
                "{}: dynamic VHD block size {block_size} is not a power of two from \
                 {SECTOR_SIZE} to {MAX_BLOCK_BYTES} bytes",
                path.display()
            );
        }
        let blocks = size.div_ceil(u64::from(block_size));
        let max_entries = be32(&header, HEADER_MAX_TABLE_ENTRIES);
        if blocks > u64::from(max_entries) {
            bail!(
                "{}: dynamic VHD needs {blocks} blocks but its table holds {max_entries}",
                path.display()
            );
        }
        if blocks > MAX_BAT_ENTRIES {
            bail!(
                "{}: dynamic VHD has {blocks} blocks of {block_size} bytes, more than the \
                 {MAX_BAT_ENTRIES} this holds a table for",
                path.display()
            );
        }
        let table_offset = be64(&header, HEADER_TABLE_OFFSET);
        let table_end = table_offset
            .checked_add(blocks * 4)
            .filter(|&end| end <= len)
            .ok_or_else(|| {
                anyhow!(
                    "{}: dynamic VHD block table at {table_offset} runs past the end of the \
                     file",
                    path.display()
                )
            })?;
        let mut raw = vec![0u8; (blocks * 4) as usize];
        read_at(file, table_offset, &mut raw)
            .with_context(|| format!("reading dynamic VHD block table of {}", path.display()))?;
        let bat: Vec<u32> = raw.chunks_exact(4).map(|entry| be32(entry, 0)).collect();

        let sectors_per_block = u64::from(block_size) / SECTOR;
        let mut dynamic = Self {
            bat,
            table_offset,
            sectors_per_block,
            bitmap_bytes: sectors_per_block.div_ceil(8).next_multiple_of(SECTOR),
            metadata: [
                (0, FOOTER_BYTES as u64),
                (header_offset, header_end),
                (table_offset, table_end),
            ],
            known_end: header_end.max(table_end),
            footer,
            full: HashSet::new(),
        };
        for block in 0..dynamic.bat.len() {
            dynamic
                .admit(block)
                .map_err(|e| anyhow!("{}: dynamic VHD {e}", path.display()))?;
        }
        Ok(dynamic)
    }

    fn block_bytes(&self) -> u64 {
        self.bitmap_bytes + self.sectors_per_block * SECTOR
    }

    /// Accept the BAT entry just read for `block`: refuse one that overlaps
    /// the file's own structures, and count its end in `known_end`.
    fn admit(&mut self, block: usize) -> io::Result<()> {
        let entry = self.bat[block];
        if entry == UNALLOCATED {
            return Ok(());
        }
        let start = u64::from(entry) * SECTOR;
        let end = start + self.block_bytes();
        if self.metadata.iter().any(|&(lo, hi)| start < hi && lo < end) {
            self.bat[block] = UNALLOCATED;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "block {block} at sector {entry} overlaps the image's own header or block \
                     table"
                ),
            ));
        }
        self.known_end = self.known_end.max(end);
        Ok(())
    }

    /// The file sector `block` starts at, or [`UNALLOCATED`]. An entry
    /// last seen unallocated is read again, in case the block has since
    /// been appended through another handle.
    fn entry(&mut self, file: &mut File, block: usize) -> io::Result<u32> {
        if self.bat[block] == UNALLOCATED {
            let mut raw = [0u8; 4];
            read_at(file, self.table_offset + block as u64 * 4, &mut raw)?;
            self.bat[block] = u32::from_be_bytes(raw);
            self.admit(block)?;
        }
        Ok(self.bat[block])
    }

    /// Where sector `lba`'s data sits in the file, or `None` for a sector
    /// in a block that has no place yet (and so reads as zeros).
    fn locate(&mut self, file: &mut File, lba: u64) -> io::Result<Option<u64>> {
        let entry = self.entry(file, (lba / self.sectors_per_block) as usize)?;
        Ok((entry != UNALLOCATED).then(|| {
            u64::from(entry) * SECTOR + self.bitmap_bytes + (lba % self.sectors_per_block) * SECTOR
        }))
    }

    fn write(&mut self, file: &mut File, lba: u64, data: &[u8]) -> io::Result<()> {
        let block = (lba / self.sectors_per_block) as usize;
        if self.entry(file, block)? == UNALLOCATED {
            // Such a block reads as zeros already; appending one to say so
            // again would only grow the file.
            if data.iter().all(|&byte| byte == 0) {
                return Ok(());
            }
            self.append_block(file, block)?;
        }
        let at = self
            .locate(file, lba)?
            .expect("the block was given a place above");
        file.seek(SeekFrom::Start(at))?;
        file.write_all(data)?;
        self.mark_written(file, block, lba % self.sectors_per_block)
    }

    /// Give `block` its place in the file: a sector bitmap with every
    /// sector marked written (as qemu and WinUAE write it) and zeroed data
    /// go where the footer was, the footer goes after them, and only then
    /// does the BAT point at the block -- so a write cut off part way
    /// leaves a disk that reads as it did before.
    ///
    /// The footer is looked for on the file as it is now, not as it was at
    /// open. When it is missing -- the last append was cut off -- the block
    /// goes past whatever that left behind, rounded up to a sector.
    fn append_block(&mut self, file: &mut File, block: usize) -> io::Result<()> {
        let len = file.metadata()?.len();
        let mut tail = [0u8; FOOTER_COOKIE.len()];
        let footer_at_end = len >= FOOTER_BYTES as u64 && {
            read_at(file, len - FOOTER_BYTES as u64, &mut tail)?;
            tail == *FOOTER_COOKIE
        };
        let start = if footer_at_end {
            len - FOOTER_BYTES as u64
        } else {
            len.next_multiple_of(SECTOR)
        }
        .max(self.known_end);
        let entry = u32::try_from(start / SECTOR)
            .ok()
            .filter(|&entry| entry != UNALLOCATED)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::StorageFull,
                    "dynamic VHD cannot grow past 2 TiB",
                )
            })?;
        let block_size = self.sectors_per_block * SECTOR;
        file.seek(SeekFrom::Start(start))?;
        file.write_all(&vec![0xFF; self.bitmap_bytes as usize])?;
        let zeros = vec![0u8; block_size.min(1 << 20) as usize];
        let mut left = block_size;
        while left > 0 {
            let chunk = left.min(zeros.len() as u64) as usize;
            file.write_all(&zeros[..chunk])?;
            left -= chunk as u64;
        }
        file.write_all(&self.footer)?;
        file.seek(SeekFrom::Start(self.table_offset + block as u64 * 4))?;
        file.write_all(&entry.to_be_bytes())?;
        self.bat[block] = entry;
        self.known_end = start + self.block_bytes();
        self.full.insert(block);
        Ok(())
    }

    /// Make sure the bitmap of `block` marks `sector` as written.
    ///
    /// Reading here ignores the bitmap, as qemu and WinUAE do: a sector
    /// never written is zero in the block's data anyway. Windows and
    /// Virtual PC read a clear bit as "never written" and return zeros
    /// without looking, though, so a write into a block some other tool
    /// appended with only some sectors marked has to set its bit to be seen
    /// there. The byte is read from the file each time rather than cached,
    /// so a bit another handle set is never written back clear.
    fn mark_written(&mut self, file: &mut File, block: usize, sector: u64) -> io::Result<()> {
        if self.full.contains(&block) {
            return Ok(());
        }
        let at = u64::from(self.bat[block]) * SECTOR + sector / 8;
        // The most significant bit of each byte is the lowest sector.
        let bit = 0x80 >> (sector % 8);
        let mut byte = [0u8; 1];
        read_at(file, at, &mut byte)?;
        if byte[0] & bit == 0 {
            byte[0] |= bit;
            file.seek(SeekFrom::Start(at))?;
            file.write_all(&byte)?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A footer of `disk_type` for a disk of `size` bytes whose dynamic
    /// header (when it has one) is at `data_offset`.
    fn footer(disk_type: u32, size: u64, data_offset: u64) -> [u8; FOOTER_BYTES] {
        let mut f = [0u8; FOOTER_BYTES];
        f[..8].copy_from_slice(FOOTER_COOKIE);
        f[8..12].copy_from_slice(&2u32.to_be_bytes());
        f[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        f[FOOTER_DATA_OFFSET..FOOTER_DATA_OFFSET + 8].copy_from_slice(&data_offset.to_be_bytes());
        f[28..32].copy_from_slice(b"test");
        f[40..48].copy_from_slice(&size.to_be_bytes());
        f[FOOTER_CURRENT_SIZE..FOOTER_CURRENT_SIZE + 8].copy_from_slice(&size.to_be_bytes());
        f[FOOTER_DISK_TYPE..FOOTER_DISK_TYPE + 4].copy_from_slice(&disk_type.to_be_bytes());
        let sum = checksum(&f, FOOTER_CHECKSUM);
        f[FOOTER_CHECKSUM..FOOTER_CHECKSUM + 4].copy_from_slice(&sum.to_be_bytes());
        f
    }

    /// A fixed VHD of `data`: the sectors, then the footer.
    pub(in crate::harddrive) fn fixed_vhd(data: &[u8]) -> Vec<u8> {
        let mut bytes = data.to_vec();
        bytes.extend_from_slice(&footer(DISK_FIXED, data.len() as u64, u64::MAX));
        bytes
    }

    /// An empty dynamic VHD of `size` bytes in blocks of `block_size`, laid
    /// out as qemu-img and VirtualBox lay one out: footer copy, header at 512, BAT
    /// at 1536 padded to a sector, footer.
    pub(in crate::harddrive) fn empty_dynamic_vhd(size: u64, block_size: u32) -> Vec<u8> {
        let blocks = size.div_ceil(u64::from(block_size));
        let table_offset = 1536u64;
        let table_bytes = (blocks * 4).next_multiple_of(SECTOR);
        let foot = footer(DISK_DYNAMIC, size, 512);
        let mut header = [0u8; DYNAMIC_HEADER_BYTES];
        header[..8].copy_from_slice(DYNAMIC_COOKIE);
        header[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
        header[HEADER_TABLE_OFFSET..HEADER_TABLE_OFFSET + 8]
            .copy_from_slice(&table_offset.to_be_bytes());
        header[24..28].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        header[HEADER_MAX_TABLE_ENTRIES..HEADER_MAX_TABLE_ENTRIES + 4]
            .copy_from_slice(&(blocks as u32).to_be_bytes());
        header[HEADER_BLOCK_SIZE..HEADER_BLOCK_SIZE + 4].copy_from_slice(&block_size.to_be_bytes());
        let sum = checksum(&header, HEADER_CHECKSUM);
        header[HEADER_CHECKSUM..HEADER_CHECKSUM + 4].copy_from_slice(&sum.to_be_bytes());

        let mut bytes = foot.to_vec();
        bytes.extend_from_slice(&header);
        bytes.extend(std::iter::repeat_n(0xFF, table_bytes as usize));
        bytes.extend_from_slice(&foot);
        bytes
    }

    fn temp_file(name: &str, bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    fn sector(fill: u8) -> Vec<u8> {
        vec![fill; SECTOR_SIZE]
    }

    #[test]
    fn checksum_is_the_complemented_byte_sum_without_its_own_field() {
        let f = footer(DISK_FIXED, 1 << 20, u64::MAX);
        let stored = be32(&f, FOOTER_CHECKSUM);
        let sum: u32 = f
            .iter()
            .enumerate()
            .filter(|(i, _)| !(FOOTER_CHECKSUM..FOOTER_CHECKSUM + 4).contains(i))
            .map(|(_, &b)| u32::from(b))
            .sum();
        assert_eq!(stored, !sum);
    }

    #[test]
    fn fixed_vhd_serves_the_sectors_in_front_of_its_footer_and_protects_it() {
        let mut data = vec![0u8; 16 * SECTOR_SIZE];
        data[15 * SECTOR_SIZE..15 * SECTOR_SIZE + 4].copy_from_slice(b"LAST");
        let (_dir, path) = temp_file("fixed.vhd", &fixed_vhd(&data));
        let mut disk = VhdHardDisk::open(&path, "ide", true).unwrap();
        assert_eq!(disk.total_sectors(), 16);

        let mut buf = sector(0);
        disk.read_sector(15, &mut buf).unwrap();
        assert_eq!(&buf[..4], b"LAST");
        // The footer is not a sector of the disk, to read or to write.
        assert!(disk.read_sector(16, &mut buf).is_err());
        assert!(disk.write_sector(16, &sector(0xEE)).is_err());

        disk.write_sector(15, &sector(0x5A)).unwrap();
        disk.flush().unwrap();
        let file = std::fs::read(&path).unwrap();
        assert_eq!(&file[15 * SECTOR_SIZE..16 * SECTOR_SIZE], &sector(0x5A)[..]);
        assert_eq!(&file[16 * SECTOR_SIZE..16 * SECTOR_SIZE + 8], FOOTER_COOKIE);
    }

    #[test]
    fn dynamic_vhd_reads_unallocated_blocks_as_zeros_without_growing() {
        let image = empty_dynamic_vhd(8 << 20, 2 << 20);
        let (_dir, path) = temp_file("sparse.vhd", &image);
        let mut disk = VhdHardDisk::open(&path, "ide", true).unwrap();
        assert_eq!(disk.total_sectors(), (8 << 20) / SECTOR);

        let mut buf = sector(0xAA);
        disk.read_sector(12345, &mut buf).unwrap();
        assert_eq!(buf, sector(0));
        // Writing zeros to a block with no place changes nothing.
        disk.write_sector(12345, &sector(0)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), image);
    }

    #[test]
    fn dynamic_vhd_appends_a_block_on_first_write_and_moves_the_footer() {
        let image = empty_dynamic_vhd(8 << 20, 2 << 20);
        let (_dir, path) = temp_file("grow.vhd", &image);
        let spb = (2u64 << 20) / SECTOR;
        let lba = 2 * spb + 7; // block 2
        {
            let mut disk = VhdHardDisk::open(&path, "ide", true).unwrap();
            disk.write_sector(lba, &sector(0x42)).unwrap();
            // A second write into the same block reuses it.
            disk.write_sector(lba + 1, &sector(0x43)).unwrap();
            disk.flush().unwrap();
        }
        let file = std::fs::read(&path).unwrap();
        let old_footer_at = image.len() - FOOTER_BYTES;
        let block_bytes = SECTOR_SIZE + (2 << 20);
        assert_eq!(file.len(), image.len() + block_bytes);
        // Footer at the new end, copy still at the front, both unchanged.
        assert_eq!(&file[file.len() - FOOTER_BYTES..], &image[old_footer_at..]);
        assert_eq!(&file[..FOOTER_BYTES], &image[..FOOTER_BYTES]);
        // BAT entry 2 points at the block, which went where the footer was.
        assert_eq!(
            be32(&file, 1536 + 2 * 4),
            (old_footer_at / SECTOR_SIZE) as u32
        );
        assert_eq!(be32(&file, 1536), UNALLOCATED);
        // Bitmap all set, then the data.
        assert!(file[old_footer_at..old_footer_at + SECTOR_SIZE]
            .iter()
            .all(|&b| b == 0xFF));
        let data_at = old_footer_at + SECTOR_SIZE + 7 * SECTOR_SIZE;
        assert_eq!(&file[data_at..data_at + SECTOR_SIZE], &sector(0x42)[..]);

        // Reopened, the disk reads back what was written, and zeros around it.
        let mut disk = VhdHardDisk::open(&path, "ide", true).unwrap();
        let mut buf = sector(0);
        disk.read_sector(lba, &mut buf).unwrap();
        assert_eq!(buf, sector(0x42));
        disk.read_sector(lba + 1, &mut buf).unwrap();
        assert_eq!(buf, sector(0x43));
        disk.read_sector(lba - 1, &mut buf).unwrap();
        assert_eq!(buf, sector(0));
        // The next block appended goes after the first, not over it.
        disk.write_sector(5, &sector(0x77)).unwrap();
        let file = std::fs::read(&path).unwrap();
        assert_eq!(file.len(), image.len() + 2 * block_bytes);
        assert_eq!(
            be32(&file, 1536),
            ((old_footer_at + block_bytes) / SECTOR_SIZE) as u32
        );
        disk.read_sector(lba, &mut buf).unwrap();
        assert_eq!(buf, sector(0x42));
    }

    #[test]
    fn two_handles_on_one_dynamic_vhd_append_without_colliding() {
        // A save state reopened beside the machine that took it: each
        // handle appends a block the other has not seen.
        let image = empty_dynamic_vhd(8 << 20, 2 << 20);
        let (_dir, path) = temp_file("shared.vhd", &image);
        let spb = (2u64 << 20) / SECTOR;
        let mut a = VhdHardDisk::open(&path, "ide", true).unwrap();
        let mut b = VhdHardDisk::open(&path, "ide", true).unwrap();
        a.write_sector(1, &sector(0xAA)).unwrap();
        b.write_sector(spb + 1, &sector(0xBB)).unwrap();
        a.write_sector(3 * spb, &sector(0xCC)).unwrap();

        let block_bytes = SECTOR_SIZE + (2 << 20);
        let file = std::fs::read(&path).unwrap();
        assert_eq!(file.len(), image.len() + 3 * block_bytes);
        let mut buf = sector(0);
        for disk in [&mut a, &mut b] {
            disk.read_sector(1, &mut buf).unwrap();
            assert_eq!(buf, sector(0xAA));
            disk.read_sector(spb + 1, &mut buf).unwrap();
            assert_eq!(buf, sector(0xBB));
            disk.read_sector(3 * spb, &mut buf).unwrap();
            assert_eq!(buf, sector(0xCC));
        }
    }

    #[test]
    fn dynamic_vhd_write_sets_the_bitmap_bit_another_tool_left_clear() {
        let mut image = empty_dynamic_vhd(4 << 20, 2 << 20);
        // Block 0 appended by some other tool with only sector 0 marked.
        let at = image.len() - FOOTER_BYTES;
        let foot = image[at..].to_vec();
        image.truncate(at);
        let mut bitmap = vec![0u8; SECTOR_SIZE];
        bitmap[0] = 0x80;
        image.extend_from_slice(&bitmap);
        image.extend(std::iter::repeat_n(0, 2 << 20));
        image.extend_from_slice(&foot);
        image[1536..1540].copy_from_slice(&((at / SECTOR_SIZE) as u32).to_be_bytes());
        let (_dir, path) = temp_file("bitmap.vhd", &image);

        let mut disk = VhdHardDisk::open(&path, "ide", true).unwrap();
        disk.write_sector(9, &sector(0x99)).unwrap();
        let file = std::fs::read(&path).unwrap();
        assert_eq!(
            file.len(),
            image.len(),
            "an allocated block does not grow the file"
        );
        // Sector 9 is byte 1, bit 6 counting from the top.
        assert_eq!(file[at], 0x80);
        assert_eq!(file[at + 1], 0x40);
    }

    #[test]
    fn dynamic_vhd_whose_trailing_footer_was_cut_off_opens_from_its_copy() {
        let image = empty_dynamic_vhd(4 << 20, 2 << 20);
        // Some of a block's worth of bytes past the table, no footer after.
        let mut cut = image[..image.len() - FOOTER_BYTES].to_vec();
        cut.extend(std::iter::repeat_n(0x11, 3 * SECTOR_SIZE + 17));
        let (_dir, path) = temp_file("cut.vhd", &cut);
        assert!(is_vhd_file(&path).unwrap());
        let mut disk = VhdHardDisk::open(&path, "ide", true).unwrap();
        disk.write_sector(0, &sector(0x31)).unwrap();
        let file = std::fs::read(&path).unwrap();
        // The new block starts past the leftover bytes, rounded to a sector.
        let start = cut.len().next_multiple_of(SECTOR_SIZE);
        assert_eq!(be32(&file, 1536), (start / SECTOR_SIZE) as u32);
        assert_eq!(&file[file.len() - FOOTER_BYTES..][..8], FOOTER_COOKIE);
    }

    #[test]
    fn differencing_vhd_is_refused_with_a_way_forward() {
        let mut image = empty_dynamic_vhd(4 << 20, 2 << 20);
        let differencing = footer(DISK_DIFFERENCING, 4 << 20, 512);
        image[..FOOTER_BYTES].copy_from_slice(&differencing);
        let at = image.len() - FOOTER_BYTES;
        image[at..].copy_from_slice(&differencing);
        let (_dir, path) = temp_file("child.vhd", &image);
        let err = VhdHardDisk::open(&path, "ide", true).err().unwrap();
        assert!(err.to_string().contains("differencing"), "{err}");
        assert!(err.to_string().contains("merge"), "{err}");
    }

    #[test]
    fn hostile_dynamic_headers_are_refused_before_anything_is_sized_from_them() {
        let base = empty_dynamic_vhd(4 << 20, 2 << 20);
        let patch = |edit: &dyn Fn(&mut Vec<u8>)| {
            let mut image = base.clone();
            edit(&mut image);
            let (_dir, path) = temp_file("hostile.vhd", &image);
            VhdHardDisk::open(&path, "ide", true)
                .err()
                .map(|e| e.to_string())
        };
        let set_footer_size = |image: &mut Vec<u8>, size: u64| {
            let at = image.len() - FOOTER_BYTES;
            image[at + FOOTER_CURRENT_SIZE..at + FOOTER_CURRENT_SIZE + 8]
                .copy_from_slice(&size.to_be_bytes());
        };
        // A petabyte disk.
        let err = patch(&|image| set_footer_size(image, 1 << 50)).unwrap();
        assert!(err.contains("describes"), "{err}");
        // More blocks than the table holds.
        let err = patch(&|image| set_footer_size(image, 64 << 20)).unwrap();
        assert!(err.contains("table holds"), "{err}");
        // A block size that is not a power of two.
        let err = patch(&|image| {
            image[512 + HEADER_BLOCK_SIZE..512 + HEADER_BLOCK_SIZE + 4]
                .copy_from_slice(&3000u32.to_be_bytes())
        })
        .unwrap();
        assert!(err.contains("block size"), "{err}");
        // A table past the end of the file.
        let err = patch(&|image| {
            image[512 + HEADER_TABLE_OFFSET..512 + HEADER_TABLE_OFFSET + 8]
                .copy_from_slice(&(1u64 << 40).to_be_bytes())
        })
        .unwrap();
        assert!(err.contains("past the end"), "{err}");
        // A block placed over the header.
        let err = patch(&|image| image[1536..1540].copy_from_slice(&1u32.to_be_bytes())).unwrap();
        assert!(err.contains("overlaps"), "{err}");
    }

    #[test]
    fn files_without_a_footer_are_not_vhds() {
        let (_dir, path) = temp_file("raw.hdf", &vec![0u8; 8 * SECTOR_SIZE]);
        assert!(!is_vhd_file(&path).unwrap());
        let (_dir, path) = temp_file("tiny.vhd", b"conectix");
        assert!(!is_vhd_file(&path).unwrap());
        // A fixed footer's cookie at offset 0 is just a first sector.
        let mut data = footer(DISK_FIXED, 4096, u64::MAX).to_vec();
        data.resize(8 * SECTOR_SIZE, 0);
        let (_dir, path) = temp_file("nested.hdf", &data);
        assert!(!is_vhd_file(&path).unwrap());
    }
}
