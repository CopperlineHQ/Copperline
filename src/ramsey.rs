//! Ramsey: the memory controller of the A3000 and A4000.
//!
//! Ramsey drives the motherboard fast RAM (32-bit local RAM at $07000000 on
//! the A3000, $08000000 on the A4000): it refreshes the DRAM and decides
//! whether to use page mode, burst mode and, on Ramsey-07, cycle-skip mode.
//! The CPU sees only two registers.
//!
//! Only the register file is modelled. Refresh has no observable effect on an
//! emulator that never loses a DRAM cell, and page/burst/skip mode only change
//! how fast a real memory cycle completes, which we do not simulate either.
//! The bits are still stored and read back, because Kickstart and the
//! diagnostic tools write a mode and then spin until they read it back.

/// Ramsey control register: refresh rate, page/burst mode, and a description
/// of the DRAM fitted to the motherboard. Byte-wide, on an odd address.
pub const RAMSEY_CONTROL: u32 = 0x00DE_0003;

/// Ramsey version register. Byte-wide, read-only.
pub const RAMSEY_VERSION: u32 = 0x00DE_0043;

/// Base of the page Ramsey answers on.
pub const RAMSEY_BASE: u32 = 0x00DE_0000;
/// Size of that page. Ramsey decodes only two addresses inside it; the rest
/// reads back as a floating bus.
pub const RAMSEY_SIZE: u32 = 0x0100;

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

    /// The byte at `addr`, or an all-ones floating bus off the two registers.
    fn read_byte(&self, addr: u32) -> u8 {
        match addr {
            RAMSEY_CONTROL => self.control,
            RAMSEY_VERSION => self.revision.version_id(),
            _ => 0xFF,
        }
    }

    fn write_byte(&mut self, addr: u32, value: u8) {
        if addr == RAMSEY_CONTROL {
            self.control = value;
        }
        // The version register is read-only, and nothing else decodes.
    }

    /// Read `size` bytes. Both registers are byte-wide and sit on odd
    /// addresses, so a wider access just gathers whatever each byte decodes to.
    pub fn read(&self, addr: u32, size: usize) -> u32 {
        let mut value = 0u32;
        for i in 0..size as u32 {
            value = (value << 8) | u32::from(self.read_byte(addr.wrapping_add(i)));
        }
        value
    }

    pub fn write(&mut self, addr: u32, size: usize, value: u32) {
        for i in 0..size as u32 {
            let shift = 8 * (size as u32 - 1 - i);
            self.write_byte(addr.wrapping_add(i), (value >> shift) as u8);
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
        assert_eq!(a3000.read(RAMSEY_VERSION, 1), 0x0D);
        assert_eq!(a4000.read(RAMSEY_VERSION, 1), 0x0F);
    }

    #[test]
    fn the_version_register_ignores_writes() {
        let mut r = Ramsey::new(RamseyRevision::Rev4, 1024 * 1024);
        r.write(RAMSEY_VERSION, 1, 0xA5);
        assert_eq!(r.read(RAMSEY_VERSION, 1), 0x0D);
    }

    /// ziptest and Kickstart both write a mode to the control register and
    /// then spin until they read it back. A write that does not stick hangs
    /// the guest with interrupts disabled.
    #[test]
    fn the_control_register_reads_back_what_was_written() {
        let mut r = Ramsey::new(RamseyRevision::Rev7, 4 * 1024 * 1024);
        for value in [0x00, 0xFF, CONTROL_BURST, CONTROL_PAGE | CONTROL_WRAP] {
            r.write(RAMSEY_CONTROL, 1, u32::from(value));
            assert_eq!(r.read(RAMSEY_CONTROL, 1), u32::from(value));
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

    /// Refresh comes up at index 0 (154 clocks), which is what both diagnostic
    /// tools expect to see on a machine Kickstart has not reprogrammed.
    #[test]
    fn refresh_comes_up_at_the_fastest_rate() {
        let r = Ramsey::new(RamseyRevision::Rev4, 1024 * 1024);
        assert_eq!(r.control() & CONTROL_REFRESH, 0);
        assert_eq!(r.control() & CONTROL_TEST, 0);
    }

    /// Ramsey decodes two byte addresses; the rest of the page floats high.
    #[test]
    fn undecoded_addresses_float_high() {
        let r = Ramsey::new(RamseyRevision::Rev4, 1024 * 1024);
        assert_eq!(r.read(RAMSEY_BASE, 1), 0xFF);
        assert_eq!(r.read(RAMSEY_CONTROL + 1, 1), 0xFF);
        assert_eq!(r.read(RAMSEY_VERSION - 1, 1), 0xFF);
    }
}
