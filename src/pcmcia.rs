// SPDX-License-Identifier: GPL-3.0-or-later

//! The card in the A600/A1200 PCMCIA (credit-card) slot.
//!
//! Gayle decodes the slot into the CPU's address space as the Commodore
//! schematics, the Linux `amigayle.h` header (derived from card.resource),
//! and WinUAE's `gayle.cpp` describe it:
//!
//! | CPU address           | slot cycle                                    |
//! |-----------------------|-----------------------------------------------|
//! | `$600000`-`$9FFFFF`   | common memory, 4 MiB (REG# high)               |
//! | `$A00000`-`$A1FFFF`   | attribute memory (REG# low, CE1#/CE2# memory)  |
//! | `$A20000`-`$A2FFFF`   | I/O space, 16-bit and even 8-bit registers     |
//! | `$A30000`-`$A3FFFF`   | I/O space, odd 8-bit registers (A0 forced high)|
//! | `$A40000`-`$A7FFFF`   | card RESET: a write asserts it, a read releases |
//!
//! Gayle cross-wires the byte lanes so a byte at card address N is the byte
//! at CPU address N: the CPU's even byte (D15-D8) is the card's D7-D0 byte
//! and vice versa. A 16-bit register therefore reads on the CPU with its
//! bytes exchanged, exactly as the Gayle IDE data port does, which is what
//! [`crate::ata::AtaBus`] already models; sector data passes through in
//! natural memory order.
//!
//! The slot's pins that Gayle folds into its status register are modelled
//! by [`PcmciaCard::pins`]; the register file, the change latches, and the
//! INT2/INT6 routing are Gayle's own and live in [`crate::gayle`]. This
//! module holds the card: a CompactFlash/ATA card driving the shared ATA
//! command engine through the CF register layout (memory-mapped, contiguous
//! I/O, or PC-style primary/secondary I/O, selected by the card's
//! Configuration Option Register), or an SRAM memory card with a CIS that
//! Kickstart's card.resource accepts as credit-card RAM.

use std::path::{Path, PathBuf};

use crate::ata::{AtaBus, IdeDrive, IdeReg};

/// Common memory window: `$600000`-`$9FFFFF`.
pub const COMMON_BASE: u32 = 0x0060_0000;
pub const COMMON_SIZE: u32 = 0x0040_0000;
/// Attribute memory window: `$A00000`-`$A1FFFF`.
pub const ATTRIBUTE_BASE: u32 = 0x00A0_0000;
pub const ATTRIBUTE_SIZE: u32 = 0x0002_0000;
/// I/O window, 16-bit and even 8-bit registers: `$A20000`-`$A2FFFF`.
pub const IO_BASE: u32 = 0x00A2_0000;
pub const IO_SIZE: u32 = 0x0001_0000;
/// I/O window, odd 8-bit registers: `$A30000`-`$A3FFFF`.
pub const IO_ODD_BASE: u32 = 0x00A3_0000;
/// Card reset register: `$A40000`-`$A7FFFF`.
pub const RESET_BASE: u32 = 0x00A4_0000;
pub const RESET_SIZE: u32 = 0x0004_0000;
/// Everything above common memory that Gayle decodes for the slot:
/// `$A00000`-`$A7FFFF`.
pub const SLOT_BASE: u32 = ATTRIBUTE_BASE;
pub const SLOT_SIZE: u32 = 0x0008_0000;

/// The largest SRAM card the 4 MiB common window can hold.
pub const MAX_SRAM_BYTES: usize = COMMON_SIZE as usize;

/// Zorro II fast RAM configures upward from `$200000`; more than this much
/// reaches into the common-memory window and Gayle's PCMCIA decode gives
/// way to it.
pub const MAX_FAST_RAM_WITH_SLOT: usize = 4 * 1024 * 1024;

// The slot pins as Gayle's status register presents them (bit positions
// shared with the change and enable registers).
/// Card detect (CD1#/CD2# grounded by an inserted card).
pub const PIN_CCDET: u8 = 0x40;
/// Battery voltage detect 1 (memory cards) / STSCHG# (I/O cards).
pub const PIN_BVD1: u8 = 0x20;
/// Battery voltage detect 2 (memory cards) / SPKR (I/O cards).
pub const PIN_BVD2: u8 = 0x10;
/// Write enable: 1 when the card's write-protect switch is off.
pub const PIN_WR: u8 = 0x08;
/// READY low (memory cards, "busy") / IREQ# low (I/O cards, "interrupt").
pub const PIN_BSY_IRQ: u8 = 0x04;

