//! Ramsey: the memory controller of the A3000 and A4000.
//!
//! Ramsey drives the motherboard fast RAM (32-bit local RAM ending at
//! $08000000 and growing downward, so a full 16 MiB reaches $07000000): it
//! refreshes the DRAM and decides whether to use page mode, burst mode and,
//! on Ramsey-07, cycle-skip mode. The CPU sees only two registers.
//!
//! The stock big-box profiles use the 25 MHz motherboard clock (seven clocks
//! per emulated colour clock). The RAM contents live in `Memory::mb_ram`.
//! Refresh closes an open static-column row and clocks a pending control
//! write into the DRAM timing. The register reads back the written byte at
//! once.

pub(crate) const CLOCKS_PER_CCK: u32 = 7;
/// Where a save state written before the refresh phase was recorded resumes.
const fn first_refresh() -> u64 {
    154
}

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
    /// The mode the DRAM controller is running. A refresh clocks it in.
    control: u8,
    /// The last control write, waiting for the next refresh. CPU reads of
    /// the control register already return it.
    #[serde(default)]
    pending: Option<u8>,
    #[serde(default = "first_refresh")]
    next_refresh: u64,
    #[serde(default)]
    open_row: Option<u32>,
    /// Remainder when converting board clocks to accelerator CPU clocks.
    #[serde(default)]
    pub(crate) clock_remainder: u32,
    /// The board's RSIZE jumper: the DRAM depth the control register comes
    /// up describing. None in a save state written before it was recorded.
    #[serde(default)]
    rsize: Option<bool>,
}

impl Ramsey {
    /// A Ramsey out of power-up on a board fitted with `bank_bytes` of DRAM
    /// per bank.
    ///
    /// Every control bit has a default it takes while *PWRUP is low
    /// (Commodore Ramsey specification, "RAMSEY Control Register"): page,
    /// burst and wrap off, RAMsize from the RSIZE jumper, RAMwidth set on
    /// the -04 (skip clear on the -07), and the refresh rate from the RSPEED
    /// input, which selects 238 clocks on a 25 MHz board. The jumper is
    /// taken to match the DRAM actually fitted.
    pub fn new(revision: RamseyRevision, bank_bytes: u32) -> Self {
        // 1Mx4 parts give 4 MiB banks; 256Kx4 parts give 1 MiB banks.
        let rsize = bank_bytes >= 4 * 1024 * 1024;
        let control = Self::power_up_control(revision, rsize);
        Self {
            revision,
            control,
            pending: None,
            next_refresh: Self::refresh_period(control).unwrap_or(u64::MAX),
            open_row: None,
            clock_remainder: 0,
            rsize: Some(rsize),
        }
    }

    fn power_up_control(revision: RamseyRevision, rsize: bool) -> u8 {
        // RSPEED is high on the 25 MHz boards modelled here: refresh 01.
        let mut control = 1 << 5;
        if rsize {
            control |= CONTROL_RAMSIZE;
        }
        // Ramsey-07 has no 1-bit DRAM to describe, and bit 4 means skip mode.
        if revision == RamseyRevision::Rev4 {
            control |= CONTROL_RAMWIDTH;
        }
        control
    }

    /// A power cycle: *PWRUP reloads every default. The system /RESET line
    /// (Ctrl-Amiga-Amiga, the CPU RESET instruction) does not reach the
    /// register, so a warm reboot starts with whatever mode the last
    /// session left and Kickstart programs it afresh.
    pub fn power_on_reset(&mut self, now: u64) {
        let rsize = self.rsize.unwrap_or(self.control & CONTROL_RAMSIZE != 0);
        let control = Self::power_up_control(self.revision, rsize);
        *self = Self {
            revision: self.revision,
            control,
            pending: None,
            next_refresh: Self::refresh_period(control)
                .map_or(u64::MAX, |period| now.saturating_add(period)),
            open_row: None,
            clock_remainder: self.clock_remainder,
            rsize: Some(rsize),
        };
    }

