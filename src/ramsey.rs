//! Ramsey: the memory controller of the A3000 and A4000.
//!
//! Ramsey drives the motherboard fast RAM (32-bit local RAM ending at
//! $08000000 and growing downward, so a full 16 MiB reaches $07000000): it
//! refreshes the DRAM and decides whether to use page mode, burst mode and,
//! on Ramsey-07, cycle-skip mode. The CPU sees only two registers.
//!
//! Only the register file lives here; the RAM bank itself is
//! [`crate::memory::Memory::mb_ram`]. Refresh has no observable effect on an
//! emulator that never loses a DRAM cell, and page/burst/skip mode only change
//! how fast a real memory cycle completes, which we do not simulate either.
//! The bits are still stored and read back, because Kickstart and the
//! diagnostic tools write a mode and then spin until they read it back.

/// Ramsey control register: refresh rate, page/burst mode, and a description
/// of the DRAM fitted to the motherboard. Byte-wide, on an odd address.
pub const RAMSEY_CONTROL: u32 = 0x00DE_0003;

/// Ramsey version register. Byte-wide, read-only.
pub const RAMSEY_VERSION: u32 = 0x00DE_0043;

/// Whether Ramsey drives the bus for a byte access at `addr`.
///
/// Ramsey sits on byte lane 3 of the page Gary decodes (see [`crate::gary`]),
/// and only two address bits pick the register, so both are mirrored many times
/// over: the control register answers at $DE0003, $DE0007, ... $DE003F, and
/// again every $100. Bits 6-7 of the address select which register, and the two
/// blocks Ramsey does not use read back $FF.
pub fn decodes(addr: u32) -> bool {
    (crate::gary::GARY_BASE..crate::gary::GARY_BASE + crate::gary::GARY_SIZE).contains(&addr)
        && (addr & 3) == 3
}

/// Which of Ramsey's registers an address selects: block 0 is the control
/// register, block 1 the version, and blocks 2 and 3 are undriven.
fn block(addr: u32) -> u32 {
    (addr >> 6) & 3
}

// Control register bits.
/// Page mode enabled.
pub const CONTROL_PAGE: u8 = 1 << 0;
/// Burst mode enabled.
pub const CONTROL_BURST: u8 = 1 << 1;
/// Allow backward bursts to wrap.
pub const CONTROL_WRAP: u8 = 1 << 2;
/// DRAM depth: 1 = 1Mx4 (4 MiB banks), 0 = 256Kx4 (1 MiB banks).
pub const CONTROL_RAMSIZE: u8 = 1 << 3;
/// Ramsey-04: DRAM width, 1 = 4-bit parts. Ramsey-07 reuses this bit for
/// cycle-skip mode and always drives 4-bit parts.
pub const CONTROL_RAMWIDTH: u8 = 1 << 4;
/// Ramsey-07: 4-clock cycles instead of 5.
pub const CONTROL_SKIP: u8 = 1 << 4;
/// Refresh rate, 00 = 154 clocks, 01 = 238, 10 = 380, 11 = off.
pub const CONTROL_REFRESH: u8 = 3 << 5;
/// Test mode.
pub const CONTROL_TEST: u8 = 1 << 7;

/// Which Ramsey is fitted. The revision is not cosmetic: the diagnostic tools
/// and Kickstart read the version register to decide how to interpret the
/// control register, and the two parts disagree about bit 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RamseyRevision {
    /// Ramsey-04, as fitted to the A3000. Supports 1-bit-wide DRAM.
    Rev4,
    /// Ramsey-07, as fitted to the A4000. 1-bit DRAM support was dropped and
    /// the bit reused for cycle-skip mode.
    Rev7,
}

impl RamseyRevision {
    /// The byte read back from the version register.
    pub fn version_id(self) -> u8 {
        match self {
            Self::Rev4 => 0x0D,
            Self::Rev7 => 0x0F,
        }
    }

    /// Bytes per bank of the DRAM these machines shipped with: 256Kx4 parts
    /// on the A3000, 1Mx4 on the A4000.
    pub fn stock_bank_bytes(self) -> u32 {
        match self {
            Self::Rev4 => 1024 * 1024,
            Self::Rev7 => 4 * 1024 * 1024,
        }
    }

