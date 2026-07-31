// SPDX-License-Identifier: GPL-3.0-or-later

//! Village Tronic Picasso II and Picasso II+ RTG boards.
//!
//! The physical card exposes two consecutive Zorro II autoconfig identities:
//! a linear VRAM aperture (product 11) and a 64 KB VGA-register aperture
//! (product 12). Both map to this one device; the original and II+ revisions
//! share those products but advertise different serials. `BoardSpec::window`
//! tags the VRAM offset in its high nibble so the ordinary `ZorroDevice`
//! interface can distinguish the apertures.

mod gd5426;

use crate::zorro::DEVICE_WINDOW_SHIFT;
use crate::zorro_device::{DeviceHost, ZorroDevice};
pub use gd5426::{CirrusGd5426, DecodedMode, PixelDepth};

const WINDOW_MASK: u32 = (1 << DEVICE_WINDOW_SHIFT) - 1;
const WINDOW_REGS: u32 = 0;
const WINDOW_VRAM: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Picasso2 {
    chip: CirrusGd5426,
    plus: bool,
    show_rtg: bool,
    interrupt_enabled: bool,
}

impl Picasso2 {
    pub fn new(vram_bytes: usize) -> Self {
        Self {
            chip: CirrusGd5426::new(vram_bytes),
            plus: false,
            show_rtg: false,
            interrupt_enabled: false,
        }
    }

    pub fn new_plus(vram_bytes: usize) -> Self {
        Self {
            chip: CirrusGd5426::new_gd5428(vram_bytes),
            plus: true,
            show_rtg: false,
            interrupt_enabled: false,
        }
    }

    pub fn rtg_active(&self) -> bool {
        self.show_rtg && self.chip.video_valid()
    }

    pub fn rtg_frame(&self, out: &mut Vec<u32>) -> Option<(u32, u32)> {
        self.show_rtg
            .then(|| self.chip.compose_frame(out))
            .flatten()
    }

    fn read_register_window(&mut self, off: u32, size: usize) -> u32 {
        let mut value = 0u32;
        for i in 0..size {
            value = (value << 8) | u32::from(self.read_register_byte(off + i as u32));
        }
        value
    }

    fn read_register_byte(&mut self, off: u32) -> u8 {
        if off >= 0x2000 {
            return 0xff;
        }
        let port = if off >= 0x1000 {
            (off & 0x0fff).saturating_add(1)
        } else {
            off
        };
        if !(0x3b0..=0x3df).contains(&port) {
            return 0xff;
        }
        self.chip.io_read(port as u16)
    }

    fn write_register_window(&mut self, off: u32, size: usize, value: u32) {
        for i in 0..size {
            let byte = (value >> (8 * (size - 1 - i))) as u8;
            self.write_register_byte(off + i as u32, byte);
        }
    }

    fn write_register_byte(&mut self, off: u32, value: u8) {
        if off >= 0x8000 {
            if off & 1 == 0 {
                match off >> 12 {
                    0x8 | 0xa => self.set_monitor_switch(true),
                    0x9 | 0xb => self.set_monitor_switch(false),
                    _ => {}
                }
            }
            return;
        }
        if off == 0x1000 {
            self.interrupt_enabled = false;
            return;
        }
        if off == 0x1001 {
            self.interrupt_enabled = true;
            return;
        }
        if off >= 0x2000 {
            return;
        }
        let port = if off >= 0x1000 {
            (off & 0x0fff).saturating_add(1)
        } else {
            off
        };
        // POS102 and the 46E8 sleep/wakeup register are board-level no-ops.
        if (0x3b0..=0x3df).contains(&port) {
            self.chip.io_write(port as u16, value);
        }
    }

    fn set_monitor_switch(&mut self, show_rtg: bool) {
        if self.show_rtg != show_rtg && crate::envcfg::flag("COPPERLINE_DIAG_PICASSO") {
            log::info!(
                "picasso2: monitor switch -> {}",
                if show_rtg { "VGA" } else { "Amiga" }
            );
        }
        self.show_rtg = show_rtg;
    }
}

