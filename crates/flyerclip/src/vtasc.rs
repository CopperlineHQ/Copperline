// SPDX-License-Identifier: GPL-3.0-or-later

//! VTASC: the Flyer's composite-video field codec.
//!
//! A transcription of `amiflyer/src/flyerlibvtasc.asm` (rev 1.5), the
//! flyer.library software model of the Flyer's VTASC P/M coder chips. The
//! routine names in comments (`CompLine`, `DecompLine`, `FIRfilterLine`,
//! `GenRndNum` ...) are the asm labels each piece mirrors. Arithmetic is done
//! in the same widths as the 68000 code (byte adds wrap, `muls.w` truncates
//! to 16 bits, unsigned word compares) so results match bit for bit.
//!
//! Deliberate deviations from the asm, all documented in the spec:
//! * Decoder: a size-8 code of `0x80` is taken as the value -128 rather
//!   than an escape to a (non-existent) 9-bit size.
//! * Encoder: never emits that code (error -128 at tolerance 0 is coded as
//!   +-127, a 1-LSB error).
//! * Encoder: the padding line that starts fields 2 and 4 uses the same
//!   sub-Nyquist pixel parity as every other coded line (the asm's pad line
//!   uses the opposite parity from what its own decoder expects).
//! * Dither: one 760-sample dither segment per *coded* line, as the encoder
//!   does; the asm decoder skips the segment of the discarded first line of
//!   fields 2 and 4.

#![allow(clippy::needless_range_loop)] // index loops mirror the asm

use crate::header::{ClipHeader, FieldInfo, VTASC_OLDD2};

/// Samples per coded line (`PIXELSPERLINE`, flyerprivate.i:282): 752 + 4 pad each side.
pub const PIXELS_PER_LINE: usize = 760;
/// Samples per Toaster line (`SAMPLESPERLINE`, flyer.h:281).
pub const SAMPLES_PER_LINE: usize = 752;
/// Pad samples on each side of the 752 (CompLine, asm:1733-1744).
pub const PAD: usize = 4;
/// Lines returned per field (`TOASTERLINES`, flyer.h:282).
pub const TOASTER_LINES: usize = 242;
/// Lines per full NTSC field (`LINESPERFIELD`, flyer.h:283).
pub const LINES_PER_FIELD: usize = 243;
/// `NTSC_BLANK`, `NTSC_BLACK`, `BORDERVALUE`, `MAXTOLER` (flyerprivate.i:271-280).
pub const NTSC_BLANK: u8 = 60;
pub const NTSC_BLACK: u8 = 71;
pub const BORDER: u8 = 0x82;
pub const MAX_TOLER: u8 = 7;
/// `BITBUFSIZE` (flyerprivate.i:274-275): software fields are padded to this.
pub const BITBUF_SIZE: usize = 1024;

/// `CodeMagTable` (asm:1489-1505): bits needed to code a signed error byte.
pub static CODE_MAG_TABLE: [u8; 256] = [
    1, 2, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 5, 5, 5, 5, // 0
    6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, // 1
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, // 2
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, // 3
    8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, // 4
    8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, // 5
    8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, // 6
    8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, // 7
    8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, // 8
    8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, // 9
    8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, // A
    8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, // B
    8, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, // C
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, // D
    7, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, // E
    6, 5, 5, 5, 5, 5, 5, 5, 5, 4, 4, 4, 4, 3, 3, 2, // F
];

/// `EscapeCodes` (asm:2284-2293): the n-bit code meaning "grow to n+1 bits".
pub static ESCAPE_CODES: [u8; 9] = [0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80];
/// `SgnExtBits` / `SgnExtVals` (asm:2295-2315).
static SGN_EXT_BITS: [u8; 9] = [0, 0, 1, 2, 3, 4, 5, 6, 7];
static SGN_EXT_VALS: [u8; 9] = [0, 0, 0xFC, 0xF8, 0xF0, 0xE0, 0xC0, 0x80, 0x00];

