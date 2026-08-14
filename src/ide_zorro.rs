// SPDX-License-Identifier: GPL-3.0-or-later

//! A built-in Zorro II IDE controller compatible with LIV2's open-source
//! `lide.device`, in three AutoConfig personalities: **RIPPLE** (the primary
//! open-hardware card, two ATA channels), **RIDE** (an expansion-port board
//! sharing RIPPLE's ROM image and register layout, one channel), and
//! **AT-Bus 2008** (the register model shared by that board's whole clone
//! family, one channel). Drives may be hard disks or ATAPI CD-ROMs, either a
//! `.cue`/`.iso`/`.chd` image on any `[lide] drives` slot or a real host
//! block device attached via `[[host_disk]] attach = "lide0-master"` (etc).
//! The boot ROM is always user-supplied (a `lide.rom` / `lide-atbus.rom`
//! release image from <https://github.com/LIV2/lide.device>) -- nothing is
//! bundled or distributed.
//!
//! ## Register map (from the LIV2 CPLD RTL)
//!
//! Each ATA channel decodes a 4K block of the board window into eight
//! task-file registers, using `(offset >> 9) & 7` as the register index --
//! ATA A0-A2 are wired to CPU A9-A11, so every register is mirrored across
//! its whole 512-byte slot (the trick that lets the driver bulk-transfer a
//! sector with `movem.l`). Registers other than the 16-bit data port sit on
//! the *upper* byte lane (even addresses, D15-D8); the odd lane and
//! unpopulated slots float 0xFF. A second 4K block per channel is the
//! control block, whose alternate-status/device-control register is fixed at
//! register index 6 (offset `+0xC00` within the block).
//!
//! RIPPLE has two channels (four drives): channel 0 at window offset
//! `0x1000`/control `0x5000`, channel 1 at `0x2000`/`0x6000`. This follows
//! the RTL's own chip-select decode (two selects per physical connector --
//! task file and control block -- rather than two separate channels sharing
//! one register layout), and has been confirmed against a real `lide.rom`
//! download: RIPPLE autoconfigs, the DiagArea runs, and `lide.device` loads
//! as a resident module and finds channel 0's drive. RIDE and AT-Bus 2008
//! have one channel: task file at `0x1000`, control block at `0x2000` (so
//! `+0x2C00` is the alternate-status register the driver's channel-autodetect
//! polls against `+0x1E00`); both have also booted successfully to a real
//! Workbench against real release ROMs.
//!
//! A channel with *no* drives attached at all (as opposed to one drive
//! present and the other slot empty) must float every register, not only
//! status -- [`AtaBus::read_reg`] only special-cases status/alt-status for
//! "no drive selected", so the register-block reader checks
//! [`AtaBus::any_drive_attached`] itself and floats the rest. Without this,
//! device/head reads back a hard zero on an empty channel, which real
//! `lide.device` reads as "a device answered" and polls forever waiting for
//! it to respond -- reproduced by booting RIPPLE with only channel 0
//! populated before this check was added.
//!
//! ## ROM window, the enable latch, and banking
//!
//! The flash is byte-wide, so each 32 KiB bank fills 64 KiB of window at
//! even byte addresses (stride 2) -- odd addresses on AT-Bus 2008, whose
//! `er_InitDiagVec` is 1 rather than 8 for exactly this reason. Before the
//! first write anywhere in the board window, ROM covers the *whole* window,
//! selecting bank 0 in the low 64K and bank 1 (the optional CD filesystem)
//! in the upper 64K. That first write latches `ide_enabled`; afterwards ROM
//! remains only in the upper 64K (RIPPLE also keeps it in the low 64K
//! wherever address bits 12 and 13 agree -- which is exactly where the task
//! file and control blocks above are *not*). AT-Bus 2008 has no latch and no
//! banking: its 32K image sits on the odd lane across the whole window,
//! always, alongside the even-lane registers.
//!
//! The bank register is written anywhere in `0x8000..0x10000`: bits 7:6 of
//! the lane byte select the bank (RIPPLE: 2 banks, write-only; RIDE: 4
//! banks, readable back with `otherram_en`/`maprom_en` on the next nibble
//! down). AT-Bus 2008 has no bank register at all.
//!
//! ## No interrupts
//!
//! None of the three boards wire ATA INTRQ anywhere -- `lide.device` is a
//! purely polling driver. [`IdeZorro`] never asserts INT2/INT6.

use crate::ata::{AtaBus, AtaDevice, IdeReg};
use anyhow::{bail, Context, Result};
use std::path::Path;

/// Bytes in one flash bank (`lide.rom`/`lide-atbus.rom` are exactly this
/// size; a second bank, e.g. `cdfs.rom`, is the same size again).
pub const ROM_BANK_SIZE: usize = 0x8000;

/// Which of the three AutoConfig identities this board presents.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LidePersonality {
    /// LIV2 RIPPLE: manufacturer 0x144A, product 7, 128K window, two ATA
    /// channels (four drives).
    #[default]
    Ripple,
    /// LIV2 RIDE (IDE identity only -- the companion RAM board is not
    /// emulated): manufacturer 0x144A, product 9, 128K window, one channel.
    Ride,
    /// AT-Bus 2008 and its clone family: manufacturer 0x082C, product 6,
    /// 64K window, one channel, no ROM banking.
    AtBus2008,
}