impl ZorroDevice for Picasso2 {
    fn read(&mut self, tagged_off: u32, size: usize, _host: &mut DeviceHost) -> u32 {
        let window = tagged_off >> DEVICE_WINDOW_SHIFT;
        let off = tagged_off & WINDOW_MASK;
        match window {
            WINDOW_REGS => self.read_register_window(off, size),
            WINDOW_VRAM => self.chip.vram_read(off as usize, size),
            _ => open_bus(size),
        }
    }

    fn write(&mut self, tagged_off: u32, size: usize, value: u32, _host: &mut DeviceHost) {
        let window = tagged_off >> DEVICE_WINDOW_SHIFT;
        let off = tagged_off & WINDOW_MASK;
        match window {
            WINDOW_REGS => self.write_register_window(off, size, value),
            WINDOW_VRAM => self.chip.vram_write(off as usize, size, value),
            _ => {}
        }
    }

    fn peek_word(&self, tagged_off: u32) -> Option<u16> {
        let window = tagged_off >> DEVICE_WINDOW_SHIFT;
        let off = (tagged_off & WINDOW_MASK) as usize;
        if window != WINDOW_VRAM || off + 2 > self.chip.vram_len() {
            return None;
        }
        Some(self.chip.vram_read(off, 2) as u16)
    }

    fn tick(&mut self, cck: u32, _host: &mut DeviceHost) {
        self.chip.tick(cck);
    }

    fn int2_line(&self) -> bool {
        self.plus && self.interrupt_enabled && self.chip.vblank_pending()
    }

    fn is_idle(&self) -> bool {
        self.chip.is_idle()
    }

    fn next_event_cck(&self) -> Option<u32> {
        self.chip.next_event_cck()
    }

    fn reset(&mut self) {
        self.chip.reset();
        self.show_rtg = false;
        self.interrupt_enabled = false;
    }

    fn kind(&self) -> &'static str {
        if self.plus {
            "picasso2plus"
        } else {
            "picasso2"
        }
    }
}

impl Default for Picasso2 {
    fn default() -> Self {
        Self::new(2 * 1024 * 1024)
    }
}