/// An FIR coefficient set as the Flyer stores it (`struct FIRSET`,
/// flyer/inc/Hard.h:28-31): eight 10-bit two's-complement coefficients,
/// outermost tap first, centre tap last, and a scale (x256).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirSet {
    pub coefs: [u16; 8],
    pub scale: u16,
}

impl FirSet {
    pub fn from_words(w: &[u16; 9]) -> Self {
        let mut coefs = [0u16; 8];
        coefs.copy_from_slice(&w[..8]);
        FirSet { coefs, scale: w[8] }
    }
}

const fn fs(c: [u16; 8]) -> FirSet {
    FirSet {
        coefs: c,
        scale: 0x200,
    }
}

/// `FIRPRESETS[4][2]` (flyer/src/hard.c:94-108): `[set-1][0=pre (record), 1=post (play)]`.
/// Set 1 = 25 %, 2 = 33 % (the default everywhere), 3 = 50 %, 4 = 100 % precompensation.
pub static FIR_PRESETS: [[FirSet; 2]; 4] = [
    [
        fs([0x003, 0x004, 0x006, 0x00D, 0x00F, 0x00B, 0x019, 0x033]),
        fs([0x026, 0x3FE, 0x3CA, 0x00C, 0x3E2, 0x314, 0x3C4, 0x1CC]),
    ],
    [
        fs([0x004, 0x003, 0x005, 0x00D, 0x00D, 0x007, 0x019, 0x03A]),
        fs([0x025, 0x3D2, 0x008, 0x3E4, 0x3D5, 0x3FA, 0x2E1, 0x1ED]),
    ],
    [
        fs([0x004, 0x001, 0x002, 0x00C, 0x00A, 0x400, 0x018, 0x049]),
        fs([0x400, 0x3F5, 0x017, 0x3C3, 0x003, 0x016, 0x334, 0x164]),
    ],
    [
        fs([0x3FF, 0x003, 0x3FA, 0x00A, 0x3F0, 0x01B, 0x3CD, 0x0A2]),
        fs([0x3FF, 0x003, 0x3FA, 0x00A, 0x3F0, 0x01B, 0x3CD, 0x0A2]),
    ],
];

/// The preset the Flyer always plays back with (vid.c:3979-3982) and records with by default (vid.c:539).
pub const DEFAULT_FIR_SET: u8 = 2;

/// Pre- (`post == false`) or post-compensation coefficients of preset `set` (1-4).
pub fn fir_preset(set: u8, post: bool) -> Option<FirSet> {
    if (1..=4).contains(&set) {
        Some(FIR_PRESETS[set as usize - 1][post as usize])
    } else {
        None
    }
}

/// Prediction/sampling mode (`ALGO_*`, flyer.h:276-277, flyerprivate.i:286).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algo {
    /// Old chips (VidCmpVer 1): guess = c4 - c5 + c6 along the line.
    OldD2,
    /// guess = pixel two lines up + previous error; all 760 samples coded.
    D2,
    /// As D2 but only the samples with (coded line + x) even are coded; the
    /// rest are regenerated on decode.
    SubNyquist,
}

/// Everything the codec needs to (de)code one field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldParams {
    /// Field number; only bit 0 (field I/III vs II/IV) and bit 1 matter.
    pub field: u32,
    pub algo: Algo,
    /// Tolerance (low bits dropped from each error); also the dither size.
    pub tolerance: u8,
    /// Dither period minus one (`RndFreq`; 122 is the Flyer default).
    pub rnd_freq: u8,
    pub rnd_seed: u8,
    /// FIR set: pre-comp coefficients when encoding, post-comp when decoding.
    /// `None` = `FWF_NOFIR`.
    pub fir: Option<FirSet>,
}

