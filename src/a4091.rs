// SPDX-License-Identifier: GPL-3.0-or-later

//! Commodore A4091: a Zorro III SCSI-2 controller carrying an NCR 53C710
//! and a nibble-wide autoboot ROM.
//!
//! Board layout (offsets within the configured 16M window, matching the
//! A4091 schematics and the WinUAE/Amiberry emulation):
//!
//! - `$000000-$7FFFFF` the boot ROM, nibble-wide: ROM byte `i` presents its
//!   high nibble at window offset `i*4` and its low nibble at `i*4+2` (both
//!   with the low nibble of the lane forced to `$F`); odd offsets float
//!   `$FF`. expansion.library reassembles the DiagArea from the nibbles
//!   (DAC_NIBBLEWIDE), and the ROM's own relocator copies the driver the
//!   same way. A 32K image mirrors to fill the 64K ROM space.
//! - `$800000-$87FFFF` the 53C710 registers. The board decodes only the
//!   low 6 address bits, so the 64-byte register file mirrors across the
//!   whole window; the driver relies on this, writing TEMP/SCRATCH through
//!   the `+$40` shadow (a 68030/68040 cache write-allocate workaround) and
//!   reading them back at the base offsets. Registers are plain storage for
//!   now -- enough for the driver's walking-bits hardware test -- with the
//!   SCRIPTS processor still to come (a DSP write warns once).
//! - `$8C0003` the DIP-switch byte (SCSI host ID, termination, sync/fast
//!   negotiation enables). `$FF` means all switches off: host ID 7.
//!
//! The autoconfig identity (Commodore, product 84, 16M Zorro III,
//! er_InitDiagVec `$0200`) is supplied by [`crate::zorro::BoardSpec::a4091`];
//! the same nibbles are also baked into the first `$60` bytes of the real
//! EPROM, which is how the physical board presents them.

use anyhow::{bail, Result};
use std::path::Path;

/// The 53C710 register window within the board space.
const IO_OFFSET: u32 = 0x0080_0000;
const IO_END: u32 = 0x0088_0000;

/// The DIP-switch readback byte.
const DIP_OFFSET: u32 = 0x008C_0003;