fn open_bus(size: usize) -> u32 {
    match size {
        1 => 0xff,
        2 => 0xffff,
        4 => 0xffff_ffff,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Memory;

    fn memory() -> Memory {
        Memory {
            chip_ram: vec![0; 0x100],
            slow_ram: Vec::new(),
            mb_ram: Vec::new(),
            accel_ram: Vec::new(),
            rom: Vec::new(),
            overlay: false,
            zorro: crate::zorro::ZorroChain::default(),
            extended_rom: Vec::new(),
            extended_rom_base: 0,
            wcs: Vec::new(),
            wcs_write_protected: false,
        }
    }

    fn regs(off: u32) -> u32 {
        off
    }

    fn vram(off: u32) -> u32 {
        WINDOW_VRAM << DEVICE_WINDOW_SHIFT | off
    }

    #[test]
    fn register_word_write_reaches_index_and_data_ports() {
        let mut board = Picasso2::default();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(regs(0x3c4), 2, 0x0612, &mut host);
        assert_eq!(board.read(regs(0x3c4), 2, &mut host), 0x0612);
        board.write(regs(0x3c4), 1, 0x07, &mut host);
        board.write(regs(0x13c4), 1, 0x01, &mut host);
        assert_eq!(board.read(regs(0x13c4), 1, &mut host), 0x01);
    }

    #[test]
    fn vram_window_is_big_endian_on_the_bus_and_linear_in_storage() {
        let mut board = Picasso2::default();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(vram(2), 4, 0x1234_5678, &mut host);
        assert_eq!(board.read(vram(2), 4, &mut host), 0x1234_5678);
        assert_eq!(board.peek_word(vram(3)), Some(0x3456));
    }

    #[test]
    fn monitor_switch_resets_to_native_and_ignores_odd_writes() {
        let mut board = Picasso2::default();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(regs(0x8001), 1, 0, &mut host);
        assert!(!board.show_rtg);
        board.write(regs(0x8000), 1, 0, &mut host);
        assert!(board.show_rtg);
        board.write(regs(0x9000), 1, 0, &mut host);
        assert!(!board.show_rtg);
        board.write(regs(0xa000), 1, 0, &mut host);
        assert!(board.show_rtg);
        board.reset();
        assert!(!board.show_rtg);
    }

    #[test]
    fn monitor_switch_only_presents_a_valid_programmed_mode() {
        let mut board = Picasso2::default();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(regs(0x8000), 1, 0, &mut host);
        assert!(!board.rtg_active(), "power-on register state is not video");

        for (port, value) in [
            (0x3c4, 6),
            (0x3c5, 0x12),
            (0x3c4, 0),
            (0x3c5, 3),
            (0x3c4, 1),
            (0x3c5, 0),
            (0x3c4, 7),
            (0x3c5, 1),
            (0x3d4, 1),
            (0x3d5, 1),
            (0x3d4, 0x12),
            (0x3d5, 15),
            (0x3d4, 0x13),
            (0x3d5, 2),
        ] {
            board.chip.io_write(port, value);
        }
        assert!(board.rtg_active());
        board.chip.io_write(0x3c4, 1);
        board.chip.io_write(0x3c5, 0x20);
        assert!(
            !board.rtg_active(),
            "sequencer blanking restores native video"
        );
    }

    #[test]
    fn unused_register_and_vram_offsets_are_open_bus() {
        let mut board = Picasso2::new(1024 * 1024);
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(regs(0x2000), 1, &mut host), 0xff);
        assert_eq!(board.read(regs(0x2000), 2, &mut host), 0xffff);
        assert_eq!(board.read(vram(1024 * 1024), 4, &mut host), 0xffff_ffff);
    }

    fn program_interrupt_mode(board: &mut Picasso2) {
        for (port, value) in [
            (0x3c4, 6),
            (0x3c5, 0x12),
            (0x3c4, 0),
            (0x3c5, 3),
            (0x3c4, 7),
            (0x3c5, 1),
            (0x3d4, 0),
            (0x3d5, 15),
            (0x3d4, 1),
            (0x3d5, 1),
            (0x3d4, 6),
            (0x3d5, 20),
            (0x3d4, 0x10),
            (0x3d5, 16),
            // Bit 4 arms vertical interrupts; low bits end retrace at 18.
            (0x3d4, 0x11),
            (0x3d5, 0x12),
            (0x3d4, 0x12),
            (0x3d5, 15),
            (0x3d4, 0x13),
            (0x3d5, 2),
        ] {
            board.chip.io_write(port, value);
        }
    }

    #[test]
    fn picasso2plus_latches_int2_at_vblank_and_cr11_acknowledges_it() {
        let mut plus = Picasso2::new_plus(1024 * 1024);
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        program_interrupt_mode(&mut plus);
        plus.write(regs(0x1001), 1, 0, &mut host);
        for _ in 0..200_000 {
            plus.tick(1, &mut host);
            if plus.int2_line() {
                break;
            }
        }
        assert!(plus.int2_line());
        assert_ne!(plus.chip.io_read(0x3c2) & 0x80, 0);

        let bytes = bincode::serialize(&plus).unwrap();
        let mut resumed: Picasso2 = bincode::deserialize(&bytes).unwrap();
        assert_eq!(resumed.kind(), "picasso2plus");
        assert!(resumed.int2_line());
        resumed.chip.io_write(0x3d4, 0x27);
        assert_eq!(resumed.chip.io_read(0x3d5), 0x98);

        for board in [&mut plus, &mut resumed] {
            board.chip.io_write(0x3d4, 0x11);
            board.chip.io_write(0x3d5, 0x02);
            assert!(!board.int2_line());
            assert_eq!(board.chip.io_read(0x3c2) & 0x80, 0);
        }

        let mut plain = Picasso2::new(1024 * 1024);
        program_interrupt_mode(&mut plain);
        plain.write(regs(0x1001), 1, 0, &mut host);
        for _ in 0..200_000 {
            plain.tick(1, &mut host);
        }
        assert!(!plain.int2_line());
    }
}
