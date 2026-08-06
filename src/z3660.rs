// SPDX-License-Identifier: GPL-3.0-or-later

//! Z3660 RTG board.
//!
//! The Z3660 accelerator's FPGA presents its RTG core as an ordinary
//! Zorro III autoconfig board (manufacturer 0x144B, product 1, one 128 MB
//! window) even though the physical board sits in the A3000/A4000 CPU slot.
//! The open-source Z3660.card Picasso96 driver finds it via FindConfigDev
//! and then talks to a 32-bit register file in the first 2 KB of the window;
//! the rest of the window is board RAM (P96 VRAM from +0x200000, the
//! GFXData blit-parameter mailbox at +0x3200000).
//!
//! Implemented: the board autoconfigs, the identity/status registers
//! answer, every register access is latched, and the RAM region is honest
//! memory. The display pipeline presents the panned framebuffer
//! (all four pixel formats, palette captured from the upload stream) when
//! the driver switches the display to RTG, and the core blitter ops
//! (fill/copy/invert/template/pattern/line/planar) execute into VRAM on
//! the doorbell, with the hardware sprite (mouse pointer) composited over
//! the output. Still stubbed: the ACC_OP surface ops and exotic minterms.

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
const REG_SPRITE_BITMAP: u32 = 0x174;
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

/// Bytes per pixel of a colour mode, or `None` if the mode is not one we
/// can scan out.
fn colormode_bpp(colormode: u32) -> Option<u32> {
    match colormode {
        COLORMODE_8BIT => Some(1),
        COLORMODE_16BIT565 | COLORMODE_15BIT => Some(2),
        COLORMODE_32BIT => Some(4),
        _ => None,
    }
}

/// Apply a planar-blit function bitwise. `func` is the low nibble of the minterm
/// the P96 BlitPlanar hooks pass down: a truth table indexed by
/// (src << 1) | dst, so 12 is SRC, 3 is NOTSRC, 8 is AND, 14 is OR, 6 is EOR
/// and 10 is DST.
fn minterm_apply(func: u8, s: u32, d: u32) -> u32 {
    let sel = |bit: u8| if func & bit != 0 { u32::MAX } else { 0 };

    (sel(8) & s & d) | (sel(4) & s & !d) | (sel(2) & !s & d) | (sel(1) & !s & !d)
}

