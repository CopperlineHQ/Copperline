// SPDX-License-Identifier: GPL-3.0-or-later

//! ATA (IDE) task file and command engine, shared by the machines that have an
//! IDE port: the A600/A1200's Gayle ([`crate::gayle`]) and the A4000's
//! motherboard interface ([`crate::ide_a4000`]).
//!
//! Both are the same 16-bit ATA-1 cable with the same eight task-file registers
//! and the same control block; only the address decode and the gate array's own
//! registers differ, so the front-ends keep those and hand every register access
//! to [`AtaBus`].
//!
//! Transfers complete within the access that triggers them: a command reads or
//! writes its sectors immediately and BSY is never observable.

use crate::harddrive::{HardDriveImage, RDB_HEADS, RDB_SPT};
use std::path::Path;

pub use crate::harddrive::SECTOR_SIZE;
/// Maximum sectors per READ/WRITE MULTIPLE block we advertise in IDENTIFY
/// word 47 and accept from SET MULTIPLE.
pub const MAX_MULTIPLE: u8 = 16;

// ATA status bits. BSY is defined for completeness: transfers complete
// within the access in this model, so it is never observable.
#[allow(dead_code)]
pub(crate) const ST_BSY: u8 = 0x80;
pub(crate) const ST_DRDY: u8 = 0x40;
pub(crate) const ST_DSC: u8 = 0x10;
pub(crate) const ST_DRQ: u8 = 0x08;
pub(crate) const ST_ERR: u8 = 0x01;
// ATA error bits.
pub(crate) const ERR_ABRT: u8 = 0x04;
pub(crate) const ERR_IDNF: u8 = 0x10;
// Device control bits.
pub(crate) const CTL_NIEN: u8 = 0x02;
pub(crate) const CTL_SRST: u8 = 0x04;
// Device/head bits.
pub(crate) const DH_LBA: u8 = 0x40;
pub(crate) const DH_DRV: u8 = 0x10;

/// A register in the task file, or the control block's shared
/// alternate-status/device-control address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdeReg {
    Data,
    ErrorFeature,
    SectorCount,
    SectorNumber,
    CylLow,
    CylHigh,
    DriveHead,
    StatusCommand,
    AltStatusDevCtl,
}