impl FieldParams {
    /// Parameters for decoding (`post = true`) or re-encoding a stored field,
    /// the way `GetCompInfo` (flyer/src/clip.c:178-203) derives them.
    /// FIR preset 0 takes the clip header's custom set; an out-of-range
    /// preset falls back to set 2, which is what the Flyer hardware plays with.
    pub fn from_field_info(
        fi: &FieldInfo,
        vid_cmp_ver: u8,
        field: u32,
        clip: Option<&ClipHeader>,
        post: bool,
    ) -> Self {
        let algo = if vid_cmp_ver == VTASC_OLDD2 {
            Algo::OldD2
        } else if fi.is_sub_nyquist() {
            Algo::SubNyquist
        } else {
            Algo::D2
        };
        let fir = match fi.fir_preset {
            0 => clip.map(|c| {
                FirSet::from_words(if post {
                    &c.cust_inv_coefs
                } else {
                    &c.cust_coefs
                })
            }),
            n => fir_preset(n, post).or(fir_preset(DEFAULT_FIR_SET, post)),
        };
        FieldParams {
            field,
            algo,
            tolerance: fi.tolerance,
            rnd_freq: fi.rnd_freq,
            rnd_seed: fi.rnd_seed,
            fir,
        }
    }

    /// Defaults used by `WriteField.c` / `FlyerProc.c`: D2, tolerance 0,
    /// RndFreq 122, FIR set 2; seed = field % 255 as `CloseField` stores it
    /// (clip.c:678).
    pub fn default_encode(field: u32) -> Self {
        FieldParams {
            field,
            algo: Algo::D2,
            tolerance: 0,
            rnd_freq: 122,
            rnd_seed: (field % 255) as u8,
            fir: fir_preset(DEFAULT_FIR_SET, false),
        }
    }
}

// ---------------------------------------------------------------- bits

/// LSB-first bit reader (InputCode000, asm:1196-1247). Past the end reads 0.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn read(&mut self, n: u8) -> u8 {
        let mut v = 0u32;
        for i in 0..n {
            let byte = self.data.get(self.pos >> 3).copied().unwrap_or(0);
            v |= (((byte >> (self.pos & 7)) & 1) as u32) << i;
            self.pos += 1;
        }
        v as u8
    }
}

/// LSB-first bit writer (OutputCode000, asm:987-1036).
struct BitWriter {
    buf: Vec<u8>,
    nbits: usize,
}

impl BitWriter {
    fn put(&mut self, code: u8, len: u8) {
        for i in 0..len {
            if self.nbits & 7 == 0 {
                self.buf.push(0);
            }
            if (code >> i) & 1 != 0 {
                *self.buf.last_mut().unwrap() |= 1 << (self.nbits & 7);
            }
            self.nbits += 1;
        }
    }
    /// PurgeOut (asm:1085-1116): zero bits until a whole BITBUFSIZE is written.
    fn finish(mut self) -> Vec<u8> {
        let bytes = self.buf.len().div_ceil(BITBUF_SIZE) * BITBUF_SIZE;
        self.buf.resize(bytes, 0);
        self.buf
    }
}

// ---------------------------------------------------------------- dither

/// InitRndSeed (asm:848-872): each seed bit becomes a group of three.
fn init_rnd(seed: u8) -> u32 {
    let s = if seed == 0xFF { 0 } else { seed };
    let mut d1 = 0u32;
    for i in 0..8 {
        if (s >> i) & 1 != 0 {
            d1 += 7 << (3 * i);
        }
    }
    d1
}

/// Pseudo-random dither generator of the P-coders.
#[derive(Clone, Debug)]
pub struct Dither {
    data: u32,
    num: u8,
    cntr: u8,
    freq: u8,
    bits: u8,
}

impl Dither {
    /// StartVTASC (asm:571-573, 721-731): period = RndFreq + 1, first value 0.
    pub fn new(seed: u8, rnd_freq: u8, bits: u8) -> Self {
        Dither {
            data: init_rnd(seed),
            num: 0,
            cntr: 1,
            freq: rnd_freq.wrapping_add(1),
            bits,
        }
    }

