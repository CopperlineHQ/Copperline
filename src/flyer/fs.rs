// SPDX-License-Identifier: GPL-3.0-or-later

//! The Flyer's filesystem and block commands, over [`HostDrive`]s.
//!
//! These follow the firmware's `FILESYS.C` and the command stubs in
//! `amishar.c`. A "grip" (the Flyer's lock on a file or directory, and an
//! open file's handle) is a structure in the Flyer's memory, and the Amiga
//! is handed its Flyer address. flyer.library does not treat it as opaque:
//! `NewFileRead`, which FlyerFileSystem 1.05 reads files with, reads the
//! grip's start block, length and position straight out of the SRAM window
//! and writes the new position back. So grips here are kept in SRAM too, in
//! the firmware's layout, in an area above the downloaded Flyer.bin that
//! the Amiga never allocates from; the position in SRAM is the one used.
//!
//! Clips are read-only: a file whose first block is a CLIP header cannot
//! be written, extended or deleted (`PROTECTED`), and nothing records into
//! the drives yet. Other files (the Toaster's `.i` crouton icons, project
//! data) can be created, written, renamed and deleted, and directories
//! made: they go to the host folder at once.

use super::drive::{self, HostDrive, BLOCK, TYPE_FILE, TYPE_ROOT};
use super::mailbox::{self, err, op, rd16, rd32, rd8, wr16, wr32, wr8, Slot, SRAM_CPU_BASE};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where grips live in SRAM: above Flyer.bin (`$18000`, 96 KB) and below
/// the firmware's trap and interrupt vectors (`$3F9FC`).
pub const GRIP_AREA: usize = 0x3_8000;
pub const GRIP_AREA_END: usize = 0x3_F000;
/// A grip's size in the firmware (`struct GRIP`, 66 bytes), rounded up.
const GRIP_STRIDE: usize = 68;
const GRIP_SLOTS: usize = (GRIP_AREA_END - GRIP_AREA) / GRIP_STRIDE;
/// `"GRIP"`: the mark of a live grip.
const GRIP_MAGIC: u32 = 0x4752_4950;

/// `struct GRIP` field offsets (`filesys.h`; `F_GRIP` in the library's
/// `flyerprivate.i`).
mod g {
    pub const HEADER: usize = 4;
    pub const PARENT: usize = 8;
    pub const DL_DIR: usize = 12;
    pub const DL_BLK: usize = 16;
    pub const DL_HASH: usize = 20;
    pub const DL_LINK: usize = 22;
    pub const DL_DONE: usize = 26;
    pub const DRV: usize = 27;
    pub const TYPE: usize = 28;
    pub const OPENED: usize = 29;
    pub const STARTBLK: usize = 30;
    pub const CURPOS: usize = 34;
    pub const CURPOS_EXT: usize = 38;
    pub const FILELEN: usize = 42;
    pub const FILELEN_EXT: usize = 46;
    pub const PHYSLEN: usize = 50;
    pub const PHYSLEN_EXT: usize = 54;
    pub const PROTBITS: usize = 58;
    pub const MAGIC: usize = 62;
}

/// `struct ClipAction` offsets (`flyer.h`; the firmware's `ACTION`).
mod ca {
    pub const VOLUME: usize = 0;
    pub const VID_START_FIELD: usize = 12;
    pub const RESERVED0_1: usize = 60;
    pub const START_BLK: usize = 72;
    pub const RESERVED1_0: usize = 84;
    pub const GRIP: usize = 96;
    pub const FILE_ID: usize = 100;
    pub const ACCESS: usize = 104;
}

/// `struct FlyerVolume`: the path (an SRAM offset) and the drive.
mod fv {
    pub const PATH: usize = 0;
    pub const SCSI_DRIVE: usize = 5;
}

/// `struct ClipInfo` offsets.
mod ci {
    pub const NAME: usize = 2;
    pub const COMMENT: usize = 82;
    pub const TEXT_LEN: usize = 80;
    pub const FLAGS: usize = 162;
    pub const TYPE: usize = 163;
    pub const DATE: usize = 164;
    pub const BITS: usize = 176;
    pub const FIELDS: usize = 180;
    pub const START: usize = 184;
    pub const LENGTH: usize = 188;
    pub const INDEX_BLK: usize = 192;
    pub const AUD_CHANS: usize = 196;
    pub const VIDEO_GRADE: usize = 197;
    pub const END_BLK: usize = 198;
    pub const LENGTH_EXT: usize = 202;
}

/// The firmware's `struct VidCompInfo` (`Clip.h`): the public part, then
/// the field's data range as drive blocks.
mod vci {
    pub const ALGO: usize = 0;
    pub const TOLERANCE: usize = 1;
    pub const FIR_LEVEL: usize = 2;
    pub const RND_FREQ: usize = 3;
    pub const RND_SEED: usize = 4;
    pub const DATA_START: usize = 8;
    pub const DATA_END: usize = 12;
}

/// Compression algorithms (`ALGO_` in `Clip.h`).
const ALGO_OLDD2: u8 = 0;
const ALGO_D2: u8 = 1;
const ALGO_SN: u8 = 2;

/// `struct FlyerVolInfo` offsets.
mod fvi {
    pub const IDENT: usize = 2;
    pub const VERSION: usize = 6;
    pub const LTITLE: usize = 7;
    pub const TITLE: usize = 8;
    pub const TITLE_LEN: usize = 80;
    pub const BLOCKS: usize = 88;
    pub const BLKS_FREE: usize = 92;
    pub const FLAGS: usize = 96;
    pub const DISK_OKAY: usize = 97;
    pub const BLK_SIZE: usize = 98;
    pub const DISK_DATE: usize = 100;
    pub const FRAG_BLKS: usize = 112;
    pub const LARGEST: usize = 116;
    pub const OPTIMIZED: usize = 120;
}

const ID_ROOT: u32 = 0x524F_4F54;
/// File modes (`MODE_` in `filesys.h`).
const MODE_INPUT: u8 = 1;
const MODE_OUTPUT: u8 = 2;
const MODE_UPDATE: u8 = 3;
/// Seek modes.
const OFFSET_BEGIN: u8 = 0xFF;
const OFFSET_CURRENT: u8 = 0x00;
const OFFSET_END: u8 = 0x01;
/// The AmigaDOS "not readable" protection bit, as `FileRead` checks it.
const AMIPROT_READ: u32 = 1 << 3;
/// ClipInfo flags.
const CIF_HASVIDEO: u8 = 1 << 0;
const CIF_HASAUDIO: u8 = 1 << 1;
/// `ERR_` codes used only here.
const ERR_FULL: u8 = 0x21;
const ERR_FSFAIL: u8 = 0x24;
const ERR_WRONGTYPE: u8 = 0x25;
const ERR_EXISTS: u8 = 0x2C;
const ERR_DIRNOTEMPTY: u8 = 0x32;
const ERR_OUTOFMEM: u8 = 0x2D;
const ERR_PROTECTED: u8 = 0x2A;
const ERR_READPROT: u8 = 0x2F;
const ERR_BADSTATUS: u8 = 0x41;
pub(crate) const ERR_BADVIDHDR: u8 = 0x06;
/// SCSI drives: three buses of eight targets.
pub const DRIVES: u8 = 24;
/// The Flyer's DRAM: 4 MB in 512-byte blocks, reached only by DMA.
const DRAM_BLOCKS: usize = 0x2000;

/// The drives and the filesystem state the firmware keeps.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Filesystem {
    /// The Toaster's `Flyer` folder, holding a folder per drive.
    folder: Option<PathBuf>,
    pub drives: Vec<HostDrive>,
    /// Which grip slots are in use (slot 0 is the root grip).
    grips: Vec<bool>,
    /// The DRAM, allocated on first use.
    #[serde(with = "crate::bytes_serde")]
    dram: Vec<u8>,
    /// The custom FIR coefficients (`CustomFIRcoefs`): pre and post
    /// compensation, eight coefficients and a scale each.
    custom_fir: [[u16; 9]; 2],
}

impl Filesystem {
    /// Drives served from folders in `folder` (the Toaster's `Flyer`).
    pub fn new(folder: Option<PathBuf>) -> Self {
        Self {
            folder,
            ..Self::default()
        }
    }

    pub fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    /// Flyer.bin started: no grips yet, the root grip set up, and the
    /// drives surveyed (`DriveSurvey`).
    pub fn boot(&mut self, sram: &mut [u8]) {
        self.init_grips(sram);
        self.survey();
    }

    /// No grips but the root's.
    fn init_grips(&mut self, sram: &mut [u8]) {
        self.grips = vec![false; GRIP_SLOTS];
        self.grips[0] = true;
        sram[GRIP_AREA..GRIP_AREA_END].fill(0);
        write_grip(sram, GRIP_AREA, 0, 0, 0, TYPE_ROOT);
    }

    /// Find the drives: one per `F<bus><target>` folder.
    pub fn survey(&mut self) {
        let Some(folder) = self.folder.clone() else {
            self.drives.clear();
            return;
        };
        self.drives.retain(|d| d.folder().is_dir());
        for n in 0..DRIVES {
            if n & 7 == 7 || self.drives.iter().any(|d| d.number == n) {
                continue;
            }
            if let Some(d) = HostDrive::open(&folder, n) {
                log::info!(
                    "flyer: drive {} is {} ({})",
                    drive::drive_name(n),
                    d.volume,
                    d.folder().display()
                );
                self.drives.push(d);
            }
        }
        self.drives.sort_by_key(|d| d.number);
    }

    fn drive_index(&self, n: u8) -> Option<usize> {
        self.drives.iter().position(|d| d.number == n)
    }

    /// Whether drive `n` answers.
    pub fn present(&self, n: u8) -> bool {
        self.drive_index(n).is_some()
    }

    /// A drive by number, or the error a command to it gets.
    fn drive(&self, n: u8) -> Result<usize, u8> {
        if n >= DRIVES {
            return Err(err::BADPARAM);
        }
        self.drive_index(n).ok_or(err::SELTIMEOUT)
    }