/// Whether `addr` falls in one of the slot windows Gayle decodes.
pub fn decodes(addr: u32) -> bool {
    addr.wrapping_sub(COMMON_BASE) < COMMON_SIZE || addr.wrapping_sub(SLOT_BASE) < SLOT_SIZE
}

/// Which slot cycle a CPU address selects, with the offset inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotAccess {
    Common(u32),
    Attribute(u32),
    /// I/O cycle; the offset already has A0 forced high for the odd-byte
    /// window at `$A30000`.
    Io(u32),
    Reset,
}

pub fn classify(addr: u32) -> Option<SlotAccess> {
    let common = addr.wrapping_sub(COMMON_BASE);
    if common < COMMON_SIZE {
        return Some(SlotAccess::Common(common));
    }
    let slot = addr.wrapping_sub(SLOT_BASE);
    if slot >= SLOT_SIZE {
        return None;
    }
    Some(match slot {
        0x0_0000..=0x1_FFFF => SlotAccess::Attribute(slot),
        0x2_0000..=0x2_FFFF => SlotAccess::Io(slot - 0x2_0000),
        // The odd-register window: the same 64 KiB of I/O space with A0
        // driven high, so a byte at $A30000+2n reaches register 2n+1.
        0x3_0000..=0x3_FFFF => SlotAccess::Io((slot - 0x3_0000) | 1),
        _ => SlotAccess::Reset,
    })
}

/// What kind of card `[pcmcia] card` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardKind {
    Cf,
    Sram,
}

impl CardKind {
    pub fn token(self) -> &'static str {
        match self {
            Self::Cf => "cf",
            Self::Sram => "sram",
        }
    }
}

/// A card in the slot.
#[derive(serde::Serialize, serde::Deserialize)]
pub enum PcmciaCard {
    /// Boxed: the ATA engine's sector buffer makes it far larger than
    /// an SRAM card's header.
    Cf(Box<CfCard>),
    Sram(SramCard),
}

impl PcmciaCard {
    /// A CF card in the slot.
    pub fn cf(card: CfCard) -> Self {
        Self::Cf(Box::new(card))
    }

    pub fn kind(&self) -> CardKind {
        match self {
            Self::Cf(_) => CardKind::Cf,
            Self::Sram(_) => CardKind::Sram,
        }
    }

    /// The slot pins this card drives, in Gayle's status-register layout.
    pub fn pins(&self) -> u8 {
        match self {
            Self::Cf(card) => card.pins(),
            Self::Sram(card) => card.pins(),
        }
    }

    /// One line for logs, OSDs, and `pcmcia.query`.
    pub fn describe(&self) -> String {
        match self {
            Self::Cf(card) => card.describe(),
            Self::Sram(card) => card.describe(),
        }
    }

