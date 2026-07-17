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
const REG_MODE: u32 = 0x100;
const REG_VBLANK_STATUS: u32 = 0x17C;
const REG_BLITTER_DMA_OP: u32 = 0x180;
const REG_ACC_OP: u32 = 0x184;
const REG_ORIG_RES: u32 = 0x18C;
const REG_FW_VERSION: u32 = 0x1A0;
const REG_INT_STATUS: u32 = 0x1A8;
const REG_CUSTOM_VIDMODE: u32 = 0x200;
const REG_CUSTOM_VIDMODE_DATA: u32 = 0x204;
const REG_MONITOR_SWITCH: u32 = 0x318;

/// The GFXData blit-parameter mailbox the driver fills in board RAM before
/// ringing REG_BLITTER_DMA_OP / REG_ACC_OP (window offset 0x3200000).
const GFXDATA_OFFSET: usize = 0x0320_0000;

/// Vertical timing of the fake video output: a 60 Hz frame in PAL colour
/// clocks (3546895 Hz / 60), with VBLANK_STATUS asserted for the first
/// ~2400 ccks (~0.7 ms) of each frame. The driver's WaitVerticalSync
/// busy-waits for a full 0 -> 1 transition of the flag.
const FRAME_CCK: u32 = 59115;
const VBLANK_CCK: u32 = 2400;

/// Firmware revision reported to the driver: major 1 (FindCard requires
/// exactly 1), minor 6 (>= 3, below which the driver raises an alert).
const FW_VERSION: u32 = 0x0106;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Z3660 {
    /// Latched register words, indexed by (offset - REG_FIRST) / 4.
    regs: Vec<u32>,
    /// Board RAM behind the register file: P96 VRAM and the GFXData mailbox.
    vram: Vec<u8>,
    /// Position inside the fake 60 Hz output frame, in colour clocks;
    /// VBLANK_STATUS reads 1 while it is inside the vblank window.
    frame_phase: u32,
    /// The last CUSTOM_VIDMODE parameter index written, so the matching
    /// CUSTOM_VIDMODE_DATA write can be logged with the parameter's name.
    vidmode_param: u32,
}

impl Z3660 {
    pub fn new() -> Self {
        Self {
            regs: vec![0u32; ((REG_END - REG_FIRST) / 4) as usize],
            vram: vec![0u8; BACKED_BYTES],
            frame_phase: 0,
            vidmode_param: 0,
        }
    }

    fn reg_index(off: u32) -> usize {
        ((off - REG_FIRST) / 4) as usize
    }

    /// The current value of the aligned register word at `off`.
    fn reg_value(&self, off: u32) -> u32 {
        match off {
            REG_FW_VERSION => FW_VERSION,
            REG_VBLANK_STATUS => u32::from(self.frame_phase < VBLANK_CCK),
            REG_INT_STATUS | REG_MONITOR_SWITCH => 0,
            _ => self.regs[Self::reg_index(off)],
        }
    }

    /// Big-endian words from board RAM (the GFXData mailbox fields).
    fn be32(&self, at: usize) -> u32 {
        u32::from_be_bytes(self.vram[at..at + 4].try_into().unwrap())
    }

    fn be16(&self, at: usize) -> u16 {
        u16::from_be_bytes(self.vram[at..at + 2].try_into().unwrap())
    }

    /// Decode and log the GFXData mailbox on a doorbell ring. The struct
    /// layout is common/z3660_regs.h's `struct GFXData` (68k-endian; the op
    /// selector is the doorbell value, not the struct's op byte).
    fn log_gfxdata(&self, kind: &str, op_name: &str, op: u32) {
        let g = GFXDATA_OFFSET;
        let x: Vec<u16> = (0..4).map(|i| self.be16(g + 0x10 + 2 * i)).collect();
        let y: Vec<u16> = (0..4).map(|i| self.be16(g + 0x18 + 2 * i)).collect();
        let pitch: Vec<u16> = (0..4).map(|i| self.be16(g + 0x28 + 2 * i)).collect();
        log::info!(
            "z3660: {kind} {op_name} ({op}) dst={:#x} src={:#x} rgb={:#010x}/{:#010x} \
             x={x:?} y={y:?} pitch={pitch:?} colormode={} drawmode={} mask={:#04x} minterm={:#04x}",
            self.be32(g),
            self.be32(g + 4),
            self.be32(g + 8),
            self.be32(g + 0xC),
            self.vram[g + 0x30],
            self.vram[g + 0x31],
            self.vram[g + 0x39],
            self.vram[g + 0x3A],
        );
    }