    /// The drive a storage command goes to, from its arguments.
    pub fn target(opcode: u16, sram: &[u8], slot: Slot) -> Option<u8> {
        let action_drive = |off: usize| {
            let action = slot.arg32(sram, off) as usize;
            let volume = rd32(sram, action + ca::VOLUME) as usize;
            rd8(sram, volume + fv::SCSI_DRIVE)
        };
        let volume_drive = |off: usize| {
            let volume = slot.arg32(sram, off) as usize;
            rd8(sram, volume + fv::SCSI_DRIVE)
        };
        Some(match opcode {
            op::LOCATE
            | op::FILEOPEN
            | op::CREATEDIR
            | op::DELETE
            | op::RENAME
            | op::LOCATEFLD
            | op::FLYPLAY
            | op::FLYRECORD
            | op::SRCHARGS
            | op::DOSEARCH
            | op::ADDSEQCLIP
            | op::OPENWRITEFLD
            | op::NEWCUTLIST => action_drive(4),
            op::FILEINFO | op::MAKEFILE | op::COPYCLIP | op::STRIPAUDIO => volume_drive(4),
            op::GETCOMPINFO | op::SCSIDIRECT => slot.arg8(sram, 4),
            op::CLOSEFIELD | op::ADDSUBCLIP | op::MAKESUBCLIPS => return None,
            _ => slot.arg8(sram, 5),
        })
    }

    /// Run a storage command that reached a present drive.
    pub fn run(&mut self, sram: &mut [u8], slot: Slot, opcode: u16) -> Option<u8> {
        Some(match opcode {
            op::DRIVEINFO => self.drive_info(sram, slot),
            op::LOCATE => self.locate(sram, slot),
            op::FREEGRIP => {
                let grip = slot.arg32(sram, 6);
                self.free_grip(sram, grip);
                err::OKAY
            }
            op::COPYGRIP => {
                let drive = slot.arg8(sram, 5);
                match self.copy_grip(sram, drive, slot.arg32(sram, 6)) {
                    Ok(new) => {
                        slot.set_arg32(sram, 10, new);
                        err::OKAY
                    }
                    Err(e) => e,
                }
            }
            op::PARENT => self.parent(sram, slot),
            op::DIRLIST => self.dir_list(sram, slot),
            op::FILEINFO => self.file_info(sram, slot),
            op::FILEOPEN => self.file_open(sram, slot),
            op::FILECLOSE => {
                if let Some(at) = self.grip_at(sram, slot.arg32(sram, 6)) {
                    wr8(sram, at + g::OPENED, 0);
                }
                err::OKAY
            }
            op::FILESEEK => self.file_seek(sram, slot),
            op::FILEREAD => self.file_read(sram, slot),
            op::FILEHSREAD | op::FILEHSWRITE => self.file_hs(sram, slot),
            op::FILEWRITE => self.file_write(sram, slot),
            op::FILEEXTEND => self.file_extend(sram, slot),
            op::CREATEDIR => self.create_dir(sram, slot),
            op::DELETE => self.delete(sram, slot),
            op::RENAME => self.rename(sram, slot),
            // Protection bits, dates and comments are not kept; a host
            // folder needs no defragmenting.
            op::SETBITS | op::SETDATE | op::SETCOMMENT | op::DEFRAG => err::OKAY,
            op::READ10 => self.read10(sram, slot),
            op::LOCATEFLD => self.locate_field(sram, slot),
            op::GETCOMPINFO => self.get_comp_info(sram, slot),
            op::READSIZE => {
                // ReadCapacity: last block and block length.
                slot.set_arg32(sram, 6, drive::DRIVE_BLOCKS - 1);
                slot.set_arg32(sram, 10, BLOCK as u32);
                err::OKAY
            }
            // Raw writes, recording and clip editing: clips are read-only.
            op::WRITE10
            | op::COPYDATA
            | op::RENAMEDISK
            | op::FORMAT
            | op::MAKEFILE
            | op::COPYCLIP
            | op::WRITETEST
            | op::STRIPAUDIO
            | op::OPENWRITEFLD
            | op::FLYRECORD => ERR_PROTECTED,
            _ => return None,
        })
    }

    /// FIRXCHG (`FirXchg`, `hard.c`): read a FIR coefficient set (0 the
    /// custom one, 1-4 the presets; pre- or post-compensation), or write
    /// the custom one.
    pub fn fir_exchange(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let set = slot.arg8(sram, 4);
        let read = slot.arg8(sram, 5) != 0;
        let post = slot.arg8(sram, 6) != 0;
        if read {
            let words = if set == 0 {
                self.custom_fir[usize::from(post)]
            } else if let Some(fir) = flyerclip::vtasc::fir_preset(set, post) {
                let mut w = [0u16; 9];
                w[..8].copy_from_slice(&fir.coefs);
                w[8] = fir.scale;
                w
            } else {
                // The firmware has four presets and reads past them.
                return err::BADPARAM;
            };
            for (i, w) in words.iter().enumerate() {
                slot.set_arg16(sram, 8 + 2 * i, *w);
            }
            err::OKAY
        } else if set == 0 {
            for i in 0..9 {
                self.custom_fir[usize::from(post)][i] = slot.arg16(sram, 8 + 2 * i);
            }
            err::OKAY
        } else {
            err::BADPARAM
        }
    }

    /// LOCATEFLD (`LocateField`, `acts.c`): the block of the colour-frame
    /// header ("CFRM") holding a clip's field `VidStartField`. The frames
    /// are walked by their next-frame links from the clip's first.
    fn locate_field(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let action = slot.arg32(sram, 4) as usize;
        let (n, path) = action_target(sram, action);
        let field = rd32(sram, action + ca::VID_START_FIELD);
        let d = match self.drive(n) {
            Ok(d) => d,
            Err(e) => return e,
        };
        if drive::strip_device(&path).is_empty() {
            return err::OBJNOTFOUND;
        }
        let Some(index) = self.find(d, 0, &path) else {
            return err::OBJNOTFOUND;
        };
        let drive = &self.drives[d];
        let Some(clip) = drive
            .first_block(index)
            .and_then(|b| flyerclip::header::ClipHeader::parse(&b).ok())
        else {
            return ERR_BADVIDHDR;
        };
        if clip.tail != 0 && field >= clip.fields {
            return err::BADPARAM;
        }
        let start = drive.object(index).map_or(0, |o| o.start);
        let frame = field / 4;
        // Each frame header names its frame and the distance to the next.
        let mut at = clip.data_start;
        for _ in 0..=clip.length {
            let mut b = [0u8; BLOCK];
            if drive
                .read_file(index, u64::from(at) * BLOCK as u64, &mut b)
                .ok()
                != Some(BLOCK)
            {
                return err::CMDFAILED;
            }
            let Ok(fh) = flyerclip::header::FrameHeader::parse(&b) else {
                return err::CMDFAILED;
            };
            if fh.frame_number >= frame || fh.next_frame <= 0 {
                wr32(sram, action + ca::START_BLK, start.wrapping_add(at));
                return err::OKAY;
            }
            at = at.wrapping_add(fh.next_frame as u32);
        }
        err::CMDFAILED
    }

    /// GETCOMPINFO (`GetCompInfo`, `clip.c`): how one field of a colour
    /// frame is coded, and where its data lies on the drive. Block 0 means
    /// the live input, which is not modelled yet.
    fn get_comp_info(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let n = slot.arg8(sram, 4);
        let field = usize::from(slot.arg8(sram, 5));
        let lba = slot.arg32(sram, 6);
        let info = slot.arg32(sram, 10) as usize;
        let d = match self.drive(n) {
            Ok(d) => d,
            Err(e) => return e,
        };
        if lba == 0 {
            return err::CMDFAILED;
        }
        let mut b = [0u8; BLOCK];
        if self.drives[d].read_blocks(lba, 1, &mut b).is_err() {
            return ERR_BADSTATUS;
        }
        // The firmware takes the header as it finds it.
        let fi = 224 + 72 * (field & 3);
        let vloc = u32::from(u16::from_be_bytes([b[fi], b[fi + 1]]));
        let vlen = u32::from(u16::from_be_bytes([b[fi + 2], b[fi + 3]]));
        let algo = match b[9] {
            flyerclip::header::VTASC_NEWD2 if b[fi + 12] & flyerclip::header::FIF_SUBNYQ != 0 => {
                ALGO_SN
            }
            flyerclip::header::VTASC_NEWD2 => ALGO_D2,
            _ => ALGO_OLDD2,
        };
        let data_start = lba.wrapping_add(vloc);
        wr8(sram, info + vci::ALGO, algo);
        wr8(sram, info + vci::TOLERANCE, b[fi + 4]);
        wr8(sram, info + vci::FIR_LEVEL, b[fi + 32]);
        wr8(sram, info + vci::RND_FREQ, b[fi + 6]);
        wr8(sram, info + vci::RND_SEED, b[fi + 7]);
        wr32(sram, info + vci::DATA_START, data_start);
        wr32(sram, info + vci::DATA_END, data_start.wrapping_add(vlen));
        err::OKAY
    }

    /// CPUDMA (`CpuDma_cmd`, `ops.c`): copy blocks between DRAM and SRAM.
    /// `readflg` 0 moves DRAM to SRAM, anything else SRAM to DRAM.
    pub fn cpu_dma(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let sram_blk = slot.arg32(sram, 4) as u16 as usize;
        let dram_blk = slot.arg32(sram, 8) as u16 as usize;
        let count = usize::from(slot.arg16(sram, 12));
        let to_sram = slot.arg8(sram, 14) == 0;
        let dram = self.dram();
        for i in 0..count {
            let s = ((sram_blk + i) * BLOCK) & (mailbox::SRAM_SIZE - 1);
            let d = ((dram_blk + i) % DRAM_BLOCKS) * BLOCK;
            if to_sram {
                sram[s..s + BLOCK].copy_from_slice(&dram[d..d + BLOCK]);
            } else {
                dram[d..d + BLOCK].copy_from_slice(&sram[s..s + BLOCK]);
            }
        }
        err::OKAY
    }

    fn dram(&mut self) -> &mut [u8] {
        if self.dram.is_empty() {
            self.dram = vec![0; DRAM_BLOCKS * BLOCK];
        }
        &mut self.dram
    }

