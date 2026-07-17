// SPDX-License-Identifier: GPL-3.0-or-later

//! Z3660 RTG board stub.
//!
//! The Z3660 accelerator's FPGA presents its RTG core as an ordinary
//! Zorro III autoconfig board (manufacturer 0x144B, product 1, one 128 MB
//! window) even though the physical board sits in the A3000/A4000 CPU slot.
//! The open-source Z3660.card Picasso96 driver finds it via FindConfigDev
//! and then talks to a 32-bit register file in the first 2 KB of the window;
//! the rest of the window is board RAM (P96 VRAM from +0x200000, the
//! GFXData blit-parameter mailbox at +0x3200000).
//!
//! This is a bring-up stub, not a working display: the board autoconfigs,
//! the identity/status registers the driver probes at init answer, every
//! other register access is latched and logged so the driver's expectations
//! can be mapped, and the RAM region is honest memory. No pixels are scanned
//! out yet.

use crate::zorro_device::{DeviceHost, ZorroDevice};

/// The Z3660's autoconfig manufacturer ID.
pub const Z3660_MANUFACTURER_ID: u16 = 0x144B;
/// The RTG board's product number.
pub const Z3660_PRODUCT: u8 = 1;
/// The autoconfig window: 128 MB of Zorro III space.
pub const Z3660_WINDOW_BYTES: usize = 0x0800_0000;

/// The register file: 32-bit registers at window offsets 0x100..0x800
/// (common/z3660_regs.h in the driver source).
const REG_FIRST: u32 = 0x100;
const REG_END: u32 = 0x800;

/// Backed RAM: the driver only touches the first 52 MB of the window
/// (VRAM cap 0x3000000, GFXData at 0x3200000, template scratch at
/// 0x3210000), so backing 64 MB keeps the allocation honest without
/// carrying the full 128 MB window.
const BACKED_BYTES: usize = 0x0400_0000;

// The registers the FindCard/init path probes.
const REG_VBLANK_STATUS: u32 = 0x17C;
const REG_FW_VERSION: u32 = 0x1A0;
const REG_INT_STATUS: u32 = 0x1A8;
const REG_MONITOR_SWITCH: u32 = 0x318;

/// Firmware revision reported to the driver: major 1 (FindCard requires
/// exactly 1), minor 6 (>= 3, below which the driver raises an alert).
const FW_VERSION: u32 = 0x0106;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Z3660 {
    /// Latched register words, indexed by (offset - REG_FIRST) / 4.
    regs: Vec<u32>,
    /// Board RAM behind the register file: P96 VRAM and the GFXData mailbox.
    vram: Vec<u8>,
    /// Fake vblank flip-flop: VBLANK_STATUS alternates on every read so the
    /// driver's WaitVerticalSync busy-wait terminates.
    // TODO(codewiz): drive this from emulated frame timing instead.
    vblank: bool,
}

impl Z3660 {
    pub fn new() -> Self {
        Self {
            regs: vec![0u32; ((REG_END - REG_FIRST) / 4) as usize],
            vram: vec![0u8; BACKED_BYTES],
            vblank: false,
        }
    }

    fn reg_index(off: u32) -> usize {
        ((off - REG_FIRST) / 4) as usize
    }

    /// The current value of the aligned register word at `off`.
    fn reg_value(&mut self, off: u32) -> u32 {
        match off {
            REG_FW_VERSION => FW_VERSION,
            REG_VBLANK_STATUS => {
                self.vblank = !self.vblank;
                u32::from(self.vblank)
            }
            REG_INT_STATUS | REG_MONITOR_SWITCH => 0,
            _ => self.regs[Self::reg_index(off)],
        }
    }
}

impl Default for Z3660 {
    fn default() -> Self {
        Self::new()
    }
}