impl LidePersonality {
    /// Size of the AutoConfig window.
    pub fn window_size(self) -> u32 {
        match self {
            LidePersonality::Ripple | LidePersonality::Ride => 0x2_0000,
            LidePersonality::AtBus2008 => 0x1_0000,
        }
    }

    /// Number of ATA channels (each with a master/slave pair).
    pub fn channels(self) -> usize {
        match self {
            LidePersonality::Ripple => 2,
            LidePersonality::Ride | LidePersonality::AtBus2008 => 1,
        }
    }

    /// Maximum drives this personality can carry.
    pub fn max_drives(self) -> usize {
        self.channels() * 2
    }

    /// Number of 32K flash banks the board can select.
    fn bank_count(self) -> u8 {
        match self {
            LidePersonality::Ripple => 2,
            LidePersonality::Ride => 4,
            LidePersonality::AtBus2008 => 1,
        }
    }

    /// Whether the bank register reads back (RIDE only; RIPPLE's is
    /// write-only and AT-Bus 2008 has none).
    fn bank_readable(self) -> bool {
        matches!(self, LidePersonality::Ride)
    }

    /// Whether the board latches `ide_enabled` on its first write (RIPPLE
    /// and RIDE). AT-Bus 2008 has no latch: registers are always live and
    /// ROM is always present on the odd lane.
    fn has_latch(self) -> bool {
        !matches!(self, LidePersonality::AtBus2008)
    }

    /// Whether the ROM sits on the odd byte lane (AT-Bus 2008) rather than
    /// the even lane every LIV2 board uses.
    fn rom_lane_odd(self) -> bool {
        matches!(self, LidePersonality::AtBus2008)
    }

    fn channel_task_base(self, ch: usize) -> u32 {
        0x1000 + (ch as u32) * 0x1000
    }

    fn channel_ctrl_base(self, ch: usize) -> u32 {
        match self {
            LidePersonality::Ripple => 0x5000 + (ch as u32) * 0x1000,
            LidePersonality::Ride | LidePersonality::AtBus2008 => 0x2000,
        }
    }

    /// A stable identifier for logging, config parsing, and savestate `kind`.
    pub fn name(self) -> &'static str {
        match self {
            LidePersonality::Ripple => "ripple",
            LidePersonality::Ride => "ride",
            LidePersonality::AtBus2008 => "atbus2008",
        }
    }
}

/// The task-file register at 512-byte-block index `idx` (`(off >> 9) & 7`).
fn task_index_reg(idx: u32) -> Option<IdeReg> {
    Some(match idx {
        0 => IdeReg::Data,
        1 => IdeReg::ErrorFeature,
        2 => IdeReg::SectorCount,
        3 => IdeReg::SectorNumber,
        4 => IdeReg::CylLow,
        5 => IdeReg::CylHigh,
        6 => IdeReg::DriveHead,
        7 => IdeReg::StatusCommand,
        _ => return None,
    })
}