    /// DRIVEINFO (`FSinfo`): the volume's name, size and state. The drive
    /// is re-read first, as the firmware re-reads its root block.
    fn drive_info(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let n = slot.arg8(sram, 5);
        let info = slot.arg32(sram, 6) as usize;
        let d = match self.drive(n) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let drive = &mut self.drives[d];
        drive.scan();
        let (used, largest) = drive.usage();
        let user_blocks = drive::DRIVE_BLOCKS - 0x1_0000;
        let free = user_blocks.saturating_sub(used);
        wr32(sram, info + fvi::IDENT, ID_ROOT);
        wr8(sram, info + fvi::VERSION, 1);
        wr8(sram, info + fvi::LTITLE, 0);
        write_text(sram, info + fvi::TITLE, fvi::TITLE_LEN, &drive.volume);
        wr32(sram, info + fvi::BLOCKS, user_blocks);
        wr8(
            sram,
            info + fvi::FLAGS,
            drive::FVIF_VIDEOREADY | drive::FVIF_AUDIOREADY,
        );
        wr8(sram, info + fvi::DISK_OKAY, 1);
        wr16(sram, info + fvi::BLK_SIZE, BLOCK as u16);
        let date = drive.object(0).map_or([0; 3], |o| o.date);
        write_date(sram, info + fvi::DISK_DATE, date);
        wr32(sram, info + fvi::BLKS_FREE, free);
        wr32(sram, info + fvi::LARGEST, largest.min(free));
        wr32(sram, info + fvi::FRAG_BLKS, free - largest.min(free));
        wr32(sram, info + fvi::OPTIMIZED, free);
        err::OKAY
    }

    /// LOCATE (`FS_Locate`): a grip on a named object, relative to the
    /// action's grip, and its block (a file's first data block, else its
    /// header).
    fn locate(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let action = slot.arg32(sram, 4) as usize;
        let (n, path) = action_target(sram, action);
        let grip = rd32(sram, action + ca::GRIP);
        match self.fs_locate(sram, n, grip, &path) {
            Ok((new, block)) => {
                wr32(sram, action + ca::GRIP, new);
                wr32(sram, action + ca::START_BLK, block);
                err::OKAY
            }
            Err(e) => {
                wr32(sram, action + ca::GRIP, 0);
                e
            }
        }
    }

    fn fs_locate(
        &mut self,
        sram: &mut [u8],
        n: u8,
        grip: u32,
        path: &str,
    ) -> Result<(u32, u32), u8> {
        let d = self.drive(n)?;
        if drive::strip_device(path).is_empty() {
            let new = self.copy_grip(sram, n, grip)?;
            let at = self.grip_at(sram, new).ok_or(err::OBJNOTFOUND)?;
            return Ok((new, rd32(sram, at + g::HEADER)));
        }
        let from = self.grip_object(sram, d, grip).ok_or(err::OBJNOTFOUND)?;
        let index = self.find(d, from, path).ok_or(err::OBJNOTFOUND)?;
        let drive = &self.drives[d];
        let obj = drive.object(index).ok_or(err::OBJNOTFOUND)?;
        let header = drive.header_block(index);
        let block = if obj.kind == TYPE_FILE {
            obj.start
        } else {
            header
        };
        let parent = drive.header_block(obj.parent);
        let kind = obj.kind;
        let new = self.alloc_grip(sram, n, header, parent, kind)?;
        Ok((new, block))
    }

    /// Look a path up, reading the folder again if it is not there (a file
    /// added on the host since the last look).
    fn find(&mut self, d: usize, from: usize, path: &str) -> Option<usize> {
        if let Some(i) = self.drives[d].lookup(from, path) {
            return Some(i);
        }
        self.drives[d].scan();
        self.drives[d].lookup(from, path)
    }

    /// The host file of a file named from the root of drive `n` (a clip
    /// to play).
    pub fn clip_file(&mut self, n: u8, path: &str) -> Result<PathBuf, u8> {
        let d = self.drive(n)?;
        if drive::strip_device(path).is_empty() {
            return Err(err::OBJNOTFOUND);
        }
        let index = self.find(d, 0, path).ok_or(err::OBJNOTFOUND)?;
        let drive = &self.drives[d];
        match drive.object(index) {
            Some(o) if o.kind == TYPE_FILE => Ok(drive.host_path(index)),
            Some(_) => Err(ERR_WRONGTYPE),
            None => Err(err::OBJNOTFOUND),
        }
    }

    /// Write a new file `name` in the root of drive `n` (the cutting
    /// room's `_tmp_` sub-clips), replacing one of that name.
    pub fn write_root_file(&mut self, n: u8, name: &str, bytes: &[u8]) -> Result<(), u8> {
        let d = self.drive(n)?;
        if !drive::valid_name(name) {
            return Err(err::BADPARAM);
        }
        if drive::blocks_for(bytes.len() as u64) > self.drives[d].usage().1 {
            return Err(ERR_FULL);
        }
        let path = self.drives[d].folder().join(name);
        std::fs::write(&path, bytes).map_err(|e| io_error(n, &e))?;
        self.drives[d].scan();
        Ok(())
    }

    /// Take a clip off drive `n`, as a destructive cut deletes its
    /// master: the host file is moved into `Trash` beside the drive
    /// folders (not a drive), so the Amiga no longer sees it but the PC
    /// keeps it.
    pub fn retire_file(&mut self, n: u8, host: &Path) -> Result<(), u8> {
        let d = self.drive(n)?;
        let folder = self.folder.clone().ok_or(err::OBJNOTFOUND)?;
        let trash = folder.join("Trash");
        std::fs::create_dir_all(&trash).map_err(|e| io_error(n, &e))?;
        let name = host.file_name().ok_or(err::BADPARAM)?;
        let mut to = trash.join(name);
        let mut k = 2;
        while to.exists() {
            to = trash.join(format!("{} ({k})", name.to_string_lossy()));
            k += 1;
        }
        std::fs::rename(host, &to).map_err(|e| io_error(n, &e))?;
        log::info!("flyer: {} moved to {}", host.display(), to.display());
        self.drives[d].scan();
        Ok(())
    }

    /// PARENT: a grip on the parent directory and its header block.
    fn parent(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let n = slot.arg8(sram, 5);
        let grip = slot.arg32(sram, 6);
        let d = match self.drive(n) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let Some(index) = self.grip_object(sram, d, grip) else {
            return err::OBJNOTFOUND;
        };
        let drive = &self.drives[d];
        let obj = drive.object(index).expect("grip_object checks");
        if obj.kind == TYPE_ROOT {
            return err::OBJNOTFOUND;
        }
        let mom = obj.parent;
        let (header, kind) = (
            drive.header_block(mom),
            drive.object(mom).map_or(TYPE_ROOT, |o| o.kind),
        );
        let grandma = drive.header_block(drive.object(mom).map_or(0, |o| o.parent));
        match self.alloc_grip(sram, n, header, grandma, kind) {
            Ok(new) => {
                slot.set_arg32(sram, 10, new);
                slot.set_arg32(sram, 14, header);
                err::OKAY
            }
            Err(e) => e,
        }
    }

    /// DIRLIST (`DirList`): with `first` set, the directory (or file)
    /// itself; then its children one per call, until `EXHAUSTED`. The
    /// position is kept in the grip, as the firmware keeps it.
    fn dir_list(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let n = slot.arg8(sram, 5);
        let grip = slot.arg32(sram, 6);
        let info = slot.arg32(sram, 10) as usize;
        let first = slot.arg8(sram, 14) != 0;
        let extra = slot.arg8(sram, 15) == 0;
        let d = match self.drive(n) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let Some(at) = self.grip_at(sram, grip) else {
            return err::OBJNOTFOUND;
        };
        let Some(dir) = self.grip_object(sram, d, grip) else {
            return err::OBJNOTFOUND;
        };
        if first {
            // A listing starts from the folder as it is now: clips the
            // PC put there since (the converter) show up.
            self.drives[d].scan();
            wr8(sram, at + g::DL_DONE, 0);
            wr32(sram, at + g::DL_DIR, rd32(sram, at + g::HEADER));
            wr16(sram, at + g::DL_HASH, 0xFFFF);
            wr32(sram, at + g::DL_LINK, 0);
            self.clip_info(sram, d, dir, info, extra);
            return err::OKAY;
        }
        if rd8(sram, at + g::DL_DONE) != 0 {
            return err::EXHAUSTED;
        }
        let next = rd16(sram, at + g::DL_HASH).wrapping_add(1);
        wr16(sram, at + g::DL_HASH, next);
        let drive = &self.drives[d];
        let Some(&child) = drive
            .object(dir)
            .and_then(|o| o.children.get(usize::from(next)))
        else {
            wr8(sram, at + g::DL_DONE, 1);
            return err::EXHAUSTED;
        };
        wr32(sram, at + g::DL_BLK, drive.header_block(child));
        self.clip_info(sram, d, child, info, extra);
        err::OKAY
    }

    /// FILEINFO (`FileInfo`): a ClipInfo for a path from the root.
    fn file_info(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let volume = slot.arg32(sram, 4) as usize;
        let info = slot.arg32(sram, 8) as usize;
        let n = rd8(sram, volume + fv::SCSI_DRIVE);
        let path = read_cstr(sram, rd32(sram, volume + fv::PATH) as usize);
        let d = match self.drive(n) {
            Ok(d) => d,
            Err(e) => return e,
        };
        // LocObj looks for a child named "" when given no name.
        if drive::strip_device(&path).is_empty() {
            return err::OBJNOTFOUND;
        }
        let Some(index) = self.find(d, 0, &path) else {
            return err::OBJNOTFOUND;
        };
        self.clip_info(sram, d, index, info, true);
        err::OKAY
    }

    /// Fill a ClipInfo (`Hdr2ClipInfo`, `GetClipInfo`; `DirList` for a
    /// directory). With `extra`, a file's first block is read for the
    /// clip's fields, channels, grade and index; a file that is not a
    /// clip has no fields and no flags.
    fn clip_info(&self, sram: &mut [u8], d: usize, index: usize, info: usize, extra: bool) {
        let drive = &self.drives[d];
        let Some(obj) = drive.object(index) else {
            return;
        };
        let name = if obj.kind == TYPE_ROOT {
            drive.volume.as_str()
        } else {
            obj.name.as_str()
        };
        write_text(sram, info + ci::NAME, ci::TEXT_LEN, name);
        let comment = if obj.kind == TYPE_ROOT {
            "Flyer drive"
        } else {
            ""
        };
        write_text(sram, info + ci::COMMENT, ci::TEXT_LEN, comment);
        wr8(sram, info + ci::TYPE, obj.kind);
        write_date(sram, info + ci::DATE, obj.date);
        wr32(sram, info + ci::BITS, 0);
        if obj.kind != TYPE_FILE {
            for off in [
                ci::FIELDS,
                ci::START,
                ci::LENGTH,
                ci::INDEX_BLK,
                ci::END_BLK,
            ] {
                wr32(sram, info + off, 0);
            }
            return;
        }
        wr32(sram, info + ci::START, obj.start);
        wr32(sram, info + ci::LENGTH, obj.bytes as u32);
        wr32(sram, info + ci::LENGTH_EXT, (obj.bytes >> 32) as u32);
        wr32(
            sram,
            info + ci::END_BLK,
            obj.start.wrapping_add(drive::blocks_for(obj.bytes)),
        );
        if !extra {
            return;
        }
        wr8(sram, info + ci::FLAGS, 0);
        wr32(sram, info + ci::INDEX_BLK, 0);
        wr32(sram, info + ci::FIELDS, 0);
        let header = drive
            .first_block(index)
            .and_then(|b| flyerclip::header::ClipHeader::parse(&b).ok());
        if let Some(h) = header {
            let mut flags = 0;
            if h.vid_flag != 0 {
                flags |= CIF_HASVIDEO;
            }
            if h.audio_chans != 0 {
                flags |= CIF_HASAUDIO;
            }
            wr8(sram, info + ci::FLAGS, flags);
            wr8(sram, info + ci::AUD_CHANS, h.audio_chans);
            wr32(sram, info + ci::FIELDS, h.fields);
            let index_blk = if h.tail == 0 {
                0
            } else {
                obj.start.wrapping_add(h.tail)
            };
            wr32(sram, info + ci::INDEX_BLK, index_blk);
            wr8(sram, info + ci::VIDEO_GRADE, h.vid_grade);
        }
    }