/// Upper bound on a blit rect / framebuffer dimension. The mailbox fields
/// are guest-controlled, so clamping them keeps a bogus op from sizing a
/// huge allocation or a multi-billion-iteration loop; no real screen or
/// blit exceeds this.
const MAX_DIM: usize = 4096;

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
    /// Framebuffer start relative to VRAM_OFFSET, from the last PAN op, with
    /// its x/y viewport offsets already folded in. The sprite needs no such
    /// adjustment: SetSpritePosition sends viewport coordinates already.
    pan_offset: u32,
    /// The bitmap's row width in pixels, from the last PAN op (x[1]). This
    /// is a stride, not a visible width: a bitmap wider than the display
    /// mode scans out only the mode's worth of each row. For pixel-doubled
    /// small modes it is the real framebuffer width while ORIG_RES holds the
    /// doubled output resolution.
    pan_width: u32,
    /// Whether SetGC has programmed a display mode since power-on; RTG
    /// scanout is meaningless before the first mode set.
    mode_set: bool,
    /// Hardware sprite (the mouse pointer): composited over the frame at
    /// scanout, never written to VRAM. Pixel values 0-3 index
    /// `sprite_colors` (0 = transparent), row-major `sprite_w` wide.
    sprite_visible: bool,
    sprite_x: i32,
    sprite_y: i32,
    sprite_w: usize,
    sprite_h: usize,
    sprite_pix: Vec<u8>,
    /// Sprite pens as 0x00RRGGBB (pen 0 unused).
    sprite_colors: [u32; 4],
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
            sprite_visible: false,
            sprite_x: 0,
            sprite_y: 0,
            sprite_w: 0,
            sprite_h: 0,
            sprite_pix: Vec::new(),
            sprite_colors: [0; 4],
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
        log::debug!(
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
            REG_SPRITE_BITMAP => {
                // SetSprite: 1 = show the hardware sprite (2 = hide; the
                // driver ships only the show write, hiding via position).
                if value == 1 {
                    self.sprite_visible = true;
                } else if value == 2 {
                    self.sprite_visible = false;
                }
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
                    // MemoryBase, x[0]/y[0] = signed viewport offsets within
                    // the bitmap, x[1] = the bitmap's row width in pixels.
                    //
                    // The viewport offsets fold into the scanout base, so
                    // panning around a bitmap larger than the mode just
                    // moves the DMA start address:
                    //   pan = offset[0] + x[0] * bpp + y[0] * x[1] * bpp
                    //
                    // The firmware writes this as a shift by the GFXData
                    // colormode, which equals the pixel size for 8/16/32-bit
                    // but strides 8 bytes per pixel for colormode 3
                    // (15-bit). Panning a 15-bit screen on real hardware is
                    // therefore broken; the pixel size is what P96 means, so
                    // that is what we scan with.
                    let cm = u32::from(self.vram[GFXDATA_OFFSET + 0x30]) & 0xF;
                    let bpp = i64::from(colormode_bpp(cm).unwrap_or(1));
                    let x0 = i32::from(self.be16(GFXDATA_OFFSET + 0x10) as i16);
                    let y0 = i32::from(self.be16(GFXDATA_OFFSET + 0x18) as i16);
                    let width = u32::from(self.be16(GFXDATA_OFFSET + 0x12));
                    let base = self.be32(GFXDATA_OFFSET) as i64;
                    let pan = base + (i64::from(x0) + i64::from(y0) * i64::from(width)) * bpp;
                    // A guest offset that leaves the board's RAM scans
                    // nothing; clamp rather than wrapping into live VRAM.
                    self.pan_offset = u32::try_from(pan.max(0)).unwrap_or(u32::MAX);
                    self.pan_width = width;
                }
                self.exec_dma_op(value);
            }
            REG_ACC_OP => self.log_gfxdata("acc-op", acc_op_name(value), value),
            _ => {}
        }
    }

    // --- GFXData blitter-op executor -------------------------------------
    //
    // Semantics follow the ARM firmware (z3660-firmware src/rtg/gfx.c and
    // dma_rtg.c, GPL-3.0-or-later like Copperline). Ops run synchronously on
    // the doorbell write, which is faithful: the real board holds the CPU in
    // bus wait states while the ARM services the register.
    //
    // Byte-order note: GFXData fields are written by the 68k and read
    // byteswapped by the little-endian ARM, whose "fg >> 24" is therefore the
    // guest value's LOW byte, and whose pixel stores land in guest byte
    // order. This executor works directly in guest byte order: the 8-bit pen
    // is `rgb & 0xFF`, wider pixels are stored big-endian.
    //
    // Pitch quirk: fill/copy/invert/line pass pitch[0] in longwords
    // (BytesPerRow >> 2); template/pattern pass raw BytesPerRow (the
    // firmware divides by 4 in u32-pointer arithmetic, so bytes here).

    fn gfx_u16(&self, field: usize, i: usize) -> usize {
        self.be16(GFXDATA_OFFSET + field + 2 * i) as usize
    }

    /// One pixel store in guest byte order, bounds-checked (out-of-window
    /// pixels are dropped, as the firmware's fb writes wrap into scratch).
    fn px_put(&mut self, at: usize, bpp: usize, v: u32) {
        if at + bpp <= self.vram.len() {
            let bytes = v.to_be_bytes();
            self.vram[at..at + bpp].copy_from_slice(&bytes[4 - bpp..]);
        }
    }

    fn px_get(&self, at: usize, bpp: usize) -> u32 {
        if at + bpp > self.vram.len() {
            return 0;
        }
        self.vram[at..at + bpp]
            .iter()
            .fold(0, |acc, &b| (acc << 8) | u32::from(b))
    }

    /// OP_SET_PALETTE: bulk CLUT upload. A Zorro III driver (v1.03b21+)
    /// no longer streams colours through REG_OP one at a time; it fills the
    /// mailbox with user[0]=start, user[1]=count, u8_user[0]=secondary-bank
    /// flag, and clut1[] as `count` packed R,G,B byte triplets, then rings
    /// this op. clut1 is written by the 68k as bytes, so no byteswap applies.
    fn apply_set_palette(&mut self) {
        let g = GFXDATA_OFFSET;
        let start = self.gfx_u16(0x20, 0); // user[0]
        let count = self.gfx_u16(0x20, 1).min(256); // user[1]
        let base = if self.vram[g + 0x30] != 0 { 256 } else { 0 }; // u8_user[0]
        let clut = g + 0x5C; // clut1[]
        for i in 0..count {
            let idx = base + ((start + i) & 0xFF);
            let r = u32::from(self.vram[clut + i * 3]);
            let grn = u32::from(self.vram[clut + i * 3 + 1]);
            let b = u32::from(self.vram[clut + i * 3 + 2]);
            self.palette[idx] = (r << 16) | (grn << 8) | b;
        }
    }

    /// Execute one REG_BLITTER_DMA_OP request against VRAM.
    fn exec_dma_op(&mut self, op: u32) {
        const OP_SET_PALETTE: u32 = 17;
        // A CLUT upload carries no blit geometry: the driver leaves the
        // rect/pitch fields stale and puts the palette in the mailbox's
        // clut1[] block, so service it before the geometry reads below.
        if op == OP_SET_PALETTE {
            self.apply_set_palette();
            return;
        }
        const OP_FILLRECT: u32 = 2;
        const OP_COPYRECT: u32 = 3;
        const OP_COPYRECT_NOMASK: u32 = 4;
        const OP_RECT_TEMPLATE: u32 = 5;
        const OP_RECT_PATTERN: u32 = 6;
        const OP_DRAWLINE: u32 = 1;
        const OP_P2C: u32 = 7;
        const OP_P2D: u32 = 8;
        const OP_INVERTRECT: u32 = 9;
        const OP_SPRITE_XY: u32 = 11;
        const MINTERM_SRC_IDX: u8 = 12;
        const MINTERM_NOTSRC_IDX: u8 = 3;
        const COMPLEMENT: u8 = 2;
        const OP_SPRITE_COLOR: u32 = 12;
        const OP_SPRITE_BITMAP: u32 = 13;
        const JAM1: u8 = 0;
        // Drawmode bits (firmware rtg/gfx.h): JAM1/JAM2 in bit 0, COMPLEMENT
        // in bit 1, INVERSVID in bit 2.
        const INVERSVID: u8 = 4;

        let g = GFXDATA_OFFSET;
        // The surface offsets are guest-controlled, and usize is 32-bit on
        // wasm32, where adding VRAM_OFFSET could wrap a large offset back
        // into live VRAM. Widen, then saturate: an out-of-range base stays
        // out of range, so every access below fails its bounds check and the
        // op drops, which is what the firmware's writes into scratch do.
        let base_of =
            |v: u32| usize::try_from(u64::from(VRAM_OFFSET) + u64::from(v)).unwrap_or(usize::MAX);
        let dst = base_of(self.be32(g));
        let src = base_of(self.be32(g + 4));
        let rgb0 = self.be32(g + 8);
        let rgb1 = self.be32(g + 0xC);
        // x1/y1 are the blit width/height (x2/y2 for the planar ops); clamp
        // the extents so a guest-supplied field cannot size an oversized
        // allocation or loop. x0/y0 are positions, bounds-checked per access.
        let (x0, x1, x2) = (
            self.gfx_u16(0x10, 0),
            self.gfx_u16(0x10, 1).min(MAX_DIM),
            self.gfx_u16(0x10, 2).min(MAX_DIM),
        );
        let (y0, y1, y2) = (
            self.gfx_u16(0x18, 0),
            self.gfx_u16(0x18, 1).min(MAX_DIM),
            self.gfx_u16(0x18, 2).min(MAX_DIM),
        );
        let user0 = self.gfx_u16(0x20, 0);
        let (pitch0, pitch1) = (self.gfx_u16(0x28, 0), self.gfx_u16(0x28, 1));
        let colormode = u32::from(self.vram[g + 0x30]);
        let drawmode = self.vram[g + 0x31];
        let mask = self.vram[g + 0x39];
        let minterm = self.vram[g + 0x3A];
        let bpp = match colormode_bpp(colormode) {
            Some(b) => b as usize,
            None => return,
        };

        match op {
            OP_FILLRECT => {
                // (x0,y0) w=x1 h=y1, colour rgb0; the 8-bit mask keeps
                // unmasked destination bits (planar write masks).
                let pitch = pitch0 * 4;
                for y in 0..y1 {
                    let row = dst + (y0 + y) * pitch + x0 * bpp;
                    for x in 0..x1 {
                        let at = row + x * bpp;
                        if bpp == 1 && mask != 0xFF {
                            let d = self.px_get(at, 1) as u8;
                            let v = (d & !mask) | (rgb0 as u8 & mask);
                            self.px_put(at, 1, u32::from(v));
                        } else {
                            self.px_put(at, bpp, rgb0);
                        }
                    }
                }
            }
            OP_COPYRECT | OP_COPYRECT_NOMASK => {
                // dst rect (x0,y0) w=x1 h=y1 from src rect (x2,y2). Plain
                // COPYRECT blits within the surface at offset[0]/pitch[0];
                // the NOMASK variant reads surface offset[1]/pitch[1] and
                // carries a minterm, in the same low-nibble form the planar
                // hooks use. A source copy is the common case and stays a
                // straight byte move; everything else goes a pixel at a time
                // through the truth table.
                let dpitch = pitch0 * 4;
                let (sbase, spitch) = if op == OP_COPYRECT {
                    (dst, dpitch)
                } else {
                    (src, pitch1 * 4)
                };
                let func = minterm & 0x0F;
                let use_minterm = op == OP_COPYRECT_NOMASK && func != MINTERM_SRC_IDX;
                let apply_mask = op == OP_COPYRECT && bpp == 1 && mask != 0xFF;
                // Rects may overlap, so a row cannot be written while it is
                // still needed as source. Buffer one row rather than the
                // whole rect: x1/y1 are clamped to MAX_DIM apiece, so a
                // whole-rect snapshot would reach 64 MiB per blit, which is
                // a large allocation and a long stall for a guest-supplied
                // pair of numbers. Vertical overlap is handled by walking
                // rows away from the destination instead.
                let row_bytes = x1 * bpp;
                let mut row = vec![0u8; row_bytes];
                let down = dst + y0 * dpitch >= sbase + y2 * spitch;
                for i in 0..y1 {
                    let y = if down { y1 - 1 - i } else { i };
                    let srow = sbase + (y2 + y) * spitch + x2 * bpp;
                    for (b, r) in row.iter_mut().enumerate() {
                        *r = if srow + b < self.vram.len() {
                            self.vram[srow + b]
                        } else {
                            0
                        };
                    }
                    let drow = dst + (y0 + y) * dpitch + x0 * bpp;
                    if use_minterm {
                        for x in 0..x1 {
                            let at = drow + x * bpp;
                            let s = row[x * bpp..]
                                .iter()
                                .take(bpp)
                                .fold(0u32, |acc, &b| (acc << 8) | u32::from(b));
                            let v = minterm_apply(func, s, self.px_get(at, bpp));

                            self.px_put(at, bpp, v & (u32::MAX >> (8 * (4 - bpp))));
                        }
                    } else {
                        for (x, &s) in row.iter().enumerate() {
                            let at = drow + x;
                            if at >= self.vram.len() {
                                continue;
                            }
                            self.vram[at] = if apply_mask {
                                (self.vram[at] & !mask) | (s & mask)
                            } else {
                                s
                            };
                        }
                    }
                }
            }
            OP_INVERTRECT => {
                let pitch = pitch0 * 4;
                for y in 0..y1 {
                    let row = dst + (y0 + y) * pitch + x0 * bpp;
                    for x in 0..x1 {
                        let at = row + x * bpp;
                        let d = self.px_get(at, bpp);
                        let v = if bpp == 1 {
                            d ^ u32::from(mask)
                        } else {
                            !d & (u32::MAX >> (8 * (4 - bpp)))
                        };
                        self.px_put(at, bpp, v);
                    }
                }
            }
            OP_RECT_TEMPLATE | OP_RECT_PATTERN => {
                // 1-bit source data at offset[1]: a template row is
                // pitch[1] bytes; a pattern is 16 bits wide repeating every
                // user[0] rows (power of two). (x2,y2) is the bit phase.
                // JAM1 draws fg where set; JAM2 draws fg/bg; COMPLEMENT
                // inverts where set; INVERSVID inverts the source bits.
                let pitch = pitch0 & !3; // raw bytes, truncated like fb+n/4
                let inversion = drawmode & INVERSVID != 0;
                let dm = drawmode & 0x03;
                let (pat_rows, tpitch) = if op == OP_RECT_PATTERN {
                    (user0.max(1), 2)
                } else {
                    (usize::MAX, pitch1)
                };
                for y in 0..y1 {
                    let trow = if op == OP_RECT_PATTERN {
                        src + ((y2 + y) & (pat_rows - 1)) * tpitch
                    } else {
                        src + y * tpitch
                    };
                    let row = dst + (y0 + y) * pitch;
                    for x in 0..x1 {
                        let bit_index = x2 + x;
                        let tat = if op == OP_RECT_PATTERN {
                            trow + (bit_index & 15) / 8
                        } else {
                            trow + bit_index / 8
                        };
                        let byte = if tat < self.vram.len() {
                            self.vram[tat]
                        } else {
                            0
                        };
                        let byte = if inversion { !byte } else { byte };
                        let set = byte & (0x80 >> (bit_index & 7)) != 0;
                        let at = row + (x0 + x) * bpp;
                        match (dm, set) {
                            (2, true) => {
                                // COMPLEMENT
                                let d = self.px_get(at, bpp);
                                let v = if bpp == 1 {
                                    d ^ u32::from(mask)
                                } else {
                                    !d & (u32::MAX >> (8 * (4 - bpp)))
                                };
                                self.px_put(at, bpp, v);
                            }
                            (2, false) => {}
                            (_, true) => self.px_put_masked(at, bpp, rgb0, mask),
                            (_, false) if dm != JAM1 => {
                                self.px_put_masked(at, bpp, rgb1, mask);
                            }
                            _ => {}
                        }
                    }
                }
            }
            OP_DRAWLINE => {
                // Bresenham from (x[0],y[0]) along the signed deltas
                // (x[1],y[1]), per the P96 DrawLine spec. user[0] is
                // Line.Length -- authoritative over the delta so a clipped
                // line segment draws the right pixel count (0 = major-axis
                // span). The 16-bit pattern (user[1]) rotates one bit per
                // pixel; JAM2 draws bg on pattern-clear pixels, COMPLEMENT
                // inverts the destination.
                let pitch = pitch0 * 4;
                let (mut x, mut y) = (
                    i32::from(self.be16(g + 0x10) as i16),
                    i32::from(self.be16(g + 0x18) as i16),
                );
                let (dx, dy) = (
                    i32::from(self.be16(g + 0x12) as i16),
                    i32::from(self.be16(g + 0x1A) as i16),
                );
                let req_len = self.gfx_u16(0x20, 0);
                let mut pattern = self.gfx_u16(0x20, 1) as u16;
                if drawmode & INVERSVID != 0 {
                    pattern ^= 0xFFFF;
                }
                let complement = drawmode & COMPLEMENT != 0;
                let jam2 = drawmode & 1 != 0;
                let (dx_abs, dy_abs) = (dx.unsigned_abs() as i32, dy.unsigned_abs() as i32);
                let x_step = if dx < 0 { -1 } else { 1 };
                let y_step = if dy < 0 { -1 } else { 1 };
                let mut cur_bit = 0x8000u16;
                // A pixel past the end of a row would otherwise address the
                // start of the next one, drawing a wrapped line rather than a
                // clipped one. The row is the only bound the request carries;
                // beyond the bitmap's last row the VRAM bound in px_put applies.
                let row_px = (pitch / bpp) as i32;
                let put = |z: &mut Self, x: i32, y: i32, bit: u16| {
                    if x < 0 || y < 0 || x >= row_px {
                        return;
                    }
                    if pattern & bit == 0 && !jam2 {
                        return;
                    }
                    let at = dst + y as usize * pitch + x as usize * bpp;
                    if complement {
                        let d = z.px_get(at, bpp);
                        let v = if bpp == 1 {
                            d ^ u32::from(mask)
                        } else {
                            !d & (u32::MAX >> (8 * (4 - bpp)))
                        };
                        z.px_put(at, bpp, v);
                    } else {
                        let pen = if pattern & bit != 0 { rgb0 } else { rgb1 };
                        z.px_put_masked(at, bpp, pen, mask);
                    }
                };
                put(self, x, y, cur_bit);
                cur_bit = cur_bit.rotate_right(1);
                if dx_abs >= dy_abs {
                    let len = if req_len != 0 { req_len as i32 } else { dx_abs };
                    let mut err = dx_abs >> 1;
                    for _ in 0..len {
                        err += dy_abs;
                        if err >= dx_abs {
                            err -= dx_abs;
                            y += y_step;
                        }
                        x += x_step;
                        put(self, x, y, cur_bit);
                        cur_bit = cur_bit.rotate_right(1);
                    }
                } else {
                    let len = if req_len != 0 { req_len as i32 } else { dy_abs };
                    let mut err = dy_abs >> 1;
                    for _ in 0..len {
                        err += dx_abs;
                        if err >= dy_abs {
                            err -= dy_abs;
                            x += x_step;
                        }
                        y += y_step;
                        put(self, x, y, cur_bit);
                        cur_bit = cur_bit.rotate_right(1);
                    }
                }
            }
            OP_P2C | OP_P2D => {
                // Planar source staged at offset[1] (P2D: after a 256-entry
                // CLUT of destination-format pixel values): `planes`
                // consecutive planes of pitch[1]-byte rows. x[0] = source
                // bit phase, dest rect (x[1],y[1]) w=x[2] h=y[2],
                // layer_mask user[0] gates planes, depth user[1].
                let (phase, dxr, w) = (x0, x1, x2);
                let (dyr, h) = (y1, y2);
                let planes = self.gfx_u16(0x20, 1).min(8);
                let layer_mask = user0 as u8;
                let sp = pitch1;
                let plane_size = sp * h;
                let (pal, data) = if op == OP_P2D {
                    (src, src + 1024)
                } else {
                    (0, src)
                };
                let dpitch = pitch0 * 4;
                // P2C output is always one chunky byte per pixel;
                // BlitPlanar2Chunky leaves the mailbox colormode from a prior
                // op, so it cannot be used for the destination stride here.
                let dbpp = if op == OP_P2C { 1 } else { bpp };
                // The planar hooks carry the blit function in the low nibble of
                // the minterm, a truth table indexed by (src << 1) | dst, so
                // AND/OR/EOR/DST all arrive here and not just SRC and NOTSRC.
                let func = minterm & 0x0F;
                // One source pen, decoded from the staged planes.
                let pen_of = |z: &Self, y: usize, x: usize| -> usize {
                    let bitpos = phase + x;
                    let bit = 0x80u8 >> (bitpos & 7);
                    let mut pen = 0usize;
                    for p in 0..planes {
                        if layer_mask & (1 << p) == 0 {
                            continue;
                        }
                        let at = data + plane_size * p + y * sp + bitpos / 8;
                        let b = if at < z.vram.len() { z.vram[at] } else { 0 };
                        if b & bit != 0 {
                            pen |= 1 << p;
                        }
                    }
                    pen
                };
                let at_of = |y: usize, x: usize| dst + (dyr + y) * dpitch + (dxr + x) * dbpp;

                // Both the op and the blit function are fixed for the whole
                // blit, so each combination we special-case gets its own loop
                // rather than a loop-invariant test per pixel. The cases worth
                // one are the functions of the source alone, which need no
                // destination read: SRC everywhere, and NOTSRC on the direct
                // path, where it is a lookup like SRC and not a bitwise op.
                if op == OP_P2C {
                    if func == MINTERM_SRC_IDX {
                        for y in 0..h {
                            for x in 0..w {
                                let (pen, at) = (pen_of(self, y, x), at_of(y, x));
                                self.px_put_masked(at, 1, pen as u32, mask);
                            }
                        }
                    } else {
                        for y in 0..h {
                            for x in 0..w {
                                let (pen, at) = (pen_of(self, y, x), at_of(y, x));
                                let v = minterm_apply(func, pen as u32, self.px_get(at, 1));
                                self.px_put_masked(at, 1, v, mask);
                            }
                        }
                    }
                } else if func == MINTERM_SRC_IDX {
                    for y in 0..h {
                        for x in 0..w {
                            let (pen, at) = (pen_of(self, y, x), at_of(y, x));
                            let col = self.px_get(pal + 4 * pen, 4);
                            self.px_put(at, bpp, col);
                        }
                    }
                } else if func == MINTERM_NOTSRC_IDX {
                    // A function of the source alone acts on the pen, not on
                    // the color the pen maps to: P96 expects ~pen to select
                    // another entry of the ColorIndexMapping, while inverting
                    // the color it mapped to lands outside the palette.
                    for y in 0..h {
                        for x in 0..w {
                            let (pen, at) = (pen_of(self, y, x), at_of(y, x));
                            let col = self.px_get(pal + 4 * (pen ^ 0xFF), 4);
                            self.px_put(at, bpp, col);
                        }
                    }
                } else {
                    for y in 0..h {
                        for x in 0..w {
                            let (pen, at) = (pen_of(self, y, x), at_of(y, x));
                            let col = self.px_get(pal + 4 * pen, 4);
                            let v = minterm_apply(func, col, self.px_get(at, bpp));
                            self.px_put(at, bpp, v);
                        }
                    }
                }
            }
            OP_SPRITE_XY => {
                self.sprite_x = i32::from(self.be16(g + 0x10) as i16);
                self.sprite_y = i32::from(self.be16(g + 0x18) as i16);
            }
            OP_SPRITE_COLOR => {
                // rgb0 was assembled bytewise on the 68k as B,G,R,0; the
                // u8offset byte selects the pen (driver sends idx+1).
                let pen = (self.vram[g + 0x3B] & 3) as usize;
                let (b, gr, r) = (rgb0 >> 24, (rgb0 >> 16) & 0xFF, (rgb0 >> 8) & 0xFF);
                self.sprite_colors[pen] = (r << 16) | (gr << 8) | b;
            }
            OP_SPRITE_BITMAP => {
                // The pointer image at offset[1] is a 2-bitplane sprite (pen
                // 0-3, 0 transparent). Each source row is plane 0's bytes
                // followed by plane 1's (firmware update_hw_sprite, video.c).
                // x[2] doubles the sprite: a w/2 x h/2 source shown 2x. The
                // firmware buffer caps at 32x48, so clamp there.
                let double = x2 != 0;
                let scale = if double { 2 } else { 1 };
                let (out_w, out_h) = (x1.min(64), y1.min(64));
                let (sw, sh) = (out_w / scale, out_h / scale);
                if sw == 0 || sh == 0 {
                    log::debug!("z3660: sprite bitmap {out_w}x{out_h} empty");
                    self.sprite_pix.clear();
                    return;
                }
                let plane = sw.div_ceil(8); // bytes per plane per source row
                let row_bytes = plane * 2;
                self.sprite_w = out_w;
                self.sprite_h = out_h;
                self.sprite_pix = vec![0u8; out_w * out_h];
                for sy in 0..sh {
                    let row = src + sy * row_bytes;
                    for sx in 0..sw {
                        let byte = sx / 8;
                        let bit = 7 - (sx & 7);
                        let p0 = self.px_get(row + byte, 1) as u8;
                        let p1 = self.px_get(row + plane + byte, 1) as u8;
                        let pen = ((p0 >> bit) & 1) | (((p1 >> bit) & 1) << 1);
                        for dy in 0..scale {
                            for dx in 0..scale {
                                let (ox, oy) = (sx * scale + dx, sy * scale + dy);
                                self.sprite_pix[oy * out_w + ox] = pen;
                            }
                        }
                    }
                }
            }
            _ => {
                log::debug!("z3660: dma-op {} not executed yet", dma_op_name(op));
            }
        }
    }

    /// Foreground/background store honouring the 8-bit plane mask.
    fn px_put_masked(&mut self, at: usize, bpp: usize, v: u32, mask: u8) {
        if bpp == 1 && mask != 0xFF {
            let d = self.px_get(at, 1) as u8;
            self.px_put(at, 1, u32::from((d & !mask) | (v as u8 & mask)));
        } else {
            self.px_put(at, bpp, v);
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
        w = w.min(MAX_DIM as u32);
        h = h.min(MAX_DIM as u32);
        if w == 0 || h == 0 {
            return None;
        }
        let bpp = colormode_bpp(colormode)?;
        // Rows advance by the bitmap's stride, which is wider than the
        // visible width when the guest pans around an oversized bitmap; the
        // board scans out only the mode's worth of each row (video.c sets
        // the VDMA stride from pan_width whenever it differs from hsize).
        let stride = if self.pan_width != 0 {
            self.pan_width
        } else {
            w
        };
        let pitch = (stride * bpp) as usize;
        let base = VRAM_OFFSET.checked_add(self.pan_offset)? as usize;
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
        // The hardware sprite (mouse pointer) is composited over the output
        // stream; it never exists in VRAM. Pen 0 is transparent.
        if self.sprite_visible && !self.sprite_pix.is_empty() {
            for sy in 0..self.sprite_h {
                let py = self.sprite_y + sy as i32;
                if py < 0 || py >= h as i32 {
                    continue;
                }
                for sx in 0..self.sprite_w {
                    let px = self.sprite_x + sx as i32;
                    let pen = self.sprite_pix[sy * self.sprite_w + sx];
                    if pen == 0 || px < 0 || px >= w as i32 {
                        continue;
                    }
                    let rgb = self.sprite_colors[pen as usize];
                    out[py as usize * w as usize + px as usize] =
                        0xFF00_0000 | ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF);
                }
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
                log::debug!(
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
            log::debug!(
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
        // Clear VRAM so a cold boot comes up with zeroed board RAM, like the
        // rest of the machine (Bus::power_on_reset). A warm reset re-inits
        // the board and repaints, so clearing here too is harmless.
        self.vram.fill(0);
        self.frame_phase = 0;
        self.vidmode_param = 0;
        self.palette.fill(0);
        self.pan_offset = 0;
        self.pan_width = 0;
        self.mode_set = false;
        self.sprite_visible = false;
        self.sprite_pix.clear();
        self.sprite_colors = [0; 4];
    }

    fn kind(&self) -> &'static str {
        "z3660"
    }
}

/// `enum gfx_dma_op` (common/z3660_regs.h): REG_BLITTER_DMA_OP selectors.
fn dma_op_name(op: u32) -> &'static str {
    const NAMES: [&str; 18] = [
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
        "SET_PALETTE",
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

    /// The Zorro III driver's bulk CLUT upload (OP_SET_PALETTE): the palette
    /// arrives as packed R,G,B triplets in the mailbox's clut1[] block, keyed
    /// by user[0]=start / user[1]=count, with u8_user[0] selecting the bank.
    #[test]
    fn palette_capture_from_set_palette_op() {
        const OP_SET_PALETTE: u32 = 17;
        let g = GFXDATA_OFFSET;
        let (mut z, mut m) = (Z3660::new(), mem());
        // Primary bank, start 1, two entries: index 1 = red, index 2 = green.
        z.vram[g + 0x21] = 1; // user[0] low byte (start)
        z.vram[g + 0x23] = 2; // user[1] low byte (count)
        z.vram[g + 0x30] = 0; // u8_user[0]: primary bank
        z.vram[g + 0x5C..g + 0x62].copy_from_slice(&[0xFF, 0, 0, 0, 0xFF, 0]);
        w32(&mut z, &mut m, REG_BLITTER_DMA_OP, OP_SET_PALETTE);
        assert_eq!(z.palette[1], 0x00FF_0000);
        assert_eq!(z.palette[2], 0x0000_FF00);

        // Secondary bank: u8_user[0] != 0 offsets the destination by 256.
        z.vram[g + 0x21] = 5; // start 5
        z.vram[g + 0x23] = 1; // count 1
        z.vram[g + 0x30] = 1; // secondary bank
        z.vram[g + 0x5C..g + 0x5F].copy_from_slice(&[0, 0, 0xFF]);
        w32(&mut z, &mut m, REG_BLITTER_DMA_OP, OP_SET_PALETTE);
        assert_eq!(z.palette[256 + 5], 0x0000_00FF);
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

    /// A 2x2 screen on a 4-pixel-wide bitmap, pen p at every cell:
    ///     1 2 3 4
    ///     5 6 7 8
    ///     9 A B C
    /// Pen p is coloured 0x0000pp, so a scanned-out pen appears as
    /// 0xFF00_0000 | (p << 16).
    fn panned_bitmap(x: u16, y: u16) -> Vec<u32> {
        let (mut z, mut m) = (Z3660::new(), mem());
        w32(&mut z, &mut m, REG_ORIG_RES, (2 << 16) | 2);
        w32(&mut z, &mut m, REG_MODE, 0x16);
        for pen in 1..=12u32 {
            w32(&mut z, &mut m, REG_OP_DATA, (pen << 24) | pen);
            w32(&mut z, &mut m, REG_OP, ARM_OP_PALETTE);
            let at = VRAM_OFFSET + pen - 1;
            z.write(at, 1, pen, &mut DeviceHost::new(&mut m));
        }
        let g = GFXDATA_OFFSET as u32;
        let mut h = DeviceHost::new(&mut m);
        z.write(g, 4, 0, &mut h); // offset[0]: bitmap at VRAM start
        z.write(g + 0x10, 2, u32::from(x), &mut h); // x[0]
        z.write(g + 0x18, 2, u32::from(y), &mut h); // y[0]
        z.write(g + 0x12, 2, 4, &mut h); // x[1]: bitmap is 4 wide
        z.write(g + 0x30, 1, COLORMODE_8BIT, &mut h); // u8_user[COLORMODE]
        w32(&mut z, &mut m, REG_BLITTER_DMA_OP, DMA_OP_PAN);
        let mut out = Vec::new();
        // The visible size is the mode's, not the bitmap's.
        assert_eq!(z.rtg_frame(&mut out), Some((2, 2)));
        out
    }

    /// PAN's x[1] is the bitmap's row stride, not the visible width: a
    /// bitmap wider than the mode still scans out only the mode's width,
    /// with rows advancing by the full stride.
    #[test]
    fn pan_width_is_a_stride_not_a_visible_width() {
        let pen = |p: u32| 0xFF00_0000 | (p << 16);
        // Top-left 2x2 of the bitmap: pens 1,2 over 5,6 -- so row 1 starts
        // 4 pixels in, not 2.
        assert_eq!(panned_bitmap(0, 0), [pen(1), pen(2), pen(5), pen(6)]);
    }

    /// PAN's x[0]/y[0] move the viewport within the bitmap, which the board
    /// implements by folding them into the scanout base.
    #[test]
    fn pan_viewport_offsets_move_the_scanout_origin() {
        let pen = |p: u32| 0xFF00_0000 | (p << 16);
        // One pixel right and one row down: pens 6,7 over 10,11.
        assert_eq!(panned_bitmap(1, 1), [pen(6), pen(7), pen(10), pen(11)]);
    }
}

#[cfg(test)]
mod exec_tests {
    use super::*;
    use crate::memory::Memory;

    fn mem() -> Memory {
        Memory {
            chip_ram: vec![0u8; 0x100],
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

    /// Fill the GFXData mailbox fields a blit op reads.
    #[allow(clippy::too_many_arguments)]
    fn gfxdata(
        z: &mut Z3660,
        dst: u32,
        src: u32,
        rgb0: u32,
        rgb1: u32,
        x: [u16; 3],
        y: [u16; 3],
        user0: u16,
        pitch: [u16; 2],
        colormode: u8,
        drawmode: u8,
        mask: u8,
        minterm: u8,
    ) {
        let g = GFXDATA_OFFSET;
        z.vram[g..g + 4].copy_from_slice(&dst.to_be_bytes());
        z.vram[g + 4..g + 8].copy_from_slice(&src.to_be_bytes());
        z.vram[g + 8..g + 12].copy_from_slice(&rgb0.to_be_bytes());
        z.vram[g + 12..g + 16].copy_from_slice(&rgb1.to_be_bytes());
        for i in 0..3 {
            z.vram[g + 0x10 + 2 * i..g + 0x12 + 2 * i].copy_from_slice(&x[i].to_be_bytes());
            z.vram[g + 0x18 + 2 * i..g + 0x1A + 2 * i].copy_from_slice(&y[i].to_be_bytes());
        }
        z.vram[g + 0x20..g + 0x22].copy_from_slice(&user0.to_be_bytes());
        for i in 0..2 {
            z.vram[g + 0x28 + 2 * i..g + 0x2A + 2 * i].copy_from_slice(&pitch[i].to_be_bytes());
        }
        z.vram[g + 0x30] = colormode;
        z.vram[g + 0x31] = drawmode;
        z.vram[g + 0x39] = mask;
        z.vram[g + 0x3A] = minterm;
    }

    fn ring(z: &mut Z3660, m: &mut Memory, op: u32) {
        z.write(0x180, 4, op, &mut DeviceHost::new(m));
    }

    #[test]
    fn fillrect_fills_the_rect_and_nothing_else() {
        let (mut z, mut m) = (Z3660::new(), mem());
        // 8-bit surface, pitch 16 bytes (pitch[0] = 4 longwords): fill a
        // 3x2 rect of pen 7 at (1,1).
        gfxdata(
            &mut z,
            0,
            0,
            7,
            0,
            [1, 3, 0],
            [1, 2, 0],
            0,
            [4, 0],
            0,
            0,
            0xFF,
            0,
        );
        ring(&mut z, &mut m, 2);
        let v = VRAM_OFFSET as usize;
        assert_eq!(&z.vram[v..v + 4], &[0, 0, 0, 0], "row 0 untouched");
        assert_eq!(&z.vram[v + 16..v + 21], &[0, 7, 7, 7, 0]);
        assert_eq!(&z.vram[v + 32..v + 37], &[0, 7, 7, 7, 0]);
        assert_eq!(z.vram[v + 48], 0, "row 3 untouched");
    }

    #[test]
    fn copyrect_moves_an_overlapping_rect() {
        let (mut z, mut m) = (Z3660::new(), mem());
        let v = VRAM_OFFSET as usize;
        // One row of 8 pixels 0..8; copy 4 pixels from x=0 to x=2 (overlap).
        for i in 0..8 {
            z.vram[v + i] = i as u8;
        }
        gfxdata(
            &mut z,
            0,
            0,
            0,
            0,
            [2, 4, 0],
            [0, 1, 0],
            0,
            [4, 0],
            0,
            0,
            0xFF,
            0,
        );
        ring(&mut z, &mut m, 3);
        assert_eq!(&z.vram[v..v + 8], &[0, 1, 0, 1, 2, 3, 6, 7]);
    }

    /// Rects that overlap vertically scroll without smearing. Only one row
    /// is buffered, so rows must be walked away from the destination: a
    /// downward copy taken top-first would overwrite row n before reading
    /// it as the source for row n+1, painting the first row everywhere.
    #[test]
    fn copyrect_scrolls_down_without_smearing() {
        let (mut z, mut m) = (Z3660::new(), mem());
        let v = VRAM_OFFSET as usize;
        // Four 4-byte rows, contiguous (pitch0 = 1 longword).
        for i in 0..16 {
            z.vram[v + i] = i as u8;
        }
        // Copy rows 0..3 down one row, so each row takes the one above it.
        gfxdata(
            &mut z,
            0,
            0,
            0,
            0,
            [0, 4, 0],
            [1, 3, 0],
            0,
            [1, 0],
            0,
            0,
            0xFF,
            0,
        );
        ring(&mut z, &mut m, 3);
        assert_eq!(
            &z.vram[v..v + 16],
            &[0, 1, 2, 3, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
        );
    }

    /// A surface offset near the top of the 32-bit address space must not
    /// wrap back into live VRAM when VRAM_OFFSET is added to it.
    ///
    /// This only discriminates where usize is 32 bits: on wasm32 the
    /// unwidened add wrapped to 0x1FFFF0, inside the mailbox region, and
    /// corrupted it. On a 64-bit host the sum is merely far out of range and
    /// the op drops either way, so here this is a guard rather than a
    /// reproduction.
    #[test]
    fn blit_base_near_the_address_space_top_does_not_wrap() {
        let (mut z, mut m) = (Z3660::new(), mem());
        let v = VRAM_OFFSET as usize;
        for i in 0..8 {
            z.vram[v + i] = i as u8;
        }
        // dst = 0xFFFF_FFF0: adding VRAM_OFFSET overflows 32 bits, and a
        // wrapped base would land inside VRAM and corrupt it.
        gfxdata(
            &mut z,
            0xFFFF_FFF0,
            0,
            0xAA,
            0,
            [0, 4, 0],
            [0, 1, 0],
            0,
            [4, 0],
            0,
            0,
            0xFF,
            0,
        );
        ring(&mut z, &mut m, 2); // FILLRECT
        assert_eq!(&z.vram[v..v + 8], &[0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn template_jam2_draws_fg_and_bg() {
        let (mut z, mut m) = (Z3660::new(), mem());
        let v = VRAM_OFFSET as usize;
        // Template data 0b1010_0000 at VRAM offset 0x1000; 4x1 rect at
        // (0,0), fg pen 3, bg pen 9, JAM2 (drawmode 1), pitch[0] raw 16.
        z.vram[v + 0x1000] = 0xA0;
        gfxdata(
            &mut z,
            0,
            0x1000,
            3,
            9,
            [0, 4, 0],
            [0, 1, 0],
            0,
            [16, 1],
            0,
            1,
            0xFF,
            0,
        );
        ring(&mut z, &mut m, 5);
        assert_eq!(&z.vram[v..v + 4], &[3, 9, 3, 9]);
    }

    #[test]
    fn drawline_steps_like_bresenham() {
        let (mut z, mut m) = (Z3660::new(), mem());
        let v = VRAM_OFFSET as usize;
        // 8-bit surface, pitch 8 bytes (pitch[0] = 2 longwords). Line from
        // (0,0) with delta (2,1), pen 5, solid pattern, JAM1. Bresenham on
        // the major (x) axis puts the second pixel at (1,1) -- not (1,0) as
        // the old interpolation did.
        gfxdata(
            &mut z,
            0,
            0,
            5,
            0,
            [0, 2, 0],
            [0, 1, 0],
            0,
            [2, 0],
            0,
            0,
            0xFF,
            0,
        );
        let g = GFXDATA_OFFSET;
        z.vram[g + 0x22..g + 0x24].copy_from_slice(&0xFFFFu16.to_be_bytes()); // user[1] pattern
        ring(&mut z, &mut m, 1);
        assert_eq!(z.vram[v], 5, "(0,0)");
        assert_eq!(z.vram[v + 8 + 1], 5, "(1,1)");
        assert_eq!(z.vram[v + 8 + 2], 5, "(2,1)");
        assert_eq!(z.vram[v + 1], 0, "(1,0) must be untouched");
    }

    #[test]
    fn drawline_honors_line_length() {
        let (mut z, mut m) = (Z3660::new(), mem());
        let v = VRAM_OFFSET as usize;
        // Horizontal delta (10,0) but Line.Length (user[0]) = 3, so only
        // pixels 0..=3 are drawn -- the clipped segment, not the full delta.
        gfxdata(
            &mut z,
            0,
            0,
            7,
            0,
            [0, 10, 0],
            [0, 0, 0],
            3,
            [4, 0],
            0,
            0,
            0xFF,
            0,
        );
        let g = GFXDATA_OFFSET;
        z.vram[g + 0x22..g + 0x24].copy_from_slice(&0xFFFFu16.to_be_bytes());
        ring(&mut z, &mut m, 1);
        assert_eq!(&z.vram[v..v + 6], &[7, 7, 7, 7, 0, 0]);
    }

    /// P2C decodes staged bitplanes to pens; P2D looks the pens up in the
    /// staged destination-format CLUT.
    #[test]
    fn planar_blits_decode_planes() {
        let (mut z, mut m) = (Z3660::new(), mem());
        let v = VRAM_OFFSET as usize;
        // Two planes staged at 0x1000, 1 byte per row, 1 row: plane 0 =
        // 0b1100_0000, plane 1 = 0b0100_0000 -> pens 1, 3, 0, 0 ...
        z.vram[v + 0x1000] = 0xC0;
        z.vram[v + 0x1001] = 0x40;
        // P2C to a chunky row at (0,0): minterm SRC (12), depth 2 in
        // user[1], layer mask 0xFF in user[0], src pitch[1] = 1. Colormode
        // is deliberately 16-bit (a stale value from a prior op): P2C output
        // is one byte per pixel regardless, so the pens must land at
        // consecutive bytes, not on a 2-byte stride.
        gfxdata(
            &mut z,
            0,
            0x1000,
            0,
            0,
            [0, 0, 4],
            [0, 0, 1],
            0xFF,
            [4, 1],
            1,
            0,
            0xFF,
            12,
        );
        let g = GFXDATA_OFFSET;
        z.vram[g + 0x22..g + 0x24].copy_from_slice(&2u16.to_be_bytes()); // user[1] depth
        ring(&mut z, &mut m, 7);
        assert_eq!(&z.vram[v..v + 4], &[1, 3, 0, 0]);

        // P2D to a 16-bit row at (0,4): CLUT staged at 0x2000, planes
        // follow at +1024; pen 3 = 0xB67D.
        z.vram[v + 0x2000 + 4 * 3 + 2..v + 0x2000 + 4 * 3 + 4].copy_from_slice(&[0xB6, 0x7D]);
        z.vram[v + 0x2400] = 0xC0;
        z.vram[v + 0x2401] = 0x40;
        gfxdata(
            &mut z,
            0x100,
            0x2000,
            0,
            0,
            [0, 0, 2],
            [0, 0, 1],
            0xFF,
            [4, 1],
            1,
            0,
            0xFF,
            12,
        );
        z.vram[g + 0x22..g + 0x24].copy_from_slice(&2u16.to_be_bytes());
        ring(&mut z, &mut m, 8);
        // Pixel 0 = pen 1 (CLUT entry 0), pixel 1 = pen 3 = 0xB67D.
        assert_eq!(&z.vram[v + 0x100..v + 0x104], &[0x00, 0x00, 0xB6, 0x7D]);
    }

    /// P2C combines the decoded pens with the destination through the minterm,
    /// not just SRC: BltBitMap's AND, EOR and DST all reach the planar hook.
    #[test]
    fn p2c_honours_the_minterm() {
        let v = VRAM_OFFSET as usize;
        let g = GFXDATA_OFFSET;
        // Source planes give pens 1, 3, 0, 0 as above; the destination row
        // already holds 3, 1, 5, 5.
        for (minterm, want) in [
            (12u8, [1u8, 3, 0, 0]),        // SRC
            (3, [0xFE, 0xFC, 0xFF, 0xFF]), // NOTSRC
            (8, [1, 1, 0, 0]),             // AND
            (14, [3, 3, 5, 5]),            // OR
            (6, [2, 2, 5, 5]),             // EOR
            (10, [3, 1, 5, 5]),            // DST: destination untouched
        ] {
            let (mut z, mut m) = (Z3660::new(), mem());
            z.vram[v + 0x1000] = 0xC0;
            z.vram[v + 0x1001] = 0x40;
            z.vram[v..v + 4].copy_from_slice(&[3, 1, 5, 5]);
            gfxdata(
                &mut z,
                0,
                0x1000,
                0,
                0,
                [0, 0, 4],
                [0, 0, 1],
                0xFF,
                [4, 1],
                1,
                0,
                0xFF,
                minterm,
            );
            z.vram[g + 0x22..g + 0x24].copy_from_slice(&2u16.to_be_bytes());
            ring(&mut z, &mut m, 7);
            assert_eq!(&z.vram[v..v + 4], &want, "minterm {minterm}");
        }
    }

    /// NOTSRC on a direct screen complements the pen and looks that entry up in
    /// the ColorIndexMapping. Complementing the color the pen maps to instead
    /// lands on a color the mapping never contains, which is what P96's own
    /// reference rendering shows up.
    #[test]
    fn p2d_notsrc_inverts_the_pen() {
        let (mut z, mut m) = (Z3660::new(), mem());
        let v = VRAM_OFFSET as usize;
        let g = GFXDATA_OFFSET;

        // CLUT at 0x2000 with the planes at +1024, decoding to pens 1 and 3.
        // Only the entries the complemented pens select are filled in, so a
        // lookup by the pen itself reads zero.
        z.vram[v + 0x2000 + 4 * 0xFE + 2..v + 0x2000 + 4 * 0xFE + 4].copy_from_slice(&[0x12, 0x34]);
        z.vram[v + 0x2000 + 4 * 0xFC + 2..v + 0x2000 + 4 * 0xFC + 4].copy_from_slice(&[0x56, 0x78]);
        z.vram[v + 0x2400] = 0xC0;
        z.vram[v + 0x2401] = 0x40;
        gfxdata(
            &mut z,
            0x100,
            0x2000,
            0,
            0,
            [0, 0, 2],
            [0, 0, 1],
            0xFF,
            [4, 1],
            1,
            0,
            0xFF,
            3,
        );
        z.vram[g + 0x22..g + 0x24].copy_from_slice(&2u16.to_be_bytes());
        ring(&mut z, &mut m, 8);
        assert_eq!(&z.vram[v + 0x100..v + 0x104], &[0x12, 0x34, 0x56, 0x78]);
    }
}

#[cfg(test)]
mod sprite_tests {
    use super::*;
    use crate::memory::Memory;

    fn mem() -> Memory {
        Memory {
            chip_ram: vec![0u8; 0x100],
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

    fn w32(z: &mut Z3660, m: &mut Memory, off: u32, value: u32) {
        z.write(off, 4, value, &mut DeviceHost::new(m));
    }

    /// The pointer overlays the frame at its position without touching
    /// VRAM: SetSprite shows it, SPRITE_BITMAP uploads the 2-plane image,
    /// SPRITE_COLOR sets pen 1 (u8offset = idx+1, B/G/R bytes), SPRITE_XY
    /// places it.
    #[test]
    fn sprite_composites_over_the_frame() {
        let (mut z, mut m) = (Z3660::new(), mem());
        let g = GFXDATA_OFFSET;
        // 4x2 8-bit screen showing palette entry 0 (black).
        w32(&mut z, &mut m, REG_ORIG_RES, (4 << 16) | 2);
        w32(&mut z, &mut m, REG_MODE, 0x16);
        // Pen 1 = pure red: rgb0 guest bytes B,G,R,0 = 00 00 FF 00.
        z.vram[g + 8..g + 12].copy_from_slice(&[0x00, 0x00, 0xFF, 0x00]);
        z.vram[g + 0x3B] = 1;
        w32(&mut z, &mut m, REG_BLITTER_DMA_OP, 12);
        // 16x1 bitmap at src offset 0x1000: plane0 = 0x8000 (first pixel).
        z.vram[g + 4..g + 8].copy_from_slice(&0x1000u32.to_be_bytes());
        z.vram[g + 0x12..g + 0x14].copy_from_slice(&16u16.to_be_bytes()); // x[1]=w
        z.vram[g + 0x1A..g + 0x1C].copy_from_slice(&1u16.to_be_bytes()); // y[1]=h
        z.vram[g + 0x14..g + 0x16].copy_from_slice(&[0, 0]); // x[2] doubled=0
        z.vram[g + 0x1C..g + 0x1E].copy_from_slice(&[0, 0]); // y[2] hires-1=0
        let v = VRAM_OFFSET as usize + 0x1000;
        z.vram[v] = 0x80; // plane 0 word 0x8000
        w32(&mut z, &mut m, REG_BLITTER_DMA_OP, 13);
        // Position (1,1), show.
        z.vram[g + 0x10..g + 0x12].copy_from_slice(&1u16.to_be_bytes());
        z.vram[g + 0x18..g + 0x1A].copy_from_slice(&1u16.to_be_bytes());
        w32(&mut z, &mut m, REG_BLITTER_DMA_OP, 11);
        w32(&mut z, &mut m, REG_SPRITE_BITMAP, 1);

        let mut out = Vec::new();
        assert_eq!(z.rtg_frame(&mut out), Some((4, 2)));
        let red = 0xFF00_0000 | 0xFF; // R in the low byte
        assert_eq!(out[0], 0xFF00_0000, "screen pixel outside the sprite");
        assert_eq!(out[4 + 1], red, "sprite pixel at (1,1)");
        // Hide: the overlay disappears.
        w32(&mut z, &mut m, REG_SPRITE_BITMAP, 2);
        z.rtg_frame(&mut out);
        assert_eq!(out[4 + 1], 0xFF00_0000);
    }

    /// A doubled sprite (x[2] = 1, as the OS 3.2 32x48 pointer uses) decodes
    /// its half-size source and scales each pixel to a 2x2 block, instead of
    /// being rejected as too wide.
    #[test]
    fn doubled_sprite_scales_2x() {
        let (mut z, mut m) = (Z3660::new(), mem());
        let g = GFXDATA_OFFSET;
        w32(&mut z, &mut m, REG_ORIG_RES, (4 << 16) | 4);
        w32(&mut z, &mut m, REG_MODE, 0x16);
        // Pen 1 = red.
        z.vram[g + 8..g + 12].copy_from_slice(&[0x00, 0x00, 0xFF, 0x00]);
        z.vram[g + 0x3B] = 1;
        w32(&mut z, &mut m, REG_BLITTER_DMA_OP, 12);
        // Doubled 4x4 sprite from a 2x2 source at src 0x1000; source row is
        // one plane-0 byte then one plane-1 byte. Row 0 plane 0 = 0x80
        // (source pixel (0,0) set) -> a 2x2 red block at output (0,0).
        z.vram[g + 4..g + 8].copy_from_slice(&0x1000u32.to_be_bytes());
        z.vram[g + 0x12..g + 0x14].copy_from_slice(&4u16.to_be_bytes()); // x[1]=out w
        z.vram[g + 0x14..g + 0x16].copy_from_slice(&1u16.to_be_bytes()); // x[2]=double
        z.vram[g + 0x1A..g + 0x1C].copy_from_slice(&4u16.to_be_bytes()); // y[1]=out h
        z.vram[g + 0x1C..g + 0x1E].copy_from_slice(&[0, 0]); // y[2]=hires-1=0
        z.vram[VRAM_OFFSET as usize + 0x1000] = 0x80;
        w32(&mut z, &mut m, REG_BLITTER_DMA_OP, 13);
        z.vram[g + 0x10..g + 0x12].copy_from_slice(&[0, 0]); // pos (0,0)
        z.vram[g + 0x18..g + 0x1A].copy_from_slice(&[0, 0]);
        w32(&mut z, &mut m, REG_BLITTER_DMA_OP, 11);
        w32(&mut z, &mut m, REG_SPRITE_BITMAP, 1);

        let mut out = Vec::new();
        assert_eq!(z.rtg_frame(&mut out), Some((4, 4)));
        let red = 0xFF00_0000 | 0xFF;
        // The single source pixel fills the 2x2 top-left block.
        assert_eq!(out[0], red);
        assert_eq!(out[1], red);
        assert_eq!(out[4], red);
        assert_eq!(out[5], red);
        // Outside the block is transparent (screen shows through).
        assert_eq!(out[2], 0xFF00_0000);
        assert_eq!(out[8], 0xFF00_0000);
    }
}