    /// GenRndNum (asm:886-935): 4 XNOR(bit23, bit16) shifts of the 24-bit
    /// register, then the low `bits` bits sign-extended.
    pub fn gen(&mut self) -> i8 {
        let mut d0 = self.data;
        for _ in 0..4 {
            let b23 = (d0 >> 23) & 1;
            let b16 = (d0 >> 16) & 1;
            d0 = (d0 << 1) | (b23 == b16) as u32;
        }
        self.data = d0;
        let b = d0 as u8;
        match self.bits {
            0 => (b as i8) >> 7,
            n @ 1..=7 => (b.rotate_right(n as u32) as i8) >> (8 - n),
            _ => (b as i8) >> 7, // asm loops 65536 times here (MAXTOLER prevents it)
        }
    }

    /// The dither loops of CompLine (asm:1761-1798, add) and DecompLine
    /// (asm:2191-2226, subtract) over one 760-sample line. `None` advances
    /// the generator through the line without touching samples.
    pub fn line(&mut self, mut line: Option<&mut [u8; PIXELS_PER_LINE]>, add: bool) {
        if self.bits == 0 {
            return;
        }
        for x in 0..PIXELS_PER_LINE {
            if let Some(l) = line.as_deref_mut() {
                l[x] = if add {
                    l[x].wrapping_add(self.num)
                } else {
                    l[x].wrapping_sub(self.num)
                };
            }
            if self.cntr == self.freq {
                self.num = self.gen() as u8;
                self.cntr = 0;
            }
            self.cntr = self.cntr.wrapping_add(1);
        }
    }
}

// ---------------------------------------------------------------- FIR

/// FIRfilterLine (asm:1521-1624): symmetric 16-tap filter over the 760
/// samples, samples off either end read as NTSC_BLANK. `advance` (encode)
/// centres the taps between x and x+1; otherwise (decode) between x-1 and x.
pub fn fir_line(line: &mut [u8; PIXELS_PER_LINE], set: &FirSet, advance: bool) {
    // StartVTASC (asm:677-696): strip to 10 bits and sign-extend.
    let coefs: [i32; 8] = set.coefs.map(|c| {
        let c = (c & 0x3FF) as i32;
        if c & 0x200 != 0 {
            c - 1024
        } else {
            c
        }
    });
    let scale = (set.scale >> 8) as i16 as i32; // lsr.l #8: integral part
    let adv: i32 = if advance { 0 } else { -1 };
    let mut tmp = [0u8; PIXELS_PER_LINE];
    for x in 0..PIXELS_PER_LINE as i32 {
        let mut sum: i32 = 0;
        for d in (0..8i32).rev() {
            let r = x + d + adv + 1;
            let pr = if (r as u32) < PIXELS_PER_LINE as u32 {
                line[r as usize]
            } else {
                NTSC_BLANK
            } as i32;
            let l = x - d + adv;
            let pl = if l >= 0 { line[l as usize] } else { NTSC_BLANK } as i32;
            sum = sum.wrapping_add((pr + pl) * coefs[(7 - d) as usize]);
        }
        let s = (sum >> 2) as i16 as i32; // asr.l #2, then muls.w takes the low word
        let v = (s * scale) >> 7; // muls.w, asr.l #7
        let w = v as u16; // cmp.w #$FF (unsigned): negatives clip to $FF too
        tmp[x as usize] = if w > 0xFF { 0xFF } else { w as u8 };
    }
    *line = tmp;
}

// ---------------------------------------------------------------- helpers

fn sub_nyquist_active(coded_line: usize, x: usize) -> bool {
    (coded_line + x) & 1 == 0
}

fn tol_of(p: &FieldParams) -> u8 {
    p.tolerance.min(MAX_TOLER) // StartVTASC, asm:553-559
}