    /// FILEOPEN (`FileOpen`): open a file for reading (MODE_INPUT), make
    /// a new empty one in its place (MODE_OUTPUT), or open one to change,
    /// made if missing (MODE_UPDATE).
    fn file_open(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let action = slot.arg32(sram, 4) as usize;
        // Markers the firmware's stub leaves in the action.
        wr32(sram, action + ca::RESERVED0_1, 0x2222_2222);
        let r1 = rd32(sram, action + ca::RESERVED1_0);
        wr32(sram, action + ca::RESERVED1_0, r1 | 0x100);
        let (n, path) = action_target(sram, action);
        let grip = rd32(sram, action + ca::GRIP);
        let mode = rd8(sram, action + ca::ACCESS);
        let result = match mode {
            MODE_INPUT => self.open_existing(sram, n, grip, &path),
            MODE_UPDATE => match self.open_existing(sram, n, grip, &path) {
                Err(err::OBJNOTFOUND) => self.create_file(sram, n, grip, &path),
                r => r,
            },
            MODE_OUTPUT => self.create_file(sram, n, grip, &path),
            _ => Err(err::BADPARAM),
        };
        match result {
            Ok((id, block)) => {
                wr32(sram, action + ca::FILE_ID, id);
                wr32(sram, action + ca::START_BLK, block);
                err::OKAY
            }
            Err(e) => {
                wr32(sram, action + ca::FILE_ID, 0);
                e
            }
        }
    }

    fn open_existing(
        &mut self,
        sram: &mut [u8],
        n: u8,
        grip: u32,
        path: &str,
    ) -> Result<(u32, u32), u8> {
        let (new, block) = self.fs_locate(sram, n, grip, path)?;
        let at = self.grip_at(sram, new).expect("just allocated");
        if rd8(sram, at + g::TYPE) != TYPE_FILE {
            self.free_grip(sram, new);
            return Err(ERR_WRONGTYPE);
        }
        let d = self.drive(n)?;
        let index = self.grip_object(sram, d, new).ok_or(err::OBJNOTFOUND)?;
        let bytes = self.drives[d].object(index).map_or(0, |o| o.bytes);
        let phys = u64::from(drive::blocks_for(bytes)) * BLOCK as u64;
        wr8(sram, at + g::OPENED, 1);
        wr32(sram, at + g::STARTBLK, block);
        wr32(sram, at + g::CURPOS, 0);
        wr32(sram, at + g::CURPOS_EXT, 0);
        wr32(sram, at + g::FILELEN, bytes as u32);
        wr32(sram, at + g::FILELEN_EXT, (bytes >> 32) as u32);
        wr32(sram, at + g::PHYSLEN, phys as u32);
        wr32(sram, at + g::PHYSLEN_EXT, (phys >> 32) as u32);
        wr32(sram, at + g::PROTBITS, 0);
        Ok((new, block))
    }

    /// An open file's grip, or `OBJNOTFOUND` (`FileRead`'s check).
    fn open_file(&self, sram: &[u8], id: u32) -> Result<usize, u8> {
        let at = self.grip_at(sram, id).ok_or(err::OBJNOTFOUND)?;
        if rd8(sram, at + g::OPENED) == 0 {
            return Err(err::OBJNOTFOUND);
        }
        Ok(at)
    }

    /// FILESEEK (`FileSeek`): move an open file's position; the old one
    /// is returned.
    fn file_seek(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let mode = slot.arg8(sram, 6);
        let at = match self.open_file(sram, slot.arg32(sram, 8)) {
            Ok(at) => at,
            Err(e) => return e,
        };
        let new = (u64::from(slot.arg32(sram, 12)) << 32) | u64::from(slot.arg32(sram, 16));
        let cur = read64(sram, at + g::CURPOS_EXT, at + g::CURPOS);
        let len = read64(sram, at + g::FILELEN_EXT, at + g::FILELEN);
        slot.set_arg32(sram, 20, (cur >> 32) as u32);
        slot.set_arg32(sram, 24, cur as u32);
        let pos = match mode {
            OFFSET_BEGIN => new,
            OFFSET_CURRENT => cur.wrapping_add(new),
            OFFSET_END => len.wrapping_add(new),
            _ => return err::BADPARAM,
        };
        if pos > len {
            return err::BADPARAM;
        }
        write64(sram, at + g::CURPOS_EXT, at + g::CURPOS, pos);
        err::OKAY
    }

    /// FILEREAD (`FileRead`): bytes from an open file into SRAM, up to the
    /// end of the file.
    fn file_read(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let n = slot.arg8(sram, 5);
        let length = slot.arg32(sram, 10) as usize;
        let buffer = slot.arg32(sram, 14) as usize;
        let at = match self.open_file(sram, slot.arg32(sram, 6)) {
            Ok(at) => at,
            Err(e) => return e,
        };
        if rd32(sram, at + g::PROTBITS) & AMIPROT_READ != 0 {
            return ERR_READPROT;
        }
        let d = match self.drive(n) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let Some(index) = self.grip_object(sram, d, self.grip_handle(at)) else {
            return err::OBJNOTFOUND;
        };
        let cur = read64(sram, at + g::CURPOS_EXT, at + g::CURPOS);
        let room = mailbox::SRAM_SIZE.saturating_sub(buffer);
        let mut data = vec![0u8; length.min(room)];
        let got = match self.drives[d].read_file(index, cur, &mut data) {
            Ok(got) => got,
            Err(e) => {
                log::warn!("flyer: reading {}: {e}", drive::drive_name(n));
                return ERR_BADSTATUS;
            }
        };
        sram[buffer..buffer + got].copy_from_slice(&data[..got]);
        write64(sram, at + g::CURPOS_EXT, at + g::CURPOS, cur + got as u64);
        slot.set_arg32(sram, 18, got as u32);
        err::OKAY
    }

    /// FILEHSREAD/FILEHSWRITE (`FileHSWrite`, both opcodes): whole blocks
    /// by drive block number between the drive and SRAM, for an open file.
    /// `WFLAG` (+4) set writes.
    fn file_hs(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let write = slot.arg8(sram, 4) != 0;
        let n = slot.arg8(sram, 5);
        let start = slot.arg32(sram, 10);
        let count = slot.arg32(sram, 14) as usize;
        let buffer = slot.arg32(sram, 18) as usize;
        let at = match self.open_file(sram, slot.arg32(sram, 6)) {
            Ok(at) => at,
            Err(e) => return e,
        };
        let d = match self.drive(n) {
            Ok(d) => d,
            Err(e) => return e,
        };
        if count == 0 {
            return err::OKAY;
        }
        if count > DRAM_BLOCKS || buffer.saturating_add(count * BLOCK) > mailbox::SRAM_SIZE {
            return ERR_OUTOFMEM;
        }
        if write {
            let index = match self.writable(sram, d, at) {
                Ok(i) => i,
                Err(e) => return e,
            };
            let data = sram[buffer..buffer + count * BLOCK].to_vec();
            return match self.drives[d].write_blocks(index, start, &data) {
                Ok(()) => err::OKAY,
                Err(e) => io_error(n, &e),
            };
        }
        match self.drives[d].read_blocks(start, count, &mut sram[buffer..buffer + count * BLOCK]) {
            Ok(()) => err::OKAY,
            Err(e) => {
                log::warn!("flyer: reading {}: {e}", drive::drive_name(n));
                ERR_BADSTATUS
            }
        }
    }

    /// MODE_OUTPUT: a new, empty file named by `path` from `grip` (an old
    /// one of that name is emptied), open, with a grip on it.
    fn create_file(
        &mut self,
        sram: &mut [u8],
        n: u8,
        grip: u32,
        path: &str,
    ) -> Result<(u32, u32), u8> {
        let d = self.drive(n)?;
        let (dir, name) = self.parent_and_name(sram, d, grip, path)?;
        if let Some(old) = self.drives[d].child(dir, &name) {
            if self.drives[d]
                .object(old)
                .is_some_and(|o| o.kind != TYPE_FILE)
            {
                return Err(ERR_WRONGTYPE);
            }
            if self.drives[d].is_clip(old) {
                return Err(ERR_PROTECTED);
            }
        }
        let index = self.drives[d]
            .create_file(dir, &name)
            .map_err(|e| io_error(n, &e))?;
        let drive = &self.drives[d];
        let (header, parent) = (drive.header_block(index), drive.header_block(dir));
        let start = drive.object(index).map_or(0, |o| o.start);
        let new = self.alloc_grip(sram, n, header, parent, TYPE_FILE)?;
        let at = self.grip_at(sram, new).expect("just allocated");
        wr8(sram, at + g::OPENED, 1);
        wr32(sram, at + g::STARTBLK, start);
        set_lengths(sram, at, 0);
        log::info!(
            "flyer: {} made {}",
            drive::drive_name(n),
            self.drives[d].host_path(index).display()
        );
        Ok((new, start))
    }

    /// The directory a path names a new object in, from `grip`, and the
    /// object's name.
    fn parent_and_name(
        &mut self,
        sram: &[u8],
        d: usize,
        grip: u32,
        path: &str,
    ) -> Result<(usize, String), u8> {
        let from = self.grip_object(sram, d, grip).ok_or(err::OBJNOTFOUND)?;
        let rel = drive::strip_device(path);
        let (dir_part, name) = match rel.rfind('/') {
            Some(i) => (&rel[..=i], &rel[i + 1..]),
            None => ("", rel),
        };
        if name.is_empty() {
            return Err(err::BADPARAM);
        }
        let dir = if dir_part.is_empty() {
            from
        } else {
            self.find(d, from, dir_part).ok_or(err::OBJNOTFOUND)?
        };
        match self.drives[d].object(dir) {
            Some(o) if o.kind == TYPE_FILE => Err(ERR_WRONGTYPE),
            Some(_) => Ok((dir, name.to_string())),
            None => Err(err::OBJNOTFOUND),
        }
    }