/// The task-file register at `offset` bytes from the base of the file. Both
/// Amiga interfaces space the eight registers four bytes apart, and each
/// register occupies both 16-bit halves of its slot, so it answers at offsets
/// 4n and 4n+2 (the `& !0x02` folds the two halfword addresses together).
pub fn task_file_reg(offset: u32) -> Option<IdeReg> {
    Some(match offset & !0x02 {
        0x00 => IdeReg::Data,
        0x04 => IdeReg::ErrorFeature,
        0x08 => IdeReg::SectorCount,
        0x0C => IdeReg::SectorNumber,
        0x10 => IdeReg::CylLow,
        0x14 => IdeReg::CylHigh,
        0x18 => IdeReg::DriveHead,
        0x1C => IdeReg::StatusCommand,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Transfer {
    None,
    /// Device-to-host PIO (READ SECTORS / READ MULTIPLE / IDENTIFY).
    PioIn {
        /// Sectors still owed after the words currently in the buffer.
        remaining: u32,
        /// Sectors per DRQ block (1, or the SET MULTIPLE count).
        block: u32,
    },
    /// Host-to-device PIO (WRITE SECTORS / WRITE MULTIPLE).
    PioOut {
        remaining: u32,
        block: u32,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct IdeDrive {
    /// The sector store (shared with the SCSI targets): HDF file,
    /// directory-built FFS volume, synthesized-RDB overlay handling.
    pub disk: HardDriveImage,
    // Default geometry from the image size; INITIALIZE DEVICE PARAMETERS
    // (0x91) overrides the current translation.
    default_heads: u8,
    default_spt: u8,
    cylinders: u16,
    heads: u8,
    spt: u8,
    multiple: u8,
}

impl IdeDrive {
    /// Open an IDE unit (0 = master, 1 = slave; this picks the DHn device
    /// name a synthesized RDB advertises). The path may be a raw HDF image
    /// file, or a host directory, which is built into an in-memory FFS
    /// volume at open time; `volume_name` labels that volume (directory
    /// mounts only).
    pub fn open(path: &Path, unit: usize, volume_name: Option<&str>) -> anyhow::Result<Self> {
        let disk = HardDriveImage::open(
            path,
            &format!("DH{unit}"),
            "ide",
            "COPPERLINE IDE DISK",
            volume_name,
        )?;
        // The classic Amiga HDF geometry: 16 surfaces, 32 sectors per track
        // (what HDToolBox/RDB tooling defaults to), so the CHS the host
        // computes from an RDB's physical-drive block agrees with what the
        // drive decodes.
        let heads = RDB_HEADS as u8;
        let spt = RDB_SPT as u8;
        let cylinders =
            (disk.total_sectors() / (u64::from(heads) * u64::from(spt))).clamp(1, 65535) as u16;
        Ok(Self {
            disk,
            default_heads: heads,
            default_spt: spt,
            cylinders,
            heads,
            spt,
            multiple: 0,
        })
    }

    /// IDENTIFY DEVICE data. The Amiga IDE ports wire the drive's data bus
    /// byte-swapped relative to the 68000 (IDE D7-D0 land on CPU D15-D8), so
    /// the CPU reads every ATA word with its bytes exchanged. The ROM driver's
    /// scsi.device depends on this: it parses the stored block assuming PC byte
    /// order per word (its word helper at $FB788C and string helper at $FB7B22
    /// swap each pair back). Sector data is unaffected because the swap puts
    /// file bytes back in natural memory order. We therefore store each ATA
    /// word low-byte-first here, since the data port read returns
    /// `buf[2i] << 8 | buf[2i+1]`.
    fn identify_block(&self) -> Vec<u8> {
        let mut buf = vec![0u8; SECTOR_SIZE];
        let mut word = |idx: usize, val: u16| {
            buf[idx * 2] = (val & 0xFF) as u8;
            buf[idx * 2 + 1] = (val >> 8) as u8;
        };
        // Word 0 mirrors the Conner drives the A600HD shipped with
        // (soft-sectored, fixed, MFM-encoded transfer-rate bits).
        word(0, 0x045A);
        word(1, self.cylinders);
        word(3, u16::from(self.default_heads));
        // ATA-1 unformatted bytes per track/sector: vintage drivers
        // (ROM scsi.device) read these for the block size.
        word(4, u16::from(self.default_spt) * 512);
        word(5, 512);
        word(6, u16::from(self.default_spt));
        word(20, 3); // dual-ported buffer with read caching
        word(21, 64); // buffer size in sectors
        word(22, 4); // ECC bytes for READ/WRITE LONG
        word(48, 1); // can perform doubleword I/O (32-bit host transfers)
        word(51, 0x0200); // PIO data transfer timing mode 2
        word(52, 0x0200); // DMA data transfer timing mode (legacy field)
        word(47, 0x8000 | u16::from(MAX_MULTIPLE));
        word(49, 0x0200); // LBA supported
        word(53, 0x0001); // words 54-58 valid
        word(54, self.cylinders);
        word(55, u16::from(self.heads));
        word(56, u16::from(self.spt));
        let current = u32::from(self.cylinders) * u32::from(self.heads) * u32::from(self.spt);
        word(57, (current & 0xFFFF) as u16);
        word(58, (current >> 16) as u16);
        let lba = self.disk.total_sectors().min(u64::from(u32::MAX)) as u32;
        word(60, (lba & 0xFFFF) as u16);
        word(61, (lba >> 16) as u16);
        word(
            59,
            if self.multiple > 0 {
                0x0100 | u16::from(self.multiple)
            } else {
                0
            },
        );

        // ATA strings carry the first character of each pair in bits 15-8,
        // so with the low-byte-first storage above the pair lands swapped.
        let mut string = |start: usize, len_words: usize, text: &str| {
            let mut bytes = text.as_bytes().to_vec();
            bytes.resize(len_words * 2, b' ');
            for (i, pair) in bytes.chunks(2).enumerate() {
                buf[(start + i) * 2] = pair[1];
                buf[(start + i) * 2 + 1] = pair[0];
            }
        };
        string(10, 10, "CPRLN-0000000000");
        string(23, 4, "1.0 ");
        string(27, 20, "COPPERLINE IDE DISK");
        buf
    }
}

/// One ATA cable: the master/slave pair, the task file they share, and the
/// command engine.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct AtaBus {
    drives: [Option<IdeDrive>; 2],
    // Shared task file (one register file per bus, like the real cable).
    feature: u8,
    error: u8,
    sector_count: u8,
    sector_number: u8,
    cyl_low: u8,
    cyl_high: u8,
    drive_head: u8,
    status: u8,
    devctl: u8,
    /// INTRQ, the drive's interrupt line: raised on command completion and on
    /// each DRQ block, dropped when the host reads the status register.
    intrq: bool,
    /// INTRQ went high since the front-end last looked. Gayle latches this in
    /// its own interrupt-change register.
    irq_edge: bool,

    buf: Vec<u8>,
    buf_pos: usize,
    transfer: Transfer,
    /// Set whenever the drive does real work (command issued or data port
    /// moved during a transfer); drained by the bus for the HDD LED.
    activity: bool,
}

impl Default for AtaBus {
    fn default() -> Self {
        Self::new()
    }
}

impl AtaBus {
    pub fn new() -> Self {
        Self {
            drives: [None, None],
            feature: 0,
            error: 0x01, // diagnostics passed
            sector_count: 0x01,
            sector_number: 0x01,
            cyl_low: 0,
            cyl_high: 0,
            drive_head: 0,
            status: ST_DRDY | ST_DSC,
            devctl: 0,
            intrq: false,
            irq_edge: false,
            buf: Vec::new(),
            buf_pos: 0,
            transfer: Transfer::None,
            activity: false,
        }
    }

    pub fn attach_drive(&mut self, slot: usize, drive: IdeDrive) {
        self.drives[slot.min(1)] = Some(drive);
    }

    /// System reset: clear the register file and any in-flight transfer but
    /// keep the mounted drives.
    pub fn reset(&mut self) {
        self.feature = 0;
        self.sector_count = 0x01;
        self.sector_number = 0x01;
        self.cyl_low = 0;
        self.cyl_high = 0;
        self.drive_head = 0;
        self.devctl = 0;
        self.soft_reset();
    }

    /// Drain the activity latch set by command issue and data-port traffic.
    /// The bus polls this after each access to time the HDD LED.
    pub fn take_activity(&mut self) -> bool {
        std::mem::take(&mut self.activity)
    }

    /// The INTRQ line as the host sees it: masked by the control block's
    /// interrupt disable.
    pub fn irq_level(&self) -> bool {
        self.intrq && self.devctl & CTL_NIEN == 0
    }

    /// Drain the "INTRQ went high" edge, for a front-end that latches it.
    pub fn take_irq_edge(&mut self) -> bool {
        std::mem::take(&mut self.irq_edge)
    }

    fn selected(&self) -> usize {
        usize::from(self.drive_head & DH_DRV != 0)
    }

    fn drive(&mut self) -> Option<&mut IdeDrive> {
        self.drives[self.selected()].as_mut()
    }

    fn pair_present(&self) -> bool {
        self.drives[1 - self.selected().min(1)].is_some()
    }

    fn raise_irq(&mut self) {
        self.intrq = true;
        if self.devctl & CTL_NIEN == 0 {
            self.irq_edge = true;
        }
    }

    fn clear_irq(&mut self) {
        self.intrq = false;
    }

    // ----- register access ---------------------------------------------------

    pub fn read_reg(&mut self, reg: Option<IdeReg>, size: usize) -> u32 {
        // Selected device absent: the status register reads 0x01 (ERR set,
        // not ready) when the other device is present and 0xFF when the
        // cable is empty; every other task-file register reads zero, and a
        // status read drops a pending interrupt (the INTRQ line is shared).
        // This is how the ROM probe concludes a unit does not exist
        // instead of classifying it as a pre-ATA drive (matches WinUAE).
        if self.drives[self.selected()].is_none() {
            return match reg {
                Some(IdeReg::StatusCommand) | Some(IdeReg::AltStatusDevCtl) => {
                    self.clear_irq();
                    if self.pair_present() {
                        0x01
                    } else {
                        0xFF
                    }
                }
                _ => 0,
            };
        }
        match reg {
            Some(IdeReg::Data) => {
                let word = self.data_read_word();
                if size == 1 {
                    u32::from(word >> 8)
                } else {
                    u32::from(word)
                }
            }
            Some(IdeReg::ErrorFeature) => u32::from(self.error),
            Some(IdeReg::SectorCount) => u32::from(self.sector_count),
            Some(IdeReg::SectorNumber) => u32::from(self.sector_number),
            Some(IdeReg::CylLow) => u32::from(self.cyl_low),
            Some(IdeReg::CylHigh) => u32::from(self.cyl_high),
            Some(IdeReg::DriveHead) => u32::from(self.drive_head),
            Some(IdeReg::StatusCommand) => {
                let v = self.status;
                self.clear_irq();
                u32::from(v)
            }
            Some(IdeReg::AltStatusDevCtl) => u32::from(self.status),
            None => 0,
        }
    }

    pub fn write_reg(&mut self, reg: Option<IdeReg>, size: usize, value: u32) {
        let byte = value as u8;
        match reg {
            Some(IdeReg::Data) => {
                let word = if size == 1 {
                    (value as u16) << 8
                } else {
                    value as u16
                };
                self.data_write_word(word);
            }
            Some(IdeReg::ErrorFeature) => self.feature = byte,
            Some(IdeReg::SectorCount) => self.sector_count = byte,
            Some(IdeReg::SectorNumber) => self.sector_number = byte,
            Some(IdeReg::CylLow) => self.cyl_low = byte,
            Some(IdeReg::CylHigh) => self.cyl_high = byte,
            Some(IdeReg::DriveHead) => self.drive_head = byte,
            Some(IdeReg::StatusCommand) => self.command(byte),
            Some(IdeReg::AltStatusDevCtl) => {
                let was_reset = self.devctl & CTL_SRST != 0;
                self.devctl = byte;
                if byte & CTL_SRST != 0 && !was_reset {
                    self.soft_reset();
                }
            }
            None => {}
        }
    }

    fn soft_reset(&mut self) {
        self.status = ST_DRDY | ST_DSC;
        self.error = 0x01;
        self.transfer = Transfer::None;
        self.buf.clear();
        self.buf_pos = 0;
        self.clear_irq();
    }

    // ----- data port -------------------------------------------------------

    fn data_read_word(&mut self) -> u16 {
        if !matches!(self.transfer, Transfer::PioIn { .. }) || self.buf_pos + 1 >= self.buf.len() {
            return 0;
        }
        let word = (u16::from(self.buf[self.buf_pos]) << 8) | u16::from(self.buf[self.buf_pos + 1]);
        self.buf_pos += 2;
        self.activity = true;
        if self.buf_pos >= self.buf.len() {
            self.pio_in_block_consumed();
        }
        word
    }

    fn data_write_word(&mut self, word: u16) {
        if !matches!(self.transfer, Transfer::PioOut { .. }) || self.buf_pos + 1 >= self.buf.len() {
            return;
        }
        self.buf[self.buf_pos] = (word >> 8) as u8;
        self.buf[self.buf_pos + 1] = (word & 0xFF) as u8;
        self.buf_pos += 2;
        self.activity = true;
        if self.buf_pos >= self.buf.len() {
            self.pio_out_block_filled();
        }
    }

    fn pio_in_block_consumed(&mut self) {
        let Transfer::PioIn { remaining, block } = self.transfer else {
            // IDENTIFY-style single buffer: transfer complete.
            self.status = ST_DRDY | ST_DSC;
            self.transfer = Transfer::None;
            return;
        };
        if remaining == 0 {
            self.status = ST_DRDY | ST_DSC;
            self.transfer = Transfer::None;
            return;
        }
        let chunk = remaining.min(block);
        if self.fill_read_buffer(chunk).is_ok() {
            self.transfer = Transfer::PioIn {
                remaining: remaining - chunk,
                block,
            };
            self.status = ST_DRDY | ST_DSC | ST_DRQ;
            self.raise_irq();
        }
    }

    fn pio_out_block_filled(&mut self) {
        let Transfer::PioOut { remaining, block } = self.transfer else {
            return;
        };
        // Commit the buffered sectors at the current task-file position.
        if self.commit_write_buffer().is_err() {
            return;
        }
        if remaining == 0 {
            if let Some(drive) = self.drive() {
                drive.disk.flush();
            }
            self.status = ST_DRDY | ST_DSC;
            self.transfer = Transfer::None;
            self.raise_irq();
            return;
        }
        let chunk = remaining.min(block);
        self.buf.clear();
        self.buf.resize(chunk as usize * SECTOR_SIZE, 0);
        self.buf_pos = 0;
        self.transfer = Transfer::PioOut {
            remaining: remaining - chunk,
            block,
        };
        self.status = ST_DRDY | ST_DSC | ST_DRQ;
        self.raise_irq();
    }

    // ----- addressing -------------------------------------------------------

    /// Current LBA from the task file (LBA28 or CHS translation).
    fn current_lba(&mut self) -> Option<u64> {
        let lba_mode = self.drive_head & DH_LBA != 0;
        let head = u64::from(self.drive_head & 0x0F);
        let sector = u64::from(self.sector_number);
        let cyl = (u64::from(self.cyl_high) << 8) | u64::from(self.cyl_low);
        let drive = self.drive()?;
        if lba_mode {
            Some((head << 24) | (cyl << 8) | sector)
        } else {
            if sector == 0 {
                return None;
            }
            let heads = u64::from(drive.heads);
            let spt = u64::from(drive.spt);
            Some((cyl * heads + head) * spt + (sector - 1))
        }
    }

    /// Advance the task-file position by one sector, as real drives do, so
    /// software can resume after a partial transfer.
    fn advance_lba(&mut self) {
        if self.drive_head & DH_LBA != 0 {
            let lba = ((u32::from(self.drive_head & 0x0F) << 24)
                | (u32::from(self.cyl_high) << 16)
                | (u32::from(self.cyl_low) << 8)
                | u32::from(self.sector_number))
            .wrapping_add(1);
            self.sector_number = (lba & 0xFF) as u8;
            self.cyl_low = ((lba >> 8) & 0xFF) as u8;
            self.cyl_high = ((lba >> 16) & 0xFF) as u8;
            self.drive_head = (self.drive_head & 0xF0) | ((lba >> 24) & 0x0F) as u8;
            return;
        }
        let (heads, spt) = match self.drive() {
            Some(d) => (d.heads, d.spt),
            None => return,
        };
        if self.sector_number < spt {
            self.sector_number += 1;
            return;
        }
        self.sector_number = 1;
        let head = self.drive_head & 0x0F;
        if head + 1 < heads {
            self.drive_head = (self.drive_head & 0xF0) | (head + 1);
            return;
        }
        self.drive_head &= 0xF0;
        let cyl = ((u16::from(self.cyl_high) << 8) | u16::from(self.cyl_low)).wrapping_add(1);
        self.cyl_low = (cyl & 0xFF) as u8;
        self.cyl_high = (cyl >> 8) as u8;
    }

    fn fill_read_buffer(&mut self, sectors: u32) -> Result<(), ()> {
        self.buf.clear();
        self.buf_pos = 0;
        for _ in 0..sectors {
            let Some(lba) = self.current_lba() else {
                self.command_error(ERR_IDNF);
                return Err(());
            };
            let total = self.drive().map(|d| d.disk.total_sectors()).unwrap_or(0);
            if lba >= total {
                self.command_error(ERR_IDNF);
                return Err(());
            }
            let mut sector = [0u8; SECTOR_SIZE];
            let res = self
                .drive()
                .map(|d| d.disk.read_sector(lba, &mut sector))
                .unwrap_or_else(|| Err(std::io::ErrorKind::NotFound.into()));
            if let Err(e) = res {
                log::warn!("IDE read lba {lba}: {e}");
                self.command_error(ERR_ABRT);
                return Err(());
            }
            self.buf.extend_from_slice(&sector);
            self.advance_lba();
        }
        Ok(())
    }

    fn commit_write_buffer(&mut self) -> Result<(), ()> {
        let sectors = self.buf.len() / SECTOR_SIZE;
        for i in 0..sectors {
            let Some(lba) = self.current_lba() else {
                self.command_error(ERR_IDNF);
                return Err(());
            };
            let total = self.drive().map(|d| d.disk.total_sectors()).unwrap_or(0);
            if lba >= total {
                self.command_error(ERR_IDNF);
                return Err(());
            }
            let start = i * SECTOR_SIZE;
            let sector: [u8; SECTOR_SIZE] =
                self.buf[start..start + SECTOR_SIZE].try_into().unwrap();
            let res = self
                .drive()
                .map(|d| d.disk.write_sector(lba, &sector))
                .unwrap_or_else(|| Err(std::io::ErrorKind::NotFound.into()));
            if let Err(e) = res {
                log::warn!("IDE write lba {lba}: {e}");
                self.command_error(ERR_ABRT);
                return Err(());
            }
            self.advance_lba();
        }
        Ok(())
    }

    fn command_error(&mut self, error_bits: u8) {
        self.error = error_bits;
        self.status = ST_DRDY | ST_DSC | ST_ERR;
        self.transfer = Transfer::None;
        self.buf.clear();
        self.buf_pos = 0;
        self.raise_irq();
    }

    // ----- command dispatch --------------------------------------------------

    fn command(&mut self, cmd: u8) {
        if crate::envcfg::flag("COPPERLINE_DIAG_GAYLE") {
            let lba = self.drive_head & DH_LBA != 0;
            log::info!(
                "ide cmd {cmd:#04X} drv={} lba={} chs/lba=({:02X} {:02X} {:02X} {:02X}) n={}",
                self.selected(),
                lba,
                self.drive_head & 0x0F,
                self.cyl_high,
                self.cyl_low,
                self.sector_number,
                self.sector_count
            );
        }
        self.clear_irq();
        if self.drives[self.selected()].is_none() {
            // Every command addressed to an absent device fails with
            // command-aborted and raises the completion interrupt, so the
            // host's probe finishes promptly (matches WinUAE; the ROM's
            // INITIALIZE DEVICE PARAMETERS arrives with the DEV bit set
            // and must complete one way or the other).
            self.command_error(ERR_ABRT);
            return;
        }
        self.error = 0;
        self.status = ST_DRDY | ST_DSC;
        self.activity = true;
        let count = if self.sector_count == 0 {
            256u32
        } else {
            u32::from(self.sector_count)
        };
        match cmd {
            // IDENTIFY DEVICE
            0xEC => {
                self.buf = self.drive().map(|d| d.identify_block()).unwrap_or_default();
                self.buf_pos = 0;
                self.transfer = Transfer::PioIn {
                    remaining: 0,
                    block: 1,
                };
                self.status = ST_DRDY | ST_DSC | ST_DRQ;
                self.raise_irq();
            }
            // READ SECTORS (with/without retry) and READ MULTIPLE.
            0x20 | 0x21 | 0xC4 => {
                let block = if cmd == 0xC4 {
                    let m = self.drive().map(|d| d.multiple).unwrap_or(0);
                    if m == 0 {
                        self.command_error(ERR_ABRT);
                        return;
                    }
                    u32::from(m)
                } else {
                    1
                };
                let chunk = count.min(block);
                self.transfer = Transfer::PioIn {
                    remaining: count - chunk,
                    block,
                };
                if self.fill_read_buffer(chunk).is_ok() {
                    self.status = ST_DRDY | ST_DSC | ST_DRQ;
                    self.raise_irq();
                }
            }
            // WRITE SECTORS (with/without retry) and WRITE MULTIPLE.
            0x30 | 0x31 | 0xC5 => {
                let block = if cmd == 0xC5 {
                    let m = self.drive().map(|d| d.multiple).unwrap_or(0);
                    if m == 0 {
                        self.command_error(ERR_ABRT);
                        return;
                    }
                    u32::from(m)
                } else {
                    1
                };
                let chunk = count.min(block);
                self.buf.clear();
                self.buf.resize(chunk as usize * SECTOR_SIZE, 0);
                self.buf_pos = 0;
                self.transfer = Transfer::PioOut {
                    remaining: count - chunk,
                    block,
                };
                // First DRQ block is ready without an interrupt (ATA PIO out).
                self.status = ST_DRDY | ST_DSC | ST_DRQ;
            }
            // SET MULTIPLE MODE
            0xC6 => {
                let requested = self.sector_count;
                let ok =
                    requested <= MAX_MULTIPLE && (requested == 0 || requested.is_power_of_two());
                if let (true, Some(drive)) = (ok, self.drive()) {
                    drive.multiple = requested;
                    self.status = ST_DRDY | ST_DSC;
                    self.raise_irq();
                } else {
                    self.command_error(ERR_ABRT);
                }
            }
            // INITIALIZE DEVICE PARAMETERS: set current CHS translation.
            // A zero sector count is invalid and aborts, as on real drives.
            0x91 => {
                let heads = (self.drive_head & 0x0F) + 1;
                let spt = self.sector_count;
                if spt == 0 {
                    self.command_error(ERR_ABRT);
                    return;
                }
                if let Some(drive) = self.drive() {
                    drive.heads = heads;
                    drive.spt = spt;
                    let total = drive.disk.total_sectors();
                    drive.cylinders =
                        (total / (u64::from(heads) * u64::from(spt)).max(1)).clamp(1, 65535) as u16;
                }
                self.status = ST_DRDY | ST_DSC;
                self.raise_irq();
            }
            // RECALIBRATE
            0x10..=0x1F => {
                self.status = ST_DRDY | ST_DSC;
                self.raise_irq();
            }
            // NOP: per ATA-2 always aborts.
            0x00 => self.command_error(ERR_ABRT),
            _ => {
                log::warn!("IDE: unimplemented command {cmd:#04X}");
                self.command_error(ERR_ABRT);
            }
        }
    }
}