/// 53C710 register-file byte offsets as the 68k sees them (the chip is
/// wired big-endian on the A4091, so these are the driver's REG_ addresses).
const REG_CTEST8: usize = 0x21;
/// ISTAT: bit 6 is the software-reset strobe.
const REG_ISTAT: usize = 0x22;
const ISTAT_RST: u8 = 0x40;
/// DSTAT: bit 7 (DFE) tracks "all DMA FIFO lanes empty".
const REG_DSTAT: usize = 0x0F;
const DSTAT_DFE: u8 = 0x80;
/// CTEST1: FMT lane-empty flags in the high nibble, FFL lane-full in the low.
const REG_CTEST1: usize = 0x16;
/// CTEST2: bit 3 carries the parity bit of the last DMA FIFO pop.
const REG_CTEST2: usize = 0x15;
const CTEST2_DFP: u8 = 0x08;
/// CTEST4: FBL2 (bit 2) routes CTEST6 to the DMA FIFO lane in bits 1:0.
const REG_CTEST4: usize = 0x1B;
const CTEST4_FBL2: u8 = 0x04;
/// CTEST6: the DMA FIFO data window (with FBL2 set).
const REG_CTEST6: usize = 0x19;
/// CTEST7: bit 3 supplies the parity bit pushed with a DMA FIFO write.
const REG_CTEST7: usize = 0x18;
const CTEST7_DFP: u8 = 0x08;
/// CTEST5 carries the ADCK/BBCK self-clearing test strobes.
const REG_CTEST5: usize = 0x1A;
const CTEST5_ADCK: u8 = 0x80;
const CTEST5_BBCK: u8 = 0x40;
/// DNAD: 32-bit DMA next address; ADCK increments it by the bus width.
const REG_DNAD: usize = 0x28;
/// DBC: 24-bit DMA byte counter (below DCMD at $24); BBCK decrements it.
const REG_DBC: usize = 0x25;
/// DSP: writing its low byte starts the SCRIPTS processor.
const REG_DSP: usize = 0x2C;
/// DMA FIFO depth per byte lane.
const DMA_FIFO_DEPTH: usize = 16;
/// SCNTL1: bit 2 asserts even (instead of odd) generated parity.
const REG_SCNTL1: usize = 0x02;
const SCNTL1_AESP: u8 = 0x04;
/// SODL: writes push the SCSI FIFO when CTEST4.SFWR routes them there.
const REG_SODL: usize = 0x05;
/// SSTAT2: SCSI FIFO fill count in the high nibble.
const REG_SSTAT2: usize = 0x0C;
/// CTEST3: reads pop the SCSI FIFO.
const REG_CTEST3: usize = 0x14;
/// CTEST4 bit 3: route SODL writes to the SCSI FIFO.
const CTEST4_SFWR: u8 = 0x08;
/// CTEST2 bit 4: parity bit of the last SCSI FIFO pop.
const CTEST2_SFP: u8 = 0x10;
/// SCSI FIFO depth.
const SCSI_FIFO_DEPTH: usize = 8;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct A4091 {
    rom: Vec<u8>,
    /// DIP switches as read at `$8C0003`; `$FF` = all off (host ID 7).
    dip: u8,
    /// The 53C710 register file, indexed by CPU-visible (big-endian) byte
    /// address. Plain storage until the SCRIPTS core lands.
    regs: Vec<u8>,
    /// The DMA FIFO: four byte lanes, 16 entries deep, 8 data bits plus a
    /// parity bit per entry. CTEST6 pushes/pops the lane selected by
    /// CTEST4; CTEST1 and DSTAT.DFE report the fill state.
    dma_fifo: [Vec<u16>; 4],
    /// The SCSI FIFO: 8 entries, 8 data bits plus parity. SODL pushes it
    /// (with CTEST4.SFWR), CTEST3 pops it, SSTAT2 counts it.
    scsi_fifo: Vec<u16>,
    /// One-shot warning latch for the unimplemented SCRIPTS processor.
    scripts_warned: bool,
}

/// 53C710 register-file reset values at their big-endian byte addresses:
/// SCNTL0 (arbitration bits), SCID, DSTAT.DFE (DMA FIFO empty), CTEST2.DACK,
/// DCMD, matching the chip's documented power-on state.
fn reset_regs() -> Vec<u8> {
    let mut r = vec![0u8; 0x40];
    r[0x03] = 0xC0; // SCNTL0
    r[0x07] = 0x80; // SCID
                    // DSTAT.DFE and CTEST1 are computed from the DMA FIFO state on read.
    r[0x15] = 0x01; // CTEST2: DACK
    r[0x24] = 0x40; // DCMD
    r
}

impl A4091 {
    /// Build the board from its boot ROM image (a raw 32K or 64K byte-wide
    /// EPROM dump; the board serves it nibble-wide).
    pub fn new(rom: Vec<u8>) -> Result<Self> {
        if !matches!(rom.len(), 0x8000 | 0x1_0000) {
            bail!(
                "A4091 ROM is {} bytes; expected 32K or 64K (a raw byte-wide \
                 EPROM image, e.g. the open-source a4091.rom)",
                rom.len()
            );
        }
        Ok(Self {
            rom,
            dip: 0xFF,
            regs: reset_regs(),
            dma_fifo: Default::default(),
            scsi_fifo: Vec::new(),
            scripts_warned: false,
        })
    }

    /// The parity bit the chip generates for a pushed SCSI FIFO byte: odd
    /// parity normally, even when SCNTL1.AESP is set. (SCNTL0.EPG gates
    /// generation on the real chip; without it parity would come from the
    /// bus, which has no meaning here, so the generated bit is used
    /// regardless.)
    fn scsi_parity(&self, b: u8) -> u16 {
        let even = u16::from(b.count_ones() as u8 & 1);
        if self.regs[REG_SCNTL1] & SCNTL1_AESP != 0 {
            even
        } else {
            even ^ 1
        }
    }