    /// The file an open grip holds, if it may be written (not a clip).
    fn writable(&self, sram: &[u8], d: usize, at: usize) -> Result<usize, u8> {
        let index = self
            .grip_object(sram, d, self.grip_handle(at))
            .ok_or(err::OBJNOTFOUND)?;
        if self.drives[d].is_clip(index) {
            return Err(ERR_PROTECTED);
        }
        Ok(index)
    }

    /// FILEWRITE (`FileWrite`): bytes from SRAM into an open file at its
    /// position, growing it.
    fn file_write(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let n = slot.arg8(sram, 5);
        let length = slot.arg32(sram, 10) as usize;
        let buffer = slot.arg32(sram, 14) as usize;
        let result = (|| {
            let at = self.open_file(sram, slot.arg32(sram, 6))?;
            let d = self.drive(n)?;
            let index = self.writable(sram, d, at)?;
            let cur = read64(sram, at + g::CURPOS_EXT, at + g::CURPOS);
            let len = length.min(mailbox::SRAM_SIZE.saturating_sub(buffer));
            let data = sram[buffer..buffer + len].to_vec();
            self.drives[d]
                .write_file(index, cur, &data)
                .map_err(|e| io_error(n, &e))?;
            let pos = cur + len as u64;
            write64(sram, at + g::CURPOS_EXT, at + g::CURPOS, pos);
            let bytes = self.drives[d].object(index).map_or(pos, |o| o.bytes);
            set_lengths(sram, at, bytes);
            Ok(len)
        })();
        match result {
            Ok(len) => {
                slot.set_arg32(sram, 18, len as u32);
                err::OKAY
            }
            Err(e) => e,
        }
    }

    /// FILEEXTEND (`FileExtend`): make an open file longer by `+10` bytes;
    /// the library then writes its blocks with FILEHSWRITE.
    fn file_extend(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let n = slot.arg8(sram, 5);
        let more = u64::from(slot.arg32(sram, 10));
        let result = (|| {
            let at = self.open_file(sram, slot.arg32(sram, 6))?;
            let d = self.drive(n)?;
            let index = self.writable(sram, d, at)?;
            let len = read64(sram, at + g::FILELEN_EXT, at + g::FILELEN) + more;
            self.drives[d]
                .set_len(index, len)
                .map_err(|e| io_error(n, &e))?;
            set_lengths(sram, at, len);
            Ok(())
        })();
        result.err().unwrap_or(err::OKAY)
    }

    /// The object a ClipAction names from its grip.
    fn action_object(&mut self, sram: &[u8], action: usize) -> Result<(u8, usize, usize), u8> {
        let (n, path) = action_target(sram, action);
        let d = self.drive(n)?;
        let from = self
            .grip_object(sram, d, rd32(sram, action + ca::GRIP))
            .ok_or(err::OBJNOTFOUND)?;
        if drive::strip_device(&path).is_empty() {
            return Err(err::OBJNOTFOUND);
        }
        let index = self.find(d, from, &path).ok_or(err::OBJNOTFOUND)?;
        Ok((n, d, index))
    }

    /// CREATEDIR (`CreateDir`): a new directory; the action's grip becomes
    /// a grip on it.
    fn create_dir(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let action = slot.arg32(sram, 4) as usize;
        let (n, path) = action_target(sram, action);
        let result = (|| {
            let d = self.drive(n)?;
            let (dir, name) =
                self.parent_and_name(sram, d, rd32(sram, action + ca::GRIP), &path)?;
            let index = self.drives[d]
                .create_dir(dir, &name)
                .map_err(|e| io_error(n, &e))?;
            let drive = &self.drives[d];
            let (header, parent) = (drive.header_block(index), drive.header_block(dir));
            self.alloc_grip(sram, n, header, parent, drive::TYPE_DIR)
        })();
        match result {
            Ok(grip) => {
                wr32(sram, action + ca::GRIP, grip);
                err::OKAY
            }
            Err(e) => {
                wr32(sram, action + ca::GRIP, 0);
                e
            }
        }
    }

    /// DELETE (`Delete`): a file or an empty directory; not a clip.
    fn delete(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let action = slot.arg32(sram, 4) as usize;
        let result = (|| {
            let (n, d, index) = self.action_object(sram, action)?;
            let drive = &self.drives[d];
            let obj = drive.object(index).ok_or(err::OBJNOTFOUND)?;
            match obj.kind {
                TYPE_ROOT => return Err(err::BADPARAM),
                TYPE_FILE if drive.is_clip(index) => return Err(ERR_PROTECTED),
                drive::TYPE_DIR if obj.children.iter().any(|&c| drive.object(c).is_some()) => {
                    return Err(ERR_DIRNOTEMPTY)
                }
                _ => {}
            }
            self.drives[d].delete(index).map_err(|e| io_error(n, &e))
        })();
        result.err().unwrap_or(err::OKAY)
    }

    /// RENAME (`Rename`): move an object to a new name (`+12`) relative to
    /// another grip (`+8`).
    fn rename(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let action = slot.arg32(sram, 4) as usize;
        let new_grip = slot.arg32(sram, 8);
        let new_name = read_cstr(sram, slot.arg32(sram, 12) as usize);
        let result = (|| {
            let (n, d, index) = self.action_object(sram, action)?;
            let (dir, name) = self.parent_and_name(sram, d, new_grip, &new_name)?;
            self.drives[d]
                .rename(index, dir, &name)
                .map_err(|e| io_error(n, &e))
        })();
        result.err().unwrap_or(err::OKAY)
    }

    /// READ10: blocks from a drive into DRAM (`+12` is a DRAM block).
    fn read10(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let n = slot.arg8(sram, 5);
        let lba = slot.arg32(sram, 6);
        let count = usize::from(slot.arg16(sram, 10));
        let dram_blk = slot.arg32(sram, 12) as usize;
        let d = match self.drive(n) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let mut data = vec![0u8; count * BLOCK];
        if let Err(e) = self.drives[d].read_blocks(lba, count, &mut data) {
            log::warn!("flyer: reading {}: {e}", drive::drive_name(n));
            return ERR_BADSTATUS;
        }
        let dram = self.dram();
        for (i, block) in data.chunks_exact(BLOCK).enumerate() {
            let at = ((dram_blk + i) % DRAM_BLOCKS) * BLOCK;
            dram[at..at + BLOCK].copy_from_slice(block);
        }
        err::OKAY
    }

    /// FINDDRV (`FindDrives`): the targets answering on one bus, with
    /// their SCSI version and inquiry length.
    pub fn find_drives(&mut self, sram: &mut [u8], slot: Slot) -> u8 {
        let bus = slot.arg8(sram, 5) >> 3;
        if bus >= 3 {
            return err::BADPARAM;
        }
        self.survey();
        let mut bits = 0u8;
        for id in 0..8u8 {
            let present = id < 7 && self.present(bus * 8 + id);
            if present {
                bits |= 1 << id;
            }
            // SCSI-2, 31 more bytes of inquiry data.
            slot.set_arg8(sram, 8 + usize::from(id), if present { 2 } else { 0 });
            slot.set_arg8(sram, 16 + usize::from(id), if present { 31 } else { 0 });
        }
        slot.set_arg8(sram, 6, bits);
        err::OKAY
    }

    /// The SRAM offset of the grip whose Flyer address is `handle`; 0 is
    /// the root grip. `None` for anything that is not a live grip.
    fn grip_at(&self, sram: &[u8], handle: u32) -> Option<usize> {
        if handle == 0 {
            return Some(GRIP_AREA);
        }
        let off = handle.checked_sub(SRAM_CPU_BASE)? as usize;
        if !(GRIP_AREA..GRIP_AREA_END).contains(&off)
            || !(off - GRIP_AREA).is_multiple_of(GRIP_STRIDE)
        {
            return None;
        }
        let slot = (off - GRIP_AREA) / GRIP_STRIDE;
        (self.grips.get(slot) == Some(&true) && rd32(sram, off + g::MAGIC) == GRIP_MAGIC)
            .then_some(off)
    }

    fn grip_handle(&self, at: usize) -> u32 {
        if at == GRIP_AREA {
            0
        } else {
            SRAM_CPU_BASE + at as u32
        }
    }

    /// The object a grip holds, on drive index `d`.
    fn grip_object(&self, sram: &[u8], d: usize, handle: u32) -> Option<usize> {
        let at = self.grip_at(sram, handle)?;
        self.drives[d].object_at_header(rd32(sram, at + g::HEADER))
    }

    fn alloc_grip(
        &mut self,
        sram: &mut [u8],
        drive: u8,
        header: u32,
        parent: u32,
        kind: u8,
    ) -> Result<u32, u8> {
        if self.grips.is_empty() {
            self.init_grips(sram);
        }
        let slot = self
            .grips
            .iter()
            .position(|used| !used)
            .ok_or(ERR_OUTOFMEM)?;
        self.grips[slot] = true;
        let at = GRIP_AREA + slot * GRIP_STRIDE;
        write_grip(sram, at, drive, header, parent, kind);
        Ok(SRAM_CPU_BASE + at as u32)
    }

    /// COPYGRIP (`CopyGrip`): a new grip on the same object (the root
    /// for 0).
    fn copy_grip(&mut self, sram: &mut [u8], drive: u8, handle: u32) -> Result<u32, u8> {
        let at = self.grip_at(sram, handle).ok_or(err::OBJNOTFOUND)?;
        let (header, parent, kind) = (
            rd32(sram, at + g::HEADER),
            rd32(sram, at + g::PARENT),
            rd8(sram, at + g::TYPE),
        );
        self.alloc_grip(sram, drive, header, parent, kind)
    }

    /// FREEGRIP (`FreeGrip`): the root grip and anything that is not a
    /// grip are left alone.
    fn free_grip(&mut self, sram: &mut [u8], handle: u32) {
        if handle == 0 {
            return;
        }
        if let Some(at) = self.grip_at(sram, handle) {
            wr32(sram, at + g::MAGIC, 0);
            self.grips[(at - GRIP_AREA) / GRIP_STRIDE] = false;
        }
    }