/// Subcarrier phase of a field line as FillLineActive (asm:1645-1671)
/// computes it: `true` where sample 0 of the quad sits on -I.
pub fn phase_positive(field: u32, line: usize) -> bool {
    ((line as u32 & 1) ^ (field & 1) ^ ((field >> 1) & 1)) == 0
}

/// FillLineWide (asm:1638-1701): a 760 line of colour (Y, I, Q) in sample
/// units, quad -I,-Q,+I,+Q on positive lines. `vt_y` is the line counter
/// the asm uses for the phase.
fn fill_line(vt_y: i32, field: u32, fill: (u8, i8, i8)) -> [u8; PIXELS_PER_LINE] {
    let (y, mut i, mut q) = (fill.0, fill.1 as u8, fill.2 as u8);
    if !phase_positive(field, (vt_y & 1) as usize) {
        i = i.wrapping_neg();
        q = q.wrapping_neg();
    }
    let quad = [
        y.wrapping_sub(i),
        y.wrapping_sub(q),
        y.wrapping_add(i),
        y.wrapping_add(q),
    ];
    let mut l = [0u8; PIXELS_PER_LINE];
    for s in 0..SAMPLES_PER_LINE {
        l[PAD + s] = quad[s & 3];
    }
    l
}

const FILL_BLACK: (u8, i8, i8) = (NTSC_BLACK, 0, 0);

// ---------------------------------------------------------------- encoder

struct Enc {
    bits: BitWriter,
    lines: [[u8; PIXELS_PER_LINE]; 2],
    top: usize,
    msize: u8,
    preverr: u8,
    c4: u8,
    c5: u8,
    c6: u8,
    tol: u8,
    sn: bool,
    pcoder: bool,
    fir: Option<FirSet>,
    dither: Dither,
}

impl Enc {
    fn new(p: &FieldParams) -> Self {
        let tol = tol_of(p);
        Enc {
            bits: BitWriter {
                buf: Vec::with_capacity(64 * 1024),
                nbits: 0,
            },
            lines: [[BORDER; PIXELS_PER_LINE]; 2],
            top: 0,
            msize: 8,
            preverr: 0,
            c4: BORDER,
            c5: BORDER,
            c6: BORDER,
            tol,
            sn: p.algo == Algo::SubNyquist,
            pcoder: p.algo != Algo::OldD2,
            fir: p.fir,
            dither: Dither::new(p.rnd_seed, p.rnd_freq, tol),
        }
    }

    /// CompLine (asm:1725-1945) for coded line `k`.
    fn comp_line(&mut self, mut line: [u8; PIXELS_PER_LINE], k: usize) {
        for x in 0..PAD {
            line[x] = NTSC_BLACK;
            line[PAD + SAMPLES_PER_LINE + x] = NTSC_BLACK;
        }
        if let Some(f) = &self.fir {
            fir_line(&mut line, f, true);
        }
        self.dither.line(Some(&mut line), true);

        let t = self.top;
        for x in 0..PIXELS_PER_LINE {
            if self.sn && !sub_nyquist_active(k, x) {
                self.lines[t][x] = line[x]; // inactive: kept raw, never predicted from
                continue;
            }
            let t7 = self.lines[t][x];
            let guess = if self.pcoder {
                t7.wrapping_add(self.preverr)
            } else {
                self.c4.wrapping_sub(self.c5).wrapping_add(self.c6)
            };
            let pixel = line[x];
            let mut err = ((pixel.wrapping_sub(guess) as i8) >> self.tol) as u8; // asr.b
            if err == 0x80 {
                // Only reachable at tolerance 0: 0x80 is the size-8 escape
                // code. Code +-127 instead (1 LSB off), keeping 0..255.
                err = if pixel == 0 { 0x81 } else { 0x7F };
            }
            let c7 = guess.wrapping_add(err << self.tol);
            self.preverr = c7.wrapping_sub(t7);
            let need = CODE_MAG_TABLE[err as usize];
            while self.msize < need {
                self.bits.put(ESCAPE_CODES[self.msize as usize], self.msize);
                self.msize += 1;
            }
            self.bits.put(err, self.msize);
            self.msize = need;
            self.lines[t][x] = c7;
            self.c4 = self.c5;
            self.c5 = self.c6;
            self.c6 = c7;
        }
        self.top ^= 1;
    }
}

