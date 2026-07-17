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
//! Bring-up state: the board autoconfigs, the identity/status registers
//! answer, every register access is latched and logged, and the RAM region
//! is honest memory. The display pipeline presents the panned framebuffer
//! (all four pixel formats, palette captured from the upload stream) when
//! the driver switches the display to RTG; the blitter ops are not
//! executed yet, so only CPU-rendered pixels show.

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
const REG_OP_DATA: u32 = 0x300;
const REG_OP: u32 = 0x304;
const REG_OP_CAPTUREMODE: u32 = 0x30C;
const REG_MONITOR_SWITCH: u32 = 0x318;

// REG_OP commands (the ARM mailbox the palette upload rides on).
const ARM_OP_PALETTE: u32 = 3;
const ARM_OP_PALETTE_HI: u32 = 19;

// REG_BLITTER_DMA_OP selectors acted on (enum gfx_dma_op).
const DMA_OP_PAN: u32 = 10;

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

/// P96 VRAM starts at this window offset (Z3660_MEMBASE_ADDR in gfx.c);
/// the framebuffer the driver pans to is relative to it.
const VRAM_OFFSET: u32 = 0x0020_0000;

// MNTVA colour modes (the MODE register's bits 8-11).
const COLORMODE_8BIT: u32 = 0;
const COLORMODE_16BIT565: u32 = 1;
const COLORMODE_32BIT: u32 = 2;
const COLORMODE_15BIT: u32 = 3;

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
    /// The 512-entry palette (256 primary + 256 secondary), captured from
    /// the OP_DATA/OP palette upload stream as packed 0x00RRGGBB.
    palette: Vec<u32>,
    /// Framebuffer start relative to VRAM_OFFSET, from the last PAN op.
    pan_offset: u32,
    /// Framebuffer row width in pixels, from the last PAN op (x[1]). For
    /// pixel-doubled small modes this is the real framebuffer width while
    /// ORIG_RES holds the doubled output resolution.
    pan_width: u32,
    /// Whether SetGC has programmed a display mode since power-on; RTG
    /// scanout is meaningless before the first mode set.
    mode_set: bool,
}

impl Z3660 {
    pub fn new() -> Self {
        Self {
            regs: vec![0u32; ((REG_END - REG_FIRST) / 4) as usize],
            vram: vec![0u8; BACKED_BYTES],
            frame_phase: 0,
            vidmode_param: 0,
            palette: vec![0u32; 512],
            pan_offset: 0,
            pan_width: 0,
            mode_set: false,
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
                self.mode_set = true;
                log::info!(
                    "z3660: mode set: vmode {} colormode {} scalemode {}",
                    vmode_name(value & 0xFF),
                    colormode_name((value >> 8) & 0xF),
                    (value >> 12) & 0xF,
                );
            }
            REG_OP => {
                // The palette rides the ARM op mailbox: OP_DATA holds
                // index<<24 | R<<16 | G<<8 | B, OP selects the bank.
                let data = self.reg_value(REG_OP_DATA);
                match value {
                    ARM_OP_PALETTE => self.palette[(data >> 24) as usize] = data & 0x00FF_FFFF,
                    ARM_OP_PALETTE_HI => {
                        self.palette[256 + (data >> 24) as usize] = data & 0x00FF_FFFF;
                    }
                    _ => {}
                }
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
            REG_BLITTER_DMA_OP => {
                self.log_gfxdata("dma-op", dma_op_name(value), value);
                if value == DMA_OP_PAN {
                    // GFXData: offset[0] = framebuffer start relative to
                    // MemoryBase, x[1] = row width in pixels.
                    self.pan_offset = self.be32(GFXDATA_OFFSET);
                    self.pan_width = u32::from(self.be16(GFXDATA_OFFSET + 0x12));
                }
            }
            REG_ACC_OP => self.log_gfxdata("acc-op", acc_op_name(value), value),
            _ => {}
        }
    }

    /// Whether the board is driving the display: SetSwitch(1) turns video
    /// capture off (RTG shown); SetSwitch(0) turns it on (the real firmware
    /// then shows the captured native video, which for the emulator means
    /// presenting the chipset output as usual).
    pub fn rtg_active(&self) -> bool {
        self.mode_set && self.reg_value(REG_OP_CAPTUREMODE) == 0
    }