    /// Grips in use, the root's included (for tests and the log).
    pub fn grips_in_use(&self) -> usize {
        self.grips.iter().filter(|&&used| used).count()
    }
}

/// An open file's length in its grip, and the blocks that holds.
fn set_lengths(sram: &mut [u8], at: usize, bytes: u64) {
    let phys = u64::from(drive::blocks_for(bytes)) * BLOCK as u64;
    write64(sram, at + g::FILELEN_EXT, at + g::FILELEN, bytes);
    write64(sram, at + g::PHYSLEN_EXT, at + g::PHYSLEN, phys);
}

/// The Flyer error for a host file operation that failed.
fn io_error(n: u8, e: &std::io::Error) -> u8 {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::NotFound => err::OBJNOTFOUND,
        ErrorKind::AlreadyExists => ERR_EXISTS,
        ErrorKind::InvalidInput => err::BADPARAM,
        ErrorKind::StorageFull => ERR_FULL,
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => ERR_PROTECTED,
        _ => {
            log::warn!("flyer: writing {}: {e}", drive::drive_name(n));
            ERR_FSFAIL
        }
    }
}

/// Write a fresh grip (`AllocGrip` plus the object fields).
fn write_grip(sram: &mut [u8], at: usize, drive: u8, header: u32, parent: u32, kind: u8) {
    sram[at..at + GRIP_STRIDE].fill(0);
    wr32(sram, at + g::HEADER, header);
    wr32(sram, at + g::PARENT, parent);
    wr16(sram, at + g::DL_HASH, 0xFFFF);
    wr8(sram, at + g::DRV, drive);
    wr8(sram, at + g::TYPE, kind);
    wr32(sram, at + g::MAGIC, GRIP_MAGIC);
}

/// The drive and path a ClipAction names through its FlyerVolume.
fn action_target(sram: &[u8], action: usize) -> (u8, String) {
    let volume = rd32(sram, action + ca::VOLUME) as usize;
    let drive = rd8(sram, volume + fv::SCSI_DRIVE);
    let path = read_cstr(sram, rd32(sram, volume + fv::PATH) as usize);
    (drive, path)
}

/// A NUL-terminated Latin-1 string in SRAM (at most 255 characters).
pub fn read_cstr(sram: &[u8], at: usize) -> String {
    (0..255)
        .map(|i| rd8(sram, at + i))
        .take_while(|&b| b != 0)
        .map(char::from)
        .collect()
}

/// A string into a fixed field, NUL-terminated (`CopyString`).
fn write_text(sram: &mut [u8], at: usize, len: usize, text: &str) {
    let bytes: Vec<u8> = text
        .chars()
        .map(|c| u8::try_from(c).unwrap_or(b'?'))
        .collect();
    let n = bytes.len().min(len - 1);
    for (i, &b) in bytes[..n].iter().enumerate() {
        wr8(sram, at + i, b);
    }
    wr8(sram, at + n, 0);
}

fn write_date(sram: &mut [u8], at: usize, date: [u32; 3]) {
    for (i, v) in date.iter().enumerate() {
        wr32(sram, at + 4 * i, *v);
    }
}

fn read64(sram: &[u8], hi: usize, lo: usize) -> u64 {
    (u64::from(rd32(sram, hi)) << 32) | u64::from(rd32(sram, lo))
}

