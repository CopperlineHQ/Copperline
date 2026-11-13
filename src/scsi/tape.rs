// SPDX-License-Identifier: GPL-3.0-or-later

//! SCSI-2 sequential-access target: a tape drive (peripheral type 1) whose
//! cartridge is a host file or a host directory.
//!
//! # The tape
//!
//! A tape is a row of *tape files*, each closed by a filemark, followed by
//! end-of-data:
//!
//! ```text
//! BOT | file 0 | FM | file 1 | FM | ... | file n-1 | FM | EOD
//! ```
//!
//! A directory cartridge keeps one host file per tape file, in name order
//! (hidden files and subdirectories are not part of the tape; a link to a
//! file is). A single-file cartridge is a tape that holds exactly one tape
//! file: the host file, which is a tape file when it exists, however
//! short, and a blank tape when it does not. Both store nothing but the
//! bytes the initiator wrote, so a `tar` archive is a tape file as it
//! stands and a tape file written by the guest is an ordinary host file.
//!
//! The drive only ever touches the cartridge: tape files are names inside
//! the configured directory (or the configured file itself), read from the
//! host or made up here, and never paths taken from a save state.
//!
//! Host files carry no record marks, so the tape does not remember how its
//! data was blocked; the drive's block length decides it:
//!
//! - With a fixed block length set (MODE SELECT, 512 at power-on), a tape
//!   file is a run of blocks of that length. A last block shorter than the
//!   block length is an incorrect-length block.
//! - In variable-block mode (block length 0) each READ takes the bytes it
//!   asks for as one block, up to the end of the tape file; what is left
//!   there makes a short block.
//!
//! Every tape file ends in a filemark, whether or not the initiator wrote
//! one after its data: stopping a write and rewinding closes the file, as
//! tape drivers do on close. WRITE FILEMARKS therefore steps to the next
//! tape file, and each further filemark makes an empty one.
//!
//! Writing is sequential-access writing: data written at a position
//! replaces everything from there to the end of the tape.
//!
//! # Position and exceptions
//!
//! The position is a tape file number and a byte offset in it; the tape
//! file after the last one is end-of-data. The exception conditions are
//! reported the SCSI-2 way, as CHECK CONDITION with fixed-format sense
//! whose information field holds the residue (requested minus done, in
//! blocks for a fixed-block command and bytes for a variable one):
//!
//! - FILEMARK (sense key NO SENSE): a read or a forward space ran into the
//!   end of a tape file; the position is past the filemark. A backward
//!   space stops on the beginning-of-tape side of it.
//! - EOM: at beginning-of-tape a backward space stops with NO SENSE and
//!   EOM. A write the cartridge has no room for fails with VOLUME OVERFLOW
//!   and EOM: data for a second tape file on a single-file cartridge, a
//!   tape file past the [`MAX_TAPE_FILES`] a directory holds, or data past
//!   the tape's capacity when it has one. A tape with a capacity also has
//!   an early-warning zone at its end, where a write is carried out and
//!   reported with NO SENSE and EOM so the initiator can close its data.
//!   Filemarks after a single-file cartridge's one tape file only mark the
//!   end of the data again and are accepted.
//! - ILI: a block is not the length the READ asked for. SILI suppresses
//!   the report for a short block in variable mode.
//! - BLANK CHECK, end-of-data detected: a read or space reached EOD.
//!
//! There is no timing model: the transport is instant, like a disk target
//! without drive mechanics.

use super::{
    be24, cdb_len, sense_for_io_error, ScsiExec, ASC_INVALID_FIELD_IN_CDB, ASC_INVALID_OPCODE,
    ASC_LOGICAL_UNIT_NOT_READY, ASC_LUN_NOT_SUPPORTED, ASC_MEDIUM_NOT_PRESENT,
    ASC_PARAMETER_LIST_LENGTH_ERROR, ASC_WRITE_PROTECTED, CHECK_CONDITION, GOOD, SENSE_LEN,
    SK_ABORTED_COMMAND, SK_DATA_PROTECT, SK_ILLEGAL_REQUEST, SK_NOT_READY,
};
use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const SK_NO_SENSE: u8 = 0x00;
const SK_BLANK_CHECK: u8 = 0x08;
const SK_VOLUME_OVERFLOW: u8 = 0x0D;

/// Sense byte 2 flags.
const SENSE_FILEMARK: u8 = 0x80;
const SENSE_EOM: u8 = 0x40;
const SENSE_ILI: u8 = 0x20;

// Additional sense code qualifiers under ASC 00h.
const ASCQ_FILEMARK_DETECTED: u8 = 0x01;
const ASCQ_END_OF_MEDIUM: u8 = 0x02;
const ASCQ_BEGINNING_OF_MEDIUM: u8 = 0x04;
const ASCQ_END_OF_DATA: u8 = 0x05;
/// ASC 04h qualifier: the cartridge is in the drive but not loaded, and a
/// LOAD is what makes the unit ready.
const ASCQ_INITIALIZING_COMMAND_REQUIRED: u8 = 0x02;
const ASC_INVALID_FIELD_IN_PARAMETER_LIST: u8 = 0x26;

/// Block length at power-on.
pub const DEFAULT_BLOCK_LEN: u32 = 512;
/// READ BLOCK LIMITS: the longest block one command moves. Also the cap on
/// a single transfer, so a hostile length cannot ask the host for more.
pub const MAX_BLOCK_LEN: u32 = 0x00FF_FFFF;
/// READ BLOCK LIMITS: the shortest block.
pub const MIN_BLOCK_LEN: u32 = 1;
/// Bound on one fixed-block READ/WRITE (blocks times block length), the
/// same ceiling the disk target puts on a single transfer.
const MAX_TRANSFER_BYTES: u64 = 64 * 1024 * 1024;

/// The most tape files a directory cartridge holds: the end of the tape.
/// It keeps a guest that writes filemarks without end from filling the
/// host directory, and the five-digit names of the files it writes
/// unique.
pub const MAX_TAPE_FILES: usize = 10_000;

/// The early-warning zone of a tape with a capacity is its last 1/32nd,
/// and no more than this.
const EARLY_WARNING_MAX_BYTES: u64 = 1024 * 1024;

/// Mode page 10h, device configuration: fixed length.
const PAGE_DEVICE_CONFIGURATION: u8 = 0x10;

/// What the cartridge is on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Cartridge {
    /// One host file: a tape holding exactly one tape file.
    File,
    /// A host directory: one host file per tape file, in name order.
    Directory,
}

impl Cartridge {
    fn noun(self) -> &'static str {
        match self {
            Cartridge::File => "file",
            Cartridge::Directory => "directory",
        }
    }
}

/// A SCSI-2 sequential-access (tape) target.
///
/// The cartridge's files are the host's, not the machine's: a save state
/// holds the drive (position, mode parameters, which cartridge) and the
/// names the tape files had, never paths to them. Restoring one reads the
/// cartridge again and puts the head where the state had it, as far as the
/// tape that is there now allows (see [`TapeState`]).
pub struct ScsiTape {
    /// The host file or directory.
    path: PathBuf,
    cartridge: Cartridge,
    /// Names of the tape files in tape order: file names inside a directory
    /// cartridge, never paths. A single-file cartridge has none (blank) or
    /// the one tape file, which is `path` itself.
    files: Vec<OsString>,
    /// The cartridge's write-protect tab.
    write_protected: bool,
    /// How many bytes of data the tape holds, when it has an end.
    capacity: Option<u64>,
    /// The tape is threaded: UNLOAD clears it and LOAD sets it again.
    loaded: bool,
    /// Current block length; 0 is variable-block mode.
    block_len: u32,
    /// Length of the last block moved in variable mode: what SPACE counts
    /// as a block there, since the tape itself has no record marks.
    last_block_len: u32,
    /// Tape file the head is in; `files.len()` is end-of-data.
    file: usize,
    /// Byte offset in that tape file.
    offset: u64,
    /// Buffered-mode field of the mode parameter header, kept as written.
    buffered_mode: u8,
    sense: [u8; SENSE_LEN],
}

/// What a save state holds of a tape drive. `names` is what the cartridge
/// held when the state was saved, kept only to notice that it has changed
/// since: the restored drive reads the cartridge as it is now, and a head
/// position the tape no longer has is moved to the nearest one it does
/// (the end of a shortened tape file, or end-of-data), with a warning, so
/// a changed cartridge shows up when the state is loaded instead of as a
/// medium error later.
#[derive(serde::Serialize, serde::Deserialize)]
struct TapeState {
    path: PathBuf,
    cartridge: Cartridge,
    names: Vec<String>,
    write_protected: bool,
    #[serde(default)]
    capacity: Option<u64>,
    loaded: bool,
    block_len: u32,
    last_block_len: u32,
    file: usize,
    offset: u64,
    buffered_mode: u8,
    sense: [u8; SENSE_LEN],
}

impl serde::Serialize for ScsiTape {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        TapeState {
            path: self.path.clone(),
            cartridge: self.cartridge,
            names: self.names(),
            write_protected: self.write_protected,
            capacity: self.capacity,
            loaded: self.loaded,
            block_len: self.block_len,
            last_block_len: self.last_block_len,
            file: self.file,
            offset: self.offset,
            buffered_mode: self.buffered_mode,
            sense: self.sense,
        }
        .serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for ScsiTape {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let state = TapeState::deserialize(deserializer)?;
        crate::hostbind::restored(crate::hostbind::Kind::Tape, &state.path);
        let mut tape = Self {
            path: state.path,
            cartridge: state.cartridge,
            files: Vec::new(),
            write_protected: state.write_protected,
            capacity: state.capacity,
            loaded: state.loaded,
            block_len: state.block_len,
            last_block_len: state.last_block_len,
            file: state.file,
            offset: state.offset,
            buffered_mode: state.buffered_mode,
            sense: state.sense,
        };
        tape.reconcile(&state.names);
        Ok(tape)
    }
}

/// The fixed-format sense a tape exception condition latches.
struct Exception {
    key: u8,
    /// FILEMARK / EOM / ILI.
    flags: u8,
    asc: u8,
    ascq: u8,
    /// Residue, already as the 32-bit two's-complement information field.
    information: u32,
}

impl ScsiTape {
    /// Open the cartridge at `path`.
    ///
    /// `kind` says what the cartridge is; `None` takes it from what is at
    /// `path`: a directory is a multi-file tape and a file a single-file
    /// one. A path with nothing at it is a new directory cartridge (it is
    /// created), since that is the kind of tape that can hold whatever the
    /// guest writes; a blank single-file tape has to be asked for.
    /// `capacity` is how many bytes of data the tape holds, `None` for a
    /// tape with no end short of the host's disk.
    pub fn open(
        path: &Path,
        kind: Option<Cartridge>,
        write_protected: bool,
        capacity: Option<u64>,
    ) -> anyhow::Result<Self> {
        let found = match std::fs::metadata(path) {
            Ok(meta) if meta.is_dir() => Some(Cartridge::Directory),
            Ok(_) => Some(Cartridge::File),
            Err(_) => None,
        };
        let cartridge = match (kind, found) {
            (Some(kind), Some(found)) if kind != found => anyhow::bail!(
                "{} is a {}, not the {} the tape is configured as",
                path.display(),
                found.noun(),
                kind.noun()
            ),
            (Some(kind), _) => kind,
            (None, Some(found)) => found,
            (None, None) => Cartridge::Directory,
        };
        if cartridge == Cartridge::Directory && found.is_none() {
            std::fs::create_dir_all(path)?;
        }
        let mut tape = Self {
            path: path.to_path_buf(),
            cartridge,
            files: Vec::new(),
            write_protected,
            capacity,
            loaded: true,
            block_len: DEFAULT_BLOCK_LEN,
            last_block_len: DEFAULT_BLOCK_LEN,
            file: 0,
            offset: 0,
            buffered_mode: 1,
            sense: [0u8; SENSE_LEN],
        };
        tape.scan()?;
        crate::hostbind::opened(crate::hostbind::Kind::Tape, path);
        Ok(tape)
    }

    /// One-line description for the log.
    pub fn describe(&self) -> String {
        format!(
            "{} tape file(s) in a {}{}",
            self.files.len(),
            match self.cartridge {
                Cartridge::File => "single-file cartridge",
                Cartridge::Directory => "directory cartridge",
            },
            if self.write_protected {
                ", write-protected"
            } else {
                ""
            }
        )
    }

