// SPDX-License-Identifier: GPL-3.0-or-later

//! Atéo Concepts Graffity RTG boards (Zorro II and Zorro III).
//!
//! Graffity uses the same Cirrus Logic CL-GD5428 core as Village Tronic's
//! Picasso II+ (see [`crate::picasso2`]), but wires it up differently:
//!
//! - The Zorro II board exposes two consecutive autoconfig identities like
//!   Picasso II does: a linear VRAM aperture (product 34) and a 128 KB
//!   VGA-register aperture (product 33) -- twice Picasso II's register
//!   window, though only the low VGA port range within it is live.
//! - The Zorro III board is a single 16 MB autoconfig window (product 33)
//!   holding three sub-apertures: a pure monitor-switch strobe trap at
//!   `+0x400000` (64 KB), the real VGA-register window at `+0x800000`
//!   (64 KB), and linear VRAM at `+0xC00000`.
//!
//! Unlike Picasso II, Graffity's register window addresses VGA ports
//! directly (no odd/even port-mirroring quirk), and the board has no
//! interrupt-enable latch of its own -- INT2 follows the CL-GD5428 core's own
//! vertical-blank state directly.

use crate::picasso2::CirrusGd5426;
use crate::zorro::DEVICE_WINDOW_SHIFT;
use crate::zorro_device::{DeviceHost, ZorroDevice};

const WINDOW_MASK: u32 = (1 << DEVICE_WINDOW_SHIFT) - 1;
const WINDOW_REGS: u32 = 0;
const WINDOW_VRAM: u32 = 1;

/// Real VGA I/O port range the CL-GD5428 core answers.
const VGA_PORT_RANGE: std::ops::RangeInclusive<u16> = 0x3b0..=0x3df;

fn open_bus(size: usize) -> u32 {
    match size {
        1 => 0xff,
        2 => 0xffff,
        4 => 0xffff_ffff,
        _ => 0,
    }
}

/// Read one VGA register byte at a window-relative offset that already
/// equals the port number, open bus outside the real port range.
fn read_vga_byte(chip: &mut CirrusGd5426, off: u32) -> u8 {
    let port = off as u16;
    if VGA_PORT_RANGE.contains(&port) {
        chip.io_read(port)
    } else {
        0xff
    }
}

fn write_vga_byte(chip: &mut CirrusGd5426, off: u32, value: u8) {
    let port = off as u16;
    if VGA_PORT_RANGE.contains(&port) {
        chip.io_write(port, value);
    }
}

fn read_vga_window(chip: &mut CirrusGd5426, off: u32, size: usize) -> u32 {
    let mut value = 0u32;
    for i in 0..size {
        value = (value << 8) | u32::from(read_vga_byte(chip, off + i as u32));
    }
    value
}

fn write_vga_window(chip: &mut CirrusGd5426, off: u32, size: usize, value: u32) {
    for i in 0..size {
        let byte = (value >> (8 * (size - 1 - i))) as u8;
        write_vga_byte(chip, off + i as u32, byte);
    }
}

/// Bit 5 (`0x20`) of the switch-strobe address selects the target and bit 6
/// (`0x40`) must also be set for the strobe to register -- `0x60` shows the
/// RTG screen, `0x40` restores the native Amiga display.
fn decode_switch_strobe(off: u32) -> Option<bool> {
    match off & 0x60 {
        0x60 => Some(true),
        0x40 => Some(false),
        _ => None,
    }
}

/// Graffity [Zorro II]: VRAM aperture (product 34, window 1) chained to a
/// 128 KB register aperture (product 33, window 0).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct GraffityZ2 {
    chip: CirrusGd5426,
    show_rtg: bool,
}