    /// Bytes per bank of the DRAM that would carry `total_bytes` of fitted
    /// motherboard RAM: the geometry the control register should describe so
    /// Kickstart's sizing probe and the diagnostic tools see parts matching
    /// the RAM that answers. The board has four banks, so 256Kx4 parts
    /// (1 MiB banks) cover totals up to 4 MiB and 1Mx4 parts (4 MiB banks)
    /// anything larger; each machine stays on its stock part where both
    /// could fit the total. Zero (no RAM fitted) falls back to the stock
    /// geometry. Totals beyond the four banks (motherboard expansion RAM
    /// below $07000000) keep the fully-populated 1Mx4 description: the
    /// control register has no way to describe the expansion decode, and
    /// on real hardware it would not go through Ramsey's geometry either.
    pub fn bank_bytes_for(self, total_bytes: usize) -> u32 {
        const BANK_1M: u32 = 1024 * 1024;
        const BANK_4M: u32 = 4 * 1024 * 1024;
        if total_bytes == 0 {
            return self.stock_bank_bytes();
        }
        match self {
            Self::Rev4 if total_bytes <= 4 * BANK_1M as usize => BANK_1M,
            _ if total_bytes.is_multiple_of(BANK_4M as usize) => BANK_4M,
            _ => BANK_1M,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Ramsey {
    revision: RamseyRevision,
    control: u8,
}

impl Ramsey {
    /// A Ramsey whose control register comes up describing `bank_bytes` of
    /// motherboard DRAM per bank.
    ///
    /// Real Ramsey powers up with the control register cleared and Kickstart
    /// programs it while sizing memory. We seed it with a value that already
    /// matches the RAM we emulate, so that a diagnostic run on a machine that
    /// never got as far as Kickstart's memory sizing still reports the truth.
    pub fn new(revision: RamseyRevision, bank_bytes: u32) -> Self {
        let mut control = 0;
        // 1Mx4 parts give 4 MiB banks; 256Kx4 parts give 1 MiB banks.
        if bank_bytes >= 4 * 1024 * 1024 {
            control |= CONTROL_RAMSIZE;
        }
        // Ramsey-07 has no 1-bit DRAM to describe, and bit 4 means skip mode.
        if revision == RamseyRevision::Rev4 {
            control |= CONTROL_RAMWIDTH;
        }
        Self { revision, control }
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.revision, self.bank_bytes());
    }

    pub fn revision(&self) -> RamseyRevision {
        self.revision
    }

    pub fn control(&self) -> u8 {
        self.control
    }

    /// Bytes per memory bank, as currently described by the control register.
    fn bank_bytes(&self) -> u32 {
        let addr_bits = if self.control & CONTROL_RAMSIZE != 0 {
            20
        } else {
            18
        };
        let width = if self.revision == RamseyRevision::Rev4 && self.control & CONTROL_RAMWIDTH == 0
        {
            1
        } else {
            4
        };
        (1 << addr_bits) * width
    }

    /// Read one of the two registers. Callers must have checked `decodes`.
    pub fn read_byte(&self, addr: u32) -> u8 {
        match block(addr) {
            0 => self.control,
            1 => self.revision.version_id(),
            // Ramsey answers on the lane but drives nothing in these blocks.
            _ => 0xFF,
        }
    }

    pub fn write_byte(&mut self, addr: u32, value: u8) {
        // The version register is read-only, and so are the unused blocks.
        if block(addr) == 0 {
            self.control = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_register_identifies_the_part() {
        let a3000 = Ramsey::new(RamseyRevision::Rev4, 1024 * 1024);
        let a4000 = Ramsey::new(RamseyRevision::Rev7, 4 * 1024 * 1024);
        assert_eq!(a3000.read_byte(RAMSEY_VERSION), 0x0D);
        assert_eq!(a4000.read_byte(RAMSEY_VERSION), 0x0F);
    }

    #[test]
    fn the_version_register_ignores_writes() {
        let mut r = Ramsey::new(RamseyRevision::Rev4, 1024 * 1024);
        r.write_byte(RAMSEY_VERSION, 0xA5);
        assert_eq!(r.read_byte(RAMSEY_VERSION), 0x0D);
    }

    /// ziptest and Kickstart both write a mode to the control register and
    /// then spin until they read it back. A write that does not stick hangs
    /// the guest with interrupts disabled.
    #[test]
    fn the_control_register_reads_back_what_was_written() {
        let mut r = Ramsey::new(RamseyRevision::Rev7, 4 * 1024 * 1024);
        for value in [0x00, 0xFF, CONTROL_BURST, CONTROL_PAGE | CONTROL_WRAP] {
            r.write_byte(RAMSEY_CONTROL, value);
            assert_eq!(r.read_byte(RAMSEY_CONTROL), value);
        }
    }

    /// The reset value has to describe the DRAM we actually emulate, or
    /// ziptest reports a memory configuration that was never fitted.
    #[test]
    fn the_reset_control_describes_the_fitted_dram() {
        // A4000: 1Mx4 parts, 4 MiB banks. Ramsey-07 has no width bit.
        let a4000 = Ramsey::new(RamseyRevision::Rev7, 4 * 1024 * 1024);
        assert_eq!(a4000.control() & CONTROL_RAMSIZE, CONTROL_RAMSIZE);
        assert_eq!(a4000.control() & CONTROL_SKIP, 0);
        assert_eq!(a4000.bank_bytes(), 4 * 1024 * 1024);

        // A3000: 256Kx4 parts, 1 MiB banks.
        let a3000 = Ramsey::new(RamseyRevision::Rev4, 1024 * 1024);
        assert_eq!(a3000.control() & CONTROL_RAMSIZE, 0);
        assert_eq!(a3000.control() & CONTROL_RAMWIDTH, CONTROL_RAMWIDTH);
        assert_eq!(a3000.bank_bytes(), 1024 * 1024);
    }

    /// The seeded control register must describe DRAM that could actually
    /// carry the fitted total across the board's four banks: each machine
    /// stays on its stock part while that part can cover the total.
    #[test]
    fn bank_geometry_tracks_the_fitted_total() {
        const M: usize = 1024 * 1024;
        // No RAM fitted: fall back to the stock geometry.
        assert_eq!(
            RamseyRevision::Rev4.bank_bytes_for(0),
            RamseyRevision::Rev4.stock_bank_bytes()
        );
        assert_eq!(
            RamseyRevision::Rev7.bank_bytes_for(0),
            RamseyRevision::Rev7.stock_bank_bytes()
        );
        // A3000: stock 256Kx4 parts (1 MiB banks) up to full population,
        // 1Mx4 beyond.
        assert_eq!(RamseyRevision::Rev4.bank_bytes_for(2 * M), M as u32);
        assert_eq!(RamseyRevision::Rev4.bank_bytes_for(4 * M), M as u32);
        assert_eq!(RamseyRevision::Rev4.bank_bytes_for(16 * M), 4 * M as u32);
        // A4000: stock 1Mx4 parts whenever whole 4 MiB banks fill, 256Kx4
        // for the sub-4M totals.
        assert_eq!(RamseyRevision::Rev7.bank_bytes_for(4 * M), 4 * M as u32);
        assert_eq!(RamseyRevision::Rev7.bank_bytes_for(16 * M), 4 * M as u32);
        assert_eq!(RamseyRevision::Rev7.bank_bytes_for(2 * M), M as u32);
        // Expansion totals below $07000000 keep the fully-populated
        // 1Mx4 description; the control register cannot say more.
        assert_eq!(RamseyRevision::Rev7.bank_bytes_for(64 * M), 4 * M as u32);
    }

    /// Refresh comes up at index 0 (154 clocks), which is what both diagnostic
    /// tools expect to see on a machine Kickstart has not reprogrammed.
    #[test]
    fn refresh_comes_up_at_the_fastest_rate() {
        let r = Ramsey::new(RamseyRevision::Rev4, 1024 * 1024);
        assert_eq!(r.control() & CONTROL_REFRESH, 0);
        assert_eq!(r.control() & CONTROL_TEST, 0);
    }

    /// Ramsey has exactly two registers. Anything else on the page must stay
    /// undriven so it floats and gets logged, rather than reading back a value
    /// we made up -- a guest reading the version at the wrong offset should
    /// look wrong, not plausibly wrong.
    #[test]
    fn only_the_two_registers_are_decoded() {
        assert!(decodes(RAMSEY_CONTROL));
        assert!(decodes(RAMSEY_VERSION));
        assert!(!decodes(0x00DE_0000));
        assert!(!decodes(RAMSEY_CONTROL + 1));
        assert!(!decodes(RAMSEY_VERSION - 1));
        assert!(!decodes(0x00DE_1000)); // Gayle's ID page, on the wedge machines
    }
}