    /// The host file or directory behind the cartridge.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The tape files' names, as text.
    fn names(&self) -> Vec<String> {
        self.files
            .iter()
            .map(|name| name.to_string_lossy().into_owned())
            .collect()
    }

    /// The host file behind tape file `index`: a name inside the cartridge
    /// directory, or the single-file cartridge's own path. Never anything
    /// else: names come from reading the directory or from
    /// [`Self::new_file_name`], not from a save state.
    fn file_path(&self, index: usize) -> PathBuf {
        match self.cartridge {
            Cartridge::File => self.path.clone(),
            Cartridge::Directory => self.path.join(&self.files[index]),
        }
    }

    /// Read the cartridge's tape files off the host. Done when the
    /// cartridge is opened, when a state is restored and at every LOAD, so
    /// files added to a directory while the tape was unloaded are found;
    /// the list then stays as the drive knows it until the next LOAD.
    ///
    /// A directory's tape files are its regular files, and links to
    /// regular files, in name order; hidden files (a leading `.`) and
    /// subdirectories are not part of the tape. A single-file cartridge is
    /// one tape file if the file exists (an empty file is an empty tape
    /// file, which is what a filemark written on a blank tape leaves) and a
    /// blank tape if it does not.
    fn scan(&mut self) -> std::io::Result<()> {
        self.files.clear();
        match self.cartridge {
            Cartridge::File => {
                if std::fs::metadata(&self.path).is_ok_and(|m| m.is_file()) {
                    self.files
                        .push(self.path.file_name().unwrap_or_default().to_os_string());
                }
            }
            Cartridge::Directory => {
                let mut names = Vec::new();
                for entry in std::fs::read_dir(&self.path)? {
                    let entry = entry?;
                    let name = entry.file_name();
                    if name.to_string_lossy().starts_with('.') {
                        continue;
                    }
                    // Through a link to what it names; a dangling one is
                    // not a file.
                    if std::fs::metadata(entry.path()).is_ok_and(|m| m.is_file()) {
                        names.push(name);
                    }
                }
                names.sort();
                self.files = names;
            }
        }
        Ok(())
    }

    /// Bring a restored drive back in step with the cartridge as the host
    /// has it now. `saved` is the tape files' names when the state was
    /// taken.
    fn reconcile(&mut self, saved: &[String]) {
        if let Err(e) = self.scan() {
            log::warn!(
                "scsi tape {}: the cartridge cannot be read ({e}); the restored drive is \
                 unloaded",
                self.path.display()
            );
            self.files.clear();
            self.loaded = false;
            self.file = 0;
            self.offset = 0;
            return;
        }
        let mut changed = self.names() != saved;
        if self.file > self.files.len() {
            self.file = self.files.len();
            self.offset = 0;
            changed = true;
        }
        if self.file == self.files.len() {
            changed |= self.offset != 0;
            self.offset = 0;
        } else {
            let len = self.file_len(self.file).unwrap_or(0);
            if self.offset > len {
                self.offset = len;
                changed = true;
            }
        }
        if changed {
            log::warn!(
                "scsi tape {}: the cartridge is not as the save state left it; the tape is \
                 used as it is now, with the head at tape file {} offset {}",
                self.path.display(),
                self.file,
                self.offset
            );
        }
    }

    fn file_len(&self, index: usize) -> std::io::Result<u64> {
        Ok(std::fs::metadata(self.file_path(index))?.len())
    }

    /// Bytes of data on the tape up to the head: what a write at the head
    /// keeps.
    fn used_to_head(&self) -> std::io::Result<u64> {
        let mut used = 0u64;
        for index in 0..self.file.min(self.files.len()) {
            used = used.saturating_add(self.file_len(index)?);
        }
        Ok(used.saturating_add(self.offset))
    }

    /// Where the early-warning zone starts on a tape with an end: the
    /// last part of it, where a write still succeeds but reports
    /// end-of-medium so the initiator can finish its trailer.
    fn early_warning(&self) -> Option<u64> {
        self.capacity
            .map(|capacity| capacity - (capacity / 32).min(EARLY_WARNING_MAX_BYTES))
    }

    /// Whether the cartridge has room for a tape file at `index`.
    fn has_room_for(&self, index: usize) -> bool {
        match self.cartridge {
            Cartridge::Directory => index < MAX_TAPE_FILES,
            Cartridge::File => index == 0,
        }
    }

    /// The name of a new tape file at `index`, the current end of a
    /// directory cartridge: one that sorts after every file already there,
    /// and no longer than the last name plus a few characters however many
    /// files the guest writes.
    fn new_file_name(&self, index: usize) -> OsString {
        let last = self
            .files
            .last()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let plain = format!("{index:05}.tape");
        if plain.as_str() > last.as_str() {
            return plain.into();
        }
        // The last name is one of ours with a lower number: keep whatever
        // it starts with and count on.
        if let Some(stem) = last.len().checked_sub(plain.len()) {
            if last.is_char_boundary(stem) {
                let (prefix, tail) = last.split_at(stem);
                let number = tail
                    .strip_suffix(".tape")
                    .filter(|digits| digits.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|digits| digits.parse::<usize>().ok());
                if number.is_some_and(|number| number < index) {
                    return format!("{prefix}{plain}").into();
                }
            }
        }
        // Otherwise share the last name's start up to its first character
        // that '~' outranks, and put '~' there.
        let shared = last
            .char_indices()
            .find(|&(_, c)| c < '~')
            .map_or(last.len(), |(at, _)| at);
        format!("{}~{plain}", &last[..shared]).into()
    }

    /// Cut the tape at the head: everything from the current position to
    /// the old end of the tape is gone. The tape file under the head keeps
    /// its bytes before the head (a single-file cartridge has no tape
    /// files after that one, so only a directory ever loses host files).
    fn truncate_at_position(&mut self) -> std::io::Result<()> {
        let keep = if self.file < self.files.len() {
            std::fs::OpenOptions::new()
                .write(true)
                .open(self.file_path(self.file))?
                .set_len(self.offset)?;
            self.file + 1
        } else {
            self.files.len()
        };
        while self.files.len() > keep {
            std::fs::remove_file(self.file_path(self.files.len() - 1))?;
            self.files.pop();
        }
        Ok(())
    }

    /// Make sure the tape file under the head exists (creating an empty
    /// one at end-of-data).
    fn ensure_file_at_position(&mut self) -> std::io::Result<()> {
        if self.file == self.files.len() {
            let name = match self.cartridge {
                Cartridge::File => self.path.file_name().unwrap_or_default().to_os_string(),
                Cartridge::Directory => self.new_file_name(self.file),
            };
            self.files.push(name);
            if let Err(e) = std::fs::File::create(self.file_path(self.file)) {
                self.files.pop();
                return Err(e);
            }
        }
        Ok(())
    }

    // ----- sense -------------------------------------------------------------

    fn clear_sense(&mut self) {
        self.sense = [0u8; SENSE_LEN];
    }

    fn set_sense(&mut self, key: u8, asc: u8, ascq: u8) {
        self.sense = [0u8; SENSE_LEN];
        self.sense[0] = 0x70; // current error, fixed format
        self.sense[2] = key;
        self.sense[7] = 10; // additional sense length
        self.sense[12] = asc;
        self.sense[13] = ascq;
    }

    fn check(&mut self, key: u8, asc: u8) -> (ScsiExec, u8) {
        self.set_sense(key, asc, 0);
        (ScsiExec::NoData, CHECK_CONDITION)
    }

    /// Latch an exception condition: sense with the valid bit, the
    /// FILEMARK/EOM/ILI flags and the residue in the information field.
    fn raise(&mut self, e: &Exception) {
        self.set_sense(e.key, e.asc, e.ascq);
        self.sense[0] |= 0x80; // information field valid
        self.sense[2] |= e.flags;
        self.sense[3..7].copy_from_slice(&e.information.to_be_bytes());
    }

    fn io_error(&mut self, what: &str, err: &std::io::Error) -> (ScsiExec, u8) {
        log::warn!("scsi tape {}: {what}: {err}", self.path.display());
        let (key, asc) = sense_for_io_error(err);
        self.check(key, asc)
    }

    /// The sense data REQUEST SENSE would return (for a host adapter's
    /// autosense).
    pub fn sense_bytes(&self) -> &[u8] {
        &self.sense
    }

    // ----- command dispatch ----------------------------------------------------

    fn inquiry_data(lun: u8) -> Vec<u8> {
        let mut d = vec![0u8; 36];
        // Peripheral qualifier 011b + type 1Fh for an unsupported LUN.
        d[0] = if lun == 0 { 0x01 } else { 0x7F };
        d[1] = 0x80; // removable medium
        d[2] = 0x02; // SCSI-2
        d[3] = 0x02; // response data format: SCSI-2
        d[4] = 31; // additional length
        d[8..16].copy_from_slice(b"COPPERLN");
        d[16..32].copy_from_slice(b"SCSI TAPE       ");
        d[32..36].copy_from_slice(b"1.0 ");
        d
    }

    /// Commands that need a loaded tape.
    fn needs_tape(op: u8) -> bool {
        matches!(op, 0x00 | 0x01 | 0x08 | 0x0A | 0x10 | 0x11 | 0x19)
    }

