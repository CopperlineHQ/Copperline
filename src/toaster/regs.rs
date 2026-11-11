// SPDX-License-Identifier: GPL-3.0-or-later

//! The card's control registers, as the extended line headers latch them.
//!
//! Each extended header carries four bits of fields in bits 11-8 of each of
//! its four words (`elh.i`): EH0 the framestore modes, EH1 and EH2 the
//! video muxes, EH3 the matte, EH4 the fader, EH5 the phase trims and EH6
//! pages the key clips, pedestal and gain. Values persist until the next
//! header of the same kind and apply from the line they arrive on.

use super::Header;

/// A mux position (`VTI_PV_*`, `VTI_AM_*` ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mux {
    /// Video inputs 1-4 (0-3).
    Vid(usize),
    /// Framestore playback, bank 0 ("DV1") / bank 1 ("DV2").
    Dac(usize),
    /// The encoder: the matte generator, or the Amiga picture without MATTE.
    Encoder,
    /// Position 7: Program on PV, external on LK, luminance (MONO) on the
    /// fader and IS muxes.
    Seven,
}

impl Mux {
    /// The source numbered `v` (0-7), as the muxes and the latch codes
    /// number them.
    pub fn from_code(v: u8) -> Self {
        Self::from_bits(u16::from(v))
    }

    fn from_bits(v: u16) -> Self {
        match v & 7 {
            n @ 0..=3 => Mux::Vid(usize::from(n)),
            4 => Mux::Dac(0),
            5 => Mux::Dac(1),
            6 => Mux::Encoder,
            _ => Mux::Seven,
        }
    }
}

/// Where the fader takes its mix from (`CDS`, EH4 B bits 10-8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cds {
    /// Per-quad from the Amiga (digital effects).
    Quaf,
    /// The Amiga's genlock transparency per pixel.
    PixelSw,
    /// Transparent quads (drop shadows).
    TransQuad,
    /// A luma key on the LK source.
    LumKey,
    /// A four-bit per-pixel code from the Amiga (alpha keys).
    Dibgr,
    /// A two-bit per-pixel code from the Amiga (wipes).
    Dib,
    RgQuad,
    /// The constant `CD`: cuts, fades, the T-bar.
    Cd,
}

fn bits(word: u16) -> u16 {
    (word >> 8) & 0xF
}

/// A read-only view of the latched headers.
pub struct Regs<'a> {
    pub(super) elh: &'a [Header],
}

