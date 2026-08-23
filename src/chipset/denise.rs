// SPDX-License-Identifier: GPL-3.0-or-later

//! Denise state: palette, bitplane/sprite control, display window,
//! display data fetch, and modulos.

pub const BPLCON3_PF2OF_DEFAULT: u16 = 0x0C00;
pub const COLOR_RGB_MASK: u16 = 0x0FFF;
pub const COLOR_TRANSPARENCY_BIT: u16 = 0x8000;
pub const COLOR_REGISTER_MASK: u16 = COLOR_TRANSPARENCY_BIT | COLOR_RGB_MASK;
pub const CLXCON_RESET: u16 = 0x0FFF;

/// Which Denise is socketed. Real machines mixed generations (late A500s
/// shipped an ECS Agnus with an OCS Denise), so this is independent of
/// `AgnusRevision`; the `[chipset] revision` presets pick matching pairs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DeniseRevision {
    #[default]
    Ocs,
    Ecs8373,
    /// AGA Lisa (4203).
    AgaLisa,
}

impl DeniseRevision {
    /// ECS Denise behaviour (DIWHIGH, BPLCON3, SuperHires) is a subset of
    /// Lisa's, so AGA reports true here.
    pub fn is_ecs(self) -> bool {
        !matches!(self, Self::Ocs)
    }

