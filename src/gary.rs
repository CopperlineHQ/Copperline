// SPDX-License-Identifier: GPL-3.0-or-later

//! Fat Gary: the bus controller of the A3000 and A4000.
//!
//! Gary owns the `$DE0000` page, and its address decode is cruder than a
//! register map suggests: only the byte lane and two address bits matter, so
//! every register is mirrored many times over.
//!
//! ```text
//! lane = addr & 3          0,1,2 -> Gary; 3 -> Ramsey
//! block = (addr >> 6) & 3  Ramsey: 0 -> control, 1 -> version, 2/3 -> $FF
//! ```
//!
//! The whole thing then repeats every `$100` to the end of the page. So Gary's
//! timeout register is at `$DE0000`, `$DE0004`, `$DE0100`, ... and the Ramsey
//! version register that Kickstart reads at `$DE0043` is equally at `$DE0047`
//! or `$DE0143`. This is why the diagnostic tools read addresses that look like
//! nothing in particular: they are reading a mirror.
//!
//! Gary's three registers are single bits, all in bit 7:
//!
//! - `$DE0000` TIMEOUT: what an unanswered bus cycle produces, BERR or DSACK.
//! - `$DE0001` TOENB: whether the timeout is enabled at all.
//! - `$DE0002` COLDBOOT: the power-up flag, set by a cold start and cleared by
//!   the OS. It is read/write, which is what makes it a Gary detector:
//!   xSysInfo writes bit 7, reads a custom register to clear the sticky bus,
//!   and expects to read bit 7 back -- a floating bus cannot fake that. Without
//!   it, xSysInfo decides there is no Fat Gary and then does not even look for
//!   the Ramsey behind it.
//!
//! Nothing here changes emulated behaviour: bus timeouts are not modelled (an
//! unanswered cycle floats, it does not fault), and Kickstart reads COLDBOOT
//! only to decide whether the reboot was warm. The registers exist so the
//! machine can be identified as what it is.

/// The page Gary decodes. Above `$DE8000` the decode stops (amiberry draws the
/// line in the same place); the A600/A1200's Gayle ID register at `$DE1000` is
/// inside it, which is correct -- those machines have a Gayle instead of a
/// Gary, never both.
pub const GARY_BASE: u32 = 0x00DE_0000;
pub const GARY_SIZE: u32 = 0x8000;

/// Byte lanes 0-2 are Gary's; lane 3 belongs to Ramsey.
const LANE_TIMEOUT: u32 = 0;
const LANE_TOENB: u32 = 1;
const LANE_COLDBOOT: u32 = 2;
/// Every Gary register is a single bit in bit 7.
const FLAG: u8 = 0x80;

/// Whether Gary drives the bus for a byte access at `addr`. Lane 3 is Ramsey's
/// (see [`crate::ramsey::decodes`]), so the two never collide.
pub fn decodes(addr: u32) -> bool {
    (GARY_BASE..GARY_BASE + GARY_SIZE).contains(&addr) && (addr & 3) != 3
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Gary {
    /// TIMEOUT: an unanswered bus cycle raises a bus error rather than being
    /// completed with DSACK.
    berr_on_timeout: bool,
    /// TOENB: the bus timeout is enabled.
    timeout_enabled: bool,
    /// COLDBOOT: this reset was a cold start.
    coldboot: bool,
}

impl Gary {
    /// A Gary out of a cold start, which is what the flag means.
    pub fn new() -> Self {
        Self {
            berr_on_timeout: false,
            timeout_enabled: false,
            coldboot: true,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn read_byte(&self, addr: u32) -> u8 {
        let flag = match addr & 3 {
            LANE_TIMEOUT => self.berr_on_timeout,
            LANE_TOENB => self.timeout_enabled,
            LANE_COLDBOOT => self.coldboot,
            _ => return 0xFF, // lane 3 is Ramsey's; we never decode it
        };
        if flag {
            FLAG
        } else {
            0x00
        }
    }

    pub fn write_byte(&mut self, addr: u32, value: u8) {
        let flag = value & FLAG != 0;
        match addr & 3 {
            LANE_TIMEOUT => self.berr_on_timeout = flag,
            LANE_TOENB => self.timeout_enabled = flag,
            LANE_COLDBOOT => self.coldboot = flag,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xSysInfo's Fat Gary probe: write bit 7 to the coldboot register, read it
    /// back, then write zero and read that back. A floating bus fails this, and
    /// failing it means xSysInfo never looks for the Ramsey behind Gary.
    #[test]
    fn the_coldboot_register_is_a_readable_writable_bit() {
        let mut g = Gary::new();
        g.write_byte(GARY_BASE + 2, 0x80);
        assert_eq!(g.read_byte(GARY_BASE + 2) & 0x80, 0x80);
        g.write_byte(GARY_BASE + 2, 0x00);
        assert_eq!(g.read_byte(GARY_BASE + 2) & 0x80, 0x00);
    }

    /// A cold start is what the machine comes up out of.
    #[test]
    fn a_fresh_gary_reports_a_cold_boot() {
        assert_eq!(Gary::new().read_byte(GARY_BASE + 2), 0x80);
    }

    /// Kickstart writes the timeout mode at $DE0000 on every boot; it and TOENB
    /// are separate registers on separate byte lanes.
    #[test]
    fn the_timeout_registers_are_independent() {
        let mut g = Gary::new();
        g.write_byte(GARY_BASE, 0x80); // BERR on timeout
        g.write_byte(GARY_BASE + 1, 0x00); // but the timeout is disabled
        assert_eq!(g.read_byte(GARY_BASE), 0x80);
        assert_eq!(g.read_byte(GARY_BASE + 1), 0x00);
    }

    /// The decode ignores everything but the byte lane, so every register is
    /// mirrored across the page -- and lane 3 is left to Ramsey.
    #[test]
    fn the_registers_are_mirrored_across_the_page() {
        let mut g = Gary::new();
        g.write_byte(GARY_BASE + 0x144, 0x80); // a mirror of the timeout register
        assert_eq!(g.read_byte(GARY_BASE), 0x80);

        assert!(decodes(GARY_BASE));
        assert!(decodes(GARY_BASE + 0x44));
        assert!(decodes(GARY_BASE + 0x1002)); // where xSysInfo probes for a Gayle
        assert!(!decodes(GARY_BASE + 3)); // Ramsey control
        assert!(!decodes(GARY_BASE + 0x43)); // Ramsey version
        assert!(!decodes(GARY_BASE + GARY_SIZE));
    }
}
