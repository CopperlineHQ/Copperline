// SPDX-License-Identifier: GPL-3.0-or-later

//! A Flyer SCSI drive served from a host folder.
//!
//! The real Flyer formats its drives with a filesystem of its own (a ROOT
//! block, FILE/FDIR headers in a hashed tree, and each file one contiguous
//! run of 512-byte blocks). The Amiga sees that filesystem only through
//! the Flyer's commands, but block numbers leak through them: a LOCATE or
//! FILEOPEN reports a file's first block, ClipInfo carries start, end and
//! index blocks, and the library then reads clips by block number (READ10,
//! FILEHSREAD). So a host folder is given a consistent block space here:
//! block 0 stands for the root, each file and directory has a header block
//! number, and each file one contiguous run of data blocks with room to
//! spare after it. Numbers stay put for as long as the emulator runs, also
//! across rescans of the folder; a save state carries them.
//!
//! Drive `n` is `bus * 8 + target`: `FA0` (bus A, target 0) is drive 0,
//! `FB0` drive 8, `FC0` drive 16. Each lives in a folder of that name under
//! the Toaster's `Flyer` folder. Files are the clips' bytes verbatim, as
//! FlyerFileSystem copies them (`crates/flyerclip/FORMAT.md` §2).

use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Bytes per block.
pub const BLOCK: usize = 512;
/// The first header block number of a file or directory; the root is 0.
const HEADERS_START: u32 = 0x100;
/// The first data block. Headers fill the blocks below it.
const DATA_START: u32 = 0x1_0000;
/// Blocks kept free after each file, so one that grows a little keeps its
/// place.
const SLACK_BLOCKS: u32 = 2048;
/// The size the drive reports: 8 GB, the largest drives sold with Flyers.
pub const DRIVE_BLOCKS: u32 = 0x100_0000;
/// Longest Flyer name (31 characters and a NUL).
pub const NAME_MAX: usize = 31;

/// FlyerVolInfo flags (`FVIF_`).
pub const FVIF_VIDEOREADY: u8 = 1 << 0;
pub const FVIF_AUDIOREADY: u8 = 1 << 1;
pub const FVIF_WRITEPROT: u8 = 1 << 2;

/// Object types (`TYPE_` in the firmware's `filesys.h`).
pub const TYPE_FILE: u8 = 1;
pub const TYPE_DIR: u8 = 2;
pub const TYPE_ROOT: u8 = 3;

/// One file, directory or the root.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Object {
    /// The Flyer name (the host name; empty for the root).
    pub name: String,
    /// Host path relative to the drive folder.
    rel: PathBuf,
    /// `TYPE_FILE`, `TYPE_DIR` or `TYPE_ROOT`.
    pub kind: u8,
    /// Index of the parent directory (the root is its own parent).
    pub parent: usize,
    /// Children in listing order (by name, case-insensitively).
    pub children: Vec<usize>,
    /// First data block (files).
    pub start: u32,
    /// Blocks set aside for the file's data, slack included.
    reserved: u32,
    /// Byte length (files).
    pub bytes: u64,
    /// AmigaDOS DateStamp: days since 1978, minutes, ticks.
    pub date: [u32; 3],
    /// Still in the folder at the last scan.
    pub present: bool,
}

/// What a block number holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockUse {
    /// A file's data: object, block within the file.
    Data(usize, u64),
    /// A header block (root, file or directory).
    Header(usize),
    /// Nothing.
    Free,
}

/// A drive in a host folder.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostDrive {
    /// SCSI drive number: bus * 8 + target.
    pub number: u8,
    folder: PathBuf,
    /// Volume name (`FlyerA0`, ... unless a `.flyervol` file names it).
    pub volume: String,
    /// The objects; index 0 is the root.
    objects: Vec<Object>,
    /// The next unused data block.
    next_data: u32,
}

impl HostDrive {
    /// The drive for `number` in `flyer` (the Toaster's `Flyer` folder),
    /// if its folder exists.
    pub fn open(flyer: &Path, number: u8) -> Option<Self> {
        let folder = flyer.join(drive_name(number));
        if !folder.is_dir() {
            return None;
        }
        let mut drive = Self::empty(number, folder);
        drive.scan();
        Some(drive)
    }

