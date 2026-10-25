//! Interrupt handling.

use super::cpu::CpuCore;

impl CpuCore {
    /// Set pending interrupt level.
    pub fn set_irq(&mut self, level: u8) {
        self.int_level = (level & 7) as u32;
    }

    /// Check if an interrupt should be serviced.
    pub fn check_interrupts(&self) -> bool {
        let mask = self.int_mask >> 8;
        self.int_level == 7 || self.int_level > mask
    }
}