impl GraffityZ2 {
    pub fn new(vram_bytes: usize) -> Self {
        Self {
            chip: CirrusGd5426::new_gd5428(vram_bytes),
            show_rtg: false,
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

    fn write_register_byte(&mut self, off: u32, value: u8) {
        if off & 0x8000 != 0 {
            if let Some(show_rtg) = decode_switch_strobe(off) {
                self.set_monitor_switch(show_rtg);
            }
            return;
        }
        write_vga_byte(&mut self.chip, off, value);
    }

    fn write_register_window(&mut self, off: u32, size: usize, value: u32) {
        for i in 0..size {
            let byte = (value >> (8 * (size - 1 - i))) as u8;
            self.write_register_byte(off + i as u32, byte);
        }
    }

    fn set_monitor_switch(&mut self, show_rtg: bool) {
        if self.show_rtg != show_rtg && crate::envcfg::flag("COPPERLINE_DIAG_PICASSO") {
            log::info!(
                "graffityz2: monitor switch -> {}",
                if show_rtg { "VGA" } else { "Amiga" }
            );
        }
        self.show_rtg = show_rtg;
    }
}

impl ZorroDevice for GraffityZ2 {
    fn read(&mut self, tagged_off: u32, size: usize, _host: &mut DeviceHost) -> u32 {
        let window = tagged_off >> DEVICE_WINDOW_SHIFT;
        let off = tagged_off & WINDOW_MASK;
        match window {
            WINDOW_REGS => read_vga_window(&mut self.chip, off, size),
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
        self.chip.vblank_pending()
    }

    fn reset(&mut self) {
        self.chip.reset();
        self.show_rtg = false;
    }

    fn kind(&self) -> &'static str {
        "graffityz2"
    }
}

impl Default for GraffityZ2 {
    fn default() -> Self {
        Self::new(2 * 1024 * 1024)
    }
}

/// Graffity [Zorro III] board-relative sub-aperture layout inside its single
/// 16 MB window.
const SWITCH_BASE: u32 = 0x0040_0000;
const SWITCH_SIZE: u32 = 0x1_0000;
const REGS_BASE: u32 = 0x0080_0000;
const REGS_SIZE: u32 = 0x1_0000;
const VRAM_BASE: u32 = 0x00c0_0000;

/// Graffity [Zorro III]: one 16 MB autoconfig window (product 33) with the
/// switch strobe, register, and VRAM sub-apertures at fixed offsets.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct GraffityZ3 {
    chip: CirrusGd5426,
    show_rtg: bool,
}

impl GraffityZ3 {
    pub fn new(vram_bytes: usize) -> Self {
        Self {
            chip: CirrusGd5426::new_gd5428(vram_bytes),
            show_rtg: false,
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

    fn set_monitor_switch(&mut self, show_rtg: bool) {
        if self.show_rtg != show_rtg && crate::envcfg::flag("COPPERLINE_DIAG_PICASSO") {
            log::info!(
                "graffityz3: monitor switch -> {}",
                if show_rtg { "VGA" } else { "Amiga" }
            );
        }
        self.show_rtg = show_rtg;
    }

    fn vram_offset(&self, off: u32) -> Option<usize> {
        let rel = usize::try_from(off.checked_sub(VRAM_BASE)?).ok()?;
        (rel < self.chip.vram_len()).then_some(rel)
    }
}

impl ZorroDevice for GraffityZ3 {
    fn read(&mut self, off: u32, size: usize, _host: &mut DeviceHost) -> u32 {
        if let Some(vram_off) = self.vram_offset(off) {
            return self.chip.vram_read(vram_off, size);
        }
        if (REGS_BASE..REGS_BASE + REGS_SIZE).contains(&off) {
            return read_vga_window(&mut self.chip, off - REGS_BASE, size);
        }
        open_bus(size)
    }

    fn write(&mut self, off: u32, size: usize, value: u32, _host: &mut DeviceHost) {
        if (SWITCH_BASE..SWITCH_BASE + SWITCH_SIZE).contains(&off) {
            if let Some(show_rtg) = decode_switch_strobe(off) {
                self.set_monitor_switch(show_rtg);
            }
            return;
        }
        if let Some(vram_off) = self.vram_offset(off) {
            self.chip.vram_write(vram_off, size, value);
            return;
        }
        if (REGS_BASE..REGS_BASE + REGS_SIZE).contains(&off) {
            write_vga_window(&mut self.chip, off - REGS_BASE, size, value);
        }
    }

    fn peek_word(&self, off: u32) -> Option<u16> {
        let rel = off.checked_sub(VRAM_BASE)?;
        let rel = usize::try_from(rel).ok()?;
        if rel + 2 > self.chip.vram_len() {
            return None;
        }
        Some(self.chip.vram_read(rel, 2) as u16)
    }

    fn tick(&mut self, cck: u32, _host: &mut DeviceHost) {
        self.chip.tick(cck);
    }

    fn int2_line(&self) -> bool {
        self.chip.vblank_pending()
    }

    fn reset(&mut self) {
        self.chip.reset();
        self.show_rtg = false;
    }

    fn kind(&self) -> &'static str {
        "graffityz3"
    }
}

impl Default for GraffityZ3 {
    fn default() -> Self {
        Self::new(2 * 1024 * 1024)
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

    fn z2_regs(off: u32) -> u32 {
        off
    }

    fn z2_vram(off: u32) -> u32 {
        WINDOW_VRAM << DEVICE_WINDOW_SHIFT | off
    }

    #[test]
    fn z2_register_word_write_reaches_index_and_data_ports() {
        let mut board = GraffityZ2::default();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(z2_regs(0x3c4), 2, 0x0612, &mut host);
        assert_eq!(board.read(z2_regs(0x3c4), 2, &mut host), 0x0612);
    }

    #[test]
    fn z2_vram_window_is_big_endian_on_the_bus_and_linear_in_storage() {
        let mut board = GraffityZ2::default();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(z2_vram(2), 4, 0x1234_5678, &mut host);
        assert_eq!(board.read(z2_vram(2), 4, &mut host), 0x1234_5678);
        assert_eq!(board.peek_word(z2_vram(3)), Some(0x3456));
    }

    #[test]
    fn z2_monitor_switch_resets_to_native_and_ignores_stray_bits() {
        let mut board = GraffityZ2::default();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(z2_regs(0x8001), 1, 0, &mut host);
        assert!(!board.show_rtg);
        board.write(z2_regs(0x8000 | 0x60), 1, 0, &mut host);
        assert!(board.show_rtg);
        board.write(z2_regs(0x8000 | 0x40), 1, 0, &mut host);
        assert!(!board.show_rtg);
        board.write(z2_regs(0x8000 | 0x60), 1, 0, &mut host);
        board.reset();
        assert!(!board.show_rtg);
    }

    #[test]
    fn z2_unused_register_and_vram_offsets_are_open_bus() {
        let mut board = GraffityZ2::new(1024 * 1024);
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(z2_regs(0x0000), 1, &mut host), 0xff);
        assert_eq!(board.read(z2_vram(1024 * 1024), 4, &mut host), 0xffff_ffff);
    }