    /// Side effects of a completed register write (the low lane landed).
    fn reg_written(&mut self, off: u32, value: u32) {
        match off {
            REG_MODE => {
                log::info!(
                    "z3660: mode set: vmode {} colormode {} scalemode {}",
                    vmode_name(value & 0xFF),
                    colormode_name((value >> 8) & 0xF),
                    (value >> 12) & 0xF,
                );
            }
            REG_ORIG_RES => {
                log::info!(
                    "z3660: original resolution {}x{}",
                    value >> 16,
                    value & 0xFFFF
                );
            }
            REG_CUSTOM_VIDMODE => self.vidmode_param = value,
            REG_CUSTOM_VIDMODE_DATA => {
                log::info!(
                    "z3660: vidmode param {} = {value}",
                    vmode_param_name(self.vidmode_param)
                );
            }
            REG_BLITTER_DMA_OP => self.log_gfxdata("dma-op", dma_op_name(value), value),
            REG_ACC_OP => self.log_gfxdata("acc-op", acc_op_name(value), value),
            _ => {}
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
            // Register semantics fire once the low lane has landed (the bus
            // sometimes splits a 32-bit write into two halfwords, high first).
            if off + size as u32 == aligned + 4 {
                self.reg_written(aligned, self.regs[idx]);
            }
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

    fn tick(&mut self, cck: u32, _host: &mut DeviceHost) {
        self.frame_phase = (self.frame_phase + cck) % FRAME_CCK;
    }

    fn reset(&mut self) {
        self.regs.fill(0);
        self.frame_phase = 0;
        self.vidmode_param = 0;
    }

    fn kind(&self) -> &'static str {
        "z3660"
    }
}

/// `enum gfx_dma_op` (common/z3660_regs.h): REG_BLITTER_DMA_OP selectors.
fn dma_op_name(op: u32) -> &'static str {
    const NAMES: [&str; 17] = [
        "NONE",
        "DRAWLINE",
        "FILLRECT",
        "COPYRECT",
        "COPYRECT_NOMASK",
        "RECT_TEMPLATE",
        "RECT_PATTERN",
        "P2C",
        "P2D",
        "INVERTRECT",
        "PAN",
        "SPRITE_XY",
        "SPRITE_COLOR",
        "SPRITE_BITMAP",
        "SPRITE_CLUT_BITMAP",
        "ETH_USB_OFFSETS",
        "SET_SPLIT_POS",
    ];
    NAMES.get(op as usize).copied().unwrap_or("unknown")
}

/// `enum gfx_acc_op` (common/z3660_regs.h): REG_ACC_OP selectors.
fn acc_op_name(op: u32) -> &'static str {
    const NAMES: [&str; 9] = [
        "NONE",
        "BUFFER_FLIP",
        "BUFFER_CLEAR",
        "BLIT_RECT",
        "ALLOC_SURFACE",
        "FREE_SURFACE",
        "SET_BPP_CONVERSION_TABLE",
        "DRAW_LINE",
        "FILL_RECT",
    ];
    NAMES.get(op as usize).copied().unwrap_or("unknown")
}

/// `enum custom_vmode_params` (common/z3660_regs.h).
fn vmode_param_name(param: u32) -> &'static str {
    const NAMES: [&str; 16] = [
        "HRES", "VRES", "HSTART", "HEND", "HMAX", "VSTART", "VEND", "VMAX", "POLARITY", "MHZ",
        "PHZ", "VHZ", "HDMI", "MUL", "DIV", "DIV2",
    ];
    NAMES.get(param as usize).copied().unwrap_or("unknown")
}

/// `enum zz_video_modes` (rtg/rtg.h).
fn vmode_name(mode: u32) -> &'static str {
    const NAMES: [&str; 23] = [
        "1280x720",
        "800x600",
        "640x480",
        "1024x768",
        "1280x1024",
        "1920x1080_60",
        "720x576",
        "1920x1080_50",
        "720x480",
        "640x512",
        "1600x1200",
        "2560x1440_30",
        "720x576_NS_PAL",
        "720x480_NS_PAL",
        "720x576_NS_NTSC",
        "720x480_NS_NTSC",
        "640x400",
        "1280x800",
        "1920x1200",
        "1600x900",
        "1680x1050",
        "1366x768",
        "CUSTOM",
    ];
    NAMES.get(mode as usize).copied().unwrap_or("unknown")
}

/// MNTVA colour modes (common/z3660_regs.h).
fn colormode_name(mode: u32) -> &'static str {
    match mode {
        0 => "8BIT",
        1 => "16BIT565",
        2 => "32BIT",
        3 => "15BIT",
        _ => "unknown",
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