impl ZorroDevice for Z3660 {
    fn read(&mut self, off: u32, size: usize, _host: &mut DeviceHost) -> u32 {
        if (REG_FIRST..REG_END).contains(&off) {
            let aligned = off & !3;
            if (off & 3) as usize + size > 4 {
                log::debug!("z3660: register read straddles a word at {off:#x}");
                return 0;
            }
            let word = self.reg_value(aligned);
            let shift = 8 * (4 - (off & 3) as usize - size);
            let value = (word >> shift) & (u32::MAX >> (8 * (4 - size)));
            if aligned == REG_VBLANK_STATUS {
                log::trace!("z3660: read  {} -> {value:#x}", reg_name(aligned));
            } else {
                log::info!(
                    "z3660: read  {} ({off:#05x},{size}) -> {value:#010x}",
                    reg_name(aligned)
                );
            }
            return value;
        }
        let at = off as usize;
        if at + size > self.vram.len() {
            log::debug!("z3660: read beyond backed RAM at {off:#x}");
            return 0;
        }
        self.vram[at..at + size]
            .iter()
            .fold(0, |acc, &b| (acc << 8) | u32::from(b))
    }

    fn write(&mut self, off: u32, size: usize, value: u32, _host: &mut DeviceHost) {
        if (REG_FIRST..REG_END).contains(&off) {
            let aligned = off & !3;
            if (off & 3) as usize + size > 4 {
                log::debug!("z3660: register write straddles a word at {off:#x}");
                return;
            }
            // Merge a sub-word write into the latched word's byte lanes.
            let idx = Self::reg_index(aligned);
            let shift = 8 * (4 - (off & 3) as usize - size);
            let mask = (u32::MAX >> (8 * (4 - size))) << shift;
            self.regs[idx] = (self.regs[idx] & !mask) | ((value << shift) & mask);
            log::info!(
                "z3660: write {} ({off:#05x},{size}) = {value:#010x}",
                reg_name(aligned)
            );
            return;
        }
        let at = off as usize;
        if at + size > self.vram.len() {
            log::debug!("z3660: write beyond backed RAM at {off:#x}");
            return;
        }
        for i in 0..size {
            self.vram[at + i] = (value >> (8 * (size - 1 - i))) as u8;
        }
    }

    fn peek_word(&self, off: u32) -> Option<u16> {
        // Registers are live (VBLANK_STATUS flips on read); RAM is safe.
        let at = off as usize;
        if (REG_FIRST..REG_END).contains(&off) || at + 2 > self.vram.len() {
            return None;
        }
        Some((u16::from(self.vram[at]) << 8) | u16::from(self.vram[at + 1]))
    }

    fn tick(&mut self, _cck: u32, _host: &mut DeviceHost) {}

    fn reset(&mut self) {
        self.regs.fill(0);
        self.vblank = false;
    }

    fn kind(&self) -> &'static str {
        "z3660"
    }
}

/// Register names from the driver's common/z3660_regs.h, for the access log.
fn reg_name(off: u32) -> &'static str {
    match off {
        0x100 => "MODE",
        0x104 => "CONFIG",
        0x108 => "SPRITE_X",
        0x10C => "SPRITE_Y",
        0x110 => "X1",
        0x114 => "Y1",
        0x118 => "X2",
        0x11C => "Y2",
        0x120 => "PAN",
        0x124 => "ROW_PITCH",
        0x128 => "X3",
        0x12C => "Y3",
        0x130 => "RGB",
        0x134 => "FILLRECT",
        0x138 => "COPYRECT",
        0x13C => "FILLTEMPLATE",
        0x140 => "BLIT_SRC",
        0x144 => "BLIT_DST",
        0x148 => "COLORMODE",
        0x14C => "SRC_PITCH",
        0x150 => "RGB2",
        0x154 => "P2C",
        0x158 => "DRAWLINE",
        0x15C => "P2D",
        0x160 => "USER1",
        0x164 => "USER2",
        0x168 => "USER3",
        0x16C => "USER4",
        0x170 => "INVERTRECT",
        0x174 => "SPRITE_BITMAP",
        0x178 => "SPRITE_COLORS",
        0x17C => "VBLANK_STATUS",
        0x180 => "BLITTER_DMA_OP",
        0x184 => "ACC_OP",
        0x188 => "SET_SPLIT_POS",
        0x18C => "ORIG_RES",
        0x1A0 => "FW_VERSION",
        0x1A8 => "INT_STATUS",
        0x1E0 => "SET_FEATURE",
        0x1FC => "DEBUG",
        0x200 => "CUSTOM_VIDMODE",
        0x204 => "CUSTOM_VIDMODE_DATA",
        0x300 => "OP_DATA",
        0x304 => "OP",
        0x308 => "OP_NOP",
        0x30C => "OP_CAPTUREMODE",
        0x318 => "MONITOR_SWITCH",
        _ => "reg",
    }
}