impl Regs<'_> {
    fn eh(&self, n: usize) -> Header {
        self.elh[n]
    }

    // EH0: framestore modes.
    pub fn obr(&self) -> bool {
        self.eh(0).r & 0x0100 != 0
    }
    pub fn linp(&self) -> bool {
        self.eh(0).g & 0x0400 != 0
    }
    pub fn abse(&self) -> bool {
        self.eh(0).g & 0x0200 != 0
    }
    pub fn werb(&self) -> bool {
        self.eh(0).g & 0x0100 != 0
    }
    /// LUT: the framestore plays through the card's look-up table (the B&W
    /// and colour LUT modes), not a line-addressed DVE.
    pub fn lut(&self) -> bool {
        self.eh(0).b & 0x0200 != 0
    }

    /// EH0 `LUST`: a write line is stored left-justified. The switcher's
    /// AA "zipper" write sends a 768-sample line in two passes and clears
    /// it for the second, whose bitmap is scrolled 16 hi-res pixels left:
    /// the card then stores that pass 16 samples further right.
    pub fn lust(&self) -> bool {
        self.eh(0).i & 0x0100 != 0
    }

    /// Lines flagged CERB carry Amiga data for the framestore (a write)
    /// rather than a request to digitise the input.
    pub fn write_mode(&self) -> bool {
        self.werb() && !self.abse()
    }

    // EH1.
    pub fn matte(&self) -> bool {
        self.eh(1).r & 0x0800 != 0
    }
    pub fn lk(&self) -> Mux {
        Mux::from_bits(bits(self.eh(1).r))
    }
    pub fn useron(&self) -> bool {
        self.eh(1).g & 0x0800 != 0
    }
    pub fn pv(&self) -> Mux {
        Mux::from_bits(bits(self.eh(1).g))
    }

    // EH2.
    pub fn am(&self) -> Mux {
        Mux::from_bits(bits(self.eh(2).r))
    }
    pub fn is(&self) -> Mux {
        Mux::from_bits(bits(self.eh(2).g))
    }
    pub fn bm(&self) -> Mux {
        Mux::from_bits(bits(self.eh(2).b))
    }
    /// AMWIPE: with AM = 7, the A bus takes its source per pixel from the
    /// Amiga picture (the latch effects, `LatchAM`).
    pub fn amwipe(&self) -> bool {
        self.eh(2).r & 0x0800 != 0
    }
    /// BMWIPE: the same for the B bus.
    pub fn bmwipe(&self) -> bool {
        self.eh(2).b & 0x0800 != 0
    }

    // EH3: the matte colour code (MA, MB, MP) and snow.
    pub fn matte_code(&self) -> (u8, u8, u8) {
        let e = self.eh(3);
        (bits(e.g) as u8, bits(e.b) as u8, bits(e.r) as u8)
    }
    pub fn snow(&self) -> bool {
        self.eh(3).i & 0x0100 == 0
    }

    // EH4: the fader.
    pub fn cd(&self) -> u8 {
        let e = self.eh(4);
        ((bits(e.g) << 4) | bits(e.r)) as u8
    }
    pub fn cds(&self) -> Cds {
        match bits(self.eh(4).b) & 7 {
            0 => Cds::Quaf,
            1 => Cds::PixelSw,
            2 => Cds::TransQuad,
            3 => Cds::LumKey,
            4 => Cds::Dibgr,
            5 => Cds::Dib,
            6 => Cds::RgQuad,
            _ => Cds::Cd,
        }
    }
    pub fn no_key_invert(&self) -> bool {
        self.eh(4).i & 0x0100 != 0
    }

    // EH68: the luma key clip.
    pub fn lka(&self) -> u8 {
        let e = self.eh(super::ELH_EH6_BASE + 8);
        ((bits(e.g) << 4) | bits(e.r)) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regs_of(pairs: &[(usize, Header)]) -> Vec<Header> {
        let mut elh = vec![Header::default(); super::super::ELH_COUNT];
        for (n, h) in pairs {
            elh[*n] = *h;
        }
        elh
    }

    #[test]
    fn switcher_defaults_decode() {
        let elh = regs_of(&super::super::ELH_DEFAULTS);
        let r = Regs { elh: &elh };
        assert_eq!(r.pv(), Mux::Vid(0));
        assert_eq!((r.am(), r.bm()), (Mux::Vid(0), Mux::Vid(0)));
        assert_eq!(r.cds(), Cds::Cd);
        assert_eq!(r.cd(), 0xFF);
        assert!(r.matte() && r.linp() && r.werb() && !r.abse() && !r.obr());
        assert!(!r.snow());
        assert_eq!(r.lka(), 0x80);
    }

    #[test]
    fn mux_positions() {
        let elh = regs_of(&[
            (
                1,
                Header {
                    r: 0x4E00,
                    g: 0x0D00,
                    b: 0,
                    i: 0,
                },
            ),
            (
                2,
                Header {
                    r: 0xA400,
                    g: 0x0700,
                    b: 0x0500,
                    i: 0,
                },
            ),
            (
                4,
                Header {
                    r: 0x9300,
                    g: 0x0A00,
                    b: 0x0500,
                    i: 0,
                },
            ),
        ]);
        let r = Regs { elh: &elh };
        assert_eq!(r.lk(), Mux::Encoder);
        assert!(r.useron());
        assert_eq!(r.pv(), Mux::Dac(1));
        assert_eq!(r.am(), Mux::Dac(0));
        assert_eq!(r.is(), Mux::Seven);
        assert_eq!(r.bm(), Mux::Dac(1));
        assert_eq!(r.cd(), 0xA3);
        assert_eq!(r.cds(), Cds::Dib);
    }
}