    /// The host path behind the card, if a file backs it.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Cf(card) => Some(card.ata_path()),
            Self::Sram(card) => card.path.as_deref(),
        }
    }

    /// The card's RESET pin: asserted by the system reset line and by a
    /// write to `$A40000`. A CF card drops its configuration registers and
    /// resets its ATA function; an SRAM card has nothing to reset.
    pub fn reset(&mut self) {
        if let Self::Cf(card) = self {
            card.reset();
        }
    }

    pub fn read_attribute(&mut self, off: u32) -> u8 {
        match self {
            Self::Cf(card) => card.read_attribute(off),
            Self::Sram(card) => card.read_attribute(off),
        }
    }

    pub fn write_attribute(&mut self, off: u32, value: u8) {
        if let Self::Cf(card) = self {
            card.write_attribute(off, value);
        }
    }

    /// A byte or word of common memory. `size` is 1 or 2; a word starts at
    /// an even offset.
    pub fn read_common(&mut self, off: u32, size: usize) -> u32 {
        match self {
            Self::Cf(card) => card.read_common(off, size),
            Self::Sram(card) => card.read_common(off, size),
        }
    }

    pub fn write_common(&mut self, off: u32, size: usize, value: u32) {
        match self {
            Self::Cf(card) => card.write_common(off, size, value),
            Self::Sram(card) => card.write_common(off, size, value),
        }
    }

    /// An I/O cycle. A memory card does not answer them: the bus floats.
    pub fn read_io(&mut self, off: u32, size: usize) -> Option<u32> {
        match self {
            Self::Cf(card) => card.read_io(off, size),
            Self::Sram(_) => None,
        }
    }

    pub fn write_io(&mut self, off: u32, size: usize, value: u32) {
        if let Self::Cf(card) = self {
            card.write_io(off, size, value);
        }
    }

    /// Drain the activity latch (data-port traffic, command issue), for
    /// the HDD LED.
    pub fn take_activity(&mut self) -> bool {
        match self {
            Self::Cf(card) => card.ata.take_activity(),
            Self::Sram(card) => std::mem::take(&mut card.activity),
        }
    }

    /// Write an SRAM card's contents back to its backing file if they
    /// changed. A CF card writes through its image as it goes.
    pub fn flush(&mut self) {
        if let Self::Sram(card) = self {
            card.flush();
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn pending_host_disks(&self, out: &mut Vec<(String, String, bool)>) {
        if let Self::Cf(card) = self {
            card.ata.pending_host_disks(out);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn materialize_host_disks(&mut self) -> anyhow::Result<()> {
        match self {
            Self::Cf(card) => card.ata.materialize_host_disks(),
            Self::Sram(_) => Ok(()),
        }
    }

    pub fn release_host_disks(&mut self) -> usize {
        match self {
            Self::Cf(card) => card.ata.release_host_disks(),
            Self::Sram(_) => 0,
        }
    }
}

// ----- CompactFlash / PCMCIA ATA card ---------------------------------------

/// Configuration Option Register bits (attribute `$200`).
const COR_SRESET: u8 = 0x80;
const COR_INDEX_MASK: u8 = 0x3F;
/// Attribute-memory offsets of the card configuration registers.
const COR_OFFSET: u32 = 0x200;
const CCSR_OFFSET: u32 = 0x202;
const PRR_OFFSET: u32 = 0x204;
const SCR_OFFSET: u32 = 0x206;

/// A CompactFlash card (PCMCIA ATA): the ATA engine behind the CF register
/// layout, plus the CIS and configuration registers in attribute memory.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct CfCard {
    ata: AtaBus,
    /// The CIS, one byte per even attribute address.
    cis: Vec<u8>,
    /// Configuration Option Register: bit 7 soft reset, bit 6 LevlREQ,
    /// bits 5-0 the configuration index.
    cor: u8,
    /// Card Configuration and Status Register.
    ccsr: u8,
    /// Pin Replacement Register.
    prr: u8,
    /// Socket and Copy Register.
    scr: u8,
}

impl CfCard {
    /// Wrap an ATA drive (an image opened by [`IdeDrive::open`] or a real
    /// disk from [`IdeDrive::open_host_disk`]) as a CF card.
    pub fn new(drive: IdeDrive) -> Self {
        let mut ata = AtaBus::new();
        ata.attach_drive(0, drive);
        Self {
            ata,
            cis: cf_cis(),
            cor: 0,
            ccsr: 0,
            prr: 0,
            scr: 0,
        }
    }

    /// Open a hard-disk image (anything the shared drive backend accepts:
    /// an RDB HDF, a bare partition hardfile, a gzip-compressed image, or
    /// a host directory) as the card's medium.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let disk = crate::harddrive::HardDriveImage::open(
            path,
            "CF0",
            "pcmcia",
            None,
            0,
            crate::diskimage::FileSystem::FFS,
        )?;
        Ok(Self::new(IdeDrive::from_disk(disk)))
    }

    fn ata_path(&self) -> &Path {
        self.ata.drive_path(0).unwrap_or_else(|| Path::new(""))
    }

    /// The hard disk behind the card, for frontend save persistence.
    pub fn hard_disk_mut(&mut self) -> Option<&mut crate::harddrive::HardDriveImage> {
        self.ata.hard_disk_mut(0)
    }

    fn describe(&self) -> String {
        let sectors = self.ata.drive_total_sectors(0).unwrap_or(0);
        format!(
            "CF card {} ({} MiB)",
            self.ata_path().display(),
            sectors * crate::ata::SECTOR_SIZE as u64 / (1024 * 1024)
        )
    }

    /// CF cards drive WP low (never protected) and, as memory cards before
    /// configuration, BVD1/BVD2 high (STSCHG#/SPKR inactive); READY is high
    /// since the ATA engine completes commands within the access. In an I/O
    /// configuration the same pin carries IREQ#, asserted by the ATA
    /// function's INTRQ.
    fn pins(&self) -> u8 {
        let mut pins = PIN_CCDET | PIN_BVD1 | PIN_BVD2 | PIN_WR;
        if self.io_configured() && self.ata.irq_level() {
            pins |= PIN_BSY_IRQ;
        }
        pins
    }

    fn config_index(&self) -> u8 {
        self.cor & COR_INDEX_MASK
    }

    /// Whether the card is in an I/O configuration (index 1-3), as opposed
    /// to the power-on memory-mapped one (index 0).
    fn io_configured(&self) -> bool {
        matches!(self.config_index(), 1..=3)
    }

    fn reset(&mut self) {
        self.cor = 0;
        self.ccsr = 0;
        self.prr = 0;
        self.scr = 0;
        self.ata.reset();
    }

    fn read_attribute(&mut self, off: u32) -> u8 {
        // Attribute memory is eight bits wide on the even byte; the odd
        // byte is not driven and reads as its even neighbour.
        let even = off & !1;
        match even {
            COR_OFFSET => self.cor,
            CCSR_OFFSET => self.ccsr,
            PRR_OFFSET => self.prr,
            SCR_OFFSET => self.scr,
            _ => self.cis.get((even / 2) as usize).copied().unwrap_or(0xFF),
        }
    }

    fn write_attribute(&mut self, off: u32, value: u8) {
        match off & !1 {
            COR_OFFSET => {
                if value & COR_SRESET != 0 {
                    // Soft reset through the COR: the function resets and
                    // the register keeps only the reset bit until it is
                    // written clear.
                    self.ata.reset();
                    self.cor = COR_SRESET;
                } else {
                    self.cor = value & (COR_INDEX_MASK | 0x40);
                    if crate::envcfg::flag("COPPERLINE_DIAG_GAYLE") {
                        log::info!("pcmcia: CF configuration index {}", self.config_index());
                    }
                }
            }
            CCSR_OFFSET => self.ccsr = value & 0x4E,
            PRR_OFFSET => self.prr = value,
            SCR_OFFSET => self.scr = value,
            _ => {}
        }
    }

    /// The CF task-file register at a 16-byte-block offset (memory-mapped
    /// and contiguous-I/O layouts alike, CF+ spec table "ATA register
    /// layout"): 0-7 the ATA task file, 8/9 the data register again, `$D`
    /// error/feature again, `$E` alternate status / device control, `$F`
    /// the drive address register.
    fn block_reg(off: u32) -> Option<IdeReg> {
        Some(match off & 0xF {
            0x0 | 0x8 | 0x9 => IdeReg::Data,
            0x1 | 0xD => IdeReg::ErrorFeature,
            0x2 => IdeReg::SectorCount,
            0x3 => IdeReg::SectorNumber,
            0x4 => IdeReg::CylLow,
            0x5 => IdeReg::CylHigh,
            0x6 => IdeReg::DriveHead,
            0x7 => IdeReg::StatusCommand,
            0xE => IdeReg::AltStatusDevCtl,
            _ => return None,
        })
    }

    /// Memory-mapped layout (configuration 0): the register block at the
    /// start of each 2 KiB of common memory, with `$400`-`$7FF` a window
    /// onto the data register for block transfers. Only A10-A0 take part
    /// in the decode.
    fn common_reg(off: u32) -> Option<IdeReg> {
        match off & 0x7FF {
            0x000..=0x00F => Self::block_reg(off),
            0x400..=0x7FF => Some(IdeReg::Data),
            _ => None,
        }
    }

    /// I/O layouts: contiguous 16 registers (configuration 1, A3-A0
    /// decoded), PC primary (2: `$1F0`-`$1F7`, `$3F6`-`$3F7`) or secondary
    /// (3: `$170`-`$177`, `$376`-`$377`).
    fn io_reg(&self, off: u32) -> Option<IdeReg> {
        let off = off & 0xFFFF;
        match self.config_index() {
            1 => Self::block_reg(off),
            2 | 3 => {
                let (task, control) = if self.config_index() == 2 {
                    (0x1F0, 0x3F6)
                } else {
                    (0x170, 0x376)
                };
                if (task..task + 8).contains(&off) {
                    Self::block_reg(off - task)
                } else if off == control {
                    Some(IdeReg::AltStatusDevCtl)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// One byte or word of a register block. A word lands the even
    /// register on the CPU's high byte (Gayle's lane swap) and, for the
    /// data register, is the whole 16-bit data word.
    fn read_regs(
        &mut self,
        reg_at: &dyn Fn(&Self, u32) -> Option<IdeReg>,
        off: u32,
        size: usize,
    ) -> u32 {
        let reg = reg_at(self, off);
        if size == 2 {
            if reg == Some(IdeReg::Data) {
                return self.ata.read_reg(reg, 2);
            }
            let hi = self.read_regs(reg_at, off, 1);
            let lo = self.read_regs(reg_at, off | 1, 1);
            return (hi << 8) | lo;
        }
        match reg {
            // A byte read of the data port takes the even (first) byte.
            Some(IdeReg::Data) => self.ata.read_reg(reg, 1) & 0xFF,
            Some(_) => self.ata.read_reg(reg, 1) & 0xFF,
            None => 0,
        }
    }

    fn write_regs(
        &mut self,
        reg_at: &dyn Fn(&Self, u32) -> Option<IdeReg>,
        off: u32,
        size: usize,
        value: u32,
    ) {
        let reg = reg_at(self, off);
        if size == 2 {
            if reg == Some(IdeReg::Data) {
                self.ata.write_reg(reg, 2, value & 0xFFFF);
                return;
            }
            self.write_regs(reg_at, off, 1, (value >> 8) & 0xFF);
            self.write_regs(reg_at, off | 1, 1, value & 0xFF);
            return;
        }
        if reg.is_some() {
            self.ata.write_reg(reg, 1, value & 0xFF);
        }
    }

    fn read_common(&mut self, off: u32, size: usize) -> u32 {
        if self.io_configured() {
            // In an I/O configuration the registers leave common memory.
            return 0;
        }
        self.read_regs(&|_, o| Self::common_reg(o), off, size)
    }

    fn write_common(&mut self, off: u32, size: usize, value: u32) {
        if self.io_configured() {
            return;
        }
        self.write_regs(&|_, o| Self::common_reg(o), off, size, value);
    }

    fn read_io(&mut self, off: u32, size: usize) -> Option<u32> {
        if !self.io_configured() {
            return None;
        }
        Some(self.read_regs(&Self::io_reg, off, size))
    }

    fn write_io(&mut self, off: u32, size: usize, value: u32) {
        if !self.io_configured() {
            return;
        }
        self.write_regs(&Self::io_reg, off, size, value);
    }
}

/// The CIS a CF card presents, tuple for tuple the layout of the CF+ and
/// CompactFlash specification's example CIS (as shipped by SanDisk-era
/// cards and copied by WinUAE): device, JEDEC, version, function
/// (fixed disk, ATA interface), configuration registers at `$200`, and
/// one configuration table entry per addressing mode (0 memory-mapped,
/// 1 contiguous I/O, 2 primary, 3 secondary).
fn cf_cis() -> Vec<u8> {
    let mut cis = Vec::with_capacity(256);
    // CISTPL_DEVICE: 250 ns function-specific device, 2 units.
    cis.extend_from_slice(&[0x01, 0x04, 0xDF, 0x4A, 0x01, 0xFF]);
    // CISTPL_DEVICE_OC: same at 3.3 V.
    cis.extend_from_slice(&[0x1C, 0x04, 0x02, 0xD9, 0x01, 0xFF]);
    // CISTPL_JEDEC_C.
    cis.extend_from_slice(&[0x18, 0x02, 0xDF, 0x01]);
    // CISTPL_VERS_1: PCMCIA 2.1, manufacturer / product / info strings.
    let strings: &[&[u8]] = &[b"COPPERLINE", b"68000", b"PCMCIA ATA CARD", b"1.0"];
    let mut vers = vec![0x04, 0x01];
    for s in strings {
        vers.extend_from_slice(s);
        vers.push(0);
    }
    vers.push(0xFF);
    cis.push(0x15);
    cis.push(vers.len() as u8);
    cis.extend_from_slice(&vers);
    // CISTPL_FUNCID: fixed disk, initialise at POST.
    cis.extend_from_slice(&[0x21, 0x02, 0x04, 0x01]);
    // CISTPL_FUNCE: disk interface = ATA.
    cis.extend_from_slice(&[0x22, 0x02, 0x01, 0x01]);
    // CISTPL_FUNCE: ATA extension, 5 V / 3.3 V, unique 16-bit data.
    cis.extend_from_slice(&[0x22, 0x03, 0x02, 0x0C, 0x0F]);
    // CISTPL_CONFIG: last index 3, registers at $200, COR/CCSR/PRR/SCR.
    cis.extend_from_slice(&[0x1A, 0x05, 0x01, 0x03, 0x00, 0x02, 0x0F]);
    // CISTPL_CFTABLE_ENTRY 0 (default): memory interface, 2 KiB of common
    // memory at offset 0 for the register block.
    cis.extend_from_slice(&[
        0x1B, 0x0B, 0xC0, 0xC0, 0xA1, 0x27, 0x55, 0x4D, 0x5D, 0x75, 0x08, 0x00, 0x21,
    ]);
    // CISTPL_CFTABLE_ENTRY 1: contiguous I/O, 16 registers, 8/16-bit.
    cis.extend_from_slice(&[
        0x1B, 0x0D, 0xC1, 0x41, 0x99, 0x27, 0x55, 0x4D, 0x5D, 0x75, 0x64, 0xF0, 0xFF, 0xFF, 0x20,
    ]);
    // CISTPL_CFTABLE_ENTRY 2: primary I/O ($1F0-$1F7, $3F6-$3F7).
    cis.extend_from_slice(&[
        0x1B, 0x12, 0xC2, 0x41, 0x99, 0x27, 0x55, 0x4D, 0x5D, 0x75, 0xEA, 0x61, 0xF0, 0x01, 0x07,
        0xF6, 0x03, 0x01, 0xEE, 0x20,
    ]);
    // CISTPL_CFTABLE_ENTRY 3: secondary I/O ($170-$177, $376-$377).
    cis.extend_from_slice(&[
        0x1B, 0x12, 0xC3, 0x41, 0x99, 0x27, 0x55, 0x4D, 0x5D, 0x75, 0xEA, 0x61, 0x70, 0x01, 0x07,
        0x76, 0x03, 0x01, 0xEE, 0x20,
    ]);
    // CISTPL_NO_LINK, CISTPL_END.
    cis.extend_from_slice(&[0x14, 0x00, 0xFF]);
    cis
}

// ----- SRAM memory card -----------------------------------------------------

/// An SRAM memory card: up to 4 MiB of battery-backed static RAM in the
/// common window, optionally mirrored to a host file, with the CIS that
/// Kickstart's card.resource reads to size and add it as credit-card RAM.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SramCard {
    data: Vec<u8>,
    cis: Vec<u8>,
    /// Backing file: read at insert, written back on flush and eject.
    path: Option<PathBuf>,
    /// The write-protect switch.
    read_only: bool,
    #[serde(skip)]
    dirty: bool,
    #[serde(skip)]
    activity: bool,
}

impl SramCard {
    /// Build a card of `size` bytes. With a `path`, the file's contents
    /// (up to `size`) seed the RAM and a missing file is created at the
    /// first flush; without one the RAM starts zeroed and lives only for
    /// the session (and inside save states).
    pub fn new(size: usize, path: Option<&Path>, read_only: bool) -> anyhow::Result<Self> {
        let encoded = sram_device_size_byte(size).ok_or_else(|| {
            anyhow::anyhow!(
                "PCMCIA SRAM size {} bytes cannot be described by a CISTPL_DEVICE tuple: \
                 use a multiple of 64 KiB up to 1 MiB, or of 128 KiB up to 4 MiB",
                size
            )
        })?;
        let mut data = vec![0u8; size];
        if let Some(path) = path {
            match std::fs::read(path) {
                Ok(bytes) => {
                    let n = bytes.len().min(size);
                    data[..n].copy_from_slice(&bytes[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(anyhow::Error::new(e)
                        .context(format!("PCMCIA SRAM backing file {}", path.display())))
                }
            }
        }
        Ok(Self {
            data,
            cis: sram_cis(size, encoded, read_only),
            path: path.map(Path::to_path_buf),
            read_only,
            dirty: false,
            activity: false,
        })
    }

    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// The card's RAM, for tests and the debugger.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    fn describe(&self) -> String {
        let backing = match &self.path {
            Some(p) => format!(" ({})", p.display()),
            None => String::new(),
        };
        format!(
            "SRAM card {} KiB{}{}",
            self.data.len() / 1024,
            backing,
            if self.read_only {
                ", write-protected"
            } else {
                ""
            }
        )
    }

    /// Battery good (BVD1/BVD2 high), READY high, WP from the switch.
    fn pins(&self) -> u8 {
        let mut pins = PIN_CCDET | PIN_BVD1 | PIN_BVD2;
        if !self.read_only {
            pins |= PIN_WR;
        }
        pins
    }

    fn read_attribute(&self, off: u32) -> u8 {
        self.cis
            .get(((off & !1) / 2) as usize)
            .copied()
            .unwrap_or(0xFF)
    }

    fn read_common(&mut self, off: u32, size: usize) -> u32 {
        let off = off as usize;
        // Beyond the card's own size the common window is undecoded.
        if off + size > self.data.len() {
            return 0;
        }
        self.activity = true;
        if size == 2 {
            (u32::from(self.data[off]) << 8) | u32::from(self.data[off + 1])
        } else {
            u32::from(self.data[off])
        }
    }

    fn write_common(&mut self, off: u32, size: usize, value: u32) {
        let off = off as usize;
        if self.read_only || off + size > self.data.len() {
            return;
        }
        self.activity = true;
        self.dirty = true;
        if size == 2 {
            self.data[off] = (value >> 8) as u8;
            self.data[off + 1] = value as u8;
        } else {
            self.data[off] = value as u8;
        }
    }

    /// Whether unsaved writes are waiting for a flush.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Write the RAM back to the backing file.
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(path) = &self.path else {
            return;
        };
        match std::fs::write(path, &self.data) {
            Ok(()) => self.dirty = false,
            Err(e) => log::warn!(
                "pcmcia: SRAM card write-back to {} failed: {e}",
                path.display()
            ),
        }
    }
}

impl Drop for SramCard {
    fn drop(&mut self) {
        self.flush();
    }
}

/// The CISTPL_DEVICE size byte for an SRAM card: bits 2-0 a unit size
/// (512 B, 2 K, 8 K, 32 K, 128 K, 512 K, 2 M), bits 7-3 the unit count
/// minus one. Picks the largest unit that divides the size into at most
/// 32 units; `None` when no unit does.
pub fn sram_device_size_byte(size: usize) -> Option<u8> {
    if size == 0 {
        return None;
    }
    const UNITS: [usize; 7] = [512, 2048, 8192, 32768, 131_072, 524_288, 2_097_152];
    UNITS
        .iter()
        .enumerate()
        .rev()
        .find(|(_, &unit)| size.is_multiple_of(unit) && size / unit <= 32)
        .map(|(code, &unit)| (((size / unit - 1) as u8) << 3) | code as u8)
}

/// The CIS card.resource reads off an SRAM card (the tuple set WinUAE
/// established as sufficient for Kickstart 2.05-3.x to add the card as
/// credit-card RAM): device (SRAM, 100 ns, size), geometry, version,
/// function (memory card), manufacturer, end.
fn sram_cis(size: usize, size_byte: u8, read_only: bool) -> Vec<u8> {
    let mut cis = Vec::with_capacity(96);
    // CISTPL_DEVICE: DTYPE_SRAM, WPS from the switch, DSPEED 100 ns.
    let device_id = (6 << 4) | if read_only { 8 } else { 0 } | 4;
    cis.extend_from_slice(&[0x01, 0x03, device_id, size_byte, 0xFF]);
    // CISTPL_DEVICEGEO: 16-bit bus, erase/read/write geometry of 1.
    cis.extend_from_slice(&[0x1E, 0x07, 0x02, 0x00, 0x01, 0x01, 0x01, 0x01, 0xFF]);
    // CISTPL_VERS_1.
    let info = format!("{}KB SRAM CARD", size / 1024);
    let strings: [&[u8]; 3] = [b"COPPERLINE", b"68000", info.as_bytes()];
    let mut vers = vec![0x04, 0x01];
    for s in strings {
        vers.extend_from_slice(s);
        vers.push(0);
    }
    vers.push(0xFF);
    cis.push(0x15);
    cis.push(vers.len() as u8);
    cis.extend_from_slice(&vers);
    // CISTPL_FUNCID: memory card.
    cis.extend_from_slice(&[0x21, 0x02, 0x01, 0x00]);
    // CISTPL_MANFID.
    cis.extend_from_slice(&[0x20, 0x04, 0xFF, 0xFF, 0x01, 0x01]);
    // CISTPL_END.
    cis.push(0xFF);
    cis
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_windows_decode_as_gayle_wires_them() {
        assert_eq!(classify(0x0060_0000), Some(SlotAccess::Common(0)));
        assert_eq!(classify(0x009F_FFFF), Some(SlotAccess::Common(0x3F_FFFF)));
        assert_eq!(classify(0x00A0_0000), Some(SlotAccess::Attribute(0)));
        assert_eq!(classify(0x00A1_FFFE), Some(SlotAccess::Attribute(0x1_FFFE)));
        assert_eq!(classify(0x00A2_01F0), Some(SlotAccess::Io(0x1F0)));
        // The odd-byte window forces A0 high: $A30000 + 2n is register 2n+1.
        assert_eq!(classify(0x00A3_0000), Some(SlotAccess::Io(1)));
        assert_eq!(classify(0x00A3_03F6), Some(SlotAccess::Io(0x3F7)));
        assert_eq!(classify(0x00A4_0000), Some(SlotAccess::Reset));
        assert_eq!(classify(0x00A7_FFFF), Some(SlotAccess::Reset));
        assert_eq!(classify(0x00A8_0000), None);
        assert_eq!(classify(0x005F_FFFF), None);
        assert!(decodes(0x0070_0000));
        assert!(!decodes(0x00DA_8000));
    }

    #[test]
    fn sram_size_byte_follows_the_cistpl_device_encoding() {
        // 4 MiB = 2 units of 2 MiB: count-1 = 1, code 6.
        assert_eq!(sram_device_size_byte(4 * 1024 * 1024), Some((1 << 3) | 6));
        // 512 KiB = 1 unit of 512 KiB.
        assert_eq!(sram_device_size_byte(512 * 1024), Some(5));
        // 64 KiB = 2 units of 32 KiB.
        assert_eq!(sram_device_size_byte(64 * 1024), Some((1 << 3) | 3));
        // 1088 KiB (17 x 64K) has no unit that fits 32 units.
        assert_eq!(sram_device_size_byte(17 * 64 * 1024), None);
        assert_eq!(sram_device_size_byte(0), None);
    }

    #[test]
    fn sram_card_cis_names_an_sram_device_and_the_ram_reads_back() {
        let mut card = SramCard::new(512 * 1024, None, false).unwrap();
        // CISTPL_DEVICE at attribute 0: tuple code, link, device id
        // (SRAM type 6, 100 ns), size byte, end-of-devices.
        assert_eq!(card.read_attribute(0), 0x01);
        assert_eq!(card.read_attribute(2), 0x03);
        assert_eq!(card.read_attribute(4), 0x64);
        assert_eq!(card.read_attribute(6), 5);
        assert_eq!(card.read_attribute(8), 0xFF);
        // Odd attribute bytes mirror their even neighbour.
        assert_eq!(card.read_attribute(1), 0x01);
        assert_eq!(card.pins(), PIN_CCDET | PIN_BVD1 | PIN_BVD2 | PIN_WR);

        card.write_common(0x1000, 2, 0xBEEF);
        card.write_common(0x1002, 1, 0x42);
        assert_eq!(card.read_common(0x1000, 2), 0xBEEF);
        assert_eq!(card.read_common(0x1000, 1), 0xBE);
        assert_eq!(card.read_common(0x1002, 1), 0x42);
        assert!(card.is_dirty());
        // Past the card's own size the window is undecoded.
        assert_eq!(card.read_common(512 * 1024, 2), 0);

        let protected = SramCard::new(64 * 1024, None, true).unwrap();
        assert_eq!(protected.pins() & PIN_WR, 0);
        assert_eq!(protected.read_attribute(4) & 0x08, 0x08, "WPS bit");
    }

    #[test]
    fn sram_card_backing_file_round_trips() {
        let path = std::env::temp_dir().join(format!(
            "copperline-pcmcia-sram-{}-{:?}.bin",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_file(&path).ok();
        {
            let mut card = SramCard::new(64 * 1024, Some(&path), false).unwrap();
            card.write_common(0x10, 2, 0x1234);
            card.flush();
        }
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 64 * 1024);
        assert_eq!(&bytes[0x10..0x12], &[0x12, 0x34]);
        let mut again = SramCard::new(64 * 1024, Some(&path), false).unwrap();
        assert_eq!(again.read_common(0x10, 2), 0x1234);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn cf_cis_declares_an_ata_fixed_disk_with_config_registers_at_200() {
        let cis = cf_cis();
        assert_eq!(&cis[..2], &[0x01, 0x04], "CISTPL_DEVICE first");
        let funcid = cis.windows(4).position(|w| w == [0x21, 0x02, 0x04, 0x01]);
        assert!(funcid.is_some(), "CISTPL_FUNCID fixed disk");
        let config = cis
            .windows(7)
            .position(|w| w == [0x1A, 0x05, 0x01, 0x03, 0x00, 0x02, 0x0F]);
        assert!(config.is_some(), "CISTPL_CONFIG: last index 3, base $200");
        assert_eq!(*cis.last().unwrap(), 0xFF);
        // Four configuration table entries, indices 0-3.
        let indices: Vec<u8> = cis
            .windows(3)
            .filter(|w| w[0] == 0x1B)
            .map(|w| w[2] & 0x3F)
            .collect();
        assert_eq!(indices, [0, 1, 2, 3]);
    }
}