    /// The DMA FIFO lane CTEST6 currently addresses, when CTEST4.FBL2
    /// routes it to the FIFO at all.
    fn fifo_lane(&self) -> Option<usize> {
        (self.regs[REG_CTEST4] & CTEST4_FBL2 != 0).then_some((self.regs[REG_CTEST4] & 3) as usize)
    }

    /// CTEST1 computed from the FIFO: FMT empty flags high, FFL full low.
    fn ctest1(&self) -> u8 {
        let mut v = 0u8;
        for (lane, fifo) in self.dma_fifo.iter().enumerate() {
            if fifo.is_empty() {
                v |= 0x10 << lane;
            }
            if fifo.len() >= DMA_FIFO_DEPTH {
                v |= 1 << lane;
            }
        }
        v
    }

    /// Software reset (power-on, /RST, or the ISTAT RST strobe): registers
    /// to documented reset values, FIFOs drained. ROM and switches keep.
    fn chip_reset(&mut self) {
        self.regs = reset_regs();
        for fifo in &mut self.dma_fifo {
            fifo.clear();
        }
        self.scsi_fifo.clear();
    }

    pub fn load_rom(path: &Path) -> Result<Vec<u8>> {
        std::fs::read(path)
            .map_err(|e| anyhow::anyhow!("reading A4091 ROM {}: {e}", path.display()))
    }

    fn reg32(&self, r: usize) -> u32 {
        u32::from_be_bytes(self.regs[r..r + 4].try_into().unwrap())
    }

    fn set_reg32(&mut self, r: usize, v: u32) {
        self.regs[r..r + 4].copy_from_slice(&v.to_be_bytes());
    }

    /// CTEST5 ADCK/BBCK test strobes: increment DNAD / decrement DBC by the
    /// bus width, then self-clear.
    fn ctest5_strobes(&mut self) {
        let v = self.regs[REG_CTEST5];
        if v & CTEST5_ADCK != 0 {
            let dnad = self.reg32(REG_DNAD).wrapping_add(4);
            self.set_reg32(REG_DNAD, dnad);
        }
        if v & CTEST5_BBCK != 0 {
            let dbc = (u32::from(self.regs[REG_DBC]) << 16)
                | (u32::from(self.regs[REG_DBC + 1]) << 8)
                | u32::from(self.regs[REG_DBC + 2]);
            let dbc = dbc.wrapping_sub(4) & 0x00FF_FFFF;
            self.regs[REG_DBC] = (dbc >> 16) as u8;
            self.regs[REG_DBC + 1] = (dbc >> 8) as u8;
            self.regs[REG_DBC + 2] = dbc as u8;
        }
        self.regs[REG_CTEST5] = v & !(CTEST5_ADCK | CTEST5_BBCK);
    }

    /// One byte of the nibble-wide ROM as seen in the window at `off`.
    fn rom_byte(&self, off: u32) -> u8 {
        if off & 1 != 0 {
            return 0xFF;
        }
        let b = self.rom[(off as usize / 4) % self.rom.len()];
        if off & 2 == 0 {
            b | 0x0F
        } else {
            (b << 4) | 0x0F
        }
    }
}

