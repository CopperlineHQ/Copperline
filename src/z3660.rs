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
                    // MemoryBase, x[1] = row width in pixels.
                    self.pan_offset = self.be32(GFXDATA_OFFSET);
                    self.pan_width = u32::from(self.be16(GFXDATA_OFFSET + 0x12));
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

    /// Execute one REG_BLITTER_DMA_OP request against VRAM.
    fn exec_dma_op(&mut self, op: u32) {
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
        const MINTERM_NOTSRC: u8 = 3;
        const MINTERM_SRC_IDX: u8 = 12;
        const COMPLEMENT: u8 = 2;
        const OP_SPRITE_COLOR: u32 = 12;
        const OP_SPRITE_BITMAP: u32 = 13;
        const MINTERM_SRC: u8 = 0xC0;
        const JAM1: u8 = 0;
        const INVERSVID: u8 = 8;

        let g = GFXDATA_OFFSET;
        let dst = VRAM_OFFSET as usize + self.be32(g) as usize;
        let src = VRAM_OFFSET as usize + self.be32(g + 4) as usize;
        let rgb0 = self.be32(g + 8);
        let rgb1 = self.be32(g + 0xC);
        let (x0, x1, x2) = (
            self.gfx_u16(0x10, 0),
            self.gfx_u16(0x10, 1),
            self.gfx_u16(0x10, 2),
        );
        let (y0, y1, y2) = (
            self.gfx_u16(0x18, 0),
            self.gfx_u16(0x18, 1),
            self.gfx_u16(0x18, 2),
        );
        let user0 = self.gfx_u16(0x20, 0);
        let (pitch0, pitch1) = (self.gfx_u16(0x28, 0), self.gfx_u16(0x28, 1));
        let colormode = u32::from(self.vram[g + 0x30]);
        let drawmode = self.vram[g + 0x31];
        let mask = self.vram[g + 0x39];
        let minterm = self.vram[g + 0x3A];
        let bpp: usize = match colormode {
            COLORMODE_8BIT => 1,
            COLORMODE_16BIT565 | COLORMODE_15BIT => 2,
            COLORMODE_32BIT => 4,
            _ => return,
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
                // carries a minterm (only SRC is implemented; others log).
                let dpitch = pitch0 * 4;
                let (sbase, spitch) = if op == OP_COPYRECT {
                    (dst, dpitch)
                } else {
                    (src, pitch1 * 4)
                };
                if op == OP_COPYRECT_NOMASK && minterm != MINTERM_SRC {
                    log::debug!("z3660: copyrect minterm {minterm:#04x} treated as SRC");
                }
                let apply_mask = op == OP_COPYRECT && bpp == 1 && mask != 0xFF;
                // Snapshot the source rect first: rects may overlap.
                let mut rect = vec![0u8; x1 * y1 * bpp];
                for y in 0..y1 {
                    let srow = sbase + (y2 + y) * spitch + x2 * bpp;
                    for b in 0..x1 * bpp {
                        rect[y * x1 * bpp + b] = if srow + b < self.vram.len() {
                            self.vram[srow + b]
                        } else {
                            0
                        };
                    }
                }
                for y in 0..y1 {
                    let drow = dst + (y0 + y) * dpitch + x0 * bpp;
                    for x in 0..x1 * bpp {
                        let at = drow + x;
                        if at >= self.vram.len() {
                            continue;
                        }
                        let s = rect[y * x1 * bpp + x];
                        self.vram[at] = if apply_mask {
                            (self.vram[at] & !mask) | (s & mask)
                        } else {
                            s
                        };
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
                // From (x[0],y[0]) along the signed deltas (x[1],y[1]);
                // pattern user[1] rotates per pixel (0xFFFF = solid). JAM2
                // draws bg where the pattern bit is clear; the COMPLEMENT
                // bit inverts the destination instead (shell XOR cursor).
                let pitch = pitch0 * 4;
                let (ax, ay) = (
                    i32::from(self.be16(g + 0x10) as i16),
                    i32::from(self.be16(g + 0x18) as i16),
                );
                let (dx, dy) = (
                    i32::from(self.be16(g + 0x12) as i16),
                    i32::from(self.be16(g + 0x1A) as i16),
                );
                let mut pattern = self.gfx_u16(0x20, 1) as u16;
                if drawmode & INVERSVID != 0 {
                    pattern ^= 0xFFFF;
                }
                let complement = drawmode & COMPLEMENT != 0;
                let jam2 = !complement && drawmode & 1 != 0;
                let steps = dx.abs().max(dy.abs());
                let mut cur_bit = 0x8000u16;
                for i in 0..=steps {
                    let px = ax + if steps == 0 { 0 } else { dx * i / steps };
                    let py = ay + if steps == 0 { 0 } else { dy * i / steps };
                    if px >= 0 && py >= 0 {
                        let at = dst + py as usize * pitch + px as usize * bpp;
                        if pattern & cur_bit != 0 {
                            if complement {
                                let d = self.px_get(at, bpp);
                                let v = if bpp == 1 {
                                    d ^ u32::from(mask)
                                } else {
                                    !d & (u32::MAX >> (8 * (4 - bpp)))
                                };
                                self.px_put(at, bpp, v);
                            } else {
                                self.px_put_masked(at, bpp, rgb0, mask);
                            }
                        } else if jam2 {
                            self.px_put_masked(at, bpp, rgb1, mask);
                        }
                    }
                    cur_bit = cur_bit.rotate_right(1);
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
                if minterm != MINTERM_SRC_IDX && minterm != MINTERM_NOTSRC {
                    log::debug!("z3660: p2c/p2d minterm {minterm} treated as SRC");
                }
                let inv = if minterm == MINTERM_NOTSRC { 0xFF } else { 0 };
                for y in 0..h {
                    for i in 0..w {
                        let bitpos = phase + i;
                        let bit = 0x80u8 >> (bitpos & 7);
                        let mut pen = 0usize;
                        for p in 0..planes {
                            if layer_mask & (1 << p) == 0 {
                                continue;
                            }
                            let at = data + plane_size * p + y * sp + bitpos / 8;
                            let b = if at < self.vram.len() {
                                self.vram[at] ^ inv
                            } else {
                                0
                            };
                            if b & bit != 0 {
                                pen |= 1 << p;
                            }
                        }
                        let at = dst + (dyr + y) * dpitch + (dxr + i) * bpp;
                        if op == OP_P2C {
                            self.px_put_masked(at, 1, pen as u32, mask);
                        } else {
                            let col = self.px_get(pal + 4 * pen, 4);
                            self.px_put(at, bpp, col);
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
                // Amiga 2-plane sprite rows at offset[1]: `hires` words of
                // plane 0 then plane 1 per row; pixel value = plane bits.
                let (w, h) = (x1, y1);
                let hires = y2 + 1;
                // Row: `hires` words of plane 0, then plane 1 (the driver
                // copies (w>>3)*2*hires bytes per row).
                let row_bytes = (w / 8) * 2 * hires;
                if w == 0 || h == 0 || w > 16 * hires {
                    log::debug!("z3660: sprite bitmap {w}x{h} hires {hires} unsupported");
                    self.sprite_pix.clear();
                    return;
                }
                self.sprite_w = w;
                self.sprite_h = h;
                self.sprite_pix = vec![0u8; w * h];
                for y in 0..h {
                    let row = src + y * row_bytes;
                    for x in 0..w {
                        let word = x / 16;
                        let bit = 15 - (x & 15);
                        let p0 = self.be16(row + 2 * word);
                        let p1 = self.be16(row + 2 * hires + 2 * word);
                        let v = (((p1 >> bit) & 1) << 1) | ((p0 >> bit) & 1);
                        self.sprite_pix[y * w + x] = v as u8;
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

#[cfg(test)]
mod exec_tests {
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
        // P2C to an 8-bit row at (0,0): minterm SRC (12), depth 2 in
        // user[1], layer mask 0xFF in user[0], src pitch[1] = 1.
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
            0,
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
}

#[cfg(test)]
mod sprite_tests {
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
}
