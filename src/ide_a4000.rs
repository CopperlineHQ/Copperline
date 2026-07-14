// SPDX-License-Identifier: GPL-3.0-or-later

//! A4000 motherboard IDE: an ATA task file ([`crate::ata`]) decoded at $DD2020,
//! with no gate array in front of it.
//!
//! The layout is the one Kickstart's own scsi.device probes (confirmed with
//! `[debug] log_unmapped` on an A4000 boot: it writes the drive/head register at
//! $DD203A and polls status at $DD203E). The task file has the same 4-byte
//! stride as Gayle's, based at $DD2020, and the control block sits one A12 page
//! up at $DD303A -- matching the A4000T and the Linux `buddha`/`gayle` style
//! decode. Unlike Gayle there is no interrupt-change latch: INTRQ feeds INT2
//! directly, and the driver clears it by reading the status register.

use crate::ata::{task_file_reg, AtaBus, IdeDrive, IdeReg};

/// Base of the IDE window. The task file runs to $DD203F and the control block
/// lives one A12 page up.
pub const IDE_BASE: u32 = 0x00DD_2020;
const IDE_TASKFILE_END: u32 = 0x00DD_2040;
/// Alternate status (read) / device control (write), on either byte of the word.
const IDE_CONTROL: u32 = 0x00DD_3038;
/// Interrupt status: bit 7 is the drive's INTRQ. Kickstart's scsi.device polls
/// this to decide whether the INT2 it took belongs to the IDE port, and spins
/// here forever if nothing answers (2.8M reads a boot with the window undecoded).
/// The interface has no latch: the bit follows the line, which the driver drops
/// by reading the status register.
const IDE_IRQ: u32 = 0x00DD_3020;
/// INTRQ, in the interrupt status register.
const IRQ_IDE: u8 = 0x80;

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct IdeA4000 {
    ata: AtaBus,
}

impl IdeA4000 {
    pub fn new() -> Self {
        Self { ata: AtaBus::new() }
    }

    pub fn attach_drive(&mut self, slot: usize, drive: IdeDrive) {
        self.ata.attach_drive(slot, drive);
    }

    pub fn reset(&mut self) {
        self.ata.reset();
    }

    pub fn take_activity(&mut self) -> bool {
        self.ata.take_activity()
    }

    /// INT2 (PORTS). The interface has no interrupt register of its own, so the
    /// drive's INTRQ is the line, gated only by the control block's nIEN.
    pub fn int2_line(&self) -> bool {
        self.ata.irq_level()
    }

    /// Whether the interface drives the bus at `addr`. Everything else in the
    /// $DD0000 page belongs to the SDMAC or floats.
    pub fn decodes(addr: u32) -> bool {
        Self::ide_reg(addr).is_some() || Self::is_irq_reg(addr)
    }

    fn is_irq_reg(addr: u32) -> bool {
        addr == IDE_IRQ || addr == IDE_IRQ + 1
    }

    fn ide_reg(addr: u32) -> Option<IdeReg> {
        match addr {
            _ if (IDE_BASE..IDE_TASKFILE_END).contains(&addr) => task_file_reg(addr - IDE_BASE),
            IDE_CONTROL | 0x00DD_303A => Some(IdeReg::AltStatusDevCtl),
            _ => None,
        }
    }

    pub fn read(&mut self, addr: u32, size: usize) -> u32 {
        if size == 4 {
            let hi = self.read(addr, 2);
            let lo = self.read(addr.wrapping_add(2), 2);
            return (hi << 16) | lo;
        }
        let value = if Self::is_irq_reg(addr) {
            let byte = u32::from(if self.ata.irq_level() { IRQ_IDE } else { 0 });
            // A word read puts the register on D15-D8, as the byte lanes wire it.
            if size == 2 && addr == IDE_IRQ {
                byte << 8
            } else {
                byte
            }
        } else {
            self.ata.read_reg(Self::ide_reg(addr), size)
        };
        // The edge latch is Gayle's business; drop it so it cannot go stale.
        self.ata.take_irq_edge();
        if crate::envcfg::flag("COPPERLINE_DIAG_GAYLE") {
            log::info!("a4000 ide rd {addr:#08X}/{size} -> {value:#06X}");
        }
        value
    }