impl crate::zorro_device::ZorroDevice for A4091 {
    fn read(&mut self, off: u32, size: usize, _host: &mut crate::zorro_device::DeviceHost) -> u32 {
        let mut v = 0u32;
        for i in 0..size {
            let o = off.wrapping_add(i as u32);
            let b = if o < IO_OFFSET {
                self.rom_byte(o)
            } else if o == DIP_OFFSET {
                self.dip
            } else if (IO_OFFSET..IO_END).contains(&o) {
                match (o as usize) & 0x3F {
                    // CTEST8: chip revision 2 in the high nibble; the CLF
                    // (clear FIFOs) strobe bit always reads back 0.
                    REG_CTEST8 => (self.regs[REG_CTEST8] | 0x20) & !0x04,
                    REG_CTEST1 => self.ctest1(),
                    REG_DSTAT => {
                        let dfe = self.dma_fifo.iter().all(Vec::is_empty);
                        (self.regs[REG_DSTAT] & !DSTAT_DFE) | if dfe { DSTAT_DFE } else { 0 }
                    }
                    REG_SSTAT2 => {
                        ((self.scsi_fifo.len() as u8) << 4) | (self.regs[REG_SSTAT2] & 0x0F)
                    }
                    // CTEST3 pops the SCSI FIFO; parity lands in CTEST2.SFP.
                    REG_CTEST3 if !self.scsi_fifo.is_empty() => {
                        let entry = self.scsi_fifo.remove(0);
                        self.regs[REG_CTEST2] = (self.regs[REG_CTEST2] & !CTEST2_SFP)
                            | if entry & 0x100 != 0 { CTEST2_SFP } else { 0 };
                        entry as u8
                    }
                    // CTEST6 pops the addressed DMA FIFO lane; the entry's
                    // parity bit lands in CTEST2.DFP.
                    REG_CTEST6 => match self.fifo_lane() {
                        Some(lane) if !self.dma_fifo[lane].is_empty() => {
                            let entry = self.dma_fifo[lane].remove(0);
                            self.regs[REG_CTEST2] = (self.regs[REG_CTEST2] & !CTEST2_DFP)
                                | if entry & 0x100 != 0 { CTEST2_DFP } else { 0 };
                            entry as u8
                        }
                        _ => self.regs[REG_CTEST6],
                    },
                    r => self.regs[r],
                }
            } else {
                0xFF
            };
            v = (v << 8) | u32::from(b);
        }
        v
    }

    fn write(
        &mut self,
        off: u32,
        size: usize,
        value: u32,
        _host: &mut crate::zorro_device::DeviceHost,
    ) {
        for i in 0..size {
            let o = off.wrapping_add(i as u32);
            if !(IO_OFFSET..IO_END).contains(&o) {
                continue;
            }
            let r = (o as usize) & 0x3F;
            let b = (value >> (8 * (size - 1 - i))) as u8;
            // SODL with SFWR set pushes the SCSI FIFO as well as storing.
            if r == REG_SODL
                && self.regs[REG_CTEST4] & CTEST4_SFWR != 0
                && self.scsi_fifo.len() < SCSI_FIFO_DEPTH
            {
                let parity = self.scsi_parity(b);
                self.scsi_fifo.push((parity << 8) | u16::from(b));
            }
            // CTEST6 with the FIFO addressed pushes instead of storing.
            if r == REG_CTEST6 {
                if let Some(lane) = self.fifo_lane() {
                    if self.dma_fifo[lane].len() < DMA_FIFO_DEPTH {
                        let parity = u16::from(self.regs[REG_CTEST7] & CTEST7_DFP != 0);
                        self.dma_fifo[lane].push((parity << 8) | u16::from(b));
                    }
                    continue;
                }
            }
            self.regs[r] = b;
            if r == REG_CTEST5 {
                self.ctest5_strobes();
            }
            if r == REG_ISTAT && b & ISTAT_RST != 0 {
                self.chip_reset();
                self.regs[REG_ISTAT] = ISTAT_RST;
            }
            if (REG_DSP..REG_DSP + 4).contains(&r) && !self.scripts_warned {
                self.scripts_warned = true;
                log::warn!(
                    "a4091: DSP write ({:#04X} <- {:#04X}): SCRIPTS processor \
                     not implemented yet",
                    r,
                    self.regs[r]
                );
            }
        }
    }

    fn peek_word(&self, off: u32) -> Option<u16> {
        if off < IO_OFFSET {
            Some(
                (u16::from(self.rom_byte(off)) << 8)
                    | u16::from(self.rom_byte(off.wrapping_add(1))),
            )
        } else {
            None
        }
    }

    fn tick(&mut self, _cck: u32, _host: &mut crate::zorro_device::DeviceHost) {}

    fn reset(&mut self) {
        self.chip_reset();
        self.scripts_warned = false;
    }

    fn kind(&self) -> &'static str {
        "a4091"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Memory;
    use crate::zorro::ZorroChain;
    use crate::zorro_device::{DeviceHost, ZorroDevice};