    /// DENISEID ($DFF07C). OCS Denise (8362) does not drive the register
    /// (the bus floats); ECS Denise (8373) drives $FFFC; Lisa drives $00F8.
    pub fn id(self) -> Option<u16> {
        match self {
            Self::Ocs => None,
            Self::Ecs8373 => Some(0xFFFC),
            Self::AgaLisa => Some(0x00F8),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DiwHigh {
    #[default]
    OcsImplicit,
    EcsExplicit(u16),
}

impl DiwHigh {
    pub const fn ocs_implicit() -> Self {
        Self::OcsImplicit
    }

    pub const fn ecs_explicit(bits: u16) -> Self {
        Self::EcsExplicit(bits)
    }

    pub fn v_start(self, diwstrt: u16) -> u16 {
        let low = (diwstrt >> 8) & 0x00FF;
        match self {
            Self::OcsImplicit => low,
            Self::EcsExplicit(bits) => low | ((bits & 0x0007) << 8),
        }
    }

    pub fn v_stop(self, diwstop: u16) -> u16 {
        let low = (diwstop >> 8) & 0x00FF;
        match self {
            Self::OcsImplicit => low | (u16::from(low < 0x80) << 8),
            Self::EcsExplicit(bits) => low | (((bits >> 8) & 0x0007) << 8),
        }
    }

    pub fn h_start(self, diwstrt: u16) -> u16 {
        let low = diwstrt & 0x00FF;
        match self {
            Self::OcsImplicit => low,
            Self::EcsExplicit(bits) => low | (((bits >> 5) & 0x0001) << 8),
        }
    }

    pub fn h_stop(self, diwstop: u16) -> u16 {
        let low = diwstop & 0x00FF;
        match self {
            Self::OcsImplicit => low | 0x0100,
            Self::EcsExplicit(bits) => low | (((bits >> 13) & 0x0001) << 8),
        }
    }
}

/// Display palette store, generation-agnostic. AGA's 256 colour
/// entries each hold 24 bits plus the genlock T bit, written as two 12-bit
/// halves: `hi` is the OCS-layout word (bit 15 = T, low 12 bits = the high
/// colour nibbles as $0RGB) and `lo` carries the low nibbles (written by AGA
/// LOCT=1 writes). OCS/ECS software only ever addresses bank 0 (entries
/// 0..32) with LOCT clear, which keeps hi == lo, and the OCS/ECS render path
/// consumes the 12-bit `ocs_view`, so pre-AGA behaviour is unchanged.
pub const PALETTE_ENTRIES: usize = 256;
/// AGA BPLCON2.RDRAM: enable COLORxx palette readback and make the palette
/// register window read-only while set.
pub const BPLCON2_RDRAM: u16 = 1 << 8;

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Palette {
    #[serde(with = "serde_big_array::BigArray")]
    hi: [u16; PALETTE_ENTRIES],
    #[serde(with = "serde_big_array::BigArray")]
    lo: [u16; PALETTE_ENTRIES],
}

impl std::fmt::Debug for Palette {
    /// Compact: bank 0's high words only (the classic COLORxx view), which
    /// is what pre-AGA software and the frame-state logger care about.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Palette(bank0 hi: {:04X?})", &self.hi[..32])
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Index<usize> for Palette {
    type Output = u16;

    /// Reads see the OCS-layout high word, exactly the pre-AGA COLORxx
    /// latch value.
    fn index(&self, idx: usize) -> &u16 {
        &self.hi[idx]
    }
}

impl Palette {
    pub fn new() -> Self {
        Self {
            hi: [0; PALETTE_ENTRIES],
            lo: [0; PALETTE_ENTRIES],
        }
    }

    /// Build a palette from 32 OCS-layout COLORxx words (test fixtures and
    /// snapshot reconstruction): every word lands on both nibble planes.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_ocs(words: [u16; 32]) -> Self {
        let mut palette = Self::new();
        for (idx, word) in words.iter().enumerate() {
            palette.write_ocs(idx, *word);
        }
        palette
    }

    /// OCS/ECS COLORxx write: a 12-bit write with LOCT clear sets both
    /// nibble planes (AGA-compatible upward expansion of the 12-bit value).
    pub fn write_ocs(&mut self, idx: usize, value: u16) {
        self.write_banked(0, idx, false, value);
    }

    /// AGA write path: BPLCON3 BANK selects one of 8 banks of 32; LOCT=0
    /// writes both nibble planes, LOCT=1 only the low nibbles.
    pub fn write_banked(&mut self, bank: usize, idx: usize, loct: bool, value: u16) {
        self.write_entry((bank & 7) * 32 + (idx & 31), loct, value);
    }

    /// AGA COLORxx readback: BANK selects the 32-entry block and LOCT selects
    /// the high or low component nibbles exactly as on a palette write.
    pub fn read_banked(&self, bank: usize, idx: usize, loct: bool) -> u16 {
        let entry = (bank & 7) * 32 + (idx & 31);
        if loct {
            self.lo[entry]
        } else {
            self.hi[entry]
        }
    }

    /// Absolute-entry variant of `write_banked`, used by the replay
    /// renderer's palette diffs.
    pub fn write_entry(&mut self, entry: usize, loct: bool, value: u16) {
        let entry = entry & (PALETTE_ENTRIES - 1);
        let mut view = PaletteEntry {
            hi: self.hi[entry],
            lo: self.lo[entry],
        };
        view.write(loct, value);
        self.hi[entry] = view.hi;
        self.lo[entry] = view.lo;
    }

    /// One entry's nibble planes, for callers that sample a single colour
    /// without materializing a whole palette.
    pub fn entry(&self, entry: usize) -> PaletteEntry {
        let entry = entry & (PALETTE_ENTRIES - 1);
        PaletteEntry {
            hi: self.hi[entry],
            lo: self.lo[entry],
        }
    }

    /// BPLCON3 palette addressing: BANK in bits 13-15, LOCT in bit 9.
    pub fn bank_from_bplcon3(bplcon3: u16) -> usize {
        usize::from((bplcon3 >> 13) & 7)
    }

    pub fn loct_from_bplcon3(bplcon3: u16) -> bool {
        bplcon3 & 0x0200 != 0
    }

    pub fn len(&self) -> usize {
        PALETTE_ENTRIES
    }

    #[allow(dead_code)] // clippy len-without-is-empty companion
    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn get(&self, idx: usize) -> Option<&u16> {
        self.hi.get(idx)
    }

    /// The high (OCS-layout) words of every entry, for slicing.
    pub fn hi_words(&self) -> &[u16; PALETTE_ENTRIES] {
        &self.hi
    }

    /// Full 24-bit colour of an entry (for the AGA output path): bit 31 is
    /// the genlock T bit, low 24 bits are RRGGBB composed from the high and
    /// low nibble planes.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn rgb24(&self, entry: usize) -> u32 {
        self.entry(entry).rgb24()
    }
}

/// One [`Palette`] entry's nibble planes, sampled and written with the same
/// semantics as the full palette. Lets per-pixel colour resolution work on
/// four bytes instead of copying the 1KB palette.
#[derive(Clone, Copy)]
pub struct PaletteEntry {
    hi: u16,
    lo: u16,
}

impl PaletteEntry {
    /// The OCS-layout high word: what indexing the full palette reads.
    pub fn latch(self) -> u16 {
        self.hi
    }

    /// Same masking as [`Palette::write_entry`], applied to this entry.
    pub fn write(&mut self, loct: bool, value: u16) {
        if loct {
            self.lo = value & COLOR_RGB_MASK;
        } else {
            self.hi = value & COLOR_REGISTER_MASK;
            self.lo = value & COLOR_RGB_MASK;
        }
    }

    /// Same composition as [`Palette::rgb24`], for this entry.
    pub fn rgb24(self) -> u32 {
        let hi = u32::from(self.hi);
        let lo = u32::from(self.lo);
        let t = (hi & u32::from(COLOR_TRANSPARENCY_BIT)) << 16;
        let r = ((hi >> 8) & 0xF) << 20 | ((lo >> 8) & 0xF) << 16;
        let g = ((hi >> 4) & 0xF) << 12 | ((lo >> 4) & 0xF) << 8;
        let b = (hi & 0xF) << 4 | (lo & 0xF);
        t | r | g | b
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Denise {
    pub palette: Palette,
    pub bplcon0: u16,
    pub bplcon1: u16,
    pub bplcon2: u16,
    pub bplcon3: u16,
    /// AGA BPLCON4 ($10C): BPLAM bitplane XOR mask (high byte) and the
    /// OSPRM/ESPRM sprite palette offsets (low byte). Lisa-gated; both
    /// fields are consumed by the render replay, on the separate Lisa
    /// timing paths described in docs/internals/chipset.md.
    pub bplcon4: u16,
    /// AGA CLXCON2 ($10E): collision enable/match bits for planes 7-8.
    /// Lisa-gated. Interpreted by both collision decodes (rendered and
    /// beam-timed live); a zero CLXCON2 leaves the classic six-plane
    /// behaviour untouched.
    pub clxcon2: u16,
    pub clxcon: u16,
    pub clxdat: u16,
    pub bplpt: [u32; 8],
    pub bpldat: [u16; 8],
    pub sprpt: [u32; 8],
    /// SPRxPOS/CTL/DATA/DATB as CPU/Copper writes left them, with `spr_armed`
    /// tracking those writes only (DATA arms, CTL disarms). The render
    /// replay's manual-sprite model is calibrated against this
    /// write-shadow view; sprite DMA fetches do not land here.
    pub sprpos: [u16; 8],
    pub sprctl: [u16; 8],
    pub sprdata: [u16; 8],
    pub sprdatb: [u16; 8],
    pub spr_armed: [bool; 8],
    /// The same registers as the hardware holds them: last-writer-wins
    /// across CPU/Copper writes AND sprite DMA fetches (a DMA DATA fetch
    /// arms the display latch, a DMA CTL fetch -- vstop control words or
    /// the 0/0 list terminator -- disarms it). Feeds the armed-latch
    /// redisplay in frames with sprite DMA idle: software relies on the
    /// terminator's CTL to silence a channel for good, so a later bare
    /// SPRxDATA write redisplays the DMA-written words, not the last
    /// manual pattern (Hamazing scene-switch regression class).
    pub spr_hw_pos: [u16; 8],
    pub spr_hw_ctl: [u16; 8],
    pub spr_hw_data: [u16; 8],
    pub spr_hw_datb: [u16; 8],
    pub spr_hw_armed: [bool; 8],
    pub bpl1mod: i16,
    pub bpl2mod: i16,
    pub diwstrt: u16,
    pub diwstop: u16,
    pub diwhigh: u16,
    pub diwhigh_written: bool,
    pub ddfstrt: u16,
    pub ddfstop: u16,
    dma_addr_mask: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitplaneMode {
    display_planes: usize,
    dma_planes: usize,
}

impl BitplaneMode {
    /// Decode BPLCON0's plane count. `aga` selects the Lisa/Alice decode:
    /// BPU3 (bit 4) requests 8 bitplanes (overriding BPU2-0, matching
    /// Lisa), any resolution can display all 8 (fetch bandwidth permitting,
    /// which the DMA planner enforces separately), and the OCS lowres BPU=7
    /// overfetch quirk does not apply.
    pub fn from_bplcon0(bplcon0: u16, aga: bool) -> Self {
        let code = ((bplcon0 >> 12) & 0x0007) as usize;
        let shres = bplcon0 & 0x0040 != 0;
        let hires = bplcon0 & 0x8000 != 0 && !shres;
        if aga {
            let planes = if bplcon0 & 0x0010 != 0 { 8 } else { code };
            return Self {
                display_planes: planes,
                dma_planes: planes,
            };
        }
        let display_planes = if shres { code.min(2) } else { code.min(6) };
        // Agnus's fetch-unit table schedules at most 4 plane streams in
        // hi-res and 2 in SHRES; overprogrammed counts fetch NOTHING there
        // (hardware-verified: the invplanes1 A500 photo shows no fetch for
        // hi-res BPU=7). Lo-res BPU=7 is the overprogrammed 4-plane fetch:
        // Denise still decodes six BPLDAT latches, but only the first four
        // streams are fed by DMA; higher planes display their current
        // latch values until software updates them.
        let dma_planes = if shres {
            if code <= 2 {
                code
            } else {
                0
            }
        } else if hires {
            if code <= 4 {
                code
            } else {
                0
            }
        } else if code == 7 {
            4
        } else {
            code
        };
        Self {
            display_planes,
            dma_planes,
        }
    }

    pub fn display_planes(self) -> usize {
        self.display_planes
    }

    pub fn dma_planes(self) -> usize {
        self.dma_planes
    }
}

impl Default for Denise {
    fn default() -> Self {
        Self::new()
    }
}

impl Denise {
    pub fn new() -> Self {
        Self {
            palette: Palette::new(),
            bplcon0: 0,
            bplcon1: 0,
            bplcon2: 0,
            bplcon3: BPLCON3_PF2OF_DEFAULT,
            bplcon4: 0x0011,
            clxcon2: 0,
            clxcon: CLXCON_RESET,
            clxdat: 0,
            bplpt: [0; 8],
            bpldat: [0; 8],
            sprpt: [0; 8],
            sprpos: [0; 8],
            sprctl: [0; 8],
            sprdata: [0; 8],
            sprdatb: [0; 8],
            spr_armed: [false; 8],
            spr_hw_pos: [0; 8],
            spr_hw_ctl: [0; 8],
            spr_hw_data: [0; 8],
            spr_hw_datb: [0; 8],
            spr_hw_armed: [false; 8],
            bpl1mod: 0,
            bpl2mod: 0,
            diwstrt: 0,
            diwstop: 0,
            diwhigh: 0,
            diwhigh_written: false,
            ddfstrt: 0,
            ddfstop: 0,
            dma_addr_mask: 0x001F_FFFF,
        }
    }

    pub fn set_dma_addr_mask(&mut self, mask: u32) {
        self.dma_addr_mask = mask | 1;
        let ptr_mask = self.dma_ptr_mask();
        for ptr in &mut self.bplpt {
            *ptr &= ptr_mask;
        }
        for ptr in &mut self.sprpt {
            *ptr &= ptr_mask;
        }
    }

    pub fn set_bplpt_high(&mut self, idx: usize, val: u16) {
        let cur = self.bplpt[idx];
        self.bplpt[idx] =
            ((cur & 0x0000_FFFF) | ((val as u32 & 0x001F) << 16)) & self.dma_ptr_mask();
    }
    pub fn set_bplpt_low(&mut self, idx: usize, val: u16) {
        let cur = self.bplpt[idx];
        self.bplpt[idx] = ((cur & 0x00FF_0000) | (val as u32 & 0xFFFE)) & self.dma_ptr_mask();
    }

    pub fn set_sprpt_high(&mut self, idx: usize, val: u16) {
        let cur = self.sprpt[idx];
        self.sprpt[idx] =
            ((cur & 0x0000_FFFF) | ((val as u32 & 0x001F) << 16)) & self.dma_ptr_mask();
    }

    pub fn set_sprpt_low(&mut self, idx: usize, val: u16) {
        let cur = self.sprpt[idx];
        self.sprpt[idx] = ((cur & 0x00FF_0000) | (val as u32 & 0xFFFE)) & self.dma_ptr_mask();
    }

    fn dma_ptr_mask(&self) -> u32 {
        self.dma_addr_mask & !1
    }

    pub fn write_bpldat(&mut self, idx: usize, val: u16) {
        if idx < self.bpldat.len() {
            self.bpldat[idx] = val;
        }
    }

    pub fn write_sprpos(&mut self, idx: usize, val: u16) {
        if idx < self.sprpos.len() {
            self.sprpos[idx] = val;
            self.spr_hw_pos[idx] = val;
        }
    }

    pub fn write_sprctl(&mut self, idx: usize, val: u16) {
        if idx < self.sprctl.len() {
            self.sprctl[idx] = val;
            self.spr_armed[idx] = false;
            self.spr_hw_ctl[idx] = val;
            self.spr_hw_armed[idx] = false;
        }
    }

    pub fn write_sprdata(&mut self, idx: usize, val: u16) {
        if idx < self.sprdata.len() {
            self.sprdata[idx] = val;
            self.spr_armed[idx] = true;
            self.spr_hw_data[idx] = val;
            self.spr_hw_armed[idx] = true;
        }
    }

    pub fn write_sprdatb(&mut self, idx: usize, val: u16) {
        if idx < self.sprdatb.len() {
            self.sprdatb[idx] = val;
            self.spr_hw_datb[idx] = val;
        }
    }

    /// A sprite DMA fetch landing in the registers: same effect as the
    /// matching CPU/Copper write on the hardware-true view, but invisible
    /// to the CPU/Copper write shadow the render replay is seeded from.
    pub fn dma_write_sprpos(&mut self, idx: usize, val: u16) {
        if idx < self.spr_hw_pos.len() {
            self.spr_hw_pos[idx] = val;
        }
    }

    pub fn dma_write_sprctl(&mut self, idx: usize, val: u16) {
        if idx < self.spr_hw_ctl.len() {
            self.spr_hw_ctl[idx] = val;
            self.spr_hw_armed[idx] = false;
        }
    }

    pub fn dma_write_sprdata(&mut self, idx: usize, val: u16) {
        if idx < self.spr_hw_data.len() {
            self.spr_hw_data[idx] = val;
            self.spr_hw_armed[idx] = true;
        }
    }

    pub fn dma_write_sprdatb(&mut self, idx: usize, val: u16) {
        if idx < self.spr_hw_datb.len() {
            self.spr_hw_datb[idx] = val;
        }
    }

    pub fn read_clxdat(&mut self) -> u16 {
        std::mem::take(&mut self.clxdat) | 0x8000
    }

    pub fn or_clxdat(&mut self, bits: u16) {
        self.clxdat |= bits & 0x7FFF;
    }
}

pub fn color_register_value(c: u16) -> u16 {
    c & COLOR_REGISTER_MASK
}

/// Convert Amiga 12-bit $0RGB to 0xAA_RR_GG_BB (alpha = 0xFF) for the
/// pixels framebuffer (which expects RGBA8 in little-endian byte order:
/// R, G, B, A in memory). Any non-RGB register bits are ignored.
/// Expand a 12-bit $0RGB word to 24-bit 0x00RRGGBB by nibble duplication
/// (the exact mapping rgb12_to_rgba8 uses), so OCS colour maths carried in
/// 24-bit space stays bit-identical.
pub fn rgb12_to_rgb24(c: u16) -> u32 {
    let r4 = u32::from((c >> 8) & 0xF);
    let g4 = u32::from((c >> 4) & 0xF);
    let b4 = u32::from(c & 0xF);
    ((r4 << 4 | r4) << 16) | ((g4 << 4 | g4) << 8) | (b4 << 4 | b4)
}

/// 24-bit 0x00RRGGBB to the framebuffer's RGBA8 layout (R,G,B,A in memory).
pub fn rgb24_to_rgba8(c: u32) -> u32 {
    let r = (c >> 16) & 0xFF;
    let g = (c >> 8) & 0xFF;
    let b = c & 0xFF;
    0xFF00_0000 | (b << 16) | (g << 8) | r
}

pub fn rgb12_to_rgba8(c: u16) -> u32 {
    let c = c & COLOR_RGB_MASK;
    let r4 = ((c >> 8) & 0xF) as u32;
    let g4 = ((c >> 4) & 0xF) as u32;
    let b4 = (c & 0xF) as u32;
    let r = (r4 << 4) | r4;
    let g = (g4 << 4) | g4;
    let b = (b4 << 4) | b4;
    // Memory layout: byte0=R byte1=G byte2=B byte3=A. On little-endian
    // host that's the u32 value 0xAA_BB_GG_RR.
    0xFF00_0000 | (b << 16) | (g << 8) | r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_ocs_write_sets_both_nibble_planes() {
        let mut palette = Palette::new();
        palette.write_ocs(1, 0x8FA5);
        // Reads (and the OCS view) see the OCS-layout word.
        assert_eq!(palette[1], 0x8FA5);
        assert_eq!(palette.hi_words()[1], 0x8FA5);
        // The 24-bit composition duplicates the nibbles, T bit on bit 31.
        assert_eq!(palette.rgb24(1), 0x8000_0000 | 0x00FF_AA55);
    }

    #[test]
    fn palette_banked_loct_writes_low_nibbles_only() {
        let mut palette = Palette::new();
        // Bank 2, entry 5 -> absolute entry 69.
        palette.write_banked(2, 5, false, 0x0123);
        assert_eq!(palette.rgb24(2 * 32 + 5), 0x0011_2233);
        palette.write_banked(2, 5, true, 0x0FFF);
        // High nibbles keep the first write; low nibbles take the LOCT one.
        assert_eq!(palette.rgb24(2 * 32 + 5), 0x001F_2F3F);
        // Bank 0 (the OCS view) is untouched.
        assert_eq!(palette.hi_words()[5], 0);
    }

    #[test]
    fn lisa_id_and_ecs_subset() {
        assert_eq!(DeniseRevision::AgaLisa.id(), Some(0x00F8));
        assert!(DeniseRevision::AgaLisa.is_ecs());
    }

    #[test]
    fn clxcon_resets_to_all_playfield_match_bits_enabled() {
        let denise = Denise::new();

        assert_eq!(denise.clxcon, CLXCON_RESET);
    }

    #[test]
    fn clxdat_read_returns_high_bit_and_clears_latch() {
        let mut denise = Denise::new();

        denise.or_clxdat(0x7FFF);

        assert_eq!(denise.read_clxdat(), 0xFFFF);
        assert_eq!(denise.read_clxdat(), 0x8000);
    }
}