    #[test]
    fn z3_register_window_reaches_vga_ports_through_the_0x800000_alias() {
        let mut board = GraffityZ3::default();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(REGS_BASE + 0x3c4, 2, 0x0612, &mut host);
        assert_eq!(board.read(REGS_BASE + 0x3c4, 2, &mut host), 0x0612);
    }

    #[test]
    fn z3_vram_lives_at_the_0xc00000_offset() {
        let mut board = GraffityZ3::default();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(VRAM_BASE + 2, 4, 0x1234_5678, &mut host);
        assert_eq!(board.read(VRAM_BASE + 2, 4, &mut host), 0x1234_5678);
        assert_eq!(board.peek_word(VRAM_BASE + 3), Some(0x3456));
    }

    #[test]
    fn z3_switch_strobe_uses_the_0x400000_alias_and_never_reaches_registers() {
        let mut board = GraffityZ3::default();
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        board.write(SWITCH_BASE | 0x60, 1, 0, &mut host);
        assert!(board.show_rtg);
        board.write(SWITCH_BASE | 0x40, 1, 0, &mut host);
        assert!(!board.show_rtg);
        // The switch alias never reaches the VGA core, even at VGA-port-shaped offsets.
        board.write(SWITCH_BASE + 0x3c4, 1, 0x99, &mut host);
        assert_eq!(board.chip.io_read(0x3c4), 0);
    }

    #[test]
    fn z3_out_of_window_offsets_are_open_bus() {
        let mut board = GraffityZ3::new(1024 * 1024);
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        assert_eq!(board.read(0, 1, &mut host), 0xff);
        assert_eq!(
            board.read(VRAM_BASE + 1024 * 1024, 4, &mut host),
            0xffff_ffff
        );
    }

    #[test]
    fn savestate_round_trips_both_variants() {
        let mut z2 = GraffityZ2::new(1024 * 1024);
        let mut mem = memory();
        let mut host = DeviceHost::new(&mut mem);
        z2.write(z2_regs(0x8000 | 0x60), 1, 0, &mut host);
        z2.write(z2_regs(0x3c4), 2, 0x0612, &mut host);
        z2.write(z2_vram(4), 4, 0xcafe_babe, &mut host);
        let bytes = bincode::serialize(&z2).unwrap();
        let mut resumed: GraffityZ2 = bincode::deserialize(&bytes).unwrap();
        assert_eq!(resumed.kind(), "graffityz2");
        assert!(resumed.show_rtg);
        assert_eq!(resumed.read(z2_regs(0x3c4), 2, &mut host), 0x0612);
        assert_eq!(resumed.read(z2_vram(4), 4, &mut host), 0xcafe_babe);

        let mut z3 = GraffityZ3::new(2 * 1024 * 1024);
        z3.write(SWITCH_BASE | 0x60, 1, 0, &mut host);
        z3.write(REGS_BASE + 0x3c4, 2, 0x0612, &mut host);
        z3.write(VRAM_BASE + 4, 4, 0xcafe_babe, &mut host);
        let bytes = bincode::serialize(&z3).unwrap();
        let mut resumed: GraffityZ3 = bincode::deserialize(&bytes).unwrap();
        assert_eq!(resumed.kind(), "graffityz3");
        assert!(resumed.show_rtg);
        assert_eq!(resumed.read(REGS_BASE + 0x3c4, 2, &mut host), 0x0612);
        assert_eq!(resumed.read(VRAM_BASE + 4, 4, &mut host), 0xcafe_babe);
    }
}