    fn board_with_rom(rom: Vec<u8>) -> A4091 {
        A4091::new(rom).expect("valid ROM size")
    }

    fn test_memory() -> Memory {
        Memory {
            chip_ram: vec![0u8; 512 * 1024],
            slow_ram: Vec::new(),
            rom: Vec::new(),
            overlay: false,
            zorro: ZorroChain::default(),
            extended_rom: Vec::new(),
            extended_rom_base: 0,
            wcs: Vec::new(),
            wcs_write_protected: false,
        }
    }

    fn test_rom_64k() -> Vec<u8> {
        (0..0x1_0000usize).map(|i| i as u8).collect()
    }

    #[test]
    fn rom_appears_nibble_wide() {
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        // ROM byte $12 lives at window offset $48 (high nibble) / $4A (low).
        assert_eq!(b.read(0x48, 1, &mut host), 0x1F);
        assert_eq!(b.read(0x4A, 1, &mut host), 0x2F);
        assert_eq!(b.read(0x49, 1, &mut host), 0xFF);
        assert_eq!(b.read(0x4B, 1, &mut host), 0xFF);
    }

    #[test]
    fn rom_mirrors_across_the_rom_space_and_32k_images_repeat() {
        let mut b = board_with_rom(vec![0xA5; 0x8000]);
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        // 32K image: offset $20000 (byte $8000) wraps back to byte 0.
        assert_eq!(b.read(0x0002_0000, 1, &mut host), 0xAF);
        // Anywhere below the 53C710 window still decodes ROM.
        assert_eq!(b.read(0x007F_FFFC, 1, &mut host), 0xAF);
    }

    #[test]
    fn dip_switches_read_back() {
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(b.read(DIP_OFFSET, 1, &mut host), 0xFF);
    }

    #[test]
    fn scratch_and_temp_written_via_the_shadow_read_back_at_the_base() {
        // The driver's walking-bits hardware test: write SCRATCH ($34) and
        // TEMP ($1C) through the +$40 write shadows, read back at the base.
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        b.write(IO_OFFSET + 0x40 + 0x34, 4, 0xF0E7_C3A5, &mut host);
        b.write(IO_OFFSET + 0x40 + 0x1C, 4, 0xE1CF_874B, &mut host);
        assert_eq!(b.read(IO_OFFSET + 0x34, 4, &mut host), 0xF0E7_C3A5);
        assert_eq!(b.read(IO_OFFSET + 0x1C, 4, &mut host), 0xE1CF_874B);
    }

    #[test]
    fn ctest5_adck_and_bbck_strobe_the_counters_and_self_clear() {
        // The ncr7xx tool's register test: DBC=0x10 via a 32-bit DCMD write,
        // DNAD=0, then ADCK bumps DNAD to 4 (DBC untouched) and BBCK drops
        // DBC to 0xC; both bits read back clear.
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        b.write(IO_OFFSET + 0x24, 4, 0x0000_0010, &mut host);
        b.write(IO_OFFSET + 0x28, 4, 0, &mut host);
        b.write(IO_OFFSET + 0x1A, 1, 0x80, &mut host);
        assert_eq!(b.read(IO_OFFSET + 0x28, 4, &mut host), 4);
        assert_eq!(b.read(IO_OFFSET + 0x24, 4, &mut host), 0x10);
        b.write(IO_OFFSET + 0x1A, 1, 0x40, &mut host);
        assert_eq!(b.read(IO_OFFSET + 0x24, 4, &mut host), 0xC);
        assert_eq!(b.read(IO_OFFSET + 0x1A, 1, &mut host) & 0xC0, 0);
    }