    fn empty(number: u8, folder: PathBuf) -> Self {
        let root = Object {
            name: String::new(),
            rel: PathBuf::new(),
            kind: TYPE_ROOT,
            parent: 0,
            children: Vec::new(),
            start: 0,
            reserved: 0,
            bytes: 0,
            date: [0; 3],
            present: true,
        };
        Self {
            number,
            folder,
            volume: default_volume_name(number),
            objects: vec![root],
            next_data: DATA_START,
        }
    }

    /// The host folder.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Re-read the folder: new files and directories get header and data
    /// blocks after the existing ones, files that grew past their room
    /// move to new blocks, and vanished ones are no longer listed.
    pub fn scan(&mut self) {
        if let Ok(meta) = std::fs::metadata(&self.folder) {
            self.objects[0].date = datestamp(&meta);
        }
        self.volume = std::fs::read_to_string(self.folder.join(".flyervol"))
            .ok()
            .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
            .filter(|name| valid_name(name))
            .unwrap_or_else(|| default_volume_name(self.number));
        for obj in &mut self.objects[1..] {
            obj.present = false;
        }
        self.scan_dir(0);
    }

    fn scan_dir(&mut self, dir: usize) {
        let path = self.folder.join(&self.objects[dir].rel);
        let mut entries: Vec<(String, std::fs::Metadata)> = match std::fs::read_dir(&path) {
            Ok(read) => read
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().into_string().ok()?;
                    let meta = e.metadata().ok()?;
                    Some((name, meta))
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        entries.retain(|(name, meta)| {
            !name.starts_with('.')
                && (meta.is_dir() || meta.is_file())
                && if valid_name(name) {
                    true
                } else {
                    log::warn!(
                        "flyer: {}: {name:?} is not a Flyer name; not listed",
                        drive_name(self.number)
                    );
                    false
                }
        });
        entries.sort_by_key(|(name, _)| name.to_ascii_uppercase());
        let mut children = Vec::with_capacity(entries.len());
        for (name, meta) in entries {
            let rel = self.objects[dir].rel.join(&name);
            let kind = if meta.is_dir() { TYPE_DIR } else { TYPE_FILE };
            let known = self
                .objects
                .iter()
                .position(|o| o.rel == rel && o.kind == kind && !o.present);
            let index = match known {
                Some(i) => i,
                None => {
                    self.objects.push(Object {
                        name: name.clone(),
                        rel,
                        kind,
                        parent: dir,
                        children: Vec::new(),
                        start: 0,
                        reserved: 0,
                        bytes: 0,
                        date: [0; 3],
                        present: false,
                    });
                    self.objects.len() - 1
                }
            };
            let obj = &mut self.objects[index];
            obj.present = true;
            obj.date = datestamp(&meta);
            if kind == TYPE_FILE {
                obj.bytes = meta.len();
                let blocks = blocks_for(obj.bytes);
                if obj.reserved < blocks || obj.start == 0 {
                    // Room for it after everything else.
                    let room = blocks.saturating_add(SLACK_BLOCKS);
                    obj.start = self.next_data;
                    obj.reserved = room;
                    self.next_data = self.next_data.saturating_add(room);
                }
            }
            children.push(index);
            if kind == TYPE_DIR {
                self.scan_dir(index);
            }
        }
        self.objects[dir].children = children;
    }

    pub fn object(&self, index: usize) -> Option<&Object> {
        self.objects.get(index).filter(|o| o.present)
    }

    /// The host file or folder of an object.
    pub fn host_path(&self, index: usize) -> PathBuf {
        self.folder.join(&self.objects[index].rel)
    }

    /// The header block number of an object.
    pub fn header_block(&self, index: usize) -> u32 {
        if index == 0 {
            0
        } else {
            HEADERS_START + index as u32
        }
    }

    /// The object a header block number stands for.
    pub fn object_at_header(&self, block: u32) -> Option<usize> {
        let index = if block == 0 {
            0
        } else {
            block.checked_sub(HEADERS_START)? as usize
        };
        self.object(index).map(|_| index)
    }

    /// Find `name` among a directory's children, as the firmware does:
    /// case-insensitively.
    pub fn child(&self, dir: usize, name: &str) -> Option<usize> {
        self.objects[dir]
            .children
            .iter()
            .copied()
            .find(|&c| self.objects[c].present && names_match(&self.objects[c].name, name))
    }

    /// Look up a path from directory `from`, the way the firmware's
    /// `LocObj` reads it: anything up to the last `:` is a device name, a
    /// leading `/` (one per level) goes to the parent, and the parts are
    /// separated by `/`. An empty path is `from` itself.
    pub fn lookup(&self, from: usize, path: &str) -> Option<usize> {
        let mut name = strip_device(path);
        let mut at = from;
        while !name.is_empty() {
            if let Some(rest) = name.strip_prefix('/') {
                if self.objects[at].kind == TYPE_ROOT {
                    return None;
                }
                at = self.objects[at].parent;
                name = rest;
                continue;
            }
            let (part, rest, more) = match name.find('/') {
                Some(i) => (&name[..i], &name[i + 1..], true),
                None => (name, "", false),
            };
            at = self.child(at, part)?;
            if more && self.objects[at].kind == TYPE_FILE {
                return None;
            }
            name = rest;
        }
        Some(at)
    }

    /// What block `lba` holds.
    pub fn block_use(&self, lba: u32) -> BlockUse {
        if lba < DATA_START {
            return match self.object_at_header(lba) {
                Some(i) => BlockUse::Header(i),
                None => BlockUse::Free,
            };
        }
        self.objects
            .iter()
            .enumerate()
            .find(|(_, o)| {
                o.present
                    && o.kind == TYPE_FILE
                    && lba >= o.start
                    && lba < o.start.saturating_add(blocks_for(o.bytes))
            })
            .map_or(BlockUse::Free, |(i, o)| {
                BlockUse::Data(i, u64::from(lba - o.start))
            })
    }

    /// Read `count` blocks from `lba` into `out` (`count * 512` bytes).
    /// Blocks that hold no file data read as zeros; a file's last block is
    /// zero past its end.
    pub fn read_blocks(&self, lba: u32, count: usize, out: &mut [u8]) -> std::io::Result<()> {
        out.fill(0);
        let mut n = 0;
        while n < count {
            let BlockUse::Data(index, block) = self.block_use(lba.wrapping_add(n as u32)) else {
                n += 1;
                continue;
            };
            let obj = &self.objects[index];
            // Read the rest of this file's run in one go.
            let file_blocks = blocks_for(obj.bytes) as u64;
            let run = ((file_blocks - block) as usize).min(count - n);
            let offset = block * BLOCK as u64;
            let len = (run * BLOCK).min((obj.bytes - offset) as usize);
            let mut file = std::fs::File::open(self.folder.join(&obj.rel))?;
            file.seek(SeekFrom::Start(offset))?;
            read_up_to(&mut file, &mut out[n * BLOCK..n * BLOCK + len])?;
            n += run;
        }
        Ok(())
    }

    /// Read bytes from a file at `offset`; returns how many it got.
    pub fn read_file(&self, index: usize, offset: u64, out: &mut [u8]) -> std::io::Result<usize> {
        let obj = &self.objects[index];
        if offset >= obj.bytes {
            return Ok(0);
        }
        let len = out.len().min((obj.bytes - offset) as usize);
        let mut file = std::fs::File::open(self.folder.join(&obj.rel))?;
        file.seek(SeekFrom::Start(offset))?;
        read_up_to(&mut file, &mut out[..len])
    }

    /// The first block of a file (its CLIP header for a clip), if it has
    /// one.
    pub fn first_block(&self, index: usize) -> Option<[u8; BLOCK]> {
        let mut b = [0u8; BLOCK];
        let got = self.read_file(index, 0, &mut b).ok()?;
        (got == BLOCK).then_some(b)
    }

    /// Whether a file is a clip (its first block a CLIP header). Clips
    /// are kept read-only: the Amiga can make and change other files.
    pub fn is_clip(&self, index: usize) -> bool {
        self.first_block(index)
            .is_some_and(|b| flyerclip::header::ClipHeader::parse(&b).is_ok())
    }

    /// Create file `name` in directory `dir`, or empty it if it is there;
    /// its index.
    pub fn create_file(&mut self, dir: usize, name: &str) -> std::io::Result<usize> {
        if !valid_name(name) {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        let path = match self.child(dir, name) {
            Some(i) if self.objects[i].kind != TYPE_FILE => {
                return Err(std::io::ErrorKind::InvalidInput.into())
            }
            Some(i) => self.host_path(i),
            None => self.host_path(dir).join(name),
        };
        std::fs::File::create(&path)?;
        self.scan();
        self.child(dir, name)
            .ok_or_else(|| std::io::ErrorKind::NotFound.into())
    }

    /// Create directory `name` in `dir`; its index.
    pub fn create_dir(&mut self, dir: usize, name: &str) -> std::io::Result<usize> {
        if !valid_name(name) {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        if self.child(dir, name).is_some() {
            return Err(std::io::ErrorKind::AlreadyExists.into());
        }
        std::fs::create_dir(self.host_path(dir).join(name))?;
        self.scan();
        self.child(dir, name)
            .ok_or_else(|| std::io::ErrorKind::NotFound.into())
    }

    /// Delete a file, or an empty directory.
    pub fn delete(&mut self, index: usize) -> std::io::Result<()> {
        let path = self.host_path(index);
        match self.objects[index].kind {
            TYPE_FILE => std::fs::remove_file(path)?,
            TYPE_DIR => std::fs::remove_dir(path)?,
            _ => return Err(std::io::ErrorKind::InvalidInput.into()),
        }
        self.scan();
        Ok(())
    }

    /// Move an object to `name` in directory `dir`.
    pub fn rename(&mut self, index: usize, dir: usize, name: &str) -> std::io::Result<()> {
        if !valid_name(name) || index == 0 {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        if self.child(dir, name).is_some_and(|c| c != index) {
            return Err(std::io::ErrorKind::AlreadyExists.into());
        }
        std::fs::rename(self.host_path(index), self.host_path(dir).join(name))?;
        self.scan();
        Ok(())
    }

    /// Set a file's length. It must stay inside the blocks set aside for
    /// it, as a Flyer file stays one contiguous run (`StorageFull`).
    pub fn set_len(&mut self, index: usize, len: u64) -> std::io::Result<()> {
        if blocks_for(len) > self.objects[index].reserved {
            return Err(std::io::ErrorKind::StorageFull.into());
        }
        std::fs::OpenOptions::new()
            .write(true)
            .open(self.host_path(index))?
            .set_len(len)?;
        self.objects[index].bytes = len;
        Ok(())
    }

    /// Write bytes into a file at `offset`, growing it as needed.
    pub fn write_file(&mut self, index: usize, offset: u64, data: &[u8]) -> std::io::Result<()> {
        let end = offset + data.len() as u64;
        if end > self.objects[index].bytes {
            self.set_len(index, end)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(self.host_path(index))?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(data)
    }

    /// Write whole blocks from `lba` into file `index`'s data, as far as
    /// its length goes (its last block's tail is not part of the file).
    pub fn write_blocks(&mut self, index: usize, lba: u32, data: &[u8]) -> std::io::Result<()> {
        let obj = &self.objects[index];
        let Some(first) = lba.checked_sub(obj.start) else {
            return Err(std::io::ErrorKind::InvalidInput.into());
        };
        let offset = u64::from(first) * BLOCK as u64;
        if offset >= obj.bytes {
            return Ok(());
        }
        let n = (data.len() as u64).min(obj.bytes - offset) as usize;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(self.host_path(index))?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&data[..n])
    }

    /// Data blocks in use, and the largest free run.
    pub fn usage(&self) -> (u32, u32) {
        let used: u32 = self
            .objects
            .iter()
            .filter(|o| o.present && o.kind == TYPE_FILE)
            .map(|o| blocks_for(o.bytes))
            .fold(0, u32::saturating_add);
        (used, DRIVE_BLOCKS.saturating_sub(self.next_data))
    }
}

/// The folder and device name of a drive: `FA0` for bus A target 0.
pub fn drive_name(number: u8) -> String {
    format!("F{}{}", char::from(b'A' + (number >> 3)), number & 7)
}

/// The volume name a drive gets without a `.flyervol`: `FlyerA0`, ...
pub fn default_volume_name(number: u8) -> String {
    format!("Flyer{}{}", char::from(b'A' + (number >> 3)), number & 7)
}

/// Blocks a file of `bytes` takes.
pub fn blocks_for(bytes: u64) -> u32 {
    u32::try_from(bytes.div_ceil(BLOCK as u64)).unwrap_or(u32::MAX)
}

/// A name the Flyer's filesystem can hold: 1-31 Latin-1 characters with
/// no `/` or `:`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= NAME_MAX
        && name
            .chars()
            .all(|c| (c as u32) < 0x100 && c >= ' ' && c != '/' && c != ':')
}

/// The firmware's `CompareStrings`: equal ignoring ASCII case.
pub fn names_match(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The firmware's `StripDevName`: what follows the last `:`.
pub fn strip_device(path: &str) -> &str {
    path.rfind(':').map_or(path, |i| &path[i + 1..])
}

/// A host file's modification time as an AmigaDOS DateStamp.
fn datestamp(meta: &std::fs::Metadata) -> [u32; 3] {
    let secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    unix_to_datestamp(secs)
}

/// Seconds since 1970 as days since 1978, minutes and 50 Hz ticks.
pub fn unix_to_datestamp(secs: u64) -> [u32; 3] {
    const AMIGA_EPOCH: u64 = 2922 * 86_400;
    let secs = secs.saturating_sub(AMIGA_EPOCH);
    let days = secs / 86_400;
    let rest = secs % 86_400;
    [days as u32, (rest / 60) as u32, ((rest % 60) * 50) as u32]
}

fn read_up_to(file: &mut std::fs::File, out: &mut [u8]) -> std::io::Result<usize> {
    let mut got = 0;
    while got < out.len() {
        match file.read(&mut out[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let fa0 = dir.path().join("FA0");
        std::fs::create_dir_all(fa0.join("Projects/Old")).unwrap();
        std::fs::write(fa0.join("Clip One"), vec![7u8; 1500]).unwrap();
        std::fs::write(fa0.join("Projects/notes"), b"hello").unwrap();
        std::fs::write(fa0.join(".hidden"), b"x").unwrap();
        std::fs::write(fa0.join("a name that is far too long for the Flyer"), b"x").unwrap();
        dir
    }

    #[test]
    fn drive_names_follow_bus_and_target() {
        assert_eq!(drive_name(0), "FA0");
        assert_eq!(drive_name(8), "FB0");
        assert_eq!(drive_name(17), "FC1");
        assert_eq!(default_volume_name(0), "FlyerA0");
        assert_eq!(default_volume_name(16), "FlyerC0");
    }

    #[test]
    fn a_folder_becomes_a_tree_of_flyer_objects() {
        let dir = folder();
        assert!(HostDrive::open(dir.path(), 8).is_none());
        let drive = HostDrive::open(dir.path(), 0).unwrap();
        assert_eq!(drive.volume, "FlyerA0");
        let root = drive.object(0).unwrap();
        assert_eq!(root.kind, TYPE_ROOT);
        let names: Vec<_> = root
            .children
            .iter()
            .map(|&c| drive.object(c).unwrap().name.as_str())
            .collect();
        assert_eq!(names, ["Clip One", "Projects"]);
        let clip = drive.lookup(0, "clip one").unwrap();
        assert_eq!(drive.object(clip).unwrap().bytes, 1500);
        let notes = drive.lookup(0, "FlyerA0:Projects/Notes").unwrap();
        assert_eq!(drive.object(notes).unwrap().kind, TYPE_FILE);
        let projects = drive.lookup(0, "Projects").unwrap();
        assert_eq!(drive.lookup(projects, "/"), Some(0));
        assert_eq!(drive.lookup(projects, "/Clip One"), Some(clip));
        assert_eq!(
            drive.lookup(projects, "Old"),
            drive.lookup(0, "Projects/Old")
        );
        assert_eq!(drive.lookup(0, "/"), None);
        assert_eq!(drive.lookup(0, "Clip One/x"), None);
        assert_eq!(drive.lookup(0, "missing"), None);
        assert_eq!(drive.lookup(0, ""), Some(0));
        assert_eq!(
            drive.object_at_header(drive.header_block(notes)),
            Some(notes)
        );
        assert_eq!(drive.object_at_header(0), Some(0));
    }

    #[test]
    fn files_get_contiguous_blocks_that_read_back_their_bytes() {
        let dir = folder();
        let drive = HostDrive::open(dir.path(), 0).unwrap();
        let clip = drive.lookup(0, "Clip One").unwrap();
        let start = drive.object(clip).unwrap().start;
        assert!(start >= DATA_START);
        assert_eq!(drive.block_use(start), BlockUse::Data(clip, 0));
        assert_eq!(drive.block_use(start + 2), BlockUse::Data(clip, 2));
        assert_eq!(drive.block_use(start + 3), BlockUse::Free);
        let mut buf = vec![0xAAu8; 4 * BLOCK];
        drive.read_blocks(start, 4, &mut buf).unwrap();
        assert!(buf[..1500].iter().all(|&b| b == 7));
        assert!(buf[1500..].iter().all(|&b| b == 0));
        let mut part = [0u8; 10];
        assert_eq!(drive.read_file(clip, 1495, &mut part).unwrap(), 5);
        assert_eq!(drive.read_file(clip, 1500, &mut part).unwrap(), 0);
    }

    #[test]
    fn rescans_keep_block_numbers_and_pick_up_changes() {
        let dir = folder();
        let mut drive = HostDrive::open(dir.path(), 0).unwrap();
        let clip = drive.lookup(0, "Clip One").unwrap();
        let start = drive.object(clip).unwrap().start;
        let header = drive.header_block(clip);
        std::fs::write(dir.path().join("FA0/New"), b"new").unwrap();
        std::fs::remove_file(dir.path().join("FA0/Projects/notes")).unwrap();
        std::fs::write(dir.path().join("FA0/.flyervol"), "Clips\n").unwrap();
        drive.scan();
        assert_eq!(drive.volume, "Clips");
        assert_eq!(drive.lookup(0, "Clip One"), Some(clip));
        assert_eq!(drive.object(clip).unwrap().start, start);
        assert_eq!(drive.header_block(clip), header);
        assert!(drive.lookup(0, "New").is_some());
        assert!(drive.lookup(0, "Projects/notes").is_none());
        // A file that grows past its room moves.
        std::fs::write(
            dir.path().join("FA0/Clip One"),
            vec![1u8; (SLACK_BLOCKS as usize + 8) * BLOCK],
        )
        .unwrap();
        drive.scan();
        assert_ne!(drive.object(clip).unwrap().start, start);
    }

    #[test]
    fn datestamps_count_from_1978() {
        assert_eq!(unix_to_datestamp(2922 * 86_400), [0, 0, 0]);
        assert_eq!(unix_to_datestamp(2922 * 86_400 + 86_400 + 61), [1, 1, 50]);
    }

    #[test]
    fn names_follow_the_flyer_rules() {
        assert!(valid_name("Clip 1"));
        assert!(!valid_name(""));
        assert!(!valid_name("a:b"));
        assert!(!valid_name(&"x".repeat(32)));
        assert!(names_match("CLIP", "clip"));
        assert_eq!(strip_device("FlyerA0:a/b"), "a/b");
        assert_eq!(strip_device("a/b"), "a/b");
    }
}