/// The control block only populates index 6 (alternate status / device
/// control); every other slot in that block is unpopulated and floats.
fn ctrl_index_reg(idx: u32) -> Option<IdeReg> {
    (idx == 6).then_some(IdeReg::AltStatusDevCtl)
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct IdeZorro {
    personality: LidePersonality,
    ata: [AtaBus; 2],
    /// The flash image: empty (hardware-only mode, no autoboot), one 32K
    /// bank, or several concatenated banks (a CD filesystem as bank 1, or
    /// RIDE's up to four).
    flash: Vec<u8>,
    /// Selected flash bank (bits 7:6 of the lane byte last written to the
    /// bank register).
    bank: u8,
    /// RIDE's `otherram_en`/`maprom_en` bits, latched alongside `bank` and
    /// read back on the next nibble down; unused by RIPPLE/AT-Bus 2008.
    ride_ctrl: u8,
    /// Latches true on the first write anywhere in the window (RIPPLE/RIDE);
    /// starts true and never changes on AT-Bus 2008, which has no latch.
    ide_enabled: bool,
}

impl IdeZorro {
    /// Build the board. `flash` is empty for hardware-only mode (no ROM, no
    /// autoboot -- drives still work under a disk-loaded `lide.device`), or
    /// one or more concatenated 32K banks (see [`load_rom`](Self::load_rom)).
    pub fn new(personality: LidePersonality, flash: Vec<u8>) -> Result<Self> {
        if !flash.is_empty() {
            if !flash.len().is_multiple_of(ROM_BANK_SIZE) {
                bail!(
                    "lide ROM image is {} bytes; expected a multiple of 32768 \
                     (a lide.rom/lide-atbus.rom release image, optionally with cdfs.rom appended)",
                    flash.len()
                );
            }
            let banks = flash.len() / ROM_BANK_SIZE;
            if banks > personality.bank_count() as usize {
                bail!(
                    "lide ROM image carries {banks} bank(s) of 32768 bytes; {} only has {}",
                    personality.name(),
                    personality.bank_count()
                );
            }
        }
        // Hardware-only mode (no ROM) has nothing to unlatch: registers are
        // live from power-on, same as a board with no latch at all.
        let ide_enabled = !personality.has_latch() || flash.is_empty();
        Ok(Self {
            personality,
            ata: [AtaBus::new(), AtaBus::new()],
            flash,
            bank: 0,
            ride_ctrl: 0,
            ide_enabled,
        })
    }

    /// Load one 32K ROM bank image (`lide.rom`, `lide-atbus.rom`, or a
    /// second-bank image like `cdfs.rom`) from disk.
    ///
    /// A real EEPROM/flash chip is a fixed 32K regardless of how much of it
    /// the mask actually uses; unprogrammed cells read as `0xFF`. Distributed
    /// dumps (`cdfs.rom` in particular) commonly stop at the last meaningful
    /// byte rather than including that trailing fill, so a short file is
    /// padded out to the full bank size the same way -- not rejected. A file
    /// *larger* than one bank is still an error: that is unambiguously the
    /// wrong image (e.g. a two-bank release picked for a `rom_bank2` field
    /// that only takes one).
    pub fn load_rom(path: &Path) -> Result<Vec<u8>> {
        let mut rom =
            std::fs::read(path).with_context(|| format!("reading lide ROM {}", path.display()))?;
        if rom.len() > ROM_BANK_SIZE {
            bail!(
                "lide ROM {} is {} bytes; expected at most 32768 (a lide.rom/lide-atbus.rom \
                 release image, one bank at a time)",
                path.display(),
                rom.len()
            );
        }
        rom.resize(ROM_BANK_SIZE, 0xFF);
        Ok(rom)
    }

    pub fn attach_drive(&mut self, channel: usize, slot: usize, drive: impl Into<AtaDevice>) {
        self.ata[channel.min(1)].attach_drive(slot, drive);
    }

    /// The first ATAPI CD-ROM drive across this board's channels, if any
    /// slot holds one; the runtime disc-swap target.
    pub fn first_atapi_ref(&self) -> Option<&crate::scsi::ScsiCdRom> {
        self.ata.iter().find_map(AtaBus::first_atapi_ref)
    }

    /// Mutable counterpart of [`Self::first_atapi_ref`].
    pub fn first_atapi_mut(&mut self) -> Option<&mut crate::scsi::ScsiCdRom> {
        self.ata.iter_mut().find_map(AtaBus::first_atapi_mut)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn pending_host_disks(&self, out: &mut Vec<(String, String, bool)>) {
        for bus in &self.ata {
            bus.pending_host_disks(out);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn materialize_host_disks(&mut self) -> anyhow::Result<()> {
        for bus in &mut self.ata {
            bus.materialize_host_disks()?;
        }
        Ok(())
    }

    /// Let go of any real disk of the host's, and say how many went.
    pub fn release_host_disks(&mut self) -> usize {
        self.ata.iter_mut().map(AtaBus::release_host_disks).sum()
    }

    /// System reset: clear both ATA channels and, on boards with a latch,
    /// re-cover the window with ROM (matching a real board's power-on state).
    pub fn reset(&mut self) {
        for bus in &mut self.ata {
            bus.reset();
        }
        self.bank = 0;
        self.ride_ctrl = 0;
        self.ide_enabled = !self.personality.has_latch() || self.flash.is_empty();
    }

    /// Drain the activity latch for the HDD LED.
    pub fn take_activity(&mut self) -> bool {
        // Both buses must be drained regardless of the first result.
        let a = self.ata[0].take_activity();
        let b = self.ata[1].take_activity();
        a | b
    }

    pub fn kind(&self) -> &'static str {
        match self.personality {
            LidePersonality::Ripple => "lide-ripple",
            LidePersonality::Ride => "lide-ride",
            LidePersonality::AtBus2008 => "lide-atbus2008",
        }
    }

    // ----- address decode -------------------------------------------------

    /// Which channel/block a window offset falls in, if it names a task file
    /// or control block at all (regardless of whether registers are
    /// currently live -- callers check `ide_enabled` themselves, since the
    /// same addresses serve ROM before the enable latch).
    fn register_block(&self, off: u32) -> Option<(usize, bool)> {
        for ch in 0..self.personality.channels() {
            let task = self.personality.channel_task_base(ch);
            if (task..task + 0x1000).contains(&off) {
                return Some((ch, false));
            }
            let ctrl = self.personality.channel_ctrl_base(ch);
            if (ctrl..ctrl + 0x1000).contains(&off) {
                return Some((ch, true));
            }
        }
        None
    }

    fn read_register_block(&mut self, ch: usize, is_ctrl: bool, off: u32, size: usize) -> u32 {
        let idx = (off >> 9) & 7;
        let reg = if is_ctrl {
            ctrl_index_reg(idx)
        } else {
            task_index_reg(idx)
        };
        if !self.ata[ch].any_drive_attached() {
            // Nothing at all on this physical connector: every register
            // floats, not just status. AtaBus's "no drive selected" case
            // only special-cases status/alt-status; other registers would
            // otherwise read a hard zero, which a driver's probe can read as
            // "a device answered" rather than "empty channel" -- exactly the
            // gap that hung the real lide.device driver's RIPPLE channel-2
            // probe in testing (issue reproduced against a downloaded
            // lide.rom before this check was added).
            return if size == 1 { 0xFF } else { 0xFFFF };
        }
        match reg {
            Some(IdeReg::Data) => self.ata[ch].read_reg(Some(IdeReg::Data), size),
            Some(reg) => {
                let byte = self.ata[ch].read_reg(Some(reg), 1) as u8;
                if size == 1 {
                    u32::from(if off & 1 == 0 { byte } else { 0xFF })
                } else {
                    (u32::from(byte) << 8) | 0xFF
                }
            }
            None => {
                if size == 1 {
                    0xFF
                } else {
                    0xFFFF
                }
            }
        }
    }

    fn write_register_block(
        &mut self,
        ch: usize,
        is_ctrl: bool,
        off: u32,
        size: usize,
        value: u32,
    ) {
        let idx = (off >> 9) & 7;
        let reg = if is_ctrl {
            ctrl_index_reg(idx)
        } else {
            task_index_reg(idx)
        };
        match reg {
            Some(IdeReg::Data) => self.ata[ch].write_reg(Some(IdeReg::Data), size, value),
            Some(reg) => {
                let byte = match size {
                    1 if off & 1 == 0 => value & 0xFF,
                    1 => return, // odd lane: no register there
                    _ => (value >> 8) & 0xFF,
                };
                self.ata[ch].write_reg(Some(reg), 1, byte);
            }
            None => {}
        }
    }

    /// Whether ROM answers at `off` given the current latch state.
    fn rom_visible(&self, off: u32) -> bool {
        if self.flash.is_empty() {
            return false;
        }
        match self.personality {
            LidePersonality::AtBus2008 => true,
            _ if !self.ide_enabled => true,
            LidePersonality::Ripple => {
                if off < 0x1_0000 {
                    ((off >> 12) & 1) == ((off >> 13) & 1)
                } else {
                    true
                }
            }
            LidePersonality::Ride => off >= 0x1_0000,
        }
    }

    fn rom_bank_for_offset(&self, off: u32) -> u8 {
        match self.personality {
            LidePersonality::AtBus2008 => 0,
            _ if !self.ide_enabled => ((off >> 16) & 1) as u8,
            _ => self
                .bank
                .min(self.personality.bank_count().saturating_sub(1)),
        }
    }

    fn read_rom(&self, off: u32, size: usize) -> u32 {
        if size == 2 {
            let hi = self.read_rom(off, 1);
            let lo = self.read_rom(off.wrapping_add(1), 1);
            return (hi << 8) | lo;
        }
        let lane_bit = u32::from(self.personality.rom_lane_odd());
        if off & 1 != lane_bit {
            return 0xFF;
        }
        let bank = self.rom_bank_for_offset(off) as usize;
        let idx_in_bank = ((off & 0xFFFF) >> 1) as usize;
        let byte_idx = bank * ROM_BANK_SIZE + idx_in_bank;
        u32::from(self.flash.get(byte_idx).copied().unwrap_or(0xFF))
    }

    fn read_bank_reg(&self, size: usize) -> u32 {
        // D15:D14 = bank[1:0], D13 = otherram_en, D12 = maprom_en; the low
        // nibble of the upper byte and the whole lower byte are unspecified
        // (returned as zero here for determinism).
        let byte = ((self.bank & 0x3) << 6) | ((self.ride_ctrl & 0x3) << 4);
        if size == 1 {
            u32::from(byte)
        } else {
            u32::from(byte) << 8
        }
    }

    fn write_bank(&mut self, size: usize, value: u32) {
        let byte = if size == 1 {
            (value & 0xFF) as u8
        } else {
            (value >> 8) as u8
        };
        let requested = (byte >> 6) & 0x3;
        self.bank = requested.min(self.personality.bank_count().saturating_sub(1));
        if self.personality == LidePersonality::Ride {
            self.ride_ctrl = (byte >> 4) & 0x3;
        }
    }

    // ----- memory-mapped access --------------------------------------------

    pub fn read(&mut self, off: u32, size: usize) -> u32 {
        if size == 4 {
            let hi = self.read(off, 2);
            let lo = self.read(off.wrapping_add(2), 2);
            return (hi << 16) | lo;
        }
        let value = if let Some((ch, is_ctrl)) = self.register_block(off) {
            if self.personality.rom_lane_odd() && off & 1 == 1 && self.rom_visible(off) {
                self.read_rom(off, size)
            } else if self.ide_enabled {
                self.read_register_block(ch, is_ctrl, off, size)
            } else if self.rom_visible(off) {
                self.read_rom(off, size)
            } else if size == 1 {
                0xFF
            } else {
                0xFFFF
            }
        } else if self.personality.bank_readable()
            && self.ide_enabled
            && (0x8000..0x1_0000).contains(&off)
        {
            self.read_bank_reg(size)
        } else if self.rom_visible(off) {
            self.read_rom(off, size)
        } else if size == 1 {
            0xFF
        } else {
            0xFFFF
        };
        if crate::envcfg::flag("COPPERLINE_DIAG_LIDE") {
            log::info!("lide {} rd {off:#06X}/{size} -> {value:#06X}", self.kind());
        }
        value
    }

    pub fn write(&mut self, off: u32, size: usize, value: u32) {
        if size == 4 {
            self.write(off, 2, value >> 16);
            self.write(off.wrapping_add(2), 2, value & 0xFFFF);
            return;
        }
        if crate::envcfg::flag("COPPERLINE_DIAG_LIDE") {
            log::info!("lide {} wr {off:#06X}/{size} <- {value:#06X}", self.kind());
        }
        if self.personality.has_latch() && !self.ide_enabled {
            self.ide_enabled = true;
        }
        if self.personality.has_latch() && (0x8000..0x1_0000).contains(&off) {
            self.write_bank(size, value);
            return;
        }
        if let Some((ch, is_ctrl)) = self.register_block(off) {
            self.write_register_block(ch, is_ctrl, off, size, value);
        }
        // Otherwise the write lands on ROM or unpopulated space: no effect.
    }

    fn peek_word(&self, off: u32) -> Option<u16> {
        let off = off & !1;
        if let Some(_block) = self.register_block(off) {
            if self.ide_enabled {
                return None;
            }
        } else if self.personality.bank_readable()
            && self.ide_enabled
            && (0x8000..0x1_0000).contains(&off)
        {
            return Some(self.read_bank_reg(2) as u16);
        }
        if self.rom_visible(off) {
            return Some(self.read_rom(off, 2) as u16);
        }
        None
    }
}

impl crate::zorro_device::ZorroDevice for IdeZorro {
    fn read(&mut self, off: u32, size: usize, _host: &mut crate::zorro_device::DeviceHost) -> u32 {
        Self::read(self, off, size)
    }

    fn write(
        &mut self,
        off: u32,
        size: usize,
        value: u32,
        _host: &mut crate::zorro_device::DeviceHost,
    ) {
        Self::write(self, off, size, value)
    }

    fn peek_word(&self, off: u32) -> Option<u16> {
        Self::peek_word(self, off)
    }

    fn tick(&mut self, cck: u32, host: &mut crate::zorro_device::DeviceHost) {
        // No interrupts, no DMA, no timers of its own, but every attached
        // ATAPI drive still needs its per-cck tick for pending disc-swap
        // mounting and CD-DA audio streaming -- RIPPLE has up to four
        // ATAPI-capable slots across its two channels, not just one.
        let cd_audio = host.cd_audio();
        for bus in &mut self.ata {
            bus.tick_atapi(cck, cd_audio);
        }
    }

    fn take_activity(&mut self) -> bool {
        Self::take_activity(self)
    }

    fn reset(&mut self) {
        Self::reset(self)
    }

    fn kind(&self) -> &'static str {
        Self::kind(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ata::IdeDrive;
    use crate::harddrive::SECTOR_SIZE;
    use std::path::PathBuf;

    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64;
        (nanos << 16) | NEXT.fetch_add(1, Ordering::Relaxed)
    }

    fn temp_image(sectors: u64) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "copperline-lide-test-{}-{}.hdf",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::write(&path, vec![0u8; (sectors * SECTOR_SIZE as u64) as usize]).unwrap();
        path
    }

    fn fake_flash(banks: usize) -> Vec<u8> {
        let mut flash = vec![0u8; banks * ROM_BANK_SIZE];
        for (i, b) in flash.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        // 'LIDE' footer in the last four flash bytes of bank 0: at stride-2
        // window offsets 0xFFF8/0xFFFA/0xFFFC/0xFFFE, each byte on the even
        // (upper) lane, one flash index per window byte-pair.
        let footer_off = ROM_BANK_SIZE - 4;
        flash[footer_off..footer_off + 4].copy_from_slice(b"LIDE");
        flash
    }

    #[test]
    fn register_mirror_decodes_every_block_in_512_byte_strides() {
        let board = IdeZorro::new(LidePersonality::Ripple, Vec::new()).unwrap();
        for (idx, want) in [
            (0u32, IdeReg::Data),
            (1, IdeReg::ErrorFeature),
            (2, IdeReg::SectorCount),
            (3, IdeReg::SectorNumber),
            (4, IdeReg::CylLow),
            (5, IdeReg::CylHigh),
            (6, IdeReg::DriveHead),
            (7, IdeReg::StatusCommand),
        ] {
            assert_eq!(task_index_reg(idx), Some(want));
            // The same register answers at every mirror within the block.
            for mirror in [0x1000 + idx * 0x200, 0x1000 + idx * 0x200 + 0x1FE] {
                assert_eq!(board.register_block(mirror), Some((0, false)));
            }
        }
        assert_eq!(ctrl_index_reg(6), Some(IdeReg::AltStatusDevCtl));
        assert_eq!(ctrl_index_reg(0), None);
    }

    /// A board with only image-backed drives has no real host disk to let go
    /// of, on any personality/channel count. Mirrors the depth of Gayle's own
    /// host-disk wrapper coverage (none beyond this shape).
    #[test]
    fn release_host_disks_on_a_board_with_only_image_drives_returns_zero() {
        let path = temp_image(64);
        let mut board = IdeZorro::new(LidePersonality::Ripple, Vec::new()).unwrap();
        board.attach_drive(
            0,
            0,
            IdeDrive::open(&path, 0, None, 0, crate::diskimage::FileSystem::FFS).unwrap(),
        );
        assert_eq!(board.release_host_disks(), 0);

        let mut empty = IdeZorro::new(LidePersonality::AtBus2008, Vec::new()).unwrap();
        assert_eq!(empty.release_host_disks(), 0);
    }

    #[test]
    fn byte_lanes_put_registers_on_the_upper_byte_and_float_the_dead_lane() {
        let path = temp_image(64);
        let mut board = IdeZorro::new(LidePersonality::Ride, Vec::new()).unwrap();
        board.attach_drive(
            0,
            0,
            IdeDrive::open(&path, 0, None, 0, crate::diskimage::FileSystem::FFS).unwrap(),
        );
        board.write(0x1000 + 6 * 0x200, 1, 0xE0); // drive/head, even (upper) lane
        assert_eq!(board.read(0x1000 + 6 * 0x200, 1) as u8, 0xE0);
        assert_eq!(board.read(0x1000 + 6 * 0x200 + 1, 1) as u8, 0xFF);
        assert_eq!(board.read(0x1000 + 6 * 0x200, 2) as u16, 0xE0FF);
        // A word write carries the register on D15-D8.
        board.write(0x1000 + 6 * 0x200, 2, 0xA000);
        assert_eq!(board.read(0x1000 + 6 * 0x200, 1) as u8, 0xA0);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn data_port_presents_hdf_byte_order_with_no_extra_swap() {
        let path = temp_image(64);
        let mut img = std::fs::read(&path).unwrap();
        for i in 0..SECTOR_SIZE {
            img[7 * SECTOR_SIZE + i] = (i % 241) as u8;
        }
        std::fs::write(&path, &img).unwrap();
        let mut board = IdeZorro::new(LidePersonality::Ride, Vec::new()).unwrap();
        board.attach_drive(
            0,
            0,
            IdeDrive::open(&path, 0, None, 0, crate::diskimage::FileSystem::FFS).unwrap(),
        );

        // Select LBA 7, one sector, READ SECTORS ($20).
        board.write(0x1000 + 6 * 0x200, 1, 0xE0); // drive/head: LBA, drive 0
        board.write(0x1000 + 2 * 0x200, 1, 1); // sector count
        board.write(0x1000 + 3 * 0x200, 1, 7); // LBA low
        board.write(0x1000 + 4 * 0x200, 1, 0); // LBA mid
        board.write(0x1000 + 5 * 0x200, 1, 0); // LBA high
        board.write(0x1000 + 7 * 0x200, 1, 0x20); // command
        for i in (0..SECTOR_SIZE).step_by(2) {
            let word = board.read(0x1000, 2) as u16;
            let expect = ((i % 241) as u16) << 8 | (((i + 1) % 241) as u16);
            assert_eq!(word, expect, "word {}", i / 2);
        }
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn empty_cable_floats_status() {
        let mut board = IdeZorro::new(LidePersonality::Ripple, Vec::new()).unwrap();
        assert_eq!(board.read(0x1000 + 7 * 0x200, 1) as u8, 0xFF);
    }

    /// A completely unpopulated channel must float *every* register, not
    /// just status/alt-status -- otherwise device/head and the other
    /// task-file registers read a hard zero (AtaBus's "no drive selected"
    /// case only special-cases status), which the real `lide.device`
    /// driver's channel-2 probe reads as "a device answered" rather than an
    /// empty connector, spinning forever waiting for it to respond. This was
    /// caught by booting a real `lide.rom` download under RIPPLE with only
    /// channel 0 populated: it hung at this exact register before the fix in
    /// `read_register_block`, and boots cleanly after.
    #[test]
    fn a_channel_with_no_drives_at_all_floats_every_register() {
        let mut board = IdeZorro::new(LidePersonality::Ripple, Vec::new()).unwrap();
        // Channel 0 has a drive; channel 1 has none at all.
        let path = temp_image(64);
        board.attach_drive(
            0,
            0,
            IdeDrive::open(&path, 0, None, 0, crate::diskimage::FileSystem::FFS).unwrap(),
        );

        // Channel 1's device/head register: AtaBus alone would answer 0
        // (write lands in the register file, but nothing is selected so a
        // real read floats); the board must float it like status.
        board.write(0x2000 + 6 * 0x200, 1, 0xA0);
        assert_eq!(board.read(0x2000 + 6 * 0x200, 1) as u8, 0xFF);
        assert_eq!(board.read(0x2000 + 6 * 0x200, 2) as u16, 0xFFFF);
        assert_eq!(board.read(0x2000, 2) as u16, 0xFFFF); // data port too

        // Channel 0, which does have a drive, still behaves normally.
        board.write(0x1000 + 6 * 0x200, 1, 0xE0);
        assert_eq!(board.read(0x1000 + 6 * 0x200, 1) as u8, 0xE0);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn latch_and_overlay_transitions_on_ripple() {
        let flash = fake_flash(2);
        let mut board = IdeZorro::new(LidePersonality::Ripple, flash.clone()).unwrap();
        // Pre-write: the whole window is ROM, including the footer and the
        // second bank at the top of the window.
        assert_eq!(board.read(0, 1) as u8, flash[0]);
        // The 'LIDE' footer: four bytes on the even lane at stride-2 window
        // offsets, from the last four flash bytes of bank 0.
        let footer: Vec<u8> = [0xFFF8u32, 0xFFFA, 0xFFFC, 0xFFFE]
            .iter()
            .map(|&off| board.read(off, 1) as u8)
            .collect();
        assert_eq!(footer, b"LIDE");
        assert_eq!(board.read(0x1_0000, 1) as u8, flash[ROM_BANK_SIZE]);

        // Any write anywhere latches ide_enabled.
        board.write(0x1E00, 1, 0);
        assert!(board.ide_enabled);
        // The task file is now live: no drive at all on either slot floats.
        assert_eq!(board.read(0x1E00, 1) as u8, 0xFF);
        // ...the upper 64K is still ROM, still bank 0 (the bank register
        // defaults to 0; bank switching is covered separately)...
        assert_eq!(board.read(0x1_0000, 1) as u8, flash[0]);
        // ...and the low-64K block containing offset 0 (bit12==bit13) too.
        assert_eq!(board.read(0, 1) as u8, flash[0]);
        // But the task-file block itself (bit12 != bit13) is not ROM.
        assert!(!board.rom_visible(0x1000));
    }

    #[test]
    fn bank_register_selects_the_flash_bank_post_latch() {
        let flash = fake_flash(2);
        let mut board = IdeZorro::new(LidePersonality::Ripple, flash.clone()).unwrap();
        board.write(0x1E00, 1, 0); // latch
        board.write(0x8000, 2, 0x4000); // bits 7:6 = 01 -> bank 1
        assert_eq!(board.read(0x1_0000, 1) as u8, flash[ROM_BANK_SIZE]);
        // Read-back is not supported on RIPPLE: falls through to the ROM
        // rule at that address (bit12==bit13 for 0x8000).
        assert_eq!(board.read(0x8000, 1) as u8, flash[ROM_BANK_SIZE + 0x4000]);
    }

    #[test]
    fn ride_bank_register_reads_back() {
        let mut board = IdeZorro::new(LidePersonality::Ride, fake_flash(4)).unwrap();
        board.write(0x1E00, 1, 0); // latch
        board.write(0x8000, 1, 0xD0); // bank=11 (3), otherram/maprom bits = 01
        assert_eq!(board.read(0x8000, 1) as u8, 0xD0);
        assert_eq!(
            board.read(0x1_0000, 1) as u8,
            fake_flash(4)[3 * ROM_BANK_SIZE]
        );
    }

    #[test]
    fn channel_detect_signature_differs_by_channel_count() {
        // RIPPLE: 0x1E00 (ch0 status) and 0x2C00 (ch1 drive/head) are
        // independent registers. A drive-head write only reads back once a
        // drive is selected (AtaBus zeroes non-status registers on an empty
        // cable), so attach a master on each channel; both values below
        // clear DH_DRV (bit 4) and so select that master.
        let mut ripple = IdeZorro::new(LidePersonality::Ripple, Vec::new()).unwrap();
        ripple.attach_drive(
            0,
            0,
            IdeDrive::open(
                &temp_image(64),
                0,
                None,
                0,
                crate::diskimage::FileSystem::FFS,
            )
            .unwrap(),
        );
        ripple.attach_drive(
            1,
            0,
            IdeDrive::open(
                &temp_image(64),
                0,
                None,
                0,
                crate::diskimage::FileSystem::FFS,
            )
            .unwrap(),
        );
        ripple.write(0x1000, 1, 0); // latch via a task-file write
        ripple.write(0x1000 + 6 * 0x200, 1, 0xA0); // ch0 drive/head
        ripple.write(0x2000 + 6 * 0x200, 1, 0xE0); // ch1 drive/head
        assert_eq!(ripple.read(0x1000 + 6 * 0x200, 1) as u8, 0xA0);
        assert_eq!(ripple.read(0x2000 + 6 * 0x200, 1) as u8, 0xE0);

        // RIDE: 0x2C00 is ch0's alternate status, tracking 0x1E00.
        let mut ride = IdeZorro::new(LidePersonality::Ride, Vec::new()).unwrap();
        ride.write(0x1000, 1, 0); // latch
        assert_eq!(ride.read(0x1E00, 1) as u8, ride.read(0x2C00, 1) as u8);
    }

    #[test]
    fn atbus_rom_lane_is_odd_and_registers_stay_live_from_reset() {
        let flash = fake_flash(1);
        let mut board = IdeZorro::new(LidePersonality::AtBus2008, flash.clone()).unwrap();
        // No latch: registers answer immediately.
        assert_eq!(board.read(0x1000 + 7 * 0x200, 1) as u8, 0xFF); // empty cable
                                                                   // ROM on the odd lane; even lane in ROM-only space floats.
        assert_eq!(board.read(0x8000, 1) as u8, 0xFF);
        assert_eq!(board.read(0x8001, 1) as u8, flash[0x4000]);
        // The odd ROM lane is also live where it overlaps the channel 0
        // control block (0x2000..0x3000) -- this is exactly where the boot
        // ROM's chainloader fetches its relocatable driver payload
        // (DRIVEROFFSET + the odd-board adjustment lands at 0x2001). Before
        // this was fixed, `read()` matched the control block first and
        // never consulted ROM on this lane, so the chainloader read back
        // floated 0xFF instead of the driver's hunk header and silently
        // failed to load lide.device.
        assert_eq!(board.read(0x2000, 1) as u8, 0xFF); // even lane: register
        assert_eq!(board.read(0x2001, 1) as u8, flash[0x1000]); // odd lane: ROM
    }

    #[test]
    fn hardware_only_mode_has_no_rom_and_live_registers_immediately() {
        let mut board = IdeZorro::new(LidePersonality::Ripple, Vec::new()).unwrap();
        assert_eq!(crate::zorro_device::ZorroDevice::peek_word(&board, 0), None);
        assert_eq!(board.read(0x1E00, 1) as u8, 0xFF); // live register, empty cable
        assert!(board.ide_enabled); // hardware-only: nothing to latch
    }

    /// A short dump (a real `cdfs.rom` release commonly stops at the last
    /// meaningful byte rather than including the EEPROM's trailing
    /// unprogrammed fill) is padded out to a full bank, not rejected -- only
    /// a file bigger than one bank is unambiguously the wrong image.
    #[test]
    fn load_rom_pads_a_short_dump_and_rejects_an_oversized_one() {
        let short_path = std::env::temp_dir().join(format!(
            "copperline-lide-test-short-rom-{}-{}.rom",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::write(&short_path, vec![0x42u8; ROM_BANK_SIZE - 956]).unwrap();
        let rom = IdeZorro::load_rom(&short_path).expect("a short dump is padded, not rejected");
        assert_eq!(rom.len(), ROM_BANK_SIZE);
        assert_eq!(rom[0], 0x42);
        assert_eq!(
            rom[ROM_BANK_SIZE - 956],
            0xFF,
            "padded with unprogrammed fill"
        );
        assert_eq!(rom[ROM_BANK_SIZE - 1], 0xFF);
        let _ = std::fs::remove_file(&short_path);

        let long_path = std::env::temp_dir().join(format!(
            "copperline-lide-test-long-rom-{}-{}.rom",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::write(&long_path, vec![0u8; ROM_BANK_SIZE + 1]).unwrap();
        assert!(IdeZorro::load_rom(&long_path).is_err());
        let _ = std::fs::remove_file(&long_path);
    }

    #[test]
    fn rejects_malformed_rom_sizes() {
        assert!(IdeZorro::new(LidePersonality::Ripple, vec![0u8; 100]).is_err());
        // Too many banks for the personality.
        assert!(IdeZorro::new(LidePersonality::AtBus2008, fake_flash(2)).is_err());
    }

    /// A `.iso` path attaches as an ATAPI drive rather than being rejected:
    /// IDENTIFY PACKET DEVICE (0xA1) answers, and plain IDENTIFY DEVICE
    /// (0xEC) aborts. The full PACKET protocol is exercised in `ata.rs`.
    #[test]
    fn a_cd_image_path_attaches_as_atapi() {
        let path = std::env::temp_dir().join(format!(
            "copperline-lide-test-cd-{}-{}.iso",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::write(&path, vec![0u8; 2048]).unwrap();
        let mut board = IdeZorro::new(LidePersonality::Ripple, Vec::new()).unwrap();
        board.attach_drive(0, 0, crate::ata::AtapiDrive::open(&path).unwrap());

        board.write(0x1000 + 6 * 0x200, 1, 0xE0); // drive/head: drive 0
        board.write(0x1000 + 7 * 0x200, 1, 0xA1); // IDENTIFY PACKET DEVICE
        assert_eq!(
            board.read(0x1000 + 7 * 0x200, 1) as u8,
            crate::ata::ST_DRDY | crate::ata::ST_DSC | crate::ata::ST_DRQ
        );

        board.write(0x1000 + 6 * 0x200, 1, 0xE0);
        board.write(0x1000 + 7 * 0x200, 1, 0xEC); // IDENTIFY DEVICE
        assert_eq!(
            board.read(0x1000 + 7 * 0x200, 1) as u8,
            crate::ata::ST_DRDY | crate::ata::ST_DSC | crate::ata::ST_ERR,
            "IDENTIFY DEVICE must abort against an ATAPI slot"
        );
        std::fs::remove_file(&path).ok();
    }
}