/// Encode one field: `lines` are up to 242 Toaster lines of 752 samples
/// (missing lines are filled black, extra lines ignored). Returns the coded
/// field, padded with zero bits to a multiple of 1024 bytes as the
/// flyer.library writes it. Lines are coded as OpenWriteField2/WriteLine2/
/// CloseField2 do: fields II/IV get a leading black pad line, and every
/// field is filled with black to 243 counted lines.
pub fn encode_field<L: AsRef<[u8]>>(lines: &[L], p: &FieldParams) -> Vec<u8> {
    let mut enc = Enc::new(p);
    let odd = p.field & 1 == 1;
    let mut k = 0usize;
    if odd {
        enc.comp_line(fill_line(0, p.field, FILL_BLACK), k); // asm:754-767
        k += 1;
    }
    let mut vt_y = 0i32;
    for y in 0..TOASTER_LINES {
        let mut l = fill_line(vt_y, p.field, FILL_BLACK);
        if let Some(src) = lines.get(y) {
            let src = src.as_ref();
            let n = src.len().min(SAMPLES_PER_LINE);
            l[PAD..PAD + n].copy_from_slice(&src[..n]);
        }
        enc.comp_line(l, k);
        k += 1;
        vt_y += 1;
    }
    while (vt_y as usize) < LINES_PER_FIELD {
        enc.comp_line(fill_line(vt_y, p.field, FILL_BLACK), k); // CloseField2, asm:272-291
        k += 1;
        vt_y += 1;
    }
    enc.bits.finish()
}

// ---------------------------------------------------------------- decoder

const MEM_TOP: usize = 0; // vt_topline
const MEM_EXTRA: usize = 1; // vt_extraline
const MEM_LINES: usize = 2; // vt_Lines follows them in VTASCinfo (flyerprivate.i:332-334)

struct Dec<'a> {
    br: BitReader<'a>,
    /// vt_topline, vt_extraline, vt_Lines[2*760]: modelled as one block of
    /// memory because the sub-Nyquist regen reads two bytes before a line.
    mem: [u8; MEM_LINES + 2 * PIXELS_PER_LINE],
    msize: u8,
    preverr: u8,
    c4: u8,
    c5: u8,
    c6: u8,
    y: i16,
    field: u8,
    tol: u8,
    sn: bool,
    pcoder: bool,
}

impl<'a> Dec<'a> {
    fn new(data: &'a [u8], p: &FieldParams) -> Self {
        let mut mem = [BORDER; MEM_LINES + 2 * PIXELS_PER_LINE];
        mem[MEM_TOP] = 1;
        mem[MEM_EXTRA] = 2;
        Dec {
            br: BitReader { data, pos: 0 },
            mem,
            msize: 8,
            preverr: 0,
            c4: BORDER,
            c5: BORDER,
            c6: BORDER,
            y: 0,
            field: p.field as u8,
            tol: tol_of(p),
            sn: p.algo == Algo::SubNyquist,
            pcoder: p.algo != Algo::OldD2,
        }
    }

    /// One error code (asm:2090-2109), returns the sign-extended error.
    fn get_code(&mut self, d7: &mut u8) -> u8 {
        let mut code;
        loop {
            code = self.br.read(*d7);
            if *d7 < 8 && code == ESCAPE_CODES[*d7 as usize] {
                *d7 += 1;
                continue;
            }
            break;
        }
        let sb = SGN_EXT_BITS[*d7 as usize];
        if (code >> sb) & 1 != 0 {
            code = code.wrapping_add(SGN_EXT_VALS[*d7 as usize]);
        }
        *d7 = CODE_MAG_TABLE[code as usize];
        code
    }