    /// Parse and execute a CDB up to (but not including) any data-out
    /// payload; see [`super::ScsiDisk::execute`].
    pub fn execute(&mut self, cdb: &[u8], lun: u8) -> (ScsiExec, u8) {
        let Some(&op) = cdb.first() else {
            return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
        };
        if cdb.len() < cdb_len(op) {
            return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
        }
        // REQUEST SENSE must report (then clear) the previous command's
        // sense data; every other command starts with it cleared.
        if op != 0x03 {
            self.clear_sense();
        }
        if lun != 0 {
            return match op {
                0x12 => {
                    let alloc = usize::from(cdb[4]);
                    let data = Self::inquiry_data(lun);
                    (ScsiExec::DataIn(data[..alloc.min(36)].to_vec()), GOOD)
                }
                _ => self.check(SK_ILLEGAL_REQUEST, ASC_LUN_NOT_SUPPORTED),
            };
        }
        if !self.loaded && Self::needs_tape(op) {
            // The cartridge is still in the drive; LOAD makes it ready.
            self.set_sense(
                SK_NOT_READY,
                ASC_LOGICAL_UNIT_NOT_READY,
                ASCQ_INITIALIZING_COMMAND_REQUIRED,
            );
            return (ScsiExec::NoData, CHECK_CONDITION);
        }
        match op {
            // TEST UNIT READY
            0x00 => (ScsiExec::NoData, GOOD),
            // REWIND (IMMED or not: the transport is instant)
            0x01 => {
                self.file = 0;
                self.offset = 0;
                (ScsiExec::NoData, GOOD)
            }
            // REQUEST SENSE
            0x03 => {
                let alloc = match cdb[4] {
                    0 => 4, // SCSI-1: zero means four bytes
                    n => usize::from(n),
                };
                let data = self.sense[..alloc.min(SENSE_LEN)].to_vec();
                self.clear_sense();
                (ScsiExec::DataIn(data), GOOD)
            }
            // READ BLOCK LIMITS
            0x05 => {
                let max = MAX_BLOCK_LEN.to_be_bytes();
                let min = (MIN_BLOCK_LEN as u16).to_be_bytes();
                (
                    ScsiExec::DataIn(vec![0, max[1], max[2], max[3], min[0], min[1]]),
                    GOOD,
                )
            }
            // READ(6)
            0x08 => self.read(cdb),
            // WRITE(6)
            0x0A => self.write_prepare(cdb),
            // WRITE FILEMARKS
            0x10 => self.write_filemarks(cdb),
            // SPACE
            0x11 => self.space(cdb),
            // INQUIRY
            0x12 => {
                if cdb[1] & 0x01 != 0 {
                    // EVPD: only the supported-pages page.
                    if cdb[2] == 0x00 {
                        let data = [0x01u8, 0, 0, 1, 0];
                        let alloc = usize::from(cdb[4]);
                        return (ScsiExec::DataIn(data[..alloc.min(5)].to_vec()), GOOD);
                    }
                    return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
                }
                let alloc = usize::from(cdb[4]);
                let data = Self::inquiry_data(0);
                (ScsiExec::DataIn(data[..alloc.min(36)].to_vec()), GOOD)
            }
            // MODE SELECT(6)
            0x15 => match usize::from(cdb[4]) {
                0 => (ScsiExec::NoData, GOOD),
                len => (ScsiExec::DataOut(len), GOOD),
            },
            // RESERVE / RELEASE
            0x16 | 0x17 => (ScsiExec::NoData, GOOD),
            // ERASE
            0x19 => self.erase(cdb),
            // MODE SENSE(6)
            0x1A => self.mode_sense(cdb),
            // LOAD / UNLOAD
            0x1B => self.load_unload(cdb),
            // SEND DIAGNOSTIC (self-test) / PREVENT ALLOW MEDIUM REMOVAL
            0x1D | 0x1E => (ScsiExec::NoData, GOOD),
            _ => self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_OPCODE),
        }
    }

    /// Complete a data-out command once the payload has arrived.
    pub fn complete_out(&mut self, cdb: &[u8], data: &[u8]) -> u8 {
        match cdb.first() {
            Some(0x0A) => self.write_commit(cdb, data),
            Some(0x15) => self.mode_select(cdb, data),
            _ => GOOD,
        }
    }

    // ----- READ ----------------------------------------------------------------

    /// Read up to `len` bytes of the tape file under the head.
    fn read_bytes(&mut self, len: u64) -> std::io::Result<Vec<u8>> {
        let mut file = std::fs::File::open(self.file_path(self.file))?;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut data = Vec::new();
        file.take(len).read_to_end(&mut data)?;
        Ok(data)
    }

    fn read(&mut self, cdb: &[u8]) -> (ScsiExec, u8) {
        let fixed = cdb[1] & 0x01 != 0;
        let sili = cdb[1] & 0x02 != 0;
        let count = be24(cdb, 2);
        // SILI is for variable-length reads; with FIXED it is an error, and
        // a fixed-block read needs a block length to count in.
        if fixed && (sili || self.block_len == 0) {
            return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
        }
        if count == 0 {
            return (ScsiExec::NoData, GOOD);
        }
        if fixed {
            self.read_fixed(count)
        } else {
            self.read_variable(count, sili)
        }
    }

    /// The exception a read or forward space meets with no data left
    /// under the head: end-of-data, or the filemark closing this tape file
    /// (which the head then passes). `residue` is the information field.
    fn end_of_file_exception(&mut self, residue: u32) -> Exception {
        if self.file >= self.files.len() {
            Exception {
                key: SK_BLANK_CHECK,
                flags: 0,
                asc: 0x00,
                ascq: ASCQ_END_OF_DATA,
                information: residue,
            }
        } else {
            self.file += 1;
            self.offset = 0;
            Exception {
                key: SK_NO_SENSE,
                flags: SENSE_FILEMARK,
                asc: 0x00,
                ascq: ASCQ_FILEMARK_DETECTED,
                information: residue,
            }
        }
    }

    fn remaining_in_file(&self) -> std::io::Result<u64> {
        if self.file >= self.files.len() {
            return Ok(0);
        }
        Ok(self.file_len(self.file)?.saturating_sub(self.offset))
    }

    /// READ with FIXED: `blocks` blocks of the current block length. The
    /// data that was read is transferred even when the command then ends
    /// in an exception; the residue counts the blocks not read, and does
    /// not count an incorrect-length block as read.
    fn read_fixed(&mut self, blocks: u32) -> (ScsiExec, u8) {
        let block = u64::from(self.block_len);
        if u64::from(blocks) * block > MAX_TRANSFER_BYTES {
            return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
        }
        let remaining = match self.remaining_in_file() {
            Ok(n) => n,
            Err(e) => return self.io_error("reading", &e),
        };
        let whole = (remaining / block).min(u64::from(blocks));
        let tail = remaining - whole * block;
        // A last block shorter than the block length comes along with the
        // whole ones if the request reaches it.
        let short_tail = whole < u64::from(blocks) && tail > 0 && tail < block;
        let want = whole * block + if short_tail { tail } else { 0 };
        let data = match want {
            0 => Vec::new(),
            _ => match self.read_bytes(want) {
                Ok(data) => data,
                Err(e) => return self.io_error("reading", &e),
            },
        };
        self.offset += data.len() as u64;
        let residue = blocks - whole as u32;
        if residue == 0 {
            return (ScsiExec::DataIn(data), GOOD);
        }
        let exception = if short_tail {
            Exception {
                key: SK_NO_SENSE,
                flags: SENSE_ILI,
                asc: 0x00,
                ascq: 0x00,
                information: residue,
            }
        } else {
            self.end_of_file_exception(residue)
        };
        self.raise(&exception);
        if data.is_empty() {
            (ScsiExec::NoData, CHECK_CONDITION)
        } else {
            (ScsiExec::DataIn(data), CHECK_CONDITION)
        }
    }

    /// READ without FIXED: one block into a `len`-byte buffer.
    fn read_variable(&mut self, len: u32, sili: bool) -> (ScsiExec, u8) {
        let remaining = match self.remaining_in_file() {
            Ok(n) => n,
            Err(e) => return self.io_error("reading", &e),
        };
        if remaining == 0 {
            let exception = self.end_of_file_exception(len);
            self.raise(&exception);
            return (ScsiExec::NoData, CHECK_CONDITION);
        }
        // The block under the head: a block-length block when the drive
        // has a fixed block length, otherwise whatever the read asks for;
        // either way no longer than what is left of the tape file.
        let block = match self.block_len {
            0 => u64::from(len),
            fixed => u64::from(fixed),
        }
        .min(remaining);
        let data = match self.read_bytes(block.min(u64::from(len))) {
            Ok(data) => data,
            Err(e) => return self.io_error("reading", &e),
        };
        // The head passes the whole block even when the buffer was
        // shorter than it.
        self.offset += block;
        self.last_block_len = block as u32;
        let block = block as u32;
        if block == len || (block < len && sili) {
            return (ScsiExec::DataIn(data), GOOD);
        }
        // Requested minus actual: positive for a short block, negative
        // (two's complement) for one longer than the buffer.
        self.raise(&Exception {
            key: SK_NO_SENSE,
            flags: SENSE_ILI,
            asc: 0x00,
            ascq: 0x00,
            information: len.wrapping_sub(block),
        });
        (ScsiExec::DataIn(data), CHECK_CONDITION)
    }

    // ----- WRITE ---------------------------------------------------------------

    /// The byte count a WRITE(6) CDB moves, or `None` for a malformed one.
    fn write_len(&self, cdb: &[u8]) -> Option<u64> {
        let fixed = cdb[1] & 0x01 != 0;
        let count = u64::from(be24(cdb, 2));
        if !fixed {
            return Some(count);
        }
        if self.block_len == 0 {
            return None;
        }
        let bytes = count * u64::from(self.block_len);
        (bytes <= MAX_TRANSFER_BYTES).then_some(bytes)
    }

    /// The sense of a write the tape has no room for.
    fn volume_overflow(residue: u32) -> Exception {
        Exception {
            key: SK_VOLUME_OVERFLOW,
            flags: SENSE_EOM,
            asc: 0x00,
            ascq: ASCQ_END_OF_MEDIUM,
            information: residue,
        }
    }

    /// The sense of a write that succeeded inside the early-warning zone:
    /// done, and the end of the tape is near.
    fn early_warning_reached() -> Exception {
        Exception {
            key: SK_NO_SENSE,
            flags: SENSE_EOM,
            asc: 0x00,
            ascq: ASCQ_END_OF_MEDIUM,
            information: 0,
        }
    }

    /// Bytes a write at the head may add before the end of the tape, or
    /// `None` on a tape with no end.
    fn room_at_head(&self) -> std::io::Result<Option<u64>> {
        match self.capacity {
            None => Ok(None),
            Some(capacity) => Ok(Some(capacity.saturating_sub(self.used_to_head()?))),
        }
    }

    /// Whether the head is inside the early-warning zone.
    fn in_early_warning(&self) -> bool {
        match self.early_warning() {
            None => false,
            Some(start) => self.used_to_head().is_ok_and(|used| used >= start),
        }
    }

    fn write_prepare(&mut self, cdb: &[u8]) -> (ScsiExec, u8) {
        let Some(bytes) = self.write_len(cdb) else {
            return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
        };
        if bytes == 0 {
            return (ScsiExec::NoData, GOOD);
        }
        if self.write_protected {
            return self.check(SK_DATA_PROTECT, ASC_WRITE_PROTECTED);
        }
        // No data phase for a write none of which fits: there is no tape
        // file to put it in, or not room for one block of it.
        let one_block = match cdb[1] & 0x01 != 0 {
            true => u64::from(self.block_len),
            false => bytes,
        };
        let room = match self.room_at_head() {
            Ok(room) => room,
            Err(e) => return self.io_error("writing", &e),
        };
        if !self.has_room_for(self.file) || room.is_some_and(|room| room < one_block) {
            // Nothing was written: the residue is the whole request.
            self.raise(&Self::volume_overflow(be24(cdb, 2)));
            return (ScsiExec::NoData, CHECK_CONDITION);
        }
        (ScsiExec::DataOut(bytes as usize), GOOD)
    }

    fn write_commit(&mut self, cdb: &[u8], data: &[u8]) -> u8 {
        if cdb.len() < 6 {
            self.set_sense(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB, 0);
            return CHECK_CONDITION;
        }
        let fixed = cdb[1] & 0x01 != 0;
        let Some(want) = self.write_len(cdb) else {
            self.set_sense(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB, 0);
            return CHECK_CONDITION;
        };
        // What the command counts in: blocks in fixed mode, bytes otherwise.
        let unit = match fixed {
            true => u64::from(self.block_len),
            false => 1,
        };
        let room = match self.room_at_head() {
            Ok(room) => room.unwrap_or(u64::MAX),
            Err(e) => return self.io_error("writing", &e).1,
        };
        // Only what the CDB asked for is written, only what arrived, and
        // only what the tape has room for; in fixed mode only whole blocks,
        // and in variable mode the one block whole or not at all.
        let arrived = (data.len() as u64).min(want);
        let mut len = arrived.min(room);
        if fixed {
            len -= len % unit;
        } else if len < arrived {
            len = 0;
        }
        let fits = len;
        let len = len as usize;
        // Nothing to write leaves the tape as it is: it is not cut at the
        // head for a block that never arrived or does not fit.
        if len > 0 {
            let result = (|| -> std::io::Result<()> {
                self.truncate_at_position()?;
                self.ensure_file_at_position()?;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(self.file_path(self.file))?;
                file.seek(SeekFrom::Start(self.offset))?;
                file.write_all(&data[..len])
            })();
            if let Err(e) = result {
                return self.io_error("writing", &e).1;
            }
            self.offset += fits;
            if !fixed {
                self.last_block_len = len as u32;
            }
        }
        let residue = ((want - fits) / unit) as u32;
        let whole_arrived = match fixed {
            true => arrived - arrived % unit,
            false => arrived,
        };
        if fits < whole_arrived {
            // The tape ran out under the write.
            self.raise(&Self::volume_overflow(residue));
            return CHECK_CONDITION;
        }
        if arrived < want {
            // The data phase ended before the command's count was reached,
            // which only an abort on the bus does. What arrived whole is on
            // the tape; the rest is the residue.
            self.raise(&Exception {
                key: SK_ABORTED_COMMAND,
                flags: 0,
                asc: 0x00,
                ascq: 0x00,
                information: residue,
            });
            return CHECK_CONDITION;
        }
        if self.in_early_warning() {
            self.raise(&Self::early_warning_reached());
            return CHECK_CONDITION;
        }
        GOOD
    }

    fn write_filemarks(&mut self, cdb: &[u8]) -> (ScsiExec, u8) {
        // WSMK: setmarks are not something this tape can hold.
        if cdb[1] & 0x02 != 0 {
            return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB);
        }
        let count = be24(cdb, 2) as usize;
        if count == 0 {
            // Flush the drive's buffer: nothing is buffered.
            return (ScsiExec::NoData, GOOD);
        }
        if self.write_protected {
            return self.check(SK_DATA_PROTECT, ASC_WRITE_PROTECTED);
        }
        // Each filemark closes a tape file, and a directory cartridge has
        // to have room for every file a filemark closes. A single-file
        // cartridge holds one tape file; filemarks after it mark the end of
        // the data and nothing more (the two filemarks that close a backup
        // are both accepted), so they are taken and leave no trace.
        let closes = match self.cartridge {
            Cartridge::Directory => count.min(MAX_TAPE_FILES.saturating_sub(self.file)),
            Cartridge::File => count.min(1usize.saturating_sub(self.file)),
        };
        let accepted = match self.cartridge {
            Cartridge::Directory => closes,
            Cartridge::File => count,
        };
        let result = (|| -> std::io::Result<()> {
            if closes == 0 {
                return Ok(());
            }
            self.truncate_at_position()?;
            for _ in 0..closes {
                self.ensure_file_at_position()?;
                self.file += 1;
                self.offset = 0;
            }
            Ok(())
        })();
        if let Err(e) = result {
            return self.io_error("writing a filemark", &e);
        }
        if accepted < count {
            self.raise(&Self::volume_overflow((count - accepted) as u32));
            return (ScsiExec::NoData, CHECK_CONDITION);
        }
        if self.in_early_warning() {
            self.raise(&Self::early_warning_reached());
            return (ScsiExec::NoData, CHECK_CONDITION);
        }
        (ScsiExec::NoData, GOOD)
    }

    // ----- SPACE -----------------------------------------------------------------

    fn space(&mut self, cdb: &[u8]) -> (ScsiExec, u8) {
        // 24-bit two's-complement count.
        let raw = be24(cdb, 2);
        let count = if raw & 0x80_0000 != 0 {
            i64::from(raw) - 0x100_0000
        } else {
            i64::from(raw)
        };
        let result = match cdb[1] & 0x07 {
            0 => self.space_blocks(count),
            1 => Ok(self.space_filemarks(count)),
            3 => {
                self.file = self.files.len();
                self.offset = 0;
                Ok(None)
            }
            // Sequential filemarks and setmarks.
            _ => return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB),
        };
        match result {
            Ok(None) => (ScsiExec::NoData, GOOD),
            Ok(Some(exception)) => {
                self.raise(&exception);
                (ScsiExec::NoData, CHECK_CONDITION)
            }
            Err(e) => self.io_error("spacing", &e),
        }
    }

    /// The beginning-of-tape stop of a backward space.
    fn beginning_of_tape(residue: i64) -> Exception {
        Exception {
            key: SK_NO_SENSE,
            flags: SENSE_EOM,
            asc: 0x00,
            ascq: ASCQ_BEGINNING_OF_MEDIUM,
            information: residue as u32,
        }
    }

    /// SPACE over blocks. A block is the block length, or in variable mode
    /// the length of the last block moved (the tape has no record marks to
    /// say otherwise). The residue keeps the sign of the count.
    fn space_blocks(&mut self, count: i64) -> std::io::Result<Option<Exception>> {
        let block = u64::from(match self.block_len {
            0 => self.last_block_len.max(1),
            fixed => fixed,
        });
        if count >= 0 {
            let remaining = self.remaining_in_file()?;
            // A short last block still counts as one block.
            let available = remaining.div_ceil(block);
            let moved = available.min(count as u64);
            self.offset += (moved * block).min(remaining);
            let residue = count as u64 - moved;
            if residue == 0 {
                return Ok(None);
            }
            return Ok(Some(self.end_of_file_exception(residue as u32)));
        }
        let back = count.unsigned_abs();
        let available = self.offset.div_ceil(block);
        let moved = available.min(back);
        self.offset = self.offset.saturating_sub(moved * block);
        let residue = back - moved;
        if residue == 0 {
            return Ok(None);
        }
        let residue = -(residue as i64);
        if self.file == 0 {
            return Ok(Some(Self::beginning_of_tape(residue)));
        }
        // Back over the previous tape file's filemark: the head stops on
        // its beginning-of-tape side.
        self.file -= 1;
        self.offset = self.file_len(self.file)?;
        Ok(Some(Exception {
            key: SK_NO_SENSE,
            flags: SENSE_FILEMARK,
            asc: 0x00,
            ascq: ASCQ_FILEMARK_DETECTED,
            information: residue as u32,
        }))
    }

    /// SPACE over filemarks. Forward the head ends past the last one
    /// spaced over; backward it ends on the beginning-of-tape side of it.
    /// A count of zero moves nothing.
    fn space_filemarks(&mut self, count: i64) -> Option<Exception> {
        if count == 0 {
            return None;
        }
        if count > 0 {
            let ahead = (self.files.len() - self.file.min(self.files.len())) as i64;
            if count <= ahead {
                self.file += count as usize;
                self.offset = 0;
                return None;
            }
            self.file = self.files.len();
            self.offset = 0;
            return Some(Exception {
                key: SK_BLANK_CHECK,
                flags: 0,
                asc: 0x00,
                ascq: ASCQ_END_OF_DATA,
                information: (count - ahead) as u32,
            });
        }
        let back = count.unsigned_abs() as usize;
        // Filemarks behind the head: one per tape file before this one.
        if back <= self.file {
            self.file -= back;
            // The far side of that filemark is the end of its tape file.
            self.offset = self.file_len(self.file).unwrap_or(0);
            return None;
        }
        let residue = -((back - self.file) as i64);
        self.file = 0;
        self.offset = 0;
        Some(Self::beginning_of_tape(residue))
    }

    // ----- ERASE / LOAD ------------------------------------------------------------

    fn erase(&mut self, cdb: &[u8]) -> (ScsiExec, u8) {
        if self.write_protected {
            return self.check(SK_DATA_PROTECT, ASC_WRITE_PROTECTED);
        }
        // LONG erases from the head to the end of the tape. A short erase
        // lays down an erase gap, which has nothing to store here.
        if cdb[1] & 0x01 != 0 {
            if let Err(e) = self.truncate_at_position() {
                return self.io_error("erasing", &e);
            }
            // An emptied tape file under the head is no tape file at all:
            // its host file goes, a single-file cartridge's included, since
            // a file that exists is a tape file and a blank tape has none.
            if self.file < self.files.len() && self.offset == 0 {
                if let Err(e) = std::fs::remove_file(self.file_path(self.file)) {
                    return self.io_error("erasing", &e);
                }
                self.files.pop();
            }
        }
        (ScsiExec::NoData, GOOD)
    }

    fn load_unload(&mut self, cdb: &[u8]) -> (ScsiExec, u8) {
        let load = cdb[4] & 0x01 != 0;
        // Either way the tape ends up at its beginning: a load threads it
        // from there and an unload rewinds first.
        self.file = 0;
        self.offset = 0;
        if load {
            if let Err(e) = self.scan() {
                self.loaded = false;
                log::warn!("scsi tape {}: loading: {e}", self.path.display());
                return self.check(SK_NOT_READY, ASC_MEDIUM_NOT_PRESENT);
            }
            self.loaded = true;
        } else {
            self.loaded = false;
        }
        (ScsiExec::NoData, GOOD)
    }

    // ----- mode parameters -----------------------------------------------------------

    /// Mode page 10h, device configuration. The drive has one format and
    /// one partition, no block identifiers (there is no READ POSITION or
    /// LOCATE), no setmarks and no early warning; the one thing it does
    /// say is that it generates end-of-data itself.
    fn device_configuration_page() -> [u8; 16] {
        let mut page = [0u8; 16];
        page[0] = PAGE_DEVICE_CONFIGURATION;
        page[1] = 14; // page length
        page[10] = 0x10; // EEG: enable EOD generation
        page
    }

    fn mode_sense(&mut self, cdb: &[u8]) -> (ScsiExec, u8) {
        let dbd = cdb[1] & 0x08 != 0;
        let page_control = cdb[2] >> 6;
        let page = cdb[2] & 0x3F;
        // Saved values (PC 11b) are not kept.
        if page_control == 3 {
            return self.check(SK_ILLEGAL_REQUEST, 0x39);
        }
        let mut data = vec![0u8; 4];
        data[1] = 0; // medium type: default
        data[2] = (u8::from(self.write_protected) << 7) | ((self.buffered_mode & 0x07) << 4);
        if !dbd {
            data[3] = 8;
            let mut descriptor = [0u8; 8];
            // Density code 0 (the drive's default) and block count 0 (all
            // of the tape), then the block length.
            descriptor[5..8].copy_from_slice(&self.block_len.to_be_bytes()[1..]);
            data.extend_from_slice(&descriptor);
        }
        match page {
            // No page asked for: header and block descriptor only.
            0x00 => {}
            PAGE_DEVICE_CONFIGURATION | 0x3F => {
                let mut page = Self::device_configuration_page();
                // Changeable values (PC 01b): none.
                if page_control == 1 {
                    page[2..].fill(0);
                }
                data.extend_from_slice(&page);
            }
            _ => return self.check(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_CDB),
        }
        data[0] = (data.len() - 1) as u8;
        let alloc = usize::from(cdb[4]);
        data.truncate(alloc);
        if data.is_empty() {
            return (ScsiExec::NoData, GOOD);
        }
        (ScsiExec::DataIn(data), GOOD)
    }

    /// MODE SELECT(6): the buffered mode and the block descriptor's block
    /// length are what the drive takes; the device configuration page is
    /// accepted with nothing in it to change.
    fn mode_select(&mut self, cdb: &[u8], data: &[u8]) -> u8 {
        let expected = usize::from(cdb.get(4).copied().unwrap_or(0));
        if data.len() < expected.min(4) || data.len() < 4 {
            self.set_sense(SK_ILLEGAL_REQUEST, ASC_PARAMETER_LIST_LENGTH_ERROR, 0);
            return CHECK_CONDITION;
        }
        let descriptor_len = usize::from(data[3]);
        if !matches!(descriptor_len, 0 | 8) || data.len() < 4 + descriptor_len {
            self.set_sense(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_PARAMETER_LIST, 0);
            return CHECK_CONDITION;
        }
        let mut block_len = self.block_len;
        if descriptor_len == 8 {
            block_len = be24(data, 4 + 5);
            if block_len > MAX_BLOCK_LEN {
                self.set_sense(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_PARAMETER_LIST, 0);
                return CHECK_CONDITION;
            }
        }
        // Mode pages after the block descriptor: each must be one the
        // drive has, whole, and the right length.
        let mut pages = &data[4 + descriptor_len..];
        while !pages.is_empty() {
            if pages.len() < 2 {
                self.set_sense(SK_ILLEGAL_REQUEST, ASC_PARAMETER_LIST_LENGTH_ERROR, 0);
                return CHECK_CONDITION;
            }
            let code = pages[0] & 0x3F;
            let len = usize::from(pages[1]) + 2;
            if code != PAGE_DEVICE_CONFIGURATION || len != 16 || pages.len() < len {
                self.set_sense(SK_ILLEGAL_REQUEST, ASC_INVALID_FIELD_IN_PARAMETER_LIST, 0);
                return CHECK_CONDITION;
            }
            pages = &pages[len..];
        }
        self.buffered_mode = (data[2] >> 4) & 0x07;
        self.block_len = block_len;
        GOOD
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh scratch directory under the host temp directory.
    fn scratch(name: &str) -> PathBuf {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "copperline-scsi-tape-{}-{unique}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `len` bytes that say where they are in tape file `file`.
    /// Test data whose every 256-byte stretch differs, so a block read
    /// from the wrong place in a tape file does not compare equal.
    fn pattern(file: u8, len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| {
                file.wrapping_mul(31)
                    .wrapping_add(i as u8)
                    .wrapping_add(((i >> 8) as u8).wrapping_mul(13))
            })
            .collect()
    }

    fn directory_tape(name: &str, files: &[(&str, Vec<u8>)]) -> (PathBuf, ScsiTape) {
        let dir = scratch(name);
        for (file, data) in files {
            std::fs::write(dir.join(file), data).unwrap();
        }
        let tape = ScsiTape::open(&dir, None, false, None).unwrap();
        (dir, tape)
    }

    fn read6(fixed: bool, sili: bool, count: u32) -> [u8; 6] {
        let c = count.to_be_bytes();
        [
            0x08,
            u8::from(fixed) | (u8::from(sili) << 1),
            c[1],
            c[2],
            c[3],
            0,
        ]
    }

    fn write6(fixed: bool, count: u32) -> [u8; 6] {
        let c = count.to_be_bytes();
        [0x0A, u8::from(fixed), c[1], c[2], c[3], 0]
    }

    fn space6(code: u8, count: i32) -> [u8; 6] {
        let c = count.to_be_bytes();
        [0x11, code, c[1], c[2], c[3], 0]
    }

    /// Run a command with no data-out phase; returns data-in and status.
    fn run(tape: &mut ScsiTape, cdb: &[u8]) -> (Vec<u8>, u8) {
        match tape.execute(cdb, 0) {
            (ScsiExec::DataIn(data), status) => (data, status),
            (ScsiExec::NoData, status) => (Vec::new(), status),
            (ScsiExec::DataOut(_), _) => panic!("unexpected data-out phase for {cdb:02X?}"),
        }
    }

    /// Run a data-out command with `data` as its payload.
    fn run_out(tape: &mut ScsiTape, cdb: &[u8], data: &[u8]) -> u8 {
        match tape.execute(cdb, 0) {
            (ScsiExec::DataOut(expected), GOOD) => {
                assert_eq!(expected, data.len(), "data-out length for {cdb:02X?}");
                tape.complete_out(cdb, data)
            }
            (ScsiExec::NoData, status) => status,
            _ => panic!("unexpected phase for {cdb:02X?}"),
        }
    }

    /// REQUEST SENSE: (sense key, FILEMARK|EOM|ILI flags, valid bit,
    /// information, ASC, ASCQ).
    fn sense(tape: &mut ScsiTape) -> (u8, u8, bool, u32, u8, u8) {
        let (data, status) = run(tape, &[0x03, 0, 0, 0, 18, 0]);
        assert_eq!(status, GOOD);
        assert_eq!(data.len(), 18);
        (
            data[2] & 0x0F,
            data[2] & 0xE0,
            data[0] & 0x80 != 0,
            u32::from_be_bytes([data[3], data[4], data[5], data[6]]),
            data[12],
            data[13],
        )
    }

    fn set_block_len(tape: &mut ScsiTape, len: u32) {
        let l = len.to_be_bytes();
        let list = [0, 0, 0x10, 8, 0, 0, 0, 0, 0, l[1], l[2], l[3]];
        assert_eq!(run_out(tape, &[0x15, 0x10, 0, 0, 12, 0], &list), GOOD);
    }

    #[test]
    fn inquiry_identifies_a_removable_sequential_access_device() {
        let (dir, mut tape) = directory_tape("inquiry", &[]);
        let (data, status) = run(&mut tape, &[0x12, 0, 0, 0, 36, 0]);
        assert_eq!(status, GOOD);
        assert_eq!(data.len(), 36);
        assert_eq!(data[0], 0x01, "peripheral device type: sequential access");
        assert_eq!(data[1] & 0x80, 0x80, "removable medium");
        assert_eq!(data[2], 0x02);
        assert_eq!(&data[8..16], b"COPPERLN");
        assert_eq!(&data[16..25], b"SCSI TAPE");
        // An unsupported LUN answers INQUIRY with "no device".
        let (exec, status) = tape.execute(&[0x12, 0, 0, 0, 36, 0], 1);
        assert_eq!(status, GOOD);
        let ScsiExec::DataIn(data) = exec else {
            panic!("INQUIRY returns data");
        };
        assert_eq!(data[0], 0x7F);
        // Short allocation length truncates.
        let (data, _) = run(&mut tape, &[0x12, 0, 0, 0, 5, 0]);
        assert_eq!(data.len(), 5);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn read_block_limits_and_mode_sense_report_the_block_length() {
        let (dir, mut tape) = directory_tape("limits", &[]);
        let (data, status) = run(&mut tape, &[0x05, 0, 0, 0, 0, 0]);
        assert_eq!(status, GOOD);
        assert_eq!(data, [0, 0xFF, 0xFF, 0xFF, 0x00, 0x01]);

        // Header + block descriptor: 512-byte blocks at power-on.
        let (data, status) = run(&mut tape, &[0x1A, 0, 0, 0, 255, 0]);
        assert_eq!(status, GOOD);
        assert_eq!(data.len(), 12);
        assert_eq!(data[0], 11);
        assert_eq!(data[2] & 0x80, 0, "not write-protected");
        assert_eq!(data[3], 8);
        assert_eq!(&data[9..12], &[0x00, 0x02, 0x00]);

        set_block_len(&mut tape, 1024);
        let (data, _) = run(&mut tape, &[0x1A, 0, 0, 0, 255, 0]);
        assert_eq!(&data[9..12], &[0x00, 0x04, 0x00]);

        // DBD drops the descriptor; page 10h follows the header.
        let (data, status) = run(&mut tape, &[0x1A, 0x08, 0x10, 0, 255, 0]);
        assert_eq!(status, GOOD);
        assert_eq!(data.len(), 4 + 16);
        assert_eq!(data[3], 0);
        assert_eq!(data[4], 0x10);
        assert_eq!(data[5], 14);
        // All pages is the same one page; an unknown page is refused.
        let (all, _) = run(&mut tape, &[0x1A, 0x08, 0x3F, 0, 255, 0]);
        assert_eq!(all, data);
        let (_, status) = run(&mut tape, &[0x1A, 0, 0x02, 0, 255, 0]);
        assert_eq!(status, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_ILLEGAL_REQUEST);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mode_sense_reports_the_write_protect_tab() {
        let dir = scratch("wp-mode");
        let mut tape = ScsiTape::open(&dir, None, true, None).unwrap();
        let (data, _) = run(&mut tape, &[0x1A, 0, 0, 0, 255, 0]);
        assert_eq!(data[2] & 0x80, 0x80);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mode_select_validates_its_parameter_list() {
        let (dir, mut tape) = directory_tape("mode-select", &[]);
        // Variable-block mode.
        set_block_len(&mut tape, 0);
        let (data, _) = run(&mut tape, &[0x1A, 0, 0, 0, 255, 0]);
        assert_eq!(&data[9..12], &[0, 0, 0]);
        // A header alone changes only the buffered mode.
        assert_eq!(
            run_out(&mut tape, &[0x15, 0x10, 0, 0, 4, 0], &[0, 0, 0x00, 0]),
            GOOD
        );
        let (data, _) = run(&mut tape, &[0x1A, 0, 0, 0, 255, 0]);
        assert_eq!(data[2] & 0x70, 0);
        assert_eq!(&data[9..12], &[0, 0, 0]);
        // The device configuration page is accepted as it was reported.
        let (page, _) = run(&mut tape, &[0x1A, 0x08, 0x10, 0, 255, 0]);
        let mut list = vec![0, 0, 0x10, 0];
        list.extend_from_slice(&page[4..]);
        assert_eq!(
            run_out(&mut tape, &[0x15, 0x10, 0, 0, list.len() as u8, 0], &list),
            GOOD
        );
        // A block descriptor of the wrong size, and a page the drive does
        // not have, are refused and change nothing.
        for list in [
            vec![0u8, 0, 0, 4, 0, 0, 0, 0],
            vec![
                0, 0, 0, 0, 0x0F, 14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        ] {
            assert_eq!(
                run_out(&mut tape, &[0x15, 0x10, 0, 0, list.len() as u8, 0], &list),
                CHECK_CONDITION
            );
            let (key, _, _, _, asc, _) = sense(&mut tape);
            assert_eq!((key, asc), (SK_ILLEGAL_REQUEST, 0x26));
        }
        // A truncated header.
        assert_eq!(
            run_out(&mut tape, &[0x15, 0x10, 0, 0, 2, 0], &[0, 0]),
            CHECK_CONDITION
        );
        assert_eq!(sense(&mut tape).4, ASC_PARAMETER_LIST_LENGTH_ERROR);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fixed_block_reads_walk_a_tape_file_and_stop_at_its_filemark() {
        let (dir, mut tape) = directory_tape(
            "fixed-read",
            &[("a.tar", pattern(1, 1536)), ("b.tar", pattern(2, 512))],
        );
        let (data, status) = run(&mut tape, &read6(true, false, 2));
        assert_eq!(status, GOOD);
        assert_eq!(data, pattern(1, 1536)[..1024]);

        // Three more asked for, one left: the block, then the filemark.
        let (data, status) = run(&mut tape, &read6(true, false, 3));
        assert_eq!(status, CHECK_CONDITION);
        assert_eq!(data, pattern(1, 1536)[1024..]);
        let (key, flags, valid, info, asc, ascq) = sense(&mut tape);
        assert_eq!(key, SK_NO_SENSE);
        assert_eq!(flags, SENSE_FILEMARK);
        assert!(valid);
        assert_eq!(info, 2, "blocks not read");
        assert_eq!((asc, ascq), (0x00, ASCQ_FILEMARK_DETECTED));

        // The head is past the filemark: the next read is file 1.
        let (data, status) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(status, GOOD);
        assert_eq!(data, pattern(2, 512));
        // Its filemark, with nothing transferred...
        let (data, status) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(status, CHECK_CONDITION);
        assert!(data.is_empty());
        assert_eq!(sense(&mut tape).1, SENSE_FILEMARK);
        // ...then end-of-data, as often as it is asked.
        for _ in 0..2 {
            let (data, status) = run(&mut tape, &read6(true, false, 4));
            assert_eq!(status, CHECK_CONDITION);
            assert!(data.is_empty());
            let (key, flags, valid, info, asc, ascq) = sense(&mut tape);
            assert_eq!(key, SK_BLANK_CHECK);
            assert_eq!(flags, 0);
            assert!(valid);
            assert_eq!(info, 4);
            assert_eq!((asc, ascq), (0x00, ASCQ_END_OF_DATA));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_short_last_block_is_an_incorrect_length_block_in_fixed_mode() {
        let (dir, mut tape) = directory_tape("fixed-ili", &[("a", pattern(3, 512 + 100))]);
        let (data, status) = run(&mut tape, &read6(true, false, 4));
        assert_eq!(status, CHECK_CONDITION);
        // The whole block and the short one both arrive.
        assert_eq!(data, pattern(3, 612));
        let (key, flags, valid, info, _, _) = sense(&mut tape);
        assert_eq!(key, SK_NO_SENSE);
        assert_eq!(flags, SENSE_ILI);
        assert!(valid);
        assert_eq!(info, 3, "the incorrect-length block does not count as read");
        // The head is past the short block: next comes the filemark.
        let (_, status) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(status, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).1, SENSE_FILEMARK);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fixed_reads_need_a_block_length_and_refuse_sili() {
        let (dir, mut tape) = directory_tape("fixed-args", &[("a", pattern(1, 1024))]);
        let (_, status) = run(&mut tape, &read6(true, true, 1));
        assert_eq!(status, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_ILLEGAL_REQUEST);
        set_block_len(&mut tape, 0);
        let (_, status) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(status, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_ILLEGAL_REQUEST);
        // A zero-length read moves nothing and is not an error.
        let (data, status) = run(&mut tape, &read6(false, false, 0));
        assert_eq!(status, GOOD);
        assert!(data.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn variable_reads_take_what_they_ask_for_and_report_a_short_block() {
        let (dir, mut tape) = directory_tape("variable", &[("a", pattern(4, 2500))]);
        set_block_len(&mut tape, 0);
        let (data, status) = run(&mut tape, &read6(false, false, 1000));
        assert_eq!(status, GOOD);
        assert_eq!(data, pattern(4, 2500)[..1000]);
        let (data, status) = run(&mut tape, &read6(false, false, 1000));
        assert_eq!(status, GOOD);
        assert_eq!(data, pattern(4, 2500)[1000..2000]);
        // 500 bytes left for a 1000-byte read: a short block.
        let (data, status) = run(&mut tape, &read6(false, false, 1000));
        assert_eq!(status, CHECK_CONDITION);
        assert_eq!(data, pattern(4, 2500)[2000..]);
        let (key, flags, valid, info, _, _) = sense(&mut tape);
        assert_eq!(key, SK_NO_SENSE);
        assert_eq!(flags, SENSE_ILI);
        assert!(valid);
        assert_eq!(info, 500, "requested minus actual");
        // Then the filemark, with the whole request as residue.
        let (data, status) = run(&mut tape, &read6(false, false, 1000));
        assert_eq!(status, CHECK_CONDITION);
        assert!(data.is_empty());
        let (_, flags, _, info, _, _) = sense(&mut tape);
        assert_eq!(flags, SENSE_FILEMARK);
        assert_eq!(info, 1000);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sili_suppresses_the_short_block_report_but_not_an_overlength_one() {
        let (dir, mut tape) = directory_tape("sili", &[("a", pattern(5, 700))]);
        set_block_len(&mut tape, 0);
        let (data, status) = run(&mut tape, &read6(false, true, 1000));
        assert_eq!(status, GOOD);
        assert_eq!(data.len(), 700);

        // With a 512-byte block length, a variable read into a smaller
        // buffer gets the start of the block; the rest is skipped and the
        // residue is negative.
        run(&mut tape, &[0x01, 0, 0, 0, 0, 0]);
        set_block_len(&mut tape, 512);
        let (data, status) = run(&mut tape, &read6(false, true, 100));
        assert_eq!(status, CHECK_CONDITION);
        assert_eq!(data, pattern(5, 700)[..100]);
        let (key, flags, valid, info, _, _) = sense(&mut tape);
        assert_eq!(key, SK_NO_SENSE);
        assert_eq!(flags, SENSE_ILI);
        assert!(valid);
        assert_eq!(info as i32, 100 - 512);
        // The head passed the whole block: the next one starts at 512.
        let (data, status) = run(&mut tape, &read6(false, true, 512));
        assert_eq!(status, GOOD);
        assert_eq!(data, pattern(5, 700)[512..]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn writes_make_tape_files_and_filemarks_step_to_the_next() {
        let dir = scratch("write");
        let mut tape = ScsiTape::open(&dir, None, false, None).unwrap();
        // A blank tape: end-of-data at once.
        let (_, status) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(status, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_BLANK_CHECK);

        assert_eq!(
            run_out(&mut tape, &write6(true, 2), &pattern(1, 1024)),
            GOOD
        );
        assert_eq!(run_out(&mut tape, &write6(true, 1), &pattern(9, 512)), GOOD);
        assert_eq!(run(&mut tape, &[0x10, 0, 0, 0, 1, 0]).1, GOOD);
        set_block_len(&mut tape, 0);
        assert_eq!(
            run_out(&mut tape, &write6(false, 300), &pattern(2, 300)),
            GOOD
        );
        // Two filemarks: the second closes an empty tape file.
        assert_eq!(run(&mut tape, &[0x10, 0, 0, 0, 2, 0]).1, GOOD);

        // On the host: three files, in tape order by name.
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["00000.tape", "00001.tape", "00002.tape"]);
        let mut first = pattern(1, 1024);
        first.extend(pattern(9, 512));
        assert_eq!(std::fs::read(dir.join("00000.tape")).unwrap(), first);
        assert_eq!(
            std::fs::read(dir.join("00001.tape")).unwrap(),
            pattern(2, 300)
        );
        assert!(std::fs::read(dir.join("00002.tape")).unwrap().is_empty());

        // And back through the drive.
        assert_eq!(run(&mut tape, &[0x01, 0, 0, 0, 0, 0]).1, GOOD);
        let (data, status) = run(&mut tape, &read6(false, false, 4096));
        assert_eq!(status, CHECK_CONDITION, "a short block");
        assert_eq!(data, first);
        assert_eq!(sense(&mut tape).1, SENSE_ILI);
        assert_eq!(
            run(&mut tape, &read6(false, false, 4096)).1,
            CHECK_CONDITION
        );
        assert_eq!(sense(&mut tape).1, SENSE_FILEMARK);
        let (data, _) = run(&mut tape, &read6(false, true, 4096));
        assert_eq!(data, pattern(2, 300));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn writing_in_the_middle_cuts_the_tape_there() {
        let (dir, mut tape) = directory_tape(
            "overwrite",
            &[
                ("a", pattern(1, 2048)),
                ("b", pattern(2, 1024)),
                ("c", pattern(3, 512)),
            ],
        );
        // One block into file 0, write one block: the rest of file 0 and
        // files 1 and 2 are gone.
        run(&mut tape, &read6(true, false, 1));
        assert_eq!(run_out(&mut tape, &write6(true, 1), &pattern(7, 512)), GOOD);
        let mut expected = pattern(1, 512);
        expected.extend(pattern(7, 512));
        assert_eq!(std::fs::read(dir.join("a")).unwrap(), expected);
        assert!(!dir.join("b").exists());
        assert!(!dir.join("c").exists());
        // The head is at the end of what was written: filemark, then EOD.
        assert_eq!(run(&mut tape, &read6(true, false, 1)).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).1, SENSE_FILEMARK);
        assert_eq!(run(&mut tape, &read6(true, false, 1)).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_BLANK_CHECK);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_tape_file_sorts_after_the_files_already_in_the_directory() {
        let (dir, mut tape) = directory_tape(
            "naming",
            &[
                ("backup.tar", pattern(1, 512)),
                ("zeta.tar", pattern(2, 512)),
            ],
        );
        assert_eq!(run(&mut tape, &space6(3, 0)).1, GOOD);
        assert_eq!(run_out(&mut tape, &write6(true, 1), &pattern(3, 512)), GOOD);
        // A freshly opened drive finds it as the third tape file.
        let mut reopened = ScsiTape::open(&dir, None, false, None).unwrap();
        assert_eq!(reopened.files.len(), 3);
        assert_eq!(run(&mut reopened, &space6(1, 2)).1, GOOD);
        let (data, status) = run(&mut reopened, &read6(true, false, 1));
        assert_eq!(status, GOOD);
        assert_eq!(data, pattern(3, 512));
        let name = reopened.files[2].to_string_lossy();
        assert!(name.as_ref() > "zeta.tar", "{name}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_tape_file_sorts_last_whatever_the_existing_names_are_made_of() {
        // Names no ASCII prefix can outrank: the new file takes the last
        // name as its prefix.
        let (dir, mut tape) = directory_tape(
            "naming-unicode",
            &[
                ("\u{c4}rzte.tar", pattern(1, 512)),
                ("~~~.tar", pattern(2, 512)),
            ],
        );
        assert_eq!(run(&mut tape, &space6(3, 0)).1, GOOD);
        assert_eq!(run_out(&mut tape, &write6(true, 1), &pattern(3, 512)), GOOD);
        assert_eq!(run(&mut tape, &[0x10, 0, 0, 0, 1, 0]).1, GOOD);
        assert_eq!(run_out(&mut tape, &write6(true, 1), &pattern(4, 512)), GOOD);
        let mut reopened = ScsiTape::open(&dir, None, false, None).unwrap();
        assert_eq!(reopened.files.len(), 4);
        assert_eq!(run(&mut reopened, &space6(1, 2)).1, GOOD);
        let (data, _) = run(&mut reopened, &read6(true, false, 1));
        assert_eq!(data, pattern(3, 512));
        assert_eq!(run(&mut reopened, &space6(1, 1)).1, GOOD);
        let (data, _) = run(&mut reopened, &read6(true, false, 1));
        assert_eq!(data, pattern(4, 512));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_directory_cartridge_ends_after_its_last_tape_file() {
        let dir = scratch("capacity");
        let mut tape = ScsiTape::open(&dir, None, false, None).unwrap();
        // The head is at the last tape file the cartridge has room for.
        tape.file = MAX_TAPE_FILES - 1;
        tape.files = (0..MAX_TAPE_FILES - 1)
            .map(|i| format!("{i:05}.tape").into())
            .collect();
        // A request for sixteen million filemarks writes the one that
        // fits and reports the rest as residue at the end of the tape.
        let (_, status) = run(&mut tape, &[0x10, 0, 0xFF, 0xFF, 0xFF, 0]);
        assert_eq!(status, CHECK_CONDITION);
        let (key, flags, valid, info, _, _) = sense(&mut tape);
        assert_eq!((key, flags, valid), (SK_VOLUME_OVERFLOW, SENSE_EOM, true));
        assert_eq!(info, 0xFF_FFFF - 1);
        assert_eq!(tape.files.len(), MAX_TAPE_FILES);
        // And there is no room for data after it.
        let (exec, status) = tape.execute(&write6(true, 1), 0);
        assert!(matches!(exec, ScsiExec::NoData));
        assert_eq!(status, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_VOLUME_OVERFLOW);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_data_phase_that_delivers_nothing_leaves_the_tape_alone() {
        let (dir, mut tape) = directory_tape(
            "empty-write",
            &[("a", pattern(1, 1024)), ("b", pattern(2, 512))],
        );
        run(&mut tape, &read6(true, false, 1));
        // The command asked for one block; the adapter delivered half of
        // one, which makes no block at all.
        let cdb = write6(true, 1);
        assert!(matches!(tape.execute(&cdb, 0).0, ScsiExec::DataOut(512)));
        assert_eq!(tape.complete_out(&cdb, &[0u8; 256]), CHECK_CONDITION);
        // Reported as the aborted command it is, with the block that was
        // not written as the residue.
        let (key, flags, valid, info, _, _) = sense(&mut tape);
        assert_eq!((key, flags, valid, info), (SK_ABORTED_COMMAND, 0, true, 1));
        assert_eq!(std::fs::read(dir.join("a")).unwrap(), pattern(1, 1024));
        assert!(dir.join("b").exists());

        // Two blocks asked for, one and a half delivered: the whole block
        // is written (cutting the tape there) and one is the residue.
        let cdb = write6(true, 2);
        assert!(matches!(tape.execute(&cdb, 0).0, ScsiExec::DataOut(1024)));
        assert_eq!(tape.complete_out(&cdb, &pattern(9, 768)), CHECK_CONDITION);
        let (key, _, _, info, _, _) = sense(&mut tape);
        assert_eq!((key, info), (SK_ABORTED_COMMAND, 1));
        let mut expected = pattern(1, 512);
        expected.extend_from_slice(&pattern(9, 768)[..512]);
        assert_eq!(std::fs::read(dir.join("a")).unwrap(), expected);
        assert!(!dir.join("b").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn generated_names_stay_short_however_many_tape_files_follow_a_named_one() {
        // "backup.tar" outranks NNNNN.tape, so the first new file takes a
        // '~'; every later one only counts on from it.
        let (dir, mut tape) = directory_tape("naming-chain", &[("backup.tar", pattern(1, 512))]);
        assert_eq!(run(&mut tape, &space6(3, 0)).1, GOOD);
        for _ in 0..40 {
            assert_eq!(run_out(&mut tape, &write6(true, 1), &pattern(2, 512)), GOOD);
            assert_eq!(run(&mut tape, &[0x10, 0, 0, 0, 1, 0]).1, GOOD);
        }
        let names = tape.names();
        assert_eq!(names.len(), 41);
        assert_eq!(names[1], "~00001.tape");
        assert_eq!(names[40], "~00040.tape");
        // In tape order on the host too.
        let reopened = ScsiTape::open(&dir, None, false, None).unwrap();
        assert_eq!(reopened.names(), names);

        // A user file numbered ahead of the count gets the same treatment.
        let (dir2, mut tape) = directory_tape("naming-ahead", &[("00007.tape", pattern(1, 512))]);
        assert_eq!(run(&mut tape, &space6(3, 0)).1, GOOD);
        assert_eq!(run_out(&mut tape, &write6(true, 1), &pattern(2, 512)), GOOD);
        assert_eq!(run(&mut tape, &[0x10, 0, 0, 0, 1, 0]).1, GOOD);
        assert_eq!(run_out(&mut tape, &write6(true, 1), &pattern(3, 512)), GOOD);
        assert_eq!(tape.names(), ["00007.tape", "~00001.tape", "~00002.tape"]);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    #[test]
    fn a_path_with_nothing_at_it_is_a_new_directory_unless_a_file_is_asked_for() {
        let dir = scratch("absent");
        let cartridge = dir.join("backups");
        let mut tape = ScsiTape::open(&cartridge, None, false, None).unwrap();
        assert!(cartridge.is_dir());
        // Room for more than one tape file, as a directory has.
        assert_eq!(run_out(&mut tape, &write6(true, 1), &pattern(1, 512)), GOOD);
        assert_eq!(run(&mut tape, &[0x10, 0, 0, 0, 1, 0]).1, GOOD);
        assert_eq!(run_out(&mut tape, &write6(true, 1), &pattern(2, 512)), GOOD);
        assert_eq!(tape.names(), ["00000.tape", "00001.tape"]);

        // Asked for as a file, it is a blank single-file tape.
        let single = dir.join("one.tar");
        let tape = ScsiTape::open(&single, Some(Cartridge::File), false, None).unwrap();
        assert!(!single.exists());
        assert!(tape.files.is_empty());

        // What is there has to be what was asked for.
        assert!(ScsiTape::open(&cartridge, Some(Cartridge::File), false, None).is_err());
        std::fs::write(&single, b"x").unwrap();
        assert!(ScsiTape::open(&single, Some(Cartridge::Directory), false, None).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_filemark_on_a_blank_single_file_tape_survives_a_reload() {
        let dir = scratch("blank-filemark");
        let path = dir.join("one.tar");
        let mut tape = ScsiTape::open(&path, Some(Cartridge::File), false, None).unwrap();
        assert_eq!(run(&mut tape, &[0x10, 0, 0, 0, 1, 0]).1, GOOD);
        // An empty tape file: the host file exists and is empty.
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert_eq!(run(&mut tape, &[0x1B, 0, 0, 0, 0, 0]).1, GOOD);
        assert_eq!(run(&mut tape, &[0x1B, 0, 0, 0, 1, 0]).1, GOOD);
        // Still a filemark and then end-of-data, not a blank tape.
        assert_eq!(run(&mut tape, &read6(true, false, 1)).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).1, SENSE_FILEMARK);
        assert_eq!(run(&mut tape, &read6(true, false, 1)).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_BLANK_CHECK);

        // A long erase from the beginning makes it blank again: no file.
        assert_eq!(run(&mut tape, &[0x01, 0, 0, 0, 0, 0]).1, GOOD);
        assert_eq!(run(&mut tape, &[0x19, 0x01, 0, 0, 0, 0]).1, GOOD);
        assert!(!path.exists());
        assert_eq!(run(&mut tape, &read6(true, false, 1)).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_BLANK_CHECK);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_tape_with_a_capacity_warns_near_its_end_and_then_overflows() {
        // 64 KiB of tape: the early-warning zone is its last 1/32nd, 2 KiB.
        let dir = scratch("capacity-bytes");
        let mut tape = ScsiTape::open(&dir, None, false, Some(64 * 1024)).unwrap();
        // 60 KiB: plain success.
        for _ in 0..60 {
            assert_eq!(
                run_out(&mut tape, &write6(true, 2), &pattern(1, 1024)),
                GOOD
            );
        }
        assert_eq!(
            run_out(&mut tape, &write6(true, 2), &pattern(1, 1024)),
            GOOD
        );
        // 62 KiB is where the zone starts: the write that reaches it is
        // done, and says the end is near.
        assert_eq!(
            run_out(&mut tape, &write6(true, 2), &pattern(2, 1024)),
            CHECK_CONDITION
        );
        let (key, flags, valid, info, asc, ascq) = sense(&mut tape);
        assert_eq!((key, flags, valid, info), (SK_NO_SENSE, SENSE_EOM, true, 0));
        assert_eq!((asc, ascq), (0x00, ASCQ_END_OF_MEDIUM));
        assert_eq!(
            std::fs::metadata(dir.join("00000.tape")).unwrap().len(),
            62 * 1024
        );
        // A filemark in the zone is written and warns the same way.
        assert_eq!(run(&mut tape, &[0x10, 0, 0, 0, 1, 0]).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).1, SENSE_EOM);
        assert_eq!(tape.file, 1);
        // Six blocks into 2 KiB of room: four are written, two are the
        // residue of a volume overflow.
        assert_eq!(
            run_out(&mut tape, &write6(true, 6), &pattern(3, 6 * 512)),
            CHECK_CONDITION
        );
        let (key, flags, valid, info, _, _) = sense(&mut tape);
        assert_eq!(
            (key, flags, valid, info),
            (SK_VOLUME_OVERFLOW, SENSE_EOM, true, 2)
        );
        assert_eq!(
            std::fs::read(dir.join("00001.tape")).unwrap(),
            pattern(3, 6 * 512)[..2048]
        );
        // Nothing more fits: no data phase at all.
        let (exec, status) = tape.execute(&write6(true, 1), 0);
        assert!(matches!(exec, ScsiExec::NoData));
        assert_eq!(status, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_VOLUME_OVERFLOW);
        // Rewound, the room is there again: writing cuts the tape.
        assert_eq!(run(&mut tape, &[0x01, 0, 0, 0, 0, 0]).1, GOOD);
        assert_eq!(run_out(&mut tape, &write6(true, 1), &pattern(4, 512)), GOOD);

        // A variable block that does not fit whole is not written at all.
        let dir2 = scratch("capacity-variable");
        let mut tape = ScsiTape::open(&dir2, None, false, Some(1000)).unwrap();
        set_block_len(&mut tape, 0);
        assert_eq!(
            run_out(&mut tape, &write6(false, 600), &pattern(1, 600)),
            GOOD
        );
        let (exec, status) = tape.execute(&write6(false, 600), 0);
        assert!(matches!(exec, ScsiExec::NoData));
        assert_eq!(status, CHECK_CONDITION);
        let (key, _, _, info, _, _) = sense(&mut tape);
        assert_eq!((key, info), (SK_VOLUME_OVERFLOW, 600));
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_file_is_a_tape_file_and_a_dangling_one_is_not() {
        let (dir, _) = directory_tape("links", &[("b", pattern(2, 512))]);
        let outside = scratch("links-target");
        std::fs::write(outside.join("real.tar"), pattern(1, 512)).unwrap();
        std::os::unix::fs::symlink(outside.join("real.tar"), dir.join("a.tar")).unwrap();
        std::os::unix::fs::symlink(outside.join("gone"), dir.join("c.tar")).unwrap();
        let mut tape = ScsiTape::open(&dir, None, false, None).unwrap();
        assert_eq!(tape.names(), ["a.tar", "b"]);
        let (data, status) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(status, GOOD);
        assert_eq!(data, pattern(1, 512));
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn a_restored_drive_reads_its_tape_files_from_the_cartridge_not_the_state() {
        // A state names tape files; the restored drive must touch only
        // what is in its cartridge. Build a state whose names point out of
        // the cartridge and at a victim file beside it.
        let (dir, tape) = directory_tape("state-paths", &[("a", pattern(1, 1024))]);
        let victim = dir.parent().unwrap().join(format!(
            "copperline-scsi-tape-victim-{}",
            std::process::id()
        ));
        std::fs::write(&victim, b"precious").unwrap();
        let state = TapeState {
            path: dir.clone(),
            cartridge: Cartridge::Directory,
            names: vec![
                format!("../{}", victim.file_name().unwrap().to_string_lossy()),
                victim.to_string_lossy().into_owned(),
            ],
            write_protected: false,
            capacity: None,
            loaded: true,
            block_len: 512,
            last_block_len: 512,
            file: 0,
            offset: 0,
            buffered_mode: 1,
            sense: [0; SENSE_LEN],
        };
        drop(tape);
        let mut bytes = Vec::new();
        crate::savestate::chunk::encode(&state, &mut bytes).unwrap();
        let mut restored: ScsiTape = crate::savestate::chunk::decode(&bytes).unwrap();
        // The drive has the cartridge's one real tape file.
        assert_eq!(restored.names(), ["a"]);
        // REWIND, WRITE, then a long ERASE from the start: the cartridge
        // changes, the victim does not.
        assert_eq!(run(&mut restored, &[0x01, 0, 0, 0, 0, 0]).1, GOOD);
        assert_eq!(
            run_out(&mut restored, &write6(true, 1), &pattern(5, 512)),
            GOOD
        );
        assert_eq!(run(&mut restored, &[0x01, 0, 0, 0, 0, 0]).1, GOOD);
        assert_eq!(run(&mut restored, &[0x19, 0x01, 0, 0, 0, 0]).1, GOOD);
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
        assert!(!dir.join("a").exists());
        let _ = std::fs::remove_file(&victim);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_restored_drive_takes_the_tape_as_it_is_now() {
        let (dir, mut tape) = directory_tape(
            "state-changed",
            &[("a", pattern(1, 2048)), ("b", pattern(2, 1024))],
        );
        // Head 512 bytes into tape file 1.
        assert_eq!(run(&mut tape, &space6(1, 1)).1, GOOD);
        run(&mut tape, &read6(true, false, 1));
        let mut bytes = Vec::new();
        crate::savestate::chunk::encode(&tape, &mut bytes).unwrap();

        // Unchanged cartridge: the head is where it was.
        let mut same: ScsiTape = crate::savestate::chunk::decode(&bytes).unwrap();
        let (data, status) = run(&mut same, &read6(true, false, 1));
        assert_eq!(status, GOOD);
        assert_eq!(data, pattern(2, 1024)[512..]);

        // Tape file 1 shortened behind the state's back: the head is at
        // its new end, so the next read meets the filemark instead of a
        // medium error.
        std::fs::write(dir.join("b"), pattern(2, 100)).unwrap();
        let mut shorter: ScsiTape = crate::savestate::chunk::decode(&bytes).unwrap();
        assert_eq!((shorter.file, shorter.offset), (1, 100));
        assert_eq!(run(&mut shorter, &read6(true, false, 1)).1, CHECK_CONDITION);
        assert_eq!(sense(&mut shorter).1, SENSE_FILEMARK);

        // Tape file 1 gone: the head is at end-of-data.
        std::fs::remove_file(dir.join("b")).unwrap();
        let mut fewer: ScsiTape = crate::savestate::chunk::decode(&bytes).unwrap();
        assert_eq!((fewer.file, fewer.offset), (1, 0));
        assert_eq!(run(&mut fewer, &read6(true, false, 1)).1, CHECK_CONDITION);
        assert_eq!(sense(&mut fewer).0, SK_BLANK_CHECK);

        // The cartridge itself gone: the drive comes back unloaded.
        std::fs::remove_dir_all(&dir).unwrap();
        let mut gone: ScsiTape = crate::savestate::chunk::decode(&bytes).unwrap();
        assert_eq!(run(&mut gone, &[0x00, 0, 0, 0, 0, 0]).1, CHECK_CONDITION);
        assert_eq!(sense(&mut gone).0, SK_NOT_READY);
    }

    #[test]
    fn hidden_files_and_subdirectories_are_not_part_of_the_tape() {
        let (dir, _) = directory_tape("hidden", &[("a", pattern(1, 512))]);
        std::fs::write(dir.join(".DS_Store"), b"junk").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        let tape = ScsiTape::open(&dir, None, false, None).unwrap();
        assert_eq!(tape.files.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn space_over_blocks_stops_at_filemarks_and_at_beginning_of_tape() {
        let (dir, mut tape) = directory_tape(
            "space-blocks",
            &[("a", pattern(1, 2048)), ("b", pattern(2, 1024))],
        );
        assert_eq!(run(&mut tape, &space6(0, 3)).1, GOOD);
        let (data, _) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(data, pattern(1, 2048)[1536..]);

        // Back two blocks, read the third again.
        assert_eq!(run(&mut tape, &space6(0, -2)).1, GOOD);
        let (data, _) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(data, pattern(1, 2048)[1024..1536]);

        // Forward past the end of the file: one block there, then the
        // filemark, which the head passes.
        assert_eq!(run(&mut tape, &space6(0, 5)).1, CHECK_CONDITION);
        let (key, flags, valid, info, _, _) = sense(&mut tape);
        assert_eq!(
            (key, flags, valid, info),
            (SK_NO_SENSE, SENSE_FILEMARK, true, 4)
        );
        let (data, _) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(data, pattern(2, 1024)[..512]);

        // Backward out of file 1: one block there, then file 0's filemark,
        // stopping on its beginning-of-tape side with a negative residue.
        assert_eq!(run(&mut tape, &space6(0, -3)).1, CHECK_CONDITION);
        let (key, flags, valid, info, _, _) = sense(&mut tape);
        assert_eq!((key, flags, valid), (SK_NO_SENSE, SENSE_FILEMARK, true));
        assert_eq!(info as i32, -2);
        // The head is at the end of file 0: reading meets the filemark.
        assert_eq!(run(&mut tape, &read6(true, false, 1)).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).1, SENSE_FILEMARK);

        // Backward past the start of the tape.
        assert_eq!(run(&mut tape, &[0x01, 0, 0, 0, 0, 0]).1, GOOD);
        assert_eq!(run(&mut tape, &space6(0, 1)).1, GOOD);
        assert_eq!(run(&mut tape, &space6(0, -4)).1, CHECK_CONDITION);
        let (key, flags, valid, info, asc, ascq) = sense(&mut tape);
        assert_eq!((key, flags, valid), (SK_NO_SENSE, SENSE_EOM, true));
        assert_eq!(info as i32, -3);
        assert_eq!((asc, ascq), (0x00, ASCQ_BEGINNING_OF_MEDIUM));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn space_over_filemarks_and_to_end_of_data() {
        let (dir, mut tape) = directory_tape(
            "space-fm",
            &[
                ("a", pattern(1, 512)),
                ("b", pattern(2, 512)),
                ("c", pattern(3, 512)),
            ],
        );
        assert_eq!(run(&mut tape, &space6(1, 2)).1, GOOD);
        let (data, _) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(data, pattern(3, 512));

        // Back over two filemarks: the head sits at the end of file 0, so
        // a forward space over one filemark lands at the start of file 1.
        assert_eq!(run(&mut tape, &space6(1, -2)).1, GOOD);
        assert_eq!(run(&mut tape, &space6(1, 1)).1, GOOD);
        let (data, _) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(data, pattern(2, 512));

        // More filemarks than there are: end-of-data, with the residue.
        assert_eq!(run(&mut tape, &space6(1, 5)).1, CHECK_CONDITION);
        let (key, flags, valid, info, _, ascq) = sense(&mut tape);
        assert_eq!((key, flags, valid, info), (SK_BLANK_CHECK, 0, true, 3));
        assert_eq!(ascq, ASCQ_END_OF_DATA);

        // More filemarks back than there are: beginning-of-tape.
        assert_eq!(run(&mut tape, &space6(1, -7)).1, CHECK_CONDITION);
        let (key, flags, _, info, _, ascq) = sense(&mut tape);
        assert_eq!((key, flags), (SK_NO_SENSE, SENSE_EOM));
        assert_eq!(info as i32, -4);
        assert_eq!(ascq, ASCQ_BEGINNING_OF_MEDIUM);
        let (data, _) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(data, pattern(1, 512));

        // End-of-data directly; appending there adds a fourth tape file.
        assert_eq!(run(&mut tape, &space6(3, 0)).1, GOOD);
        assert_eq!(run(&mut tape, &read6(true, false, 1)).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_BLANK_CHECK);
        assert_eq!(run_out(&mut tape, &write6(true, 1), &pattern(4, 512)), GOOD);
        assert_eq!(tape.files.len(), 4);
        // Setmarks are not supported.
        assert_eq!(run(&mut tape, &space6(4, 1)).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_ILLEGAL_REQUEST);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn space_with_a_zero_count_leaves_the_head_where_it_is() {
        let (dir, mut tape) = directory_tape(
            "space-zero",
            &[("a", pattern(1, 1536)), ("b", pattern(2, 512))],
        );
        // One block into file 0, a zero count over filemarks or over
        // blocks moves nothing: the next read is the second block.
        let (data, _) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(data, pattern(1, 1536)[..512]);
        assert_eq!(run(&mut tape, &space6(1, 0)).1, GOOD);
        assert_eq!(run(&mut tape, &space6(0, 0)).1, GOOD);
        let (data, _) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(data, pattern(1, 1536)[512..1024]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn variable_mode_space_counts_blocks_of_the_last_length_moved() {
        let (dir, mut tape) = directory_tape("space-var", &[("a", pattern(1, 4000))]);
        set_block_len(&mut tape, 0);
        let (data, _) = run(&mut tape, &read6(false, false, 1000));
        assert_eq!(data.len(), 1000);
        assert_eq!(run(&mut tape, &space6(0, 2)).1, GOOD);
        let (data, _) = run(&mut tape, &read6(false, false, 1000));
        assert_eq!(data, pattern(1, 4000)[3000..]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_single_file_cartridge_holds_one_tape_file() {
        let dir = scratch("single");
        let path = dir.join("backup.tar");
        let mut tape = ScsiTape::open(&path, Some(Cartridge::File), false, None).unwrap();
        assert!(!path.exists(), "a blank tape makes no file until written");
        assert_eq!(run(&mut tape, &read6(true, false, 1)).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_BLANK_CHECK);

        assert_eq!(
            run_out(&mut tape, &write6(true, 2), &pattern(1, 1024)),
            GOOD
        );
        assert_eq!(std::fs::read(&path).unwrap(), pattern(1, 1024));
        // Its filemark is the one the file already ends in.
        assert_eq!(run(&mut tape, &[0x10, 0, 0, 0, 1, 0]).1, GOOD);
        // There is no room for a second tape file: data after the
        // filemark is refused before any data phase.
        let (exec, status) = tape.execute(&write6(true, 1), 0);
        assert!(
            matches!(exec, ScsiExec::NoData),
            "no data phase for a full tape"
        );
        assert_eq!(status, CHECK_CONDITION);
        let (key, flags, valid, info, asc, ascq) = sense(&mut tape);
        assert_eq!(
            (key, flags, valid, info),
            (SK_VOLUME_OVERFLOW, SENSE_EOM, true, 1)
        );
        assert_eq!((asc, ascq), (0x00, ASCQ_END_OF_MEDIUM));
        // Further filemarks only mark the end of the data again: accepted,
        // and the tape file is untouched.
        assert_eq!(run(&mut tape, &[0x10, 0, 0, 0, 1, 0]).1, GOOD);
        assert_eq!(std::fs::read(&path).unwrap(), pattern(1, 1024));

        // The two filemarks that close a backup, written from inside the
        // file: both are taken, and the first cuts the tape file at the
        // head.
        assert_eq!(run(&mut tape, &[0x01, 0, 0, 0, 0, 0]).1, GOOD);
        run(&mut tape, &read6(true, false, 1));
        assert_eq!(run(&mut tape, &[0x10, 0, 0, 0, 2, 0]).1, GOOD);
        assert_eq!(std::fs::read(&path).unwrap(), pattern(1, 512));
        // The head is at end-of-data, one filemark back is the file's end.
        assert_eq!(run(&mut tape, &read6(true, false, 1)).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_BLANK_CHECK);
        assert_eq!(run(&mut tape, &space6(1, -1)).1, GOOD);
        assert_eq!(run(&mut tape, &space6(0, -1)).1, GOOD);
        let (data, _) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(data, pattern(1, 512));

        // An existing file opens as a one-file tape and reads back.
        let mut reopened = ScsiTape::open(&path, Some(Cartridge::File), false, None).unwrap();
        let (data, status) = run(&mut reopened, &read6(true, false, 1));
        assert_eq!(status, GOOD);
        assert_eq!(data, pattern(1, 512));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_write_protected_tape_refuses_every_write() {
        let (dir, _) = directory_tape("protected", &[("a", pattern(1, 1024))]);
        let mut tape = ScsiTape::open(&dir, None, true, None).unwrap();
        let (exec, status) = tape.execute(&write6(true, 1), 0);
        assert!(matches!(exec, ScsiExec::NoData));
        assert_eq!(status, CHECK_CONDITION);
        let (key, _, _, _, asc, _) = sense(&mut tape);
        assert_eq!((key, asc), (SK_DATA_PROTECT, ASC_WRITE_PROTECTED));
        assert_eq!(run(&mut tape, &[0x10, 0, 0, 0, 1, 0]).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_DATA_PROTECT);
        assert_eq!(run(&mut tape, &[0x19, 0x01, 0, 0, 0, 0]).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_DATA_PROTECT);
        // Reading is unaffected, and nothing on the host changed.
        let (data, status) = run(&mut tape, &read6(true, false, 2));
        assert_eq!(status, GOOD);
        assert_eq!(data, pattern(1, 1024));
        assert_eq!(std::fs::read(dir.join("a")).unwrap(), pattern(1, 1024));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_long_erase_clears_the_tape_from_the_head_on() {
        let (dir, mut tape) =
            directory_tape("erase", &[("a", pattern(1, 1024)), ("b", pattern(2, 1024))]);
        // A short erase changes nothing.
        run(&mut tape, &read6(true, false, 1));
        assert_eq!(run(&mut tape, &[0x19, 0, 0, 0, 0, 0]).1, GOOD);
        assert_eq!(std::fs::read(dir.join("a")).unwrap().len(), 1024);
        assert!(dir.join("b").exists());
        // A long one cuts file 0 at the head and removes file 1.
        assert_eq!(run(&mut tape, &[0x19, 0x01, 0, 0, 0, 0]).1, GOOD);
        assert_eq!(std::fs::read(dir.join("a")).unwrap(), pattern(1, 512));
        assert!(!dir.join("b").exists());
        // From the beginning of the tape it leaves a blank tape.
        assert_eq!(run(&mut tape, &[0x01, 0, 0, 0, 0, 0]).1, GOOD);
        assert_eq!(run(&mut tape, &[0x19, 0x01, 0, 0, 0, 0]).1, GOOD);
        assert!(!dir.join("a").exists());
        assert_eq!(run(&mut tape, &read6(true, false, 1)).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_BLANK_CHECK);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unload_makes_the_unit_not_ready_until_the_next_load() {
        let (dir, mut tape) = directory_tape("load", &[("a", pattern(1, 1024))]);
        run(&mut tape, &read6(true, false, 1));
        assert_eq!(run(&mut tape, &[0x1B, 0, 0, 0, 0, 0]).1, GOOD);
        for cdb in [
            [0x00u8, 0, 0, 0, 0, 0],
            read6(true, false, 1),
            [0x01, 0, 0, 0, 0, 0],
            space6(1, 1),
        ] {
            assert_eq!(run(&mut tape, &cdb).1, CHECK_CONDITION, "{cdb:02X?}");
            let (key, _, _, _, asc, ascq) = sense(&mut tape);
            assert_eq!(
                (key, asc, ascq),
                (SK_NOT_READY, ASC_LOGICAL_UNIT_NOT_READY, 0x02),
                "{cdb:02X?}"
            );
        }
        // INQUIRY and MODE SENSE still answer.
        assert_eq!(run(&mut tape, &[0x12, 0, 0, 0, 36, 0]).1, GOOD);
        assert_eq!(run(&mut tape, &[0x1A, 0, 0, 0, 255, 0]).1, GOOD);

        // A tape file dropped into the directory meanwhile is found by the
        // load, which also starts the tape from its beginning.
        std::fs::write(dir.join("b"), pattern(2, 512)).unwrap();
        assert_eq!(run(&mut tape, &[0x1B, 0, 0, 0, 1, 0]).1, GOOD);
        assert_eq!(run(&mut tape, &[0x00, 0, 0, 0, 0, 0]).1, GOOD);
        let (data, _) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(data, pattern(1, 1024)[..512]);
        assert_eq!(run(&mut tape, &space6(1, 1)).1, GOOD);
        let (data, _) = run(&mut tape, &read6(true, false, 1));
        assert_eq!(data, pattern(2, 512));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn request_sense_clears_after_reporting_and_other_commands_clear_it_too() {
        let (dir, mut tape) = directory_tape("sense", &[]);
        assert_eq!(run(&mut tape, &read6(true, false, 1)).1, CHECK_CONDITION);
        assert_eq!(sense(&mut tape).0, SK_BLANK_CHECK);
        // Reported once.
        let (key, flags, valid, info, asc, ascq) = sense(&mut tape);
        assert_eq!((key, flags, valid, info, asc, ascq), (0, 0, false, 0, 0, 0));
        // A following good command also clears pending sense.
        assert_eq!(run(&mut tape, &read6(true, false, 1)).1, CHECK_CONDITION);
        assert_eq!(run(&mut tape, &[0x00, 0, 0, 0, 0, 0]).1, GOOD);
        assert_eq!(sense(&mut tape).0, 0);
        // An opcode the drive does not have.
        assert_eq!(
            run(&mut tape, &[0x34, 0, 0, 0, 0, 0, 0, 0, 0, 0]).1,
            CHECK_CONDITION
        );
        let (key, _, _, _, asc, _) = sense(&mut tape);
        assert_eq!((key, asc), (SK_ILLEGAL_REQUEST, ASC_INVALID_OPCODE));
        // A truncated CDB is refused rather than indexed past its end.
        assert_eq!(tape.execute(&[0x08, 0x01], 0).1, CHECK_CONDITION);
        assert_eq!(tape.execute(&[], 0).1, CHECK_CONDITION);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_save_state_keeps_the_head_position_and_mode() {
        let (dir, mut tape) =
            directory_tape("state", &[("a", pattern(1, 2048)), ("b", pattern(2, 1024))]);
        set_block_len(&mut tape, 1024);
        assert_eq!(run(&mut tape, &space6(1, 1)).1, GOOD);
        let mut bytes = Vec::new();
        crate::savestate::chunk::encode(&tape, &mut bytes).unwrap();
        let mut restored: ScsiTape = crate::savestate::chunk::decode(&bytes).unwrap();
        let (data, status) = run(&mut restored, &read6(true, false, 1));
        assert_eq!(status, GOOD);
        assert_eq!(data, pattern(2, 1024));
        let _ = std::fs::remove_dir_all(dir);
    }
}