    #[test]
    fn dma_fifo_pushes_pops_with_parity_and_tracks_status() {
        // The ncr7xx DMA FIFO test: fill all four lanes through CTEST6
        // (parity from CTEST7.3), watching CTEST1/DSTAT.DFE, then drain
        // verifying data, parity in CTEST2.3, and status flags.
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        let io = |r: usize| IO_OFFSET + r as u32;

        // ISTAT.RST software reset leaves the FIFO empty.
        b.write(io(REG_ISTAT), 1, 0x40, &mut host);
        b.write(io(REG_ISTAT), 1, 0, &mut host);
        assert_eq!(b.read(io(REG_CTEST1), 1, &mut host), 0xF0);
        assert_ne!(b.read(io(REG_DSTAT), 1, &mut host) & 0x80, 0);

        for lane in 0..4u32 {
            b.write(io(REG_CTEST4), 1, 0x04 | lane, &mut host);
            for byte in 0..16u32 {
                let parity = (lane + byte) & 1;
                b.write(io(REG_CTEST7), 1, parity << 3, &mut host);
                b.write(io(REG_CTEST6), 1, (0xA0 + byte) ^ lane, &mut host);
            }
        }
        // All lanes full, FIFO not empty.
        assert_eq!(b.read(io(REG_CTEST1), 1, &mut host), 0x0F);
        assert_eq!(b.read(io(REG_DSTAT), 1, &mut host) & 0x80, 0);

        for lane in 0..4u32 {
            b.write(io(REG_CTEST4), 1, 0x04 | lane, &mut host);
            for byte in 0..16u32 {
                let v = b.read(io(REG_CTEST6), 1, &mut host);
                assert_eq!(v, (0xA0 + byte) ^ lane, "lane {lane} byte {byte}");
                let parity = (b.read(io(REG_CTEST2), 1, &mut host) >> 3) & 1;
                assert_eq!(parity, (lane + byte) & 1, "parity lane {lane} byte {byte}");
            }
        }
        assert_eq!(b.read(io(REG_CTEST1), 1, &mut host), 0xF0);
        assert_ne!(b.read(io(REG_DSTAT), 1, &mut host) & 0x80, 0);
    }

    #[test]
    fn scsi_fifo_pushes_via_sodl_pops_via_ctest3_with_generated_parity() {
        // The ncr7xx SCSI FIFO test: SFWR routes SODL pushes to the FIFO,
        // parity generated odd (or even under SCNTL1.AESP), count in
        // SSTAT2's high nibble, pops through CTEST3 with parity in
        // CTEST2.SFP.
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        let io = |r: usize| IO_OFFSET + r as u32;

        assert_eq!(b.read(io(REG_SSTAT2), 1, &mut host), 0x00);
        b.write(io(0x1B), 1, 0x08, &mut host); // CTEST4.SFWR
        let data = [0x00u8, 0x01, 0xFF, 0x5A, 0x81, 0x7E, 0x33, 0xC4];
        for (i, &d) in data.iter().enumerate() {
            // Alternate even/odd generation via SCNTL1.AESP.
            b.write(io(REG_SCNTL1), 1, ((i as u32) & 1) << 2, &mut host);
            assert_eq!(b.read(io(REG_SSTAT2), 1, &mut host) >> 4, i as u32);
            b.write(io(REG_SODL), 1, u32::from(d), &mut host);
        }
        assert_eq!(b.read(io(REG_SSTAT2), 1, &mut host), 0x80);

        for (i, &d) in data.iter().enumerate() {
            let v = b.read(io(REG_CTEST3), 1, &mut host);
            assert_eq!(v, u32::from(d), "byte {i}");
            let sfp = (b.read(io(REG_CTEST2), 1, &mut host) >> 4) & 1;
            let even = u32::from(d.count_ones() & 1);
            let expect = if i & 1 == 1 { even } else { even ^ 1 };
            assert_eq!(sfp, expect, "parity byte {i}");
            assert_eq!(b.read(io(REG_SSTAT2), 1, &mut host) >> 4, (7 - i) as u32);
        }
    }

    #[test]
    fn ctest8_reports_chip_revision_2() {
        let mut b = board_with_rom(test_rom_64k());
        let mut mem = test_memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(b.read(IO_OFFSET + 0x21, 1, &mut host) >> 4, 2);
    }

    #[test]
    fn peek_serves_rom_and_leaves_io_opaque() {
        let b = board_with_rom(test_rom_64k());
        assert_eq!(b.peek_word(0x48), Some(0x1FFF));
        assert_eq!(b.peek_word(IO_OFFSET), None);
    }
}