    fn refresh_period(control: u8) -> Option<u64> {
        [Some(154), Some(238), Some(380), None][usize::from((control >> 5) & 3)]
    }

    /// Catch up at the next externally visible operation. Skipping a frame
    /// or restoring a machine between a write and a refresh preserves phase.
    pub(crate) fn advance_to(&mut self, now: u64) {
        if now < self.next_refresh || self.next_refresh == u64::MAX {
            return;
        }
        self.open_row = None;
        if let Some(value) = self.pending.take() {
            self.control = value;
        }
        self.next_refresh = match Self::refresh_period(self.control) {
            Some(period) => now.saturating_add(period - (now - self.next_refresh) % period),
            None => u64::MAX,
        };
    }

    /// The board clock of the next refresh edge, as of the last catch-up.
    #[cfg(test)]
    pub(crate) fn next_refresh_at(&self) -> u64 {
        self.next_refresh
    }

    pub(crate) fn access_clocks(&mut self, addr: u32) -> u32 {
        let normal = if self.revision == RamseyRevision::Rev7 && self.control & CONTROL_SKIP != 0 {
            4
        } else {
            5
        };
        if self.control & CONTROL_PAGE == 0 {
            return normal;
        }
        // 256K x 4 parts use nine column bits, 1M-deep parts ten.
        let large = self.control & CONTROL_RAMSIZE != 0
            || (self.revision == RamseyRevision::Rev4 && self.control & CONTROL_RAMWIDTH == 0);
        let row = addr >> if large { 12 } else { 11 };
        match self.open_row.replace(row) {
            Some(last) if last == row => 3,
            Some(_) => 7,
            None => normal,
        }
    }

    pub(crate) fn burst_followups(&self, addr: u32) -> u32 {
        if self.control & CONTROL_BURST == 0 {
            0
        } else if self.control & CONTROL_WRAP != 0 {
            3
        } else {
            3 - ((addr >> 2) & 3)
        }
    }

    pub fn revision(&self) -> RamseyRevision {
        self.revision
    }

    /// The mode Ramsey is running. It changes only on a refresh edge, while
    /// [`Self::read_byte`] returns the most recent write at once.
    pub fn control(&self) -> u8 {
        self.control
    }

    /// Bytes per memory bank, as currently described by the control register.
    #[cfg(test)]
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
    ///
    /// The control register returns the last byte written, even before the
    /// refresh that applies it to the DRAM timing. Commodore's specification
    /// says the value cannot be read back before that refresh. Detection
    /// code that reads the register on the very next instruction, with no
    /// retry, recognises Ramsey only if the register answers at once.
    /// TODO: measure the read-back delay on an A3000/A4000 board. If it is
    /// real, return `self.control` here and keep the pending word internal.
    pub fn read_byte(&self, addr: u32) -> u8 {
        match block(addr) {
            0 => self.pending.unwrap_or(self.control),
            1 => self.revision.version_id(),
            // Ramsey answers on the lane but drives nothing in these blocks.
            _ => 0xFF,
        }
    }

    pub fn write_byte(&mut self, addr: u32, value: u8) {
        // The version register is read-only, and so are the unused blocks.
        if block(addr) == 0 {
            self.pending = Some(value);
        }
    }

