// SPDX-License-Identifier: GPL-3.0-or-later

//! DeMoN II, EPROM fitted, hidden visibility jumper, no USB host.
//! Address decoding and bus triggers follow the board's Quartus/DeMoN.v.

use super::*;

pub(super) const BASE: u32 = 0xa80000;
const ROM_SIZE: usize = 0x40000;
const BANK_SIZE: usize = ROM_SIZE + 0x100000;
const USB_BASE: usize = BANK_SIZE - 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Phase {
    #[default]
    Hidden,
    Interrupt,
    Vector(u8),
    Running,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Demon {
    phase: Phase,
    cause: u8,
    triggers: u8,
    reset_armed: bool,
}

impl Demon {
    pub(super) fn reset() -> Self {
        Self {
            reset_armed: true,
            ..Default::default()
        }
    }

    pub(super) fn visible(&self) -> bool {
        self.phase != Phase::Hidden
    }

    pub(super) fn acknowledge(&mut self) {
        self.phase = Phase::Vector(4);
    }

    fn reset_triggered(&self, addr: u32, size: usize) -> bool {
        // The CPLD decodes A23..A1. A long access at $6 puts the word at
        // $8 on the bus during its second cycle.
        self.reset_armed && addr < 10 && addr + size as u32 > 8
    }

    fn program_access(&mut self, addr: u32) {
        if self.phase == Phase::Running && addr.wrapping_sub(BASE) >= BANK_SIZE as u32 {
            self.phase = Phase::Hidden;
        }
    }

    pub(super) fn eprom_read(bank: &[u8], addr: u32, size: usize) -> u32 {
        (0..size).fold(0, |value, i| {
            value << 8 | u32::from(bank[(addr as usize + i) & (ROM_SIZE - 1)])
        })
    }

    pub(super) fn writable(off: usize) -> bool {
        (ROM_SIZE..USB_BASE).contains(&off)
    }

    pub(super) fn set_mode(&mut self, value: u8) {
        self.triggers = value & 3;
    }

    pub(super) fn read(&self, bank: &[u8], off: usize, floating: u16) -> u8 {
        let lane = floating.to_be_bytes()[off & 1];
        match off {
            0 => lane,
            1 => (lane & !3) | self.cause,
            // FT245 RXF# and TXE# are high when disconnected.
            off if off == USB_BASE + 3 => lane | 3,
            off if off >= USB_BASE => lane,
            _ => bank[off],
        }
    }
}

impl Cartridge {
    pub fn demon2(image: &[u8]) -> Result<Self, ImageError> {
        if image.len() != ROM_SIZE {
            return Err(ImageError::DemonRomSize(image.len()));
        }
        let mut bank = vec![0; BANK_SIZE];
        bank[..ROM_SIZE].copy_from_slice(image);
        Ok(Self {
            model: CartridgeModel::Demon2,
            bank,
            custom_shadow: Vec::new(),
            ciaa_shadow: [0; 16],
            ciab_shadow: [0; 16],
            nmi_pending: false,
            freezes: 0,
            demon: Some(Demon::reset()),
        })
    }

    pub(super) fn trigger_demon(&mut self, cause: u8) {
        let Some(demon) = self.demon.as_mut() else {
            return;
        };
        if !demon.visible() {
            demon.phase = Phase::Interrupt;
            demon.cause = cause;
            demon.reset_armed = false;
            self.nmi_pending = true;
            self.freezes += 1;
        }
    }

    pub(crate) fn observe_read(&mut self, addr: u32, size: usize, fetch: bool) -> Option<u32> {
        let demon = self.demon.as_mut()?;
        if let Phase::Vector(remaining) = demon.phase {
            // The CPU reads the vector as one long or as two words. Its
            // EPROM address pins follow the bus, including a relocated VBR.
            let remaining = remaining.saturating_sub(size as u8);
            demon.phase = if remaining == 0 {
                Phase::Running
            } else {
                Phase::Vector(remaining)
            };
            return Some(Demon::eprom_read(&self.bank, addr, size));
        }
        if fetch {
            demon.program_access(addr);
        }
        if demon.reset_triggered(addr, size) {
            self.trigger_demon(3);
        } else if demon.triggers & 2 != 0 && addr <= 0xbfe001 && 0xbfe001 - addr < size as u32 {
            self.trigger_demon(1);
        }
        None
    }

    /// The 68000 core's queued prefetches use its ordinary word-read hook.
    /// Resolve the code space before the new instruction's operand accesses.
    pub(crate) fn instruction_at(&mut self, addr: u32) {
        if let Some(demon) = &mut self.demon {
            demon.program_access(addr);
        }
    }

    pub(crate) fn observe_write(&mut self, addr: u32, size: usize, value: u32) {
        let Some(demon) = self.demon.as_ref() else {
            return;
        };
        if demon.reset_triggered(addr, size) {
            self.trigger_demon(3);
        } else if demon.triggers & 1 != 0 && addr <= 0xbfd100 && 0xbfd100 - addr < size as u32 {
            self.trigger_demon(2);
        }
        // The SRAM sees external CPU writes to $DFFxxx, including lane
        // strobes. Its address pins alias that page at $AFFxxx.
        if (0xdff000..0xe00000).contains(&addr) {
            for i in 0..size.min((0xe00000 - addr) as usize) {
                self.bank[0x7f000 + (addr as usize & 0xfff) + i] =
                    (value >> (8 * (size - i - 1))) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cart() -> Cartridge {
        let mut eprom = vec![0xff; ROM_SIZE];
        eprom[0x7c..0x80].copy_from_slice(&0x00a80100u32.to_be_bytes());
        Cartridge::demon2(&eprom).unwrap()
    }

    #[test]
    fn demon2_reset_decodes_either_lane_of_any_access_touching_the_word_at_8() {
        for (addr, size, triggers) in [
            (4, 4, false),
            (6, 2, false),
            (6, 4, true),
            (8, 1, true),
            (9, 1, true),
            (8, 2, true),
            (8, 4, true),
            (10, 2, false),
        ] {
            let mut c = cart();
            c.observe_read(addr, size, false);
            assert_eq!(c.nmi_pending(), triggers, "read {size} at {addr:#x}");
            let mut c = cart();
            c.observe_write(addr, size, 0);
            assert_eq!(c.nmi_pending(), triggers, "write {size} at {addr:#x}");
        }
    }

    #[test]
    fn demon2_bus_triggers_drive_the_vector_and_report_the_cause() {
        let mut c = cart();
        assert!(!c.decodes(BASE));
        c.observe_write(4, 4, 0);
        assert!(!c.nmi_pending());
        c.observe_write(8, 2, 0x1234);
        for cause in [3, 1, 2, 0] {
            assert!(c.take_nmi());
            assert_eq!(c.observe_read(0x7c, 2, false), Some(0xa8));
            assert_eq!(c.observe_read(0x7e, 2, false), Some(0x0100));
            c.instruction_at(BASE + 0x100);
            assert!(c.decodes(0xbbffff));
            assert_eq!(c.read_byte(1, 0xaaaa), 0xa8 | cause);
            assert_eq!(c.read_byte(3, 0), 0xff);
            c.write_byte(1, 3);
            c.trigger_demon(0);
            assert!(!c.nmi_pending());
            c.observe_read(0xfc0000, 2, false);
            assert!(c.entered());
            c.instruction_at(0xfc0000);
            assert!(!c.decodes(BASE));
            c.observe_read(0xbfe000, 1, false);
            c.observe_write(0xbfd101, 1, 0);
            assert!(!c.nmi_pending());
            match cause {
                3 => {
                    c.observe_read(0xbfe000, 2, false);
                }
                1 => c.observe_write(0xbfd100, 1, 0),
                2 => c.trigger_demon(0),
                _ => {}
            }
        }
        assert_eq!(c.freezes(), 4);
    }

    #[test]
    fn demon2_snoops_cpu_byte_lanes_and_saves_hidden_ram() {
        let mut c = cart();
        c.observe_write(0xdff180, 2, 0xf123);
        c.observe_write(0xdff181, 1, 0x56);
        c.trigger_demon(0);
        assert!(c.take_nmi());
        assert_eq!(c.observe_read(0x7c, 4, false), Some(0xa80100));
        assert_eq!(c.read_byte(0x7f180, 0), 0xf1);
        assert_eq!(c.read_byte(0x7f181, 0), 0x56);
        c.write_byte(0x200, 0x55);
        assert_eq!(c.read_byte(0x200, 0), 0xff);
        c.write_byte(USB_BASE - 1, 0x28);
        c.write_byte(USB_BASE + 3, 0);
        assert_eq!(c.read_byte(USB_BASE + 3, 0), 3);
        c.instruction_at(0x200);
        let saved = rmp_serde::to_vec_named(&c).unwrap();
        let mut restored: Cartridge = rmp_serde::from_slice(&saved).unwrap();
        assert!(!restored.decodes(BASE));
        restored.trigger_demon(0);
        assert_eq!(restored.read_byte(USB_BASE - 1, 0), 0x28);
        assert_eq!(restored.read_byte(0x7f181, 0), 0x56);
    }
}