    pub fn write(&mut self, addr: u32, size: usize, value: u32) {
        if size == 4 {
            self.write(addr, 2, value >> 16);
            self.write(addr.wrapping_add(2), 2, value & 0xFFFF);
            return;
        }
        if crate::envcfg::flag("COPPERLINE_DIAG_GAYLE") {
            log::info!("a4000 ide wr {addr:#08X}/{size} <- {value:#06X}");
        }
        // The interrupt status register has nothing to write: the line is the
        // drive's, and reading the status register is what drops it.
        if !Self::is_irq_reg(addr) {
            self.ata.write_reg(Self::ide_reg(addr), size, value);
        }
        self.ata.take_irq_edge();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decode the ROM's probe walks: drive/head at $DD203A, status at
    /// $DD203E, cylinder low/high at $DD2032/$DD2036 -- Gayle's 4-byte stride,
    /// based at $DD2020.
    #[test]
    fn the_task_file_decodes_where_kickstart_probes_it() {
        assert_eq!(IdeA4000::ide_reg(0x00DD_2020), Some(IdeReg::Data));
        assert_eq!(IdeA4000::ide_reg(0x00DD_2032), Some(IdeReg::CylLow));
        assert_eq!(IdeA4000::ide_reg(0x00DD_2036), Some(IdeReg::CylHigh));
        assert_eq!(IdeA4000::ide_reg(0x00DD_203A), Some(IdeReg::DriveHead));
        assert_eq!(IdeA4000::ide_reg(0x00DD_203E), Some(IdeReg::StatusCommand));
        assert_eq!(
            IdeA4000::ide_reg(0x00DD_303A),
            Some(IdeReg::AltStatusDevCtl)
        );
        // The SDMAC's registers are in the same page and are not ours.
        assert!(!IdeA4000::decodes(0x00DD_0043));
        assert!(!IdeA4000::decodes(0x00DD_2000));
        assert!(!IdeA4000::decodes(0x00DD_2040));
    }

    /// An empty cable floats the status register, which is how the ROM's probe
    /// concludes there is no drive rather than waiting on one.
    #[test]
    fn an_empty_cable_floats_the_status_register() {
        let mut ide = IdeA4000::new();
        ide.write(0x00DD_203A, 1, 0xA0);
        assert_eq!(ide.read(0x00DD_203E, 1) as u8, 0xFF);
        assert!(!ide.int2_line());
    }

    /// The interrupt status register follows INTRQ, and reading the drive's
    /// status drops it. Kickstart's scsi.device polls $DD3020 after every INT2
    /// and spins there forever if it never sees the bit (2.8M reads a boot).
    #[test]
    fn the_interrupt_register_follows_intrq_and_the_status_read_clears_it() {
        let path = std::env::temp_dir().join(format!(
            "copperline-a4000-ide-{}-{}.hdf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::write(&path, vec![0u8; 64 * crate::ata::SECTOR_SIZE]).unwrap();
        let mut ide = IdeA4000::new();
        ide.attach_drive(0, IdeDrive::open(&path, 0, None).unwrap());

        assert_eq!(
            ide.read(IDE_IRQ, 1) as u8 & IRQ_IDE,
            0,
            "idle: no interrupt"
        );

        // IDENTIFY completes and raises INTRQ.
        ide.write(0x00DD_203A, 1, 0xA0);
        ide.write(0x00DD_203E, 1, 0xEC);
        assert!(ide.int2_line());
        assert_eq!(ide.read(IDE_IRQ, 1) as u8 & IRQ_IDE, IRQ_IDE);
        // The register sits on D15-D8 of a word access.
        assert_eq!(ide.read(IDE_IRQ, 2) as u16, u16::from(IRQ_IDE) << 8);

        // Reading the status register drops the line, and the register with it.
        ide.read(0x00DD_203E, 1);
        assert_eq!(ide.read(IDE_IRQ, 1) as u8 & IRQ_IDE, 0);
        assert!(!ide.int2_line());
        std::fs::remove_file(&path).ok();
    }
}