    pub(crate) fn write_at(&mut self, addr: u32, value: u8, now: u64) {
        self.advance_to(now);
        self.write_byte(addr, value);
        // With refresh stopped, a new rate restarts the counter. The old
        // mode stays in force until the first edge at the requested rate.
        if self.next_refresh == u64::MAX && block(addr) == 0 {
            if let Some(period) = Self::refresh_period(value) {
                self.next_refresh = now.saturating_add(period);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_phase_and_open_page_survive_a_save_and_restore() {
        let mut r = Ramsey::new(RamseyRevision::Rev4, 1 << 20);
        // The power-up rate puts the first refresh at 238 clocks. The
        // page-mode word selects rate 00, so the next edge is 154 later.
        r.write_at(RAMSEY_CONTROL, CONTROL_PAGE | CONTROL_RAMWIDTH, 12);
        r.advance_to(238);
        assert_eq!(r.access_clocks(0x07c0_0000), 5);
        r.write_at(RAMSEY_CONTROL, CONTROL_RAMWIDTH, 300);
        let bytes = rmp_serde::to_vec_named(&r).unwrap();
        let mut restored: Ramsey = rmp_serde::from_slice(&bytes).unwrap();
        for ramsey in [&mut r, &mut restored] {
            assert_eq!(ramsey.read_byte(RAMSEY_CONTROL), CONTROL_RAMWIDTH);
            ramsey.advance_to(391);
            assert_eq!(ramsey.access_clocks(0x07c0_0120), 3);
            ramsey.advance_to(392);
            assert_eq!(ramsey.control(), CONTROL_RAMWIDTH);
            assert_eq!(ramsey.access_clocks(0x07c0_0120), 5);
        }
        let old: Ramsey = serde_json::from_str(r#"{"revision":"Rev4","control":16}"#).unwrap();
        assert_eq!(old.next_refresh, 154);
        assert_eq!(old.pending, None);
    }

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
    /// the guest with interrupts disabled. SysInfo writes a mode and reads it
    /// back on the next instruction, and reports no Ramsey (and skips the
    /// Gary ID probe) if the old word answers. The DRAM timing still changes
    /// only at the refresh edge.
    #[test]
    fn a_control_write_reads_back_at_once_and_takes_effect_at_refresh() {
        let mut r = Ramsey::new(RamseyRevision::Rev7, 4 * 1024 * 1024);
        let mut now = 0;
        for value in [0x00, 0xFF, CONTROL_BURST, CONTROL_PAGE | CONTROL_WRAP] {
            let old = r.control();
            r.write_at(RAMSEY_CONTROL, value, now);
            assert_eq!(r.read_byte(RAMSEY_CONTROL), value);
            now = r.next_refresh;
            r.advance_to(now - 1);
            assert_eq!(r.control(), old);
            assert_eq!(r.read_byte(RAMSEY_CONTROL), value);
            r.advance_to(now);
            assert_eq!(r.control(), value);
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

    /// RSPEED is high on a 25 MHz board, which selects refresh rate 01 (238
    /// clocks) at power-up; the test bit and the three mode bits come up
    /// clear.
    #[test]
    fn power_up_loads_the_25mhz_refresh_default() {
        for revision in [RamseyRevision::Rev4, RamseyRevision::Rev7] {
            let r = Ramsey::new(revision, 1024 * 1024);
            assert_eq!(r.control() & CONTROL_REFRESH, 1 << 5);
            assert_eq!(r.next_refresh, 238);
            assert_eq!(
                r.control() & (CONTROL_TEST | CONTROL_PAGE | CONTROL_BURST | CONTROL_WRAP),
                0
            );
        }
    }

    /// Only *PWRUP reloads the defaults. A power cycle restores them even
    /// when software has rewritten the RAMsize bit the RSIZE jumper seeded.
    #[test]
    fn a_power_cycle_reloads_the_defaults_from_the_board_jumpers() {
        let mut r = Ramsey::new(RamseyRevision::Rev7, 4 * 1024 * 1024);
        let power_up = r.control();
        r.write_at(RAMSEY_CONTROL, CONTROL_PAGE | CONTROL_BURST, 10);
        r.advance_to(238);
        assert_eq!(r.control(), CONTROL_PAGE | CONTROL_BURST);
        r.power_on_reset(1000);
        assert_eq!(r.next_refresh, 1238);
        assert_eq!(r.control(), power_up);
        assert_eq!(r.read_byte(RAMSEY_CONTROL), power_up);
        assert_eq!(r.bank_bytes(), 4 * 1024 * 1024);
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
