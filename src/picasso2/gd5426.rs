// SPDX-License-Identifier: GPL-3.0-or-later

//! Cirrus Logic CL-GD5426/CL-GD5428 graphics controller.
//!
//! This is the self-contained chip core used by the Picasso II family. It
//! models the packed-pixel register paths used by Amiga RTG drivers, direct
//! linear VRAM, the CL-GD542x BitBLT engine, and the hardware cursor. VGA text
//! rendering is intentionally absent: the board powers up behind the native
//! video pass-through and its driver programs a packed-pixel mode from
//! scratch.

use crate::chipset::paula::PAULA_CLOCK_HZ;

const MAX_DIM: usize = 4096;
const BLIT_BUSY_CCK: u32 = 16;

const SR_CURSOR_X: u8 = 0x10;
const SR_CURSOR_Y: u8 = 0x11;
const SR_CURSOR_ATTR: usize = 0x12;
const SR_CURSOR_PATTERN: usize = 0x13;

const PART_ID_GD5426: u8 = 0x90;
const PART_ID_GD5428: u8 = 0x98;

const BLIT_MODE_BACKWARDS: u8 = 0x01;
const BLIT_MODE_SYSTEM_SOURCE: u8 = 0x04;
const BLIT_MODE_TRANSPARENT: u8 = 0x08;
const BLIT_MODE_PIXEL_WIDTH: u8 = 0x30;
const BLIT_MODE_PATTERN: u8 = 0x40;
const BLIT_MODE_COLOR_EXPAND: u8 = 0x80;
const BLIT_MODEEXT_DWORD_GRANULARITY: u8 = 0x01;
const BLIT_MODEEXT_COLOR_EXPAND_INVERT: u8 = 0x02;
const BLIT_MODEEXT_SOLID_FILL: u8 = 0x04;