    /// Compose the currently displayed RTG frame into `out` as presentation
    /// pixels (RGBA byte order, alpha opaque), returning (width, height).
    /// `None` when RTG is not active. Scaled small modes (MODE scalemode
    /// != 0) compose at framebuffer resolution; the presenter scales.
    pub fn rtg_frame(&self, out: &mut Vec<u32>) -> Option<(u32, u32)> {
        if !self.rtg_active() {
            return None;
        }
        let mode = self.reg_value(REG_MODE);
        let colormode = (mode >> 8) & 0xF;
        let scaled = (mode >> 12) & 0xF != 0;
        let orig = self.reg_value(REG_ORIG_RES);
        let (out_w, out_h) = (orig >> 16, orig & 0xFFFF);
        let (mut w, mut h) = if scaled {
            (out_w / 2, out_h / 2)
        } else {
            (out_w, out_h)
        };
        if self.pan_width != 0 {
            w = self.pan_width;
        }
        w = w.min(4096);
        h = h.min(4096);
        if w == 0 || h == 0 {
            return None;
        }
        let bpp: u32 = match colormode {
            COLORMODE_8BIT => 1,
            COLORMODE_16BIT565 | COLORMODE_15BIT => 2,
            COLORMODE_32BIT => 4,
            _ => return None,
        };
        let pitch = (w * bpp) as usize;
        let base = (VRAM_OFFSET + self.pan_offset) as usize;
        out.clear();
        out.reserve((w * h) as usize);
        for y in 0..h as usize {
            let row = base + y * pitch;
            for x in 0..w as usize {
                let at = row + x * bpp as usize;
                if at + bpp as usize > self.vram.len() {
                    out.push(0xFF00_0000);
                    continue;
                }
                let (r, g, b) = match colormode {
                    COLORMODE_8BIT => {
                        let rgb = self.palette[self.vram[at] as usize];
                        ((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
                    }
                    COLORMODE_16BIT565 => {
                        let v = self.be16(at);
                        (
                            ((v >> 11) as u8 & 0x1F) << 3,
                            ((v >> 5) as u8 & 0x3F) << 2,
                            (v as u8 & 0x1F) << 3,
                        )
                    }
                    COLORMODE_15BIT => {
                        let v = self.be16(at);
                        (
                            ((v >> 10) as u8 & 0x1F) << 3,
                            ((v >> 5) as u8 & 0x1F) << 3,
                            (v as u8 & 0x1F) << 3,
                        )
                    }
                    // 32-bit is BGRA in memory (RTG_COLOR_FORMAT_BGRA).
                    _ => (self.vram[at + 2], self.vram[at + 1], self.vram[at]),
                };
                out.push(0xFF00_0000 | (u32::from(b) << 16) | (u32::from(g) << 8) | u32::from(r));
            }
        }
        Some((w, h))
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
        self.palette.fill(0);
        self.pan_offset = 0;
        self.pan_width = 0;
        self.mode_set = false;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Memory;

    fn mem() -> Memory {
        Memory {
            chip_ram: vec![0u8; 0x100],
            slow_ram: Vec::new(),
            rom: Vec::new(),
            overlay: false,
            zorro: crate::zorro::ZorroChain::default(),
            extended_rom: Vec::new(),
            extended_rom_base: 0,
            wcs: Vec::new(),
            wcs_write_protected: false,
        }
    }

    fn w32(z: &mut Z3660, mem: &mut Memory, off: u32, value: u32) {
        z.write(off, 4, value, &mut DeviceHost::new(mem));
    }

    /// The driver's palette upload (SetColorArray): OP_DATA carries
    /// index<<24|RGB, OP selects the primary or secondary bank.
    #[test]
    fn palette_capture_from_op_stream() {
        let (mut z, mut m) = (Z3660::new(), mem());
        w32(&mut z, &mut m, REG_OP_DATA, (7 << 24) | 0x0012_3456);
        w32(&mut z, &mut m, REG_OP, ARM_OP_PALETTE);
        w32(&mut z, &mut m, REG_OP_DATA, (7 << 24) | 0x00AB_CDEF);
        w32(&mut z, &mut m, REG_OP, ARM_OP_PALETTE_HI);
        assert_eq!(z.palette[7], 0x0012_3456);
        assert_eq!(z.palette[256 + 7], 0x00AB_CDEF);
    }

    /// SetGC + SetSwitch gate the scanout: no frame before a mode is set,
    /// none when capture mode (native video) is on.
    #[test]
    fn rtg_activates_on_mode_set_and_capture_off() {
        let (mut z, mut m) = (Z3660::new(), mem());
        let mut out = Vec::new();
        assert!(z.rtg_frame(&mut out).is_none());
        w32(&mut z, &mut m, REG_ORIG_RES, (4 << 16) | 2);
        w32(&mut z, &mut m, REG_MODE, 0x16); // CUSTOM, 8BIT
        assert!(z.rtg_active());
        w32(&mut z, &mut m, REG_OP_CAPTUREMODE, 1);
        assert!(!z.rtg_active());
        w32(&mut z, &mut m, REG_OP_CAPTUREMODE, 0);
        assert!(z.rtg_active());
    }

    /// An 8-bit frame reads pixels through the captured palette from the
    /// panned framebuffer start.
    #[test]
    fn clut_frame_composes_through_palette() {
        let (mut z, mut m) = (Z3660::new(), mem());
        w32(&mut z, &mut m, REG_ORIG_RES, (4 << 16) | 2);
        w32(&mut z, &mut m, REG_MODE, 0x16);
        w32(&mut z, &mut m, REG_OP_DATA, (1 << 24) | 0x00FF_0000); // index 1 = red
        w32(&mut z, &mut m, REG_OP, ARM_OP_PALETTE);
        // Framebuffer at VRAM start: row 0 = 0,1,1,0; row 1 = 1,0,0,1.
        for (i, px) in [0u8, 1, 1, 0, 1, 0, 0, 1].iter().enumerate() {
            z.write(
                VRAM_OFFSET + i as u32,
                1,
                u32::from(*px),
                &mut DeviceHost::new(&mut m),
            );
        }
        let mut out = Vec::new();
        assert_eq!(z.rtg_frame(&mut out), Some((4, 2)));
        let red = 0xFF00_0000 | 0x0000_00FF; // RGBA byte order: R in the low byte
        let black = 0xFF00_0000;
        assert_eq!(out, vec![black, red, red, black, red, black, black, red]);
    }

    /// Direct-colour pixel decoding: R5G6B5 and B8G8R8A8, big-endian in VRAM.
    #[test]
    fn direct_color_frames_decode() {
        let (mut z, mut m) = (Z3660::new(), mem());
        w32(&mut z, &mut m, REG_ORIG_RES, (1 << 16) | 1);
        // 16-bit 565: pure green = 0x07E0.
        w32(&mut z, &mut m, REG_MODE, 0x16 | (COLORMODE_16BIT565 << 8));
        z.write(VRAM_OFFSET, 2, 0x07E0, &mut DeviceHost::new(&mut m));
        let mut out = Vec::new();
        assert_eq!(z.rtg_frame(&mut out), Some((1, 1)));
        assert_eq!(out[0], 0xFF00_FC00); // green FC in byte 1
                                         // 32-bit BGRA: bytes B,G,R,A.
        w32(&mut z, &mut m, REG_MODE, 0x16 | (COLORMODE_32BIT << 8));
        z.write(VRAM_OFFSET, 4, 0x2040_80FF, &mut DeviceHost::new(&mut m));
        assert_eq!(z.rtg_frame(&mut out), Some((1, 1)));
        assert_eq!(out[0], 0xFF20_4080); // B=0x20 high, G=0x40, R=0x80 low
    }

    /// The PAN op moves the framebuffer start inside VRAM.
    #[test]
    fn pan_offsets_the_framebuffer() {
        let (mut z, mut m) = (Z3660::new(), mem());
        w32(&mut z, &mut m, REG_ORIG_RES, (1 << 16) | 1);
        w32(&mut z, &mut m, REG_MODE, 0x16);
        w32(&mut z, &mut m, REG_OP_DATA, (5 << 24) | 0x00FF_FFFF);
        w32(&mut z, &mut m, REG_OP, ARM_OP_PALETTE);
        // GFXData offset[0] = 0x1000, x[1] = 1: pan one page in.
        z.write(
            GFXDATA_OFFSET as u32,
            4,
            0x1000,
            &mut DeviceHost::new(&mut m),
        );
        z.write(
            GFXDATA_OFFSET as u32 + 0x12,
            2,
            1,
            &mut DeviceHost::new(&mut m),
        );
        w32(&mut z, &mut m, REG_BLITTER_DMA_OP, DMA_OP_PAN);
        z.write(VRAM_OFFSET + 0x1000, 1, 5, &mut DeviceHost::new(&mut m));
        let mut out = Vec::new();
        assert_eq!(z.rtg_frame(&mut out), Some((1, 1)));
        assert_eq!(out[0], 0xFFFF_FFFF);
    }
}