    /// DecompLine (asm:1967-2246) without its dither and FIR passes, which
    /// the caller applies. Writes the 760 output samples into `out`.
    fn decomp_line(&mut self, mut out: Option<&mut [u8; PIXELS_PER_LINE]>) {
        let mut d7 = self.msize;
        let mut a4 = MEM_LINES + (self.mem[MEM_TOP] as usize - 1) * PIXELS_PER_LINE; // up1
        let mut a0 = MEM_LINES + (self.mem[MEM_EXTRA] as usize - 1) * PIXELS_PER_LINE; // down1
        let mut a1 = 0usize; // index into out
        let has_out = out.is_some();
        let xmax = if self.sn {
            PIXELS_PER_LINE
        } else {
            PIXELS_PER_LINE - 1
        };
        for x in 0..=xmax {
            if self.sn {
                let parity = ((self.y as u16 as u8) ^ (x as u8) ^ self.field) & 1;
                if parity == 0 {
                    // .isactive: pixel x of this output line was coded last
                    // line; place it and regenerate the missing pixel to its left.
                    let mut d3: u16 = BORDER as u16;
                    let mut regen = true;
                    if x == PIXELS_PER_LINE {
                        if !has_out {
                            regen = false;
                        }
                    } else {
                        d3 = self.mem[a4] as u16;
                        self.mem[a0] = d3 as u8;
                        match out.as_deref_mut() {
                            None => regen = false,
                            Some(o) => {
                                o[a1] = d3 as u8;
                                if x == 0 {
                                    regen = false;
                                }
                            }
                        }
                    }
                    if regen {
                        let o = out.as_deref_mut().unwrap();
                        let left: u16 = if x == 1 {
                            BORDER as u16
                        } else {
                            o[a1 - 2] as u16
                        };
                        let d1: u16 = left + d3; // 2Y
                        let up = sub_ext(self.mem[a4 - 3], self.mem[a4 - 1]); // 2C above
                        let dn = sub_ext(self.mem[a0 - 3], self.mem[a0 - 1]); // 2C below
                        let mut v = up.wrapping_add(dn) >> 1;
                        v = v.wrapping_add(d1 as i16) >> 1;
                        o[a1 - 1] = if (v as u16) < 0x100 {
                            v as u8
                        } else {
                            (d1 >> 1) as u8
                        };
                    }
                    if has_out {
                        a1 += 1; // .noRegen (only reached with an output buffer)
                    }
                    a4 += 1; // .tossSN
                    a0 += 1;
                    continue;
                }
                // Inactive here = coded on the next line: decode it.
                if x == PIXELS_PER_LINE {
                    continue;
                }
                if self.y == LINES_PER_FIELD as i16 - 1 {
                    // Building the last line: no line below, use border.
                    self.mem[a0] = BORDER;
                    a0 += 1;
                    a4 += 1;
                    if has_out {
                        a1 += 1;
                    }
                    continue;
                }
            }
            // .getcode
            let err = self.get_code(&mut d7);
            let t7 = self.mem[a4];
            let guess = if self.pcoder {
                self.preverr.wrapping_add(t7)
            } else {
                self.c4.wrapping_sub(self.c5).wrapping_add(self.c6)
            };
            let c7 = guess.wrapping_add(err << self.tol);
            self.preverr = c7.wrapping_sub(t7);
            if self.sn {
                // .SNalgo
                self.mem[a0] = c7;
                a0 += 1;
                a4 += 1;
                if has_out {
                    a1 += 1;
                }
            } else {
                // .D2algo
                if let Some(o) = out.as_deref_mut() {
                    o[a1] = c7;
                    a1 += 1;
                }
                self.mem[a4] = c7;
                a4 += 1;
                self.c4 = self.c5;
                self.c5 = self.c6;
                self.c6 = c7;
            }
        }
        self.mem.swap(MEM_TOP, MEM_EXTRA);
        self.y = self.y.wrapping_add(1);
        self.msize = d7;
    }
}