fn write64(sram: &mut [u8], hi: usize, lo: usize, value: u64) {
    wr32(sram, hi, (value >> 32) as u32);
    wr32(sram, lo, value as u32);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flyer::mailbox::{STAT_DONE, STAT_NEW};
    use crate::flyer::ops::Firmware;

    // Where the tests lay out their structures in the library's pool.
    const VOLUME: usize = 0x1000;
    const PATH: usize = 0x1100;
    const ACTION: usize = 0x1200;
    const INFO: usize = 0x1400;
    const BUFFER: usize = 0x2000;

    struct Rig {
        fw: Firmware,
        sram: Vec<u8>,
        _dir: tempfile::TempDir,
    }

    /// A Flyer whose FA0 holds a short clip, a text file and a directory,
    /// with Flyer.bin running.
    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let fa0 = dir.path().join("Flyer/FA0");
        std::fs::create_dir_all(fa0.join("Stills")).unwrap();
        let header = flyerclip::header::ClipHeader {
            length: 3,
            fields: 8,
            tail: 2,
            audio_chans: 2,
            vid_grade: 1,
            ..Default::default()
        };
        let mut clip = header.to_bytes().to_vec();
        clip.extend((0..1024u32).map(|i| i as u8));
        std::fs::write(fa0.join("Clip"), &clip).unwrap();
        std::fs::write(fa0.join("Notes"), b"0123456789").unwrap();
        let mut fw = Firmware::new().with_drives(Some(dir.path().join("Flyer")));
        let mut sram = vec![0u8; mailbox::SRAM_SIZE];
        send(&mut fw, &mut sram, op::FIRMWARE, &[]);
        Rig {
            fw,
            sram,
            _dir: dir,
        }
    }

    /// Issue a command in slot 0 and free it again; returns the error and
    /// the slot's bytes as they were when done.
    fn send(
        fw: &mut Firmware,
        sram: &mut [u8],
        opcode: u16,
        args: &[(usize, u32)],
    ) -> (u8, Vec<u8>) {
        let slot = Slot(0);
        let base = slot.base();
        slot.set_opcode_word(sram, 1);
        sram[base + 2..base + 32].fill(0);
        for &(off, value) in args {
            slot.set_arg32(sram, off, value);
        }
        slot.set_opcode_word(sram, opcode | STAT_NEW);
        fw.after_write(sram, base, 2);
        assert_eq!(slot.status(sram), STAT_DONE);
        let result = (slot.error(sram), sram[base..base + 32].to_vec());
        slot.set_opcode_word(sram, 0);
        fw.after_write(sram, base, 2);
        result
    }

    impl Rig {
        fn send(&mut self, opcode: u16, args: &[(usize, u32)]) -> (u8, Vec<u8>) {
            send(&mut self.fw, &mut self.sram, opcode, args)
        }

        /// Point the FlyerVolume and ClipAction at `path` on `drive`.
        fn action(&mut self, drive: u8, path: &str, grip: u32, access: u8) {
            let s = &mut self.sram;
            s[PATH..PATH + 64].fill(0);
            s[PATH..PATH + path.len()].copy_from_slice(path.as_bytes());
            wr32(s, VOLUME + fv::PATH, PATH as u32);
            wr8(s, VOLUME + fv::SCSI_DRIVE, drive);
            s[ACTION..ACTION + 130].fill(0);
            wr32(s, ACTION + ca::VOLUME, VOLUME as u32);
            wr32(s, ACTION + ca::GRIP, grip);
            wr8(s, ACTION + ca::ACCESS, access);
        }

        fn locate(&mut self, path: &str, grip: u32) -> (u8, u32, u32) {
            self.action(0, path, grip, 0);
            let (e, _) = self.send(op::LOCATE, &[(4, ACTION as u32)]);
            (
                e,
                rd32(&self.sram, ACTION + ca::GRIP),
                rd32(&self.sram, ACTION + ca::START_BLK),
            )
        }

        fn info_name(&self) -> String {
            read_cstr(&self.sram, INFO + ci::NAME)
        }
    }

    fn arg32(slot: &[u8], off: usize) -> u32 {
        u32::from_be_bytes(slot[off..off + 4].try_into().unwrap())
    }

    #[test]
    fn drives_answer_only_where_a_folder_is() {
        let mut r = rig();
        let (e, slot) = r.send(op::FINDDRV, &[(4, 0)]);
        assert_eq!(e, err::OKAY);
        assert_eq!(slot[6], 1, "target 0 on bus A");
        assert_eq!((slot[8], slot[16]), (2, 31));
        let (_, slot) = r.send(op::FINDDRV, &[(4, 0x0008_0000)]);
        assert_eq!(slot[6], 0);
        // FB0 does not answer; drive 30 is not a drive.
        assert_eq!(
            r.send(op::DRIVEINFO, &[(4, 0x0008_0000), (6, INFO as u32)])
                .0,
            err::SELTIMEOUT
        );
        assert_eq!(
            r.send(op::DRIVEINFO, &[(4, 0x001E_0000), (6, INFO as u32)])
                .0,
            err::BADPARAM
        );
    }

    #[test]
    fn driveinfo_describes_the_volume() {
        let mut r = rig();
        let (e, _) = r.send(op::DRIVEINFO, &[(4, 0), (6, INFO as u32)]);
        assert_eq!(e, err::OKAY);
        let s = &r.sram;
        assert_eq!(rd32(s, INFO + fvi::IDENT), ID_ROOT);
        assert_eq!(read_cstr(s, INFO + fvi::TITLE), "FlyerA0");
        // Video and audio ready; not write-protected.
        assert_eq!(rd8(s, INFO + fvi::FLAGS), 3);
        assert_eq!(rd16(s, INFO + fvi::BLK_SIZE), 512);
        let blocks = rd32(s, INFO + fvi::BLOCKS);
        let free = rd32(s, INFO + fvi::BLKS_FREE);
        assert!(free < blocks && blocks - free == 3 + 1, "{blocks} {free}");
        assert_eq!(
            rd32(s, INFO + fvi::FRAG_BLKS) + rd32(s, INFO + fvi::LARGEST),
            free
        );
    }

    #[test]
    fn locate_hands_out_grips_in_sram_in_the_firmware_layout() {
        let mut r = rig();
        let (e, grip, block) = r.locate("FlyerA0:Clip", 0);
        assert_eq!(e, err::OKAY);
        let at = (grip - SRAM_CPU_BASE) as usize;
        assert!((GRIP_AREA..GRIP_AREA_END).contains(&at));
        assert_eq!(rd32(&r.sram, at + g::MAGIC), GRIP_MAGIC);
        assert_eq!(rd8(&r.sram, at + g::TYPE), TYPE_FILE);
        assert!(
            block >= 0x1_0000,
            "a file's LOCATE gives its first data block"
        );
        // A directory gives its header block; relative and parent paths.
        let (e, stills, header) = r.locate("Stills", 0);
        assert_eq!(e, err::OKAY);
        assert_eq!(
            rd32(&r.sram, (stills - SRAM_CPU_BASE) as usize + g::HEADER),
            header
        );
        let (e, again, _) = r.locate("/Clip", stills);
        assert_eq!(e, err::OKAY);
        assert_ne!(again, grip);
        assert_eq!(r.locate("Missing", 0).0, err::OBJNOTFOUND);
        assert_eq!(r.locate("Clip/x", 0).0, err::OBJNOTFOUND);
        // No name: a copy of the grip given.
        let (e, root, block) = r.locate("FlyerA0:", 0);
        assert_eq!((e, block), (err::OKAY, 0));
        assert_ne!(root, 0);
        let in_use = r.fw.fs.grips_in_use();
        for gr in [grip, stills, again, root] {
            assert_eq!(r.send(op::FREEGRIP, &[(4, 0), (6, gr)]).0, err::OKAY);
        }
        assert_eq!(r.fw.fs.grips_in_use(), in_use - 4);
        // Freeing twice, or the root, is harmless.
        assert_eq!(r.send(op::FREEGRIP, &[(4, 0), (6, grip)]).0, err::OKAY);
        assert_eq!(r.send(op::FREEGRIP, &[(4, 0), (6, 0)]).0, err::OKAY);
        assert_eq!(r.fw.fs.grips_in_use(), 1);
    }

    #[test]
    fn a_file_added_on_the_host_is_found() {
        let mut r = rig();
        let folder = r.fw.fs.drives[0].folder().to_path_buf();
        std::fs::write(folder.join("Later"), b"x").unwrap();
        assert_eq!(r.locate("Later", 0).0, err::OKAY);
    }

    #[test]
    fn dirlist_lists_the_directory_then_each_child() {
        let mut r = rig();
        let list = |r: &mut Rig, first: bool| {
            let flags = if first { 0x0100 } else { 0 };
            r.send(
                op::DIRLIST,
                &[(4, 0), (6, 0), (10, INFO as u32), (14, flags << 16)],
            )
            .0
        };
        assert_eq!(list(&mut r, true), err::OKAY);
        assert_eq!(r.info_name(), "FlyerA0");
        assert_eq!(read_cstr(&r.sram, INFO + ci::COMMENT), "Flyer drive");
        assert_eq!(rd8(&r.sram, INFO + ci::TYPE), TYPE_ROOT);
        let mut names = Vec::new();
        while list(&mut r, false) == err::OKAY {
            names.push((r.info_name(), rd8(&r.sram, INFO + ci::TYPE)));
        }
        assert_eq!(
            names,
            [
                ("Clip".into(), TYPE_FILE),
                ("Notes".into(), TYPE_FILE),
                ("Stills".into(), 2)
            ]
        );
        assert_eq!(list(&mut r, false), err::EXHAUSTED);
        // Starting over.
        assert_eq!(list(&mut r, true), err::OKAY);
        assert_eq!(list(&mut r, false), err::OKAY);
        assert_eq!(r.info_name(), "Clip");
    }

    #[test]
    fn fileinfo_reads_a_clips_header() {
        let mut r = rig();
        r.action(0, "FlyerA0:Clip", 0, 0);
        let (e, _) = r.send(op::FILEINFO, &[(4, VOLUME as u32), (8, INFO as u32)]);
        assert_eq!(e, err::OKAY);
        let s = &r.sram;
        assert_eq!(r.info_name(), "Clip");
        assert_eq!(rd8(s, INFO + ci::FLAGS), CIF_HASVIDEO | CIF_HASAUDIO);
        assert_eq!(rd32(s, INFO + ci::FIELDS), 8);
        assert_eq!(rd8(s, INFO + ci::AUD_CHANS), 2);
        assert_eq!(rd8(s, INFO + ci::VIDEO_GRADE), 1);
        let start = rd32(s, INFO + ci::START);
        assert_eq!(rd32(s, INFO + ci::LENGTH), 512 + 1024);
        assert_eq!(rd32(s, INFO + ci::END_BLK), start + 3);
        assert_eq!(rd32(s, INFO + ci::INDEX_BLK), start + 2);
        // Not a clip: no fields, no flags.
        r.action(0, "Notes", 0, 0);
        r.send(op::FILEINFO, &[(4, VOLUME as u32), (8, INFO as u32)]);
        assert_eq!(rd32(&r.sram, INFO + ci::FIELDS), 0);
        assert_eq!(rd8(&r.sram, INFO + ci::FLAGS), 0);
        assert_eq!(rd32(&r.sram, INFO + ci::LENGTH), 10);
        r.action(0, "Nothing", 0, 0);
        assert_eq!(
            r.send(op::FILEINFO, &[(4, VOLUME as u32), (8, INFO as u32)])
                .0,
            err::OBJNOTFOUND
        );
    }

    #[test]
    fn files_open_seek_and_read_through_their_grip() {
        let mut r = rig();
        r.action(0, "Notes", 0, MODE_INPUT);
        assert_eq!(r.send(op::FILEOPEN, &[(4, ACTION as u32)]).0, err::OKAY);
        let id = rd32(&r.sram, ACTION + ca::FILE_ID);
        let at = (id - SRAM_CPU_BASE) as usize;
        // What NewFileRead reads straight from the window.
        assert_eq!(
            rd32(&r.sram, at + g::STARTBLK),
            rd32(&r.sram, ACTION + ca::START_BLK)
        );
        assert_eq!(rd32(&r.sram, at + g::FILELEN), 10);
        assert_eq!(rd32(&r.sram, at + g::PHYSLEN), 512);
        assert_eq!(rd32(&r.sram, ACTION + ca::RESERVED0_1), 0x2222_2222);
        let (e, slot) = r.send(
            op::FILEREAD,
            &[(4, 0), (6, id), (10, 4), (14, BUFFER as u32)],
        );
        assert_eq!((e, arg32(&slot, 18)), (err::OKAY, 4));
        assert_eq!(&r.sram[BUFFER..BUFFER + 4], b"0123");
        assert_eq!(rd32(&r.sram, at + g::CURPOS), 4);
        // The Amiga moves the position itself (NewFileRead).
        wr32(&mut r.sram, at + g::CURPOS, 8);
        let (_, slot) = r.send(
            op::FILEREAD,
            &[(4, 0), (6, id), (10, 100), (14, BUFFER as u32)],
        );
        assert_eq!(arg32(&slot, 18), 2);
        assert_eq!(&r.sram[BUFFER..BUFFER + 2], b"89");
        // Seek from the end, back two bytes; the old position comes back.
        let (e, slot) = r.send(
            op::FILESEEK,
            &[
                (4, 0x0000_0100),
                (8, id),
                (12, 0xFFFF_FFFF),
                (16, 0xFFFF_FFFE),
            ],
        );
        assert_eq!((e, arg32(&slot, 24)), (err::OKAY, 10));
        assert_eq!(rd32(&r.sram, at + g::CURPOS), 8);
        assert_eq!(
            r.send(op::FILESEEK, &[(4, 0x0000_FF00), (8, id), (16, 11)])
                .0,
            err::BADPARAM
        );
        assert_eq!(
            r.send(op::FILESEEK, &[(4, 0x0000_7700), (8, id)]).0,
            err::BADPARAM
        );
        assert_eq!(r.send(op::FILECLOSE, &[(4, 0), (6, id)]).0, err::OKAY);
        assert_eq!(
            r.send(
                op::FILEREAD,
                &[(4, 0), (6, id), (10, 4), (14, BUFFER as u32)]
            )
            .0,
            err::OBJNOTFOUND
        );
        // A directory is not a file.
        r.action(0, "Stills", 0, MODE_INPUT);
        assert_eq!(r.send(op::FILEOPEN, &[(4, ACTION as u32)]).0, ERR_WRONGTYPE);
        // A clip cannot be replaced by a new file.
        r.action(0, "Clip", 0, MODE_OUTPUT);
        assert_eq!(r.send(op::FILEOPEN, &[(4, ACTION as u32)]).0, ERR_PROTECTED);
    }

    #[test]
    fn files_other_than_clips_can_be_made_written_and_deleted() {
        let mut r = rig();
        let fa0 = r._dir.path().join("Flyer/FA0");
        // MODE_OUTPUT makes an empty file, as the switcher does for an
        // icon, in a directory named in the path.
        r.action(0, "Stills/Clip.i", 0, MODE_OUTPUT);
        assert_eq!(r.send(op::FILEOPEN, &[(4, ACTION as u32)]).0, err::OKAY);
        let id = rd32(&r.sram, ACTION + ca::FILE_ID);
        let start = rd32(&r.sram, ACTION + ca::START_BLK);
        assert_eq!(
            std::fs::metadata(fa0.join("Stills/Clip.i")).unwrap().len(),
            0
        );
        // FileWrite: ten bytes at the position.
        r.sram[BUFFER..BUFFER + 10].copy_from_slice(b"FORM\0\0\0\x02IL");
        let (e, slot) = r.send(
            op::FILEWRITE,
            &[(4, 0), (6, id), (10, 10), (14, BUFFER as u32)],
        );
        assert_eq!((e, arg32(&slot, 18)), (err::OKAY, 10));
        // NewFileWrite: extend, then whole blocks by number.
        assert_eq!(
            r.send(op::FILEEXTEND, &[(4, 0), (6, id), (10, 1014)]).0,
            err::OKAY
        );
        let at = (id - SRAM_CPU_BASE) as usize;
        assert_eq!(rd32(&r.sram, at + g::FILELEN), 1024);
        assert_eq!(rd32(&r.sram, at + g::PHYSLEN), 1024);
        r.sram[BUFFER..BUFFER + 512].fill(0x77);
        let (e, _) = r.send(
            op::FILEHSWRITE,
            &[
                (4, 0x0100_0000),
                (6, id),
                (10, start + 1),
                (14, 1),
                (18, BUFFER as u32),
            ],
        );
        assert_eq!(e, err::OKAY);
        r.send(op::FILECLOSE, &[(4, 0), (6, id)]);
        let bytes = std::fs::read(fa0.join("Stills/Clip.i")).unwrap();
        assert_eq!(bytes.len(), 1024);
        assert_eq!(&bytes[..4], b"FORM");
        assert!(bytes[512..].iter().all(|&b| b == 0x77));
        // Directories, renames and deletes reach the host folder.
        r.action(0, "Icons", 0, 0);
        assert_eq!(r.send(op::CREATEDIR, &[(4, ACTION as u32)]).0, err::OKAY);
        assert!(fa0.join("Icons").is_dir());
        r.action(0, "Stills/Clip.i", 0, 0);
        r.sram[PATH + 0x80..PATH + 0x88].copy_from_slice(b"Icons/A\0");
        assert_eq!(
            r.send(
                op::RENAME,
                &[(4, ACTION as u32), (8, 0), (12, (PATH + 0x80) as u32)]
            )
            .0,
            err::OKAY
        );
        assert!(fa0.join("Icons/A").is_file());
        r.action(0, "Icons", 0, 0);
        assert_eq!(r.send(op::DELETE, &[(4, ACTION as u32)]).0, ERR_DIRNOTEMPTY);
        r.action(0, "Icons/A", 0, 0);
        assert_eq!(r.send(op::DELETE, &[(4, ACTION as u32)]).0, err::OKAY);
        assert!(!fa0.join("Icons/A").exists());
        // Clips stay as they are.
        r.action(0, "Clip", 0, 0);
        assert_eq!(r.send(op::DELETE, &[(4, ACTION as u32)]).0, ERR_PROTECTED);
        r.action(0, "Clip", 0, MODE_UPDATE);
        r.send(op::FILEOPEN, &[(4, ACTION as u32)]);
        let clip = rd32(&r.sram, ACTION + ca::FILE_ID);
        assert_eq!(
            r.send(op::FILEEXTEND, &[(4, 0), (6, clip), (10, 512)]).0,
            ERR_PROTECTED
        );
        // The drive is no longer write-protected.
        r.send(op::DRIVEINFO, &[(4, 0), (6, INFO as u32)]);
        assert_eq!(r.sram[INFO + fvi::FLAGS] & drive::FVIF_WRITEPROT, 0);
    }

    #[test]
    fn high_speed_reads_and_read10_fetch_blocks_by_number() {
        let mut r = rig();
        r.action(0, "Clip", 0, MODE_INPUT);
        r.send(op::FILEOPEN, &[(4, ACTION as u32)]);
        let id = rd32(&r.sram, ACTION + ca::FILE_ID);
        let start = rd32(&r.sram, ACTION + ca::START_BLK);
        // FileHSRead: WFLAG 0, two blocks from the clip's second.
        let (e, _) = r.send(
            op::FILEHSREAD,
            &[
                (4, 0),
                (6, id),
                (10, start + 1),
                (14, 2),
                (18, BUFFER as u32),
            ],
        );
        assert_eq!(e, err::OKAY);
        assert_eq!(r.sram[BUFFER], 0);
        assert_eq!(r.sram[BUFFER + 600], (600 % 256) as u8);
        let (e, _) = r.send(
            op::FILEHSWRITE,
            &[
                (4, 0x0100_0000),
                (6, id),
                (10, start),
                (14, 1),
                (18, BUFFER as u32),
            ],
        );
        assert_eq!(e, ERR_PROTECTED);
        // READ10 into DRAM block $1FFE, then CPUDMA to the VTASC SRAM area.
        let (e, _) = r.send(
            op::READ10,
            &[(4, 0), (6, start), (10, 0x0002_0000), (12, 0x1FFE)],
        );
        assert_eq!(e, err::OKAY);
        let (e, _) = r.send(op::CPUDMA, &[(4, 0xBE), (8, 0x1FFE), (12, 0x0002_0000)]);
        assert_eq!(e, err::OKAY);
        assert_eq!(&r.sram[0x17C00..0x17C04], b"CLIP");
        assert_eq!(r.sram[0x17C00 + 512 + 7], 7);
        // And back.
        r.sram[0x17C00] = 0x55;
        r.send(op::CPUDMA, &[(4, 0xBE), (8, 0x10), (12, 0x0001_0100)]);
        r.send(op::CPUDMA, &[(4, 0x20), (8, 0x10), (12, 0x0001_0000)]);
        assert_eq!(r.sram[0x4000], 0x55);
        assert_eq!(r.send(op::WRITE10, &[(4, 0)]).0, ERR_PROTECTED);
    }

    #[test]
    fn parent_and_copygrip_walk_the_tree() {
        let mut r = rig();
        let (_, stills, _) = r.locate("Stills", 0);
        let (e, slot) = r.send(op::PARENT, &[(4, 0), (6, stills)]);
        assert_eq!((e, arg32(&slot, 14)), (err::OKAY, 0));
        let mom = arg32(&slot, 10);
        assert_eq!(
            rd8(&r.sram, (mom - SRAM_CPU_BASE) as usize + g::TYPE),
            TYPE_ROOT
        );
        assert_eq!(r.send(op::PARENT, &[(4, 0), (6, mom)]).0, err::OBJNOTFOUND);
        let (e, slot) = r.send(op::COPYGRIP, &[(4, 0), (6, stills)]);
        assert_eq!(e, err::OKAY);
        let copy = arg32(&slot, 10);
        assert_ne!(copy, stills);
        assert_eq!(
            rd32(&r.sram, (copy - SRAM_CPU_BASE) as usize + g::HEADER),
            rd32(&r.sram, (stills - SRAM_CPU_BASE) as usize + g::HEADER)
        );
        // Not a grip.
        assert_eq!(
            r.send(op::COPYGRIP, &[(4, 0), (6, SRAM_CPU_BASE + 0x1234)])
                .0,
            err::OBJNOTFOUND
        );
    }

    /// A real three-frame clip (twelve fields), as flyerclip writes it.
    fn real_clip() -> Vec<u8> {
        use flyerclip::writer::{ClipWriter, Grade};
        let mut writer = ClipWriter::new(Grade::Std, 0);
        for f in 0..3u8 {
            let field = vec![vec![60 + 20 * f; flyerclip::vtasc::PIXELS_PER_LINE]; 243];
            writer.push_frame(
                Some(&[field.clone(), field.clone(), field.clone(), field]),
                None,
            );
        }
        writer.finish()
    }

    #[test]
    fn a_field_is_found_and_described_for_reading() {
        let mut r = rig();
        let bytes = real_clip();
        let folder = r.fw.fs.drives[0].folder().to_path_buf();
        std::fs::write(folder.join("Real"), &bytes).unwrap();
        let clip = flyerclip::clip::Clip::read(&bytes).unwrap();
        // Field 9 is in the third colour frame.
        r.action(0, "FlyerA0:Real", 0, 0);
        wr32(&mut r.sram, ACTION + ca::VID_START_FIELD, 9);
        assert_eq!(r.send(op::LOCATEFLD, &[(4, ACTION as u32)]).0, err::OKAY);
        let frame_blk = rd32(&r.sram, ACTION + ca::START_BLK);
        r.action(0, "FlyerA0:Real", 0, 0);
        let (e, _) = r.send(op::FILEINFO, &[(4, VOLUME as u32), (8, INFO as u32)]);
        assert_eq!(e, err::OKAY);
        let start = rd32(&r.sram, INFO + ci::START);
        let (_, info) = clip.field_info(9).unwrap();
        let mut hdr = [0u8; BLOCK];
        r.fw.fs.drives[0]
            .read_blocks(frame_blk, 1, &mut hdr)
            .unwrap();
        let fh = flyerclip::header::FrameHeader::parse(&hdr).unwrap();
        assert_eq!(fh.frame_number, 2);
        // GETCOMPINFO for field 1 of that frame (9 & 3): drive +4, field +5,
        // block +6, structure +10.
        let (e, _) = r.send(
            op::GETCOMPINFO,
            &[
                (4, 0x0001_0000 | (frame_blk >> 16)),
                (6, frame_blk),
                (10, INFO as u32),
            ],
        );
        assert_eq!(e, err::OKAY);
        let data_start = rd32(&r.sram, INFO + vci::DATA_START);
        let data_end = rd32(&r.sram, INFO + vci::DATA_END);
        assert_eq!(data_start, frame_blk + u32::from(info.vid_start));
        assert_eq!(data_end - data_start, u32::from(info.vid_length));
        assert_eq!(rd8(&r.sram, INFO + vci::TOLERANCE), info.tolerance);
        assert_eq!(rd8(&r.sram, INFO + vci::RND_SEED), info.rnd_seed);
        let algo = rd8(&r.sram, INFO + vci::ALGO);
        assert!(algo == ALGO_D2 || algo == ALGO_SN);
        // The library then reads the data by block number: it is the
        // field's coded data in the file.
        let want = clip.field_data(9).unwrap();
        let blocks = (data_end - data_start) as usize;
        let mut got = vec![0u8; blocks * BLOCK];
        r.fw.fs.drives[0]
            .read_blocks(data_start, blocks, &mut got)
            .unwrap();
        assert_eq!(&got[..want.len()], want);
        assert!(data_start > start);
        // Past the clip's end, and the live input.
        wr32(&mut r.sram, ACTION + ca::VID_START_FIELD, 12);
        assert_eq!(
            r.send(op::LOCATEFLD, &[(4, ACTION as u32)]).0,
            err::BADPARAM
        );
        assert_eq!(
            r.send(op::GETCOMPINFO, &[(4, 0), (6, 0), (10, INFO as u32)])
                .0,
            err::CMDFAILED
        );
        // A file that is not a clip.
        r.action(0, "Notes", 0, 0);
        assert_eq!(
            r.send(op::LOCATEFLD, &[(4, ACTION as u32)]).0,
            ERR_BADVIDHDR
        );
    }

    #[test]
    fn fir_sets_read_back_presets_and_the_custom_set() {
        let mut r = rig();
        // Preset 2, post-compensation: set +4, read +5, pre/post +6.
        let (e, slot) = r.send(op::FIRXCHG, &[(4, 0x0201_0100)]);
        assert_eq!(e, err::OKAY);
        let preset = flyerclip::vtasc::fir_preset(2, true).unwrap();
        assert_eq!(u16::from_be_bytes([slot[8], slot[9]]), preset.coefs[0]);
        assert_eq!(u16::from_be_bytes([slot[24], slot[25]]), preset.scale);
        // Write the custom pre-compensation set, read it back.
        let (e, _) = r.send(op::FIRXCHG, &[(4, 0), (8, 0x0001_0002), (24, 0x0200_0000)]);
        assert_eq!(e, err::OKAY);
        let (_, slot) = r.send(op::FIRXCHG, &[(4, 0x0001_0000)]);
        assert_eq!(&slot[8..12], &[0, 1, 0, 2]);
        assert_eq!(&slot[24..26], &[2, 0]);
        assert_eq!(r.send(op::FIRXCHG, &[(4, 0x0501_0000)]).0, err::BADPARAM);
        assert_eq!(r.send(op::FIRXCHG, &[(4, 0x0100_0000)]).0, err::BADPARAM);
    }

    #[test]
    fn grips_run_out_cleanly() {
        let mut r = rig();
        let mut last = err::OKAY;
        for _ in 0..GRIP_SLOTS + 2 {
            last = r.locate("Clip", 0).0;
        }
        assert_eq!(last, ERR_OUTOFMEM);
    }
}