fn diagnostic_enabled() -> bool {
    crate::envcfg::flag("COPPERLINE_DIAG_PICASSO")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelDepth {
    Clut8,
    Rgb555,
    Rgb565,
    Bgr888,
}

impl PixelDepth {
    fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Clut8 => 1,
            Self::Rgb555 | Self::Rgb565 => 2,
            Self::Bgr888 => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedMode {
    pub width: usize,
    pub height: usize,
    pub pitch: usize,
    pub start: usize,
    pub depth: PixelDepth,
    pub doublescan: bool,
    pub interlace: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SystemBlit {
    expected: usize,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum CirrusModel {
    Gd5426,
    Gd5428,
}

impl CirrusModel {
    fn part_id(self) -> u8 {
        match self {
            Self::Gd5426 => PART_ID_GD5426,
            Self::Gd5428 => PART_ID_GD5428,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct CirrusGd5426 {
    model: CirrusModel,
    vram: Vec<u8>,
    seq: Vec<u8>,
    gfx: Vec<u8>,
    crtc: Vec<u8>,
    attr: Vec<u8>,
    seq_index: u8,
    gfx_index: u8,
    crtc_index: u8,
    attr_index: u8,
    attr_data_phase: bool,
    misc_output: u8,
    pel_mask: u8,
    palette: Vec<[u8; 3]>,
    cursor_palette: Vec<[u8; 3]>,
    dac_write_index: u8,
    dac_read_index: u8,
    dac_component: u8,
    dac_read_mode: bool,
    hidden_dac: u8,
    hidden_dac_reads: u8,
    cursor_x: u16,
    cursor_y: u16,
    frame_phase_cck: u64,
    vblank_pending: bool,
    blit_busy_cck: u32,
    system_blit: Option<SystemBlit>,
    #[serde(skip, default = "diagnostic_enabled")]
    diagnostic: bool,
}

impl CirrusGd5426 {
    pub fn new(vram_bytes: usize) -> Self {
        Self::new_model(vram_bytes, CirrusModel::Gd5426)
    }

    pub fn new_gd5428(vram_bytes: usize) -> Self {
        Self::new_model(vram_bytes, CirrusModel::Gd5428)
    }

    fn new_model(vram_bytes: usize, model: CirrusModel) -> Self {
        debug_assert!(matches!(vram_bytes, 0x10_0000 | 0x20_0000));
        let mut seq = vec![0; 0x100];
        // Power-on VCLK values used by the CL-GD542x BIOS reference model.
        seq[0x0b] = 0x4a;
        seq[0x0c] = 0x5b;
        seq[0x0d] = 0x45;
        seq[0x0e] = 0x7e;
        seq[0x1b] = 0x2b;
        seq[0x1c] = 0x2f;
        seq[0x1d] = 0x30;
        seq[0x1e] = 0x33;
        seq[6] = 0x0f;

        let mut gfx = vec![0; 0x100];
        gfx[8] = 0xff;
        Self {
            model,
            vram: vec![0; vram_bytes],
            seq,
            gfx,
            crtc: vec![0; 0x100],
            attr: vec![0; 0x40],
            seq_index: 0,
            gfx_index: 0,
            crtc_index: 0,
            attr_index: 0,
            attr_data_phase: false,
            misc_output: 0,
            pel_mask: 0xff,
            palette: vec![[0; 3]; 256],
            cursor_palette: vec![[0; 3]; 16],
            dac_write_index: 0,
            dac_read_index: 0,
            dac_component: 0,
            dac_read_mode: false,
            hidden_dac: 0,
            hidden_dac_reads: 0,
            cursor_x: 0,
            cursor_y: 0,
            frame_phase_cck: 0,
            vblank_pending: false,
            blit_busy_cck: 0,
            system_blit: None,
            diagnostic: diagnostic_enabled(),
        }
    }

    pub fn vram_len(&self) -> usize {
        self.vram.len()
    }

    pub fn reset(&mut self) {
        let vram_bytes = self.vram.len();
        *self = Self::new_model(vram_bytes, self.model);
    }

    fn extensions_unlocked(&self) -> bool {
        self.seq[6] == 0x12
    }

    fn disarm_hidden_dac(&mut self) {
        self.hidden_dac_reads = 0;
    }

    pub fn io_read(&mut self, port: u16) -> u8 {
        if port != 0x3c6 {
            self.disarm_hidden_dac();
        }
        match port {
            0x3c0 => self.attr_index | if self.attr_data_phase { 0x20 } else { 0 },
            0x3c1 => self.attr[self.attr_index as usize & 0x1f],
            0x3c2 => self.input_status_0(),
            0x3c4 => {
                let base = self.seq_index & 0x1f;
                if self.extensions_unlocked() && base == SR_CURSOR_X {
                    ((self.cursor_x as u8 & 7) << 5) | SR_CURSOR_X
                } else if self.extensions_unlocked() && base == SR_CURSOR_Y {
                    ((self.cursor_y as u8 & 7) << 5) | SR_CURSOR_Y
                } else {
                    self.seq_index
                }
            }
            0x3c5 => self.read_sequencer(),
            0x3c6 => self.read_pel_mask_or_hidden_dac(),
            0x3c7 => {
                if self.dac_read_mode {
                    3
                } else {
                    0
                }
            }
            0x3c8 => self.dac_write_index,
            0x3c9 => self.read_dac_data(),
            0x3cc => self.misc_output,
            0x3ce => self.gfx_index,
            0x3cf => self.read_graphics(),
            0x3b4 | 0x3d4 => self.crtc_index,
            0x3b5 | 0x3d5 => self.read_crtc(),
            0x3ba | 0x3da => {
                self.attr_data_phase = false;
                self.input_status_1()
            }
            _ => 0xff,
        }
    }

    pub fn io_write(&mut self, port: u16, value: u8) {
        if port != 0x3c6 {
            self.disarm_hidden_dac();
        }
        if self.diagnostic {
            log::info!("picasso2: VGA port {port:#05x} <- {value:#04x}");
        }
        match port {
            0x3c0 | 0x3c1 => {
                if self.attr_data_phase {
                    self.attr[self.attr_index as usize & 0x1f] = value;
                } else {
                    self.attr_index = value & 0x1f;
                }
                self.attr_data_phase = !self.attr_data_phase;
            }
            0x3c2 => self.misc_output = value,
            0x3c4 => self.seq_index = value,
            0x3c5 => self.write_sequencer(value),
            0x3c6 => self.write_pel_mask_or_hidden_dac(value),
            0x3c7 => {
                self.dac_read_index = value;
                self.dac_component = 0;
                self.dac_read_mode = true;
            }
            0x3c8 => {
                self.dac_write_index = value;
                self.dac_component = 0;
                self.dac_read_mode = false;
            }
            0x3c9 => self.write_dac_data(value),
            0x3ce => self.gfx_index = value,
            0x3cf => self.write_graphics(value),
            0x3b4 | 0x3d4 => self.crtc_index = value,
            0x3b5 | 0x3d5 => self.write_crtc(value),
            _ => {}
        }
    }

    fn read_sequencer(&self) -> u8 {
        let index = self.seq_index & 0x1f;
        if index == 6 {
            return if self.extensions_unlocked() {
                0x12
            } else {
                0x0f
            };
        }
        if index > 6 && !self.extensions_unlocked() {
            return 0xff;
        }
        match index {
            0x0a => {
                let size = if self.vram.len() == 0x20_0000 {
                    0x18
                } else {
                    0x10
                };
                (self.seq[index as usize] & !0x1a) | size
            }
            0x0f => {
                let size = if self.vram.len() == 0x20_0000 {
                    0x18
                } else {
                    0x10
                };
                (self.seq[index as usize] & !0x98) | size
            }
            0x17 => (self.seq[index as usize] & !(7 << 3)) | (7 << 3),
            _ => self.seq[index as usize],
        }
    }

    fn write_sequencer(&mut self, value: u8) {
        let raw_index = self.seq_index;
        let index = raw_index & 0x1f;
        if index == 6 {
            self.seq[6] = if value & 0x17 == 0x12 { 0x12 } else { 0x0f };
            return;
        }
        if index > 6 && !self.extensions_unlocked() {
            return;
        }
        self.seq[index as usize] = value;
        match index {
            SR_CURSOR_X => self.cursor_x = (u16::from(value) << 3) | u16::from(raw_index >> 5),
            SR_CURSOR_Y => self.cursor_y = (u16::from(value) << 3) | u16::from(raw_index >> 5),
            _ => {}
        }
        if self.diagnostic && matches!(index, 0 | 1 | 7 | 0x0b..=0x0e | 0x1b..=0x1e) {
            log::info!(
                "picasso2: SR{index:02X} <- {value:02X} mode={:?}",
                self.decode_mode()
            );
        }
    }

    fn read_graphics(&self) -> u8 {
        let index = self.gfx_index as usize;
        if index > 8 && !self.extensions_unlocked() {
            return 0xff;
        }
        if index == 0x31 {
            let mut value = self.gfx[index] & !0x09;
            if self.blit_busy() {
                value |= 0x09;
            }
            value
        } else {
            self.gfx[index]
        }
    }

    fn write_graphics(&mut self, value: u8) {
        let index = self.gfx_index as usize;
        if index > 8 && !self.extensions_unlocked() {
            return;
        }
        self.gfx[index] = value;
        if index == 0x31 {
            if value & 0x04 != 0 {
                self.system_blit = None;
                self.blit_busy_cck = 0;
                self.gfx[0x31] &= !0x0b;
            } else if value & 0x02 != 0 {
                self.start_blit();
            }
        }
    }

    fn read_crtc(&self) -> u8 {
        if self.crtc_index == 0x27 {
            self.model.part_id()
        } else if self.crtc_index > 0x18 && !self.extensions_unlocked() {
            0xff
        } else {
            self.crtc[self.crtc_index as usize]
        }
    }

    fn write_crtc(&mut self, value: u8) {
        let index = self.crtc_index as usize;
        if index > 0x18 && !self.extensions_unlocked() {
            return;
        }
        if self.crtc[0x11] & 0x80 != 0 && index <= 7 {
            if index == 7 {
                self.crtc[7] = (self.crtc[7] & !0x10) | (value & 0x10);
            }
            return;
        }
        // VGA CR11 bit 4 is active-low interrupt clear. Picasso II+ drivers
        // acknowledge the latched vertical interrupt by writing it clear.
        if index == 0x11 && value & 0x10 == 0 {
            self.vblank_pending = false;
        }
        if index != 0x27 {
            self.crtc[index] = value;
        }
        if self.diagnostic
            && matches!(
                index,
                0 | 1 | 6 | 7 | 9 | 0x0c | 0x0d | 0x12 | 0x13 | 0x1a | 0x1b
            )
        {
            log::info!(
                "picasso2: CR{index:02X} <- {value:02X} mode={:?}",
                self.decode_mode()
            );
        }
    }

    fn read_pel_mask_or_hidden_dac(&mut self) -> u8 {
        if !self.extensions_unlocked() {
            return self.pel_mask;
        }
        if self.hidden_dac_reads == 4 {
            self.hidden_dac_reads = 0;
            self.hidden_dac
        } else {
            self.hidden_dac_reads += 1;
            self.pel_mask
        }
    }

    fn write_pel_mask_or_hidden_dac(&mut self, value: u8) {
        if self.extensions_unlocked() && self.hidden_dac_reads == 4 {
            self.hidden_dac = value;
            self.hidden_dac_reads = 0;
            if self.diagnostic {
                log::info!(
                    "picasso2: hidden DAC <- {value:#04x} mode={:?}",
                    self.decode_mode()
                );
            }
        } else {
            self.pel_mask = value;
            self.hidden_dac_reads = 0;
        }
    }

    fn write_dac_data(&mut self, value: u8) {
        let index = self.dac_write_index as usize;
        let component = self.dac_component as usize;
        if self.seq[SR_CURSOR_ATTR] & 0x02 != 0 {
            self.cursor_palette[index & 0x0f][component] = value & 0x3f;
        } else {
            self.palette[index][component] = value & 0x3f;
        }
        self.dac_component += 1;
        if self.dac_component == 3 {
            self.dac_component = 0;
            self.dac_write_index = self.dac_write_index.wrapping_add(1);
        }
    }

    fn read_dac_data(&mut self) -> u8 {
        let index = self.dac_read_index as usize;
        let component = self.dac_component as usize;
        let value = if self.seq[SR_CURSOR_ATTR] & 0x02 != 0 {
            self.cursor_palette[index & 0x0f][component]
        } else {
            self.palette[index][component]
        };
        self.dac_component += 1;
        if self.dac_component == 3 {
            self.dac_component = 0;
            self.dac_read_index = self.dac_read_index.wrapping_add(1);
        }
        value
    }

    pub fn decode_mode(&self) -> Option<DecodedMode> {
        if self.seq[7] & 1 == 0 {
            return None;
        }
        let width = (usize::from(self.crtc[1]) + 1) * 8;
        let raw_height = usize::from(self.crtc[0x12])
            | (usize::from(self.crtc[7] & 0x02) << 7)
            | (usize::from(self.crtc[7] & 0x40) << 3);
        let doublescan = self.crtc[9] & 0x80 != 0;
        let height = (raw_height + 1) * if doublescan { 2 } else { 1 };
        let pitch_units = usize::from(self.crtc[0x13]) | (usize::from(self.crtc[0x1b] & 0x10) << 4);
        let pitch = pitch_units * 8;
        let start_units = usize::from(u16::from_be_bytes([self.crtc[0x0c], self.crtc[0x0d]]))
            | (usize::from(self.crtc[0x1b] & 0x01) << 16)
            | (usize::from(self.crtc[0x1b] & 0x04) << 15)
            | (usize::from(self.crtc[0x1b] & 0x08) << 15);
        let start = (start_units + usize::from((self.crtc[8] & 0x60) >> 5)) * 4;
        let depth = if self.hidden_dac & 0x80 == 0 {
            PixelDepth::Clut8
        } else if self.hidden_dac & 0x40 == 0 {
            PixelDepth::Rgb555
        } else {
            match self.hidden_dac & 0x0f {
                0 => PixelDepth::Rgb555,
                1 => PixelDepth::Rgb565,
                5 => PixelDepth::Bgr888,
                0x0f => match self.seq[7] & 0x0e {
                    0x04 => PixelDepth::Bgr888,
                    0x02 | 0x06 => PixelDepth::Rgb565,
                    _ => PixelDepth::Clut8,
                },
                _ => return None,
            }
        };
        Some(DecodedMode {
            width,
            height,
            pitch,
            start,
            depth,
            doublescan,
            interlace: self.crtc[0x1a] & 1 != 0,
        })
    }

    pub fn video_valid(&self) -> bool {
        if self.seq[0] & 0x03 != 0x03 || self.seq[1] & 0x20 != 0 {
            return false;
        }
        let Some(mode) = self.decode_mode() else {
            return false;
        };
        if !(16..=MAX_DIM).contains(&mode.width) || !(16..=MAX_DIM).contains(&mode.height) {
            return false;
        }
        let source_height = if mode.doublescan {
            mode.height / 2
        } else {
            mode.height
        };
        let row_bytes = mode.width.saturating_mul(mode.depth.bytes_per_pixel());
        if mode.pitch < row_bytes || source_height == 0 {
            return false;
        }
        mode.start
            .checked_add((source_height - 1).saturating_mul(mode.pitch))
            .and_then(|end| end.checked_add(row_bytes))
            .is_some_and(|end| end <= self.vram.len())
    }

    pub fn compose_frame(&self, out: &mut Vec<u32>) -> Option<(u32, u32)> {
        if !self.video_valid() {
            return None;
        }
        let mode = self.decode_mode()?;
        out.clear();
        out.reserve(mode.width * mode.height);
        let bpp = mode.depth.bytes_per_pixel();
        for y in 0..mode.height {
            let source_y = if mode.doublescan { y / 2 } else { y };
            let row = mode.start + source_y * mode.pitch;
            for x in 0..mode.width {
                let at = row + x * bpp;
                let (r, g, b) = match mode.depth {
                    PixelDepth::Clut8 => {
                        let p = self.palette[(self.vram[at] & self.pel_mask) as usize];
                        (dac6(p[0]), dac6(p[1]), dac6(p[2]))
                    }
                    PixelDepth::Rgb555 => {
                        let v = u16::from_le_bytes([self.vram[at], self.vram[at + 1]]);
                        (
                            expand5((v >> 10) as u8),
                            expand5((v >> 5) as u8),
                            expand5(v as u8),
                        )
                    }
                    PixelDepth::Rgb565 => {
                        let v = u16::from_le_bytes([self.vram[at], self.vram[at + 1]]);
                        (
                            expand5((v >> 11) as u8),
                            expand6((v >> 5) as u8),
                            expand5(v as u8),
                        )
                    }
                    PixelDepth::Bgr888 => (self.vram[at + 2], self.vram[at + 1], self.vram[at]),
                };
                out.push(rgba_word(r, g, b));
            }
        }
        self.composite_cursor(out, mode.width, mode.height);
        Some((mode.width as u32, mode.height as u32))
    }

    fn composite_cursor(&self, out: &mut [u32], width: usize, height: usize) {
        if self.seq[SR_CURSOR_ATTR] & 1 == 0 || self.vram.len() < 0x4000 {
            return;
        }
        let large = self.seq[SR_CURSOR_ATTR] & 4 != 0;
        let size = if large { 64 } else { 32 };
        let slot = if large {
            usize::from(self.seq[SR_CURSOR_PATTERN] & 0x3c)
        } else {
            usize::from(self.seq[SR_CURSOR_PATTERN] & 0x3f)
        };
        let base = self.vram.len() - 0x4000 + slot * 256;
        let bg = palette_word(self.cursor_palette[0]);
        let fg = palette_word(self.cursor_palette[0x0f]);
        for y in 0..size {
            let py = usize::from(self.cursor_y) + y;
            if py >= height {
                continue;
            }
            for x_byte in 0..size / 8 {
                let (plane0_at, plane1_at) = if large {
                    (base + y * 16 + x_byte, base + y * 16 + 8 + x_byte)
                } else {
                    (base + y * 4 + x_byte, base + 0x80 + y * 4 + x_byte)
                };
                if plane1_at >= self.vram.len() {
                    return;
                }
                let p0 = self.vram[plane0_at];
                let p1 = self.vram[plane1_at];
                for bit in 0..8 {
                    let px = usize::from(self.cursor_x) + x_byte * 8 + bit;
                    if px >= width {
                        continue;
                    }
                    let b0 = (p0 >> (7 - bit)) & 1;
                    let b1 = (p1 >> (7 - bit)) & 1;
                    let at = py * width + px;
                    match (b0, b1) {
                        (0, 0) => {}
                        (0, 1) => out[at] = bg,
                        (1, 0) => out[at] ^= 0x00ff_ffff,
                        (1, 1) => out[at] = fg,
                        _ => unreachable!(),
                    }
                }
            }
        }
    }

    pub fn vram_read(&self, off: usize, size: usize) -> u32 {
        if off
            .checked_add(size)
            .is_none_or(|end| end > self.vram.len())
        {
            return open_bus(size);
        }
        let mut value = 0;
        for byte in &self.vram[off..off + size] {
            value = (value << 8) | u32::from(*byte);
        }
        value
    }

    pub fn vram_write(&mut self, off: usize, size: usize, value: u32) {
        let mut bytes = [0u8; 4];
        for (i, byte) in bytes[..size.min(4)].iter_mut().enumerate() {
            *byte = (value >> (8 * (size - 1 - i))) as u8;
        }
        if self.system_blit.is_some() {
            self.feed_system_blit(&bytes[..size.min(4)]);
            return;
        }
        if off
            .checked_add(size)
            .is_none_or(|end| end > self.vram.len())
        {
            return;
        }
        for (i, byte) in bytes[..size.min(4)].iter().copied().enumerate() {
            self.write_vga_byte(off + i, byte);
        }
    }

    fn write_vga_byte(&mut self, off: usize, cpu: u8) {
        let old = self.vram[off];
        let mode = self.gfx[5] & 7;
        let plane = off & 3;
        let rotate = self.gfx[3] & 7;
        let rotated = cpu.rotate_right(u32::from(rotate));
        let set_reset = if self.gfx[1] & (1 << plane) != 0 {
            if self.gfx[0] & (1 << plane) != 0 {
                0xff
            } else {
                0
            }
        } else {
            rotated
        };
        let (source, mask) = match mode {
            0 => (set_reset, self.gfx[8]),
            1 => (old, 0xff),
            2 => (if cpu & (1 << plane) != 0 { 0xff } else { 0 }, self.gfx[8]),
            3 => {
                let mask = rotated & self.gfx[8];
                (
                    if self.gfx[0] & (1 << plane) != 0 {
                        0xff
                    } else {
                        0
                    },
                    mask,
                )
            }
            // Extended write modes 4/5 are color-expansion paths. P96 uses
            // the BitBLT engine for them; direct writes remain byte stores.
            _ => (cpu, 0xff),
        };
        let rop = match (self.gfx[3] >> 3) & 3 {
            0 => source,
            1 => source & old,
            2 => source | old,
            _ => source ^ old,
        };
        self.vram[off] = (rop & mask) | (old & !mask);
    }

    fn blit_busy(&self) -> bool {
        self.blit_busy_cck != 0 || self.system_blit.is_some()
    }

    fn start_blit(&mut self) {
        if self.gfx[0x30] & BLIT_MODE_SYSTEM_SOURCE != 0 {
            let width = self.blit_width();
            let height = self.blit_height();
            let expected = self.system_source_pitch(width) * height;
            self.system_blit = Some(SystemBlit {
                expected: expected.max(1),
                bytes: Vec::with_capacity(expected.max(1)),
            });
        } else {
            self.execute_blit(None);
            self.blit_busy_cck = BLIT_BUSY_CCK;
        }
        if self.diagnostic {
            log::info!(
                "picasso2: blit {}x{} dst={:#08x} src={:#08x} mode={:#04x} rop={:#04x}",
                self.blit_width(),
                self.blit_height(),
                self.blit_addr(0x28),
                self.blit_addr(0x2c),
                self.gfx[0x30],
                self.gfx[0x32]
            );
        }
    }

    fn feed_system_blit(&mut self, bytes: &[u8]) {
        let done = if let Some(blit) = self.system_blit.as_mut() {
            let remaining = blit.expected.saturating_sub(blit.bytes.len());
            blit.bytes
                .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
            blit.bytes.len() >= blit.expected
        } else {
            false
        };
        if done {
            let data = self.system_blit.take().map(|b| b.bytes).unwrap_or_default();
            self.execute_blit(Some(&data));
            self.blit_busy_cck = BLIT_BUSY_CCK;
        }
    }

    fn blit_width(&self) -> usize {
        ((usize::from(self.gfx[0x21] & 7) << 8) | usize::from(self.gfx[0x20])) + 1
    }

    fn blit_height(&self) -> usize {
        ((usize::from(self.gfx[0x23] & 3) << 8) | usize::from(self.gfx[0x22])) + 1
    }

    fn blit_pitch(&self, low: usize) -> usize {
        (usize::from(self.gfx[low + 1] & 0x1f) << 8) | usize::from(self.gfx[low])
    }

    fn blit_addr(&self, low: usize) -> usize {
        usize::from(self.gfx[low])
            | (usize::from(self.gfx[low + 1]) << 8)
            | (usize::from(self.gfx[low + 2] & 0x1f) << 16)
    }

    fn blit_pixel_bytes(&self) -> usize {
        match self.gfx[0x30] & BLIT_MODE_PIXEL_WIDTH {
            0x00 => 1,
            0x10 => 2,
            0x20 => 3,
            _ => 4,
        }
    }

    fn system_source_pitch(&self, width: usize) -> usize {
        if self.gfx[0x30] & BLIT_MODE_COLOR_EXPAND == 0 {
            return width.next_multiple_of(4);
        }
        let pixels = width.div_ceil(self.blit_pixel_bytes());
        let granularity = if self.gfx[0x33] & BLIT_MODEEXT_DWORD_GRANULARITY != 0 {
            32
        } else {
            8
        };
        pixels.next_multiple_of(granularity) / 8
    }

    fn blit_fg_component(&self, component: usize) -> u8 {
        self.gfx[[0x01, 0x11, 0x13, 0x15][component.min(3)]]
    }

    fn blit_bg_component(&self, component: usize) -> u8 {
        self.gfx[[0x00, 0x10, 0x12, 0x14][component.min(3)]]
    }

    fn linear_blit_source(
        &self,
        system: Option<&[u8]>,
        system_pitch: usize,
        pattern: bool,
        backwards: bool,
        src_line: usize,
        src_start: usize,
        y: usize,
        x: usize,
        pixel_bytes: usize,
    ) -> u8 {
        if pattern {
            let row_bytes = 8 * pixel_bytes;
            let pattern_base = src_start & !3;
            return self
                .vram
                .get(pattern_base + (y & 7) * row_bytes + (x % row_bytes))
                .copied()
                .unwrap_or(0);
        }
        if let Some(data) = system {
            return data.get(y * system_pitch + x).copied().unwrap_or(0);
        }
        let src = if backwards {
            src_line.checked_sub(x)
        } else {
            src_line.checked_add(x)
        };
        src.and_then(|at| self.vram.get(at).copied()).unwrap_or(0)
    }

    fn execute_blit(&mut self, system: Option<&[u8]>) {
        let width = self.blit_width().min(self.vram.len());
        let height = self.blit_height().min(MAX_DIM);
        let dst_pitch = self.blit_pitch(0x24);
        let src_pitch = self.blit_pitch(0x26);
        let dst_start = self.blit_addr(0x28);
        let src_start = self.blit_addr(0x2c);
        let mode = self.gfx[0x30];
        let backwards = mode & BLIT_MODE_BACKWARDS != 0;
        let color_expand = mode & BLIT_MODE_COLOR_EXPAND != 0;
        let pattern = mode & BLIT_MODE_PATTERN != 0;
        let pixel_bytes = self.blit_pixel_bytes();
        let system_pitch = self.system_source_pitch(width);
        let modeext = self.gfx[0x33];
        let solid_fill = pattern
            && color_expand
            && mode & BLIT_MODE_TRANSPARENT == 0
            && modeext & BLIT_MODEEXT_SOLID_FILL != 0;
        // Video-source colour expansion consumes one continuous bit stream:
        // a row that ends mid-byte rounds up to the next source byte and the
        // source pitch register is never consulted. Pattern and system-source
        // expansion address their sources per row instead.
        let mut expand_addr = src_start;
        let mut expand_count = 0usize;
        let expand_span = 8 * pixel_bytes;

        for y in 0..height {
            let dst_line = if backwards {
                dst_start.saturating_sub(y.saturating_mul(dst_pitch))
            } else {
                dst_start.saturating_add(y.saturating_mul(dst_pitch))
            };
            let src_line = if backwards {
                src_start.saturating_sub(y.saturating_mul(src_pitch))
            } else {
                src_start.saturating_add(y.saturating_mul(src_pitch))
            };
            for x in 0..width {
                let expand_bit = color_expand.then(|| {
                    let pixel = x / pixel_bytes;
                    if pattern {
                        let byte = self
                            .vram
                            .get(src_start.saturating_add(y & 7))
                            .copied()
                            .unwrap_or(0);
                        byte >> (7 - (pixel & 7)) & 1
                    } else if let Some(data) = system {
                        let byte = data.get(y * system_pitch + pixel / 8).copied().unwrap_or(0);
                        byte >> (7 - (pixel & 7)) & 1
                    } else {
                        let byte = self.vram.get(expand_addr).copied().unwrap_or(0);
                        byte >> (7 - expand_count / pixel_bytes) & 1
                    }
                });
                if color_expand && !pattern && system.is_none() {
                    expand_count += 1;
                    if expand_count == expand_span {
                        expand_count = 0;
                        expand_addr = if backwards {
                            expand_addr.wrapping_sub(1)
                        } else {
                            expand_addr.wrapping_add(1)
                        };
                    }
                }
                let dst = if backwards {
                    dst_line.checked_sub(x)
                } else {
                    dst_line.checked_add(x)
                };
                let Some(dst) = dst.filter(|at| *at < self.vram.len()) else {
                    continue;
                };
                let component = x % pixel_bytes;
                let source = if solid_fill {
                    self.blit_fg_component(component)
                } else if let Some(bit) = expand_bit {
                    if bit != 0 {
                        self.blit_fg_component(component)
                    } else {
                        self.blit_bg_component(component)
                    }
                } else {
                    self.linear_blit_source(
                        system,
                        system_pitch,
                        pattern,
                        backwards,
                        src_line,
                        src_start,
                        y,
                        x,
                        pixel_bytes,
                    )
                };

                let transparent = if mode & BLIT_MODE_TRANSPARENT == 0 {
                    false
                } else if let Some(bit) = expand_bit {
                    if modeext & BLIT_MODEEXT_COLOR_EXPAND_INVERT != 0 {
                        bit != 0
                    } else {
                        bit == 0
                    }
                } else if pattern {
                    false
                } else {
                    let pixel_base = x - component;
                    (0..pixel_bytes).all(|c| {
                        let source = self.linear_blit_source(
                            system,
                            system_pitch,
                            false,
                            backwards,
                            src_line,
                            src_start,
                            y,
                            pixel_base + c,
                            pixel_bytes,
                        );
                        let mask = self.gfx[0x38 + c];
                        (source & !mask) == (self.gfx[0x34 + c] & !mask)
                    })
                };
                if transparent {
                    continue;
                }
                let dest = self.vram[dst];
                let result = apply_rop(self.gfx[0x32], source, dest);
                self.vram[dst] = result;
            }
            if color_expand && !pattern && system.is_none() && expand_count != 0 {
                expand_count = 0;
                expand_addr = if backwards {
                    expand_addr.wrapping_sub(1)
                } else {
                    expand_addr.wrapping_add(1)
                };
            }
        }
    }

    pub fn tick(&mut self, cck: u32) {
        let old_phase = self.frame_phase_cck;
        self.frame_phase_cck = self.frame_phase_cck.wrapping_add(u64::from(cck));
        if self.model == CirrusModel::Gd5428
            && self.vertical_interrupt_enabled()
            && self.crossed_retrace_start(old_phase, self.frame_phase_cck)
        {
            self.vblank_pending = true;
        }
        self.blit_busy_cck = self.blit_busy_cck.saturating_sub(cck);
        if self.blit_busy_cck == 0 && self.system_blit.is_none() {
            self.gfx[0x31] &= !0x0b;
        }
    }

    pub fn is_idle(&self) -> bool {
        self.blit_busy_cck == 0 && self.system_blit.is_none() && self.decode_mode().is_none()
    }

    pub fn next_event_cck(&self) -> Option<u32> {
        let blit = (self.blit_busy_cck != 0).then_some(self.blit_busy_cck);
        let video = self
            .decode_mode()
            .is_some()
            .then(|| self.cck_until_retrace_start());
        match (blit, video) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(event), None) | (None, Some(event)) => Some(event),
            (None, None) => None,
        }
    }

    pub fn vblank_pending(&self) -> bool {
        self.vblank_pending
    }

    fn dot_clock_hz(&self) -> f64 {
        let clock = usize::from((self.misc_output >> 2) & 3);
        let n = f64::from(self.seq[0x0b + clock] & 0x7f);
        let denom_reg = self.seq[0x1b + clock];
        let d = f64::from((denom_reg & 0x3e) >> 1);
        let post = if denom_reg & 1 != 0 { 2.0 } else { 1.0 };
        let mut hz = if n == 0.0 || d == 0.0 {
            if clock == 0 {
                25_175_000.0
            } else {
                28_322_000.0
            }
        } else {
            14_318_184.0 * n / (d * post)
        };
        match self.seq[7] & 0x06 {
            0x02 => hz /= 2.0,
            0x04 => hz /= 3.0,
            _ => {}
        }
        hz.max(1.0)
    }

    fn horizontal_total_pixels(&self) -> u64 {
        u64::from(self.crtc[0]).saturating_add(5) * 8
    }

    fn vertical_total_lines(&self) -> u64 {
        let total = u64::from(self.crtc[6])
            | (u64::from(self.crtc[7] & 0x01) << 8)
            | (u64::from(self.crtc[7] & 0x20) << 4);
        total + 2
    }

    fn frame_cck(&self) -> u64 {
        let pixels = self
            .horizontal_total_pixels()
            .saturating_mul(self.vertical_total_lines());
        ((pixels as f64 * f64::from(PAULA_CLOCK_HZ) / self.dot_clock_hz()).round() as u64).max(1)
    }

    fn retrace_start_cck(&self) -> u64 {
        let total_lines = self.vertical_total_lines().max(1);
        let line_cck = (self.frame_cck() / total_lines).max(1);
        self.retrace_start_line().min(total_lines - 1) * line_cck
    }

    fn retrace_start_line(&self) -> u64 {
        u64::from(self.crtc[0x10])
            | (u64::from(self.crtc[7] & 0x04) << 6)
            | (u64::from(self.crtc[7] & 0x80) << 2)
    }

    fn crossed_retrace_start(&self, old: u64, new: u64) -> bool {
        let frame = u128::from(self.frame_cck().max(1));
        let phase = u128::from(self.retrace_start_cck()).min(frame - 1);
        let elapsed = u128::from(new.wrapping_sub(old));
        let old = u128::from(old);
        let new = old + elapsed;
        let base = old - old % frame;
        let mut next = base + phase;
        if next <= old {
            next += frame;
        }
        next <= new
    }

    fn cck_until_retrace_start(&self) -> u32 {
        let frame = self.frame_cck().max(1);
        let phase = self.frame_phase_cck % frame;
        let retrace = self.retrace_start_cck().min(frame - 1);
        let distance = if phase < retrace {
            retrace - phase
        } else {
            frame - phase + retrace
        };
        distance.max(1).min(u64::from(u32::MAX)) as u32
    }

    fn vertical_interrupt_enabled(&self) -> bool {
        self.crtc[0x11] & 0x30 == 0x10 && self.gfx[0x17] & 0x04 == 0
    }

    fn input_status_0(&self) -> u8 {
        0x10 | if self.vblank_pending && self.vertical_interrupt_enabled() {
            0x80
        } else {
            0
        }
    }

    fn input_status_1(&self) -> u8 {
        let frame = self.frame_cck();
        let phase = self.frame_phase_cck % frame;
        let line_cck = (frame / self.vertical_total_lines().max(1)).max(1);
        let line = phase / line_cck;
        let retrace_start = self.retrace_start_line();
        let retrace_len =
            u64::from((self.crtc[0x11] & 0x0f).wrapping_sub(self.crtc[0x10] & 0x0f) & 0x0f).max(1);
        let in_retrace = line >= retrace_start && line < retrace_start.saturating_add(retrace_len);
        let mode = self.decode_mode();
        let display_lines =
            mode.map_or(0, |m| if m.doublescan { m.height / 2 } else { m.height }) as u64;
        let in_display = line < display_lines;
        (if in_retrace { 0x08 } else { 0 }) | if in_display { 0 } else { 0x01 }
    }
}

fn dac6(value: u8) -> u8 {
    let value = value & 0x3f;
    (value << 2) | (value >> 4)
}

fn expand5(value: u8) -> u8 {
    let value = value & 0x1f;
    (value << 3) | (value >> 2)
}

fn expand6(value: u8) -> u8 {
    let value = value & 0x3f;
    (value << 2) | (value >> 4)
}

fn rgba_word(r: u8, g: u8, b: u8) -> u32 {
    0xff00_0000 | (u32::from(b) << 16) | (u32::from(g) << 8) | u32::from(r)
}

fn palette_word(rgb: [u8; 3]) -> u32 {
    rgba_word(dac6(rgb[0]), dac6(rgb[1]), dac6(rgb[2]))
}

fn open_bus(size: usize) -> u32 {
    match size {
        1 => 0xff,
        2 => 0xffff,
        4 => 0xffff_ffff,
        _ => 0,
    }
}

/// The CL-GD5426/5428 implement the 16 two-operand Boolean functions using this
/// sparse set of ROP encodings. Pattern operations feed their pattern byte as
/// `source`, so the same table covers the S and P rows in the data book.
///
/// `0x90` and `0xda` are easy to transpose: `0x90` is NOR (`~src & ~dst`) and
/// `0xda` is NAND (`~src | ~dst`), not the other way round.
fn apply_rop(rop: u8, source: u8, dest: u8) -> u8 {
    match rop {
        0x00 => 0,
        0x05 => source & dest,
        0x06 => dest,
        0x09 => source & !dest,
        0x0b => !dest,
        0x0d => source,
        0x0e => 0xff,
        0x50 => !source & dest,
        0x59 => source ^ dest,
        0x6d => source | dest,
        0x90 => !source & !dest,
        0x95 => !(source ^ dest),
        0xad => source | !dest,
        0xd0 => !source,
        0xd6 => !source | dest,
        0xda => !source | !dest,
        _ => source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unlock(chip: &mut CirrusGd5426) {
        chip.io_write(0x3c4, 6);
        chip.io_write(0x3c5, 0x12);
    }

    fn seq(chip: &mut CirrusGd5426, index: u8, value: u8) {
        chip.io_write(0x3c4, index);
        chip.io_write(0x3c5, value);
    }

    fn crtc(chip: &mut CirrusGd5426, index: u8, value: u8) {
        chip.io_write(0x3d4, index);
        chip.io_write(0x3d5, value);
    }

    fn gfx(chip: &mut CirrusGd5426, index: u8, value: u8) {
        chip.io_write(0x3ce, index);
        chip.io_write(0x3cf, value);
    }

    fn blit_registers(
        chip: &mut CirrusGd5426,
        width: usize,
        height: usize,
        dst_pitch: usize,
        src_pitch: usize,
        dst: usize,
        src: usize,
        mode: u8,
    ) {
        for (index, value) in [
            (0x20, (width - 1) as u8),
            (0x21, ((width - 1) >> 8) as u8),
            (0x22, (height - 1) as u8),
            (0x23, ((height - 1) >> 8) as u8),
            (0x24, dst_pitch as u8),
            (0x25, (dst_pitch >> 8) as u8),
            (0x26, src_pitch as u8),
            (0x27, (src_pitch >> 8) as u8),
            (0x28, dst as u8),
            (0x29, (dst >> 8) as u8),
            (0x2a, (dst >> 16) as u8),
            (0x2c, src as u8),
            (0x2d, (src >> 8) as u8),
            (0x2e, (src >> 16) as u8),
            (0x30, mode),
            (0x32, 0x0d),
        ] {
            gfx(chip, index, value);
        }
    }

    fn start_blit(chip: &mut CirrusGd5426) {
        gfx(chip, 0x31, 0x02);
    }

    fn mode(chip: &mut CirrusGd5426, width: usize, height: usize, pitch: usize) {
        unlock(chip);
        seq(chip, 0, 3);
        seq(chip, 1, 0);
        seq(chip, 7, 1);
        crtc(chip, 1, (width / 8 - 1) as u8);
        crtc(chip, 0x12, (height - 1) as u8);
        let overflow = (if (height - 1) & 0x100 != 0 { 0x02 } else { 0 })
            | (if (height - 1) & 0x200 != 0 { 0x40 } else { 0 });
        crtc(chip, 7, overflow);
        crtc(chip, 0x13, (pitch / 8) as u8);
    }

    #[test]
    fn extension_lock_and_hidden_dac_protocol() {
        let mut chip = CirrusGd5426::new(0x20_0000);
        chip.io_write(0x3c4, 7);
        chip.io_write(0x3c5, 1);
        assert_eq!(chip.io_read(0x3c5), 0xff);
        unlock(&mut chip);
        assert_eq!(chip.io_read(0x3c5), 0x12);
        for _ in 0..4 {
            assert_eq!(chip.io_read(0x3c6), 0xff);
        }
        chip.io_write(0x3c6, 0xc1);
        for _ in 0..4 {
            assert_eq!(chip.io_read(0x3c6), 0xff);
        }
        assert_eq!(chip.io_read(0x3c6), 0xc1);

        // Any non-$3C6 access abandons a partially armed sequence.
        assert_eq!(chip.io_read(0x3c6), 0xff);
        assert_eq!(chip.io_read(0x3c6), 0xff);
        let _ = chip.io_read(0x3cc);
        for _ in 0..4 {
            assert_eq!(chip.io_read(0x3c6), 0xff);
        }
        assert_eq!(chip.io_read(0x3c6), 0xc1);
    }

    #[test]
    fn captured_640x480_mode_decodes_geometry_pitch_and_pan() {
        let mut chip = CirrusGd5426::new(0x20_0000);
        mode(&mut chip, 640, 480, 640);
        crtc(&mut chip, 0x0c, 0x23);
        crtc(&mut chip, 0x0d, 0x45);
        crtc(&mut chip, 0x1b, 0x01);
        assert_eq!(
            chip.decode_mode(),
            Some(DecodedMode {
                width: 640,
                height: 480,
                pitch: 640,
                start: 0x12345 * 4,
                depth: PixelDepth::Clut8,
                doublescan: false,
                interlace: false,
            })
        );
        assert!(chip.video_valid());
    }

    #[test]
    fn scanout_decodes_all_picasso_pixel_formats() {
        let mut chip = CirrusGd5426::new(0x20_0000);
        mode(&mut chip, 16, 16, 48);
        chip.palette[1] = [0x3f, 0, 0];
        chip.vram[0] = 1;
        let mut out = Vec::new();
        assert_eq!(chip.compose_frame(&mut out), Some((16, 16)));
        assert_eq!(out[0], rgba_word(0xff, 0, 0));

        chip.hidden_dac = 0xc0;
        chip.vram[0..2].copy_from_slice(&0x7c00u16.to_le_bytes());
        assert_eq!(chip.compose_frame(&mut out), Some((16, 16)));
        assert_eq!(out[0], rgba_word(0xff, 0, 0));

        chip.hidden_dac = 0xc1;
        chip.vram[0..2].copy_from_slice(&0x07e0u16.to_le_bytes());
        assert_eq!(chip.compose_frame(&mut out), Some((16, 16)));
        assert_eq!(out[0], rgba_word(0, 0xff, 0));

        chip.hidden_dac = 0xc5;
        chip.vram[0..3].copy_from_slice(&[0x20, 0x40, 0x80]);
        assert_eq!(chip.compose_frame(&mut out), Some((16, 16)));
        assert_eq!(out[0], rgba_word(0x80, 0x40, 0x20));
    }

    #[test]
    fn blitter_copies_overlap_in_both_directions_and_reports_busy() {
        let mut chip = CirrusGd5426::new(0x10_0000);
        unlock(&mut chip);
        chip.vram[0..4].copy_from_slice(&[1, 2, 3, 4]);
        blit_registers(&mut chip, 4, 1, 8, 8, 8, 0, 0);
        start_blit(&mut chip);
        assert_eq!(&chip.vram[8..12], &[1, 2, 3, 4]);
        gfx(&mut chip, 0x31, 0);
        assert_ne!(chip.io_read(0x3cf) & 0x09, 0);
        chip.tick(BLIT_BUSY_CCK - 1);
        assert_ne!(chip.io_read(0x3cf) & 0x09, 0);
        chip.tick(1);
        assert_eq!(chip.io_read(0x3cf) & 0x09, 0);

        chip.vram[0..8].copy_from_slice(&[10, 11, 12, 13, 14, 15, 16, 17]);
        // Backwards starts at the bottom-right byte. This overlapping move is
        // the direction P96 uses when scrolling toward higher addresses.
        blit_registers(&mut chip, 6, 1, 8, 8, 7, 5, BLIT_MODE_BACKWARDS);
        start_blit(&mut chip);
        assert_eq!(&chip.vram[2..8], &[10, 11, 12, 13, 14, 15]);
    }

    #[test]
    fn blitter_solid_fill_handles_truecolor_components() {
        let mut chip = CirrusGd5426::new(0x10_0000);
        unlock(&mut chip);
        gfx(&mut chip, 0x01, 0x11);
        gfx(&mut chip, 0x11, 0x22);
        gfx(&mut chip, 0x13, 0x33);
        gfx(&mut chip, 0x33, BLIT_MODEEXT_SOLID_FILL);
        blit_registers(
            &mut chip,
            6,
            1,
            6,
            0,
            0x100,
            0,
            BLIT_MODE_PATTERN | BLIT_MODE_COLOR_EXPAND | 0x20,
        );
        start_blit(&mut chip);
        assert_eq!(
            &chip.vram[0x100..0x106],
            &[0x11, 0x22, 0x33, 0x11, 0x22, 0x33]
        );
    }

    #[test]
    fn blitter_system_color_expand_and_source_transparency() {
        let mut chip = CirrusGd5426::new(0x10_0000);
        unlock(&mut chip);
        gfx(&mut chip, 0x00, 0x10);
        gfx(&mut chip, 0x01, 0xe0);
        blit_registers(
            &mut chip,
            8,
            1,
            8,
            0,
            0x100,
            0,
            BLIT_MODE_SYSTEM_SOURCE | BLIT_MODE_COLOR_EXPAND,
        );
        start_blit(&mut chip);
        chip.vram_write(0, 1, 0xa0);
        assert_eq!(
            &chip.vram[0x100..0x108],
            &[0xe0, 0x10, 0xe0, 0x10, 0x10, 0x10, 0x10, 0x10]
        );

        chip.vram[0x20..0x22].copy_from_slice(&[0, 0x7a]);
        chip.vram[0x120..0x122].copy_from_slice(&[0x55, 0x55]);
        gfx(&mut chip, 0x34, 0);
        gfx(&mut chip, 0x38, 0);
        blit_registers(&mut chip, 2, 1, 2, 2, 0x120, 0x20, BLIT_MODE_TRANSPARENT);
        start_blit(&mut chip);
        assert_eq!(&chip.vram[0x120..0x122], &[0x55, 0x7a]);
    }

    #[test]
    fn cursor_planes_decode_background_xor_foreground_and_clip() {
        let mut chip = CirrusGd5426::new(0x10_0000);
        mode(&mut chip, 16, 16, 16);
        chip.cursor_palette[0] = [0x3f, 0, 0];
        chip.cursor_palette[0x0f] = [0, 0x3f, 0];
        chip.cursor_x = 14;
        chip.cursor_y = 0;
        seq(&mut chip, 0x12, 1);
        let base = chip.vram.len() - 0x4000;
        chip.vram[base] = 0x60;
        chip.vram[base + 0x80] = 0xa0;
        let mut out = Vec::new();
        assert_eq!(chip.compose_frame(&mut out), Some((16, 16)));
        assert_eq!(out[14], rgba_word(0xff, 0, 0));
        assert_eq!(out[15], rgba_word(0xff, 0xff, 0xff));
    }

    #[test]
    fn input_status_tracks_programmed_vertical_retrace_in_emulated_time() {
        let mut chip = CirrusGd5426::new(0x10_0000);
        mode(&mut chip, 16, 16, 16);
        crtc(&mut chip, 0, 15);
        crtc(&mut chip, 6, 20);
        crtc(&mut chip, 0x10, 16);
        crtc(&mut chip, 0x11, 2);
        assert_eq!(chip.io_read(0x3da) & 0x08, 0);
        let line_cck = chip.frame_cck() / chip.vertical_total_lines();
        chip.tick((line_cck * 16) as u32);
        assert_ne!(chip.io_read(0x3da) & 0x08, 0);
        chip.tick((line_cck * 2) as u32);
        assert_eq!(chip.io_read(0x3da) & 0x08, 0);
    }

    #[test]
    fn controller_revision_selects_the_cr27_part_id() {
        let mut gd5426 = CirrusGd5426::new(0x10_0000);
        let mut gd5428 = CirrusGd5426::new_gd5428(0x10_0000);
        for (chip, expected) in [(&mut gd5426, PART_ID_GD5426), (&mut gd5428, PART_ID_GD5428)] {
            chip.io_write(0x3d4, 0x27);
            assert_eq!(chip.io_read(0x3d5), expected);
        }
    }

    #[test]
    fn gd5428_vblank_latch_obeys_vga_enable_and_ack() {
        let mut chip = CirrusGd5426::new_gd5428(0x10_0000);
        mode(&mut chip, 16, 16, 16);
        crtc(&mut chip, 0, 15);
        crtc(&mut chip, 6, 20);
        crtc(&mut chip, 0x10, 16);
        crtc(&mut chip, 0x11, 0x12);
        assert!(!chip.vblank_pending());
        assert_eq!(chip.io_read(0x3c2), 0x10);

        let line_cck = chip.frame_cck() / chip.vertical_total_lines();
        assert_eq!(chip.next_event_cck(), Some((line_cck * 16) as u32));
        chip.tick((line_cck * 16) as u32);
        assert!(chip.vblank_pending());
        assert_eq!(chip.io_read(0x3c2), 0x90);

        crtc(&mut chip, 0x11, 0x02);
        assert!(!chip.vblank_pending());
        assert_eq!(chip.io_read(0x3c2), 0x10);

        chip.tick(chip.frame_cck() as u32);
        assert!(
            !chip.vblank_pending(),
            "CR11 bit 4 clear keeps IRQ disarmed"
        );
    }

    #[test]
    fn serde_resumes_mid_hidden_dac_sequence_and_system_blit() {
        let mut original = CirrusGd5426::new(0x10_0000);
        unlock(&mut original);
        original.hidden_dac = 0xc1;
        blit_registers(&mut original, 1, 1, 1, 0, 0x100, 0, BLIT_MODE_SYSTEM_SOURCE);
        start_blit(&mut original);
        original.vram_write(0, 2, 0x7a00);
        assert_eq!(original.io_read(0x3c6), 0xff);
        assert_eq!(original.io_read(0x3c6), 0xff);

        let bytes = bincode::serialize(&original).unwrap();
        let mut resumed: CirrusGd5426 = bincode::deserialize(&bytes).unwrap();
        for chip in [&mut original, &mut resumed] {
            assert_eq!(chip.io_read(0x3c6), 0xff);
            assert_eq!(chip.io_read(0x3c6), 0xff);
            assert_eq!(chip.io_read(0x3c6), 0xc1);
            chip.vram_write(0, 2, 0);
            assert_eq!(chip.vram[0x100], 0x7a);
            chip.tick(BLIT_BUSY_CCK);
            assert!(!chip.blit_busy());
        }
        assert_eq!(
            bincode::serialize(&original).unwrap(),
            bincode::serialize(&resumed).unwrap()
        );
    }

    #[test]
    fn every_documented_rop_matches_boolean_reference() {
        let rops = [
            0x00, 0x05, 0x06, 0x09, 0x0b, 0x0d, 0x0e, 0x50, 0x59, 0x6d, 0x90, 0x95, 0xad, 0xd0,
            0xd6, 0xda,
        ];
        for rop in rops {
            for source in [0x00, 0x35, 0xaa, 0xff] {
                for dest in [0x00, 0x53, 0xaa, 0xff] {
                    // The Cirrus encoding is sparse rather than a compact
                    // four-bit truth table, so spell the documented Boolean
                    // operation out independently of `apply_rop`. NOR and NAND
                    // are written as the negated OR and AND the data book names
                    // them by, since transposing the two is the mistake this
                    // whole test exists to catch.
                    let expect = match rop {
                        0x00 => 0,
                        0x05 => source & dest,
                        0x06 => dest,
                        0x09 => source & !dest,
                        0x0b => !dest,
                        0x0d => source,
                        0x0e => 0xff,
                        0x50 => !source & dest,
                        0x59 => source ^ dest,
                        0x6d => source | dest,
                        0x90 => !(source | dest),
                        0x95 => !(source ^ dest),
                        0xad => source | !dest,
                        0xd0 => !source,
                        0xd6 => !source | dest,
                        0xda => !(source & dest),
                        _ => unreachable!(),
                    };
                    assert_eq!(apply_rop(rop, source, dest), expect, "rop {rop:#04x}");
                }
            }
        }
    }
}