/// `sub.b` followed by `ext.w` only when the subtraction borrowed (asm:2052-2062).
fn sub_ext(a: u8, b: u8) -> i16 {
    let r = a.wrapping_sub(b);
    if a < b {
        r as i8 as i16
    } else {
        r as i16
    }
}

/// Decode a field to 242 lines of all 760 coded samples (the 4 pad samples
/// each side included), after dither removal and the post FIR.
pub fn decode_field_wide(data: &[u8], p: &FieldParams) -> Vec<[u8; PIXELS_PER_LINE]> {
    let mut dec = Dec::new(data, p);
    // StartVTASC decompression set-up (asm:771-795).
    if dec.sn {
        dec.y -= 1;
    }
    if p.field & 1 == 1 {
        dec.y -= 1;
    }
    while dec.y != 0 {
        dec.decomp_line(None);
    }
    let mut dither = Dither::new(p.rnd_seed, p.rnd_freq, dec.tol);
    let mut next_seg = 0usize;
    let mut out = Vec::with_capacity(TOASTER_LINES);
    for y in 0..TOASTER_LINES {
        let mut line = [0u8; PIXELS_PER_LINE];
        dec.decomp_line(Some(&mut line));
        // Dither segment = coded line carried by this output line.
        let seg = y + (p.field & 1) as usize;
        while next_seg < seg {
            dither.line(None, false);
            next_seg += 1;
        }
        dither.line(Some(&mut line), false);
        next_seg += 1;
        if let Some(f) = &p.fir {
            fir_line(&mut line, f, false);
        }
        out.push(line);
    }
    out
}

/// Decode a field to 242 Toaster lines of 752 samples (ReadLine2, asm:335-373).
pub fn decode_field(data: &[u8], p: &FieldParams) -> Vec<Vec<u8>> {
    decode_field_wide(data, p)
        .into_iter()
        .map(|l| l[PAD..PAD + SAMPLES_PER_LINE].to_vec())
        .collect()
}

/// Which samples of a decoded Toaster line were coded (the rest were
/// regenerated): `true` for all in D2 modes; in sub-Nyquist mode sample
/// `s` of line `y` was coded iff (y + (field&1) + s + 4) is even.
pub fn sample_was_coded(p: &FieldParams, line: usize, sample: usize) -> bool {
    p.algo != Algo::SubNyquist || sub_nyquist_active(line + (p.field & 1) as usize, sample + PAD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_mag_table_is_signed_width() {
        for v in 0..=255u8 {
            let s = v as i8 as i32;
            let need = if s == 0 {
                1
            } else {
                (1..=8).find(|&n| s.abs() < (1 << (n - 1))).unwrap_or(8)
            };
            assert_eq!(CODE_MAG_TABLE[v as usize] as i32, need, "v={v:#x}");
        }
    }

    #[test]
    fn rng_first_values() {
        let mut d = Dither::new(1, 0, 2);
        assert_eq!(d.data, 7);
        assert_eq!(d.gen(), -1);
        assert_eq!(d.data, 0x7F);
        assert_eq!(init_rnd(0xFF), 0);
        assert_eq!(init_rnd(0x81), 7 | (7 << 21));
    }

    #[test]
    fn bit_order_lsb_first() {
        let mut w = BitWriter {
            buf: vec![],
            nbits: 0,
        };
        w.put(0xC5, 8);
        w.put(0, 7);
        w.put(1, 1);
        w.put(2, 2);
        assert_eq!(&w.buf[..3], &[0xC5, 0x80, 0x02]);
        let mut r = BitReader {
            data: &w.buf,
            pos: 0,
        };
        assert_eq!(r.read(8), 0xC5);
        assert_eq!(r.read(7), 0);
        assert_eq!(r.read(1), 1);
        assert_eq!(r.read(2), 2);
    }
}
