// SPDX-License-Identifier: LGPL-2.1-or-later

//! The Boss reverb chip: an entrance delay behind a low-pass filter, three
//! series allpasses and three combs tapped in parallel -- or, in the
//! fourth mode, one long tap delay. Every constant here -- buffer sizes,
//! filter and feedback factors, output taps -- was traced from the real
//! devices' reverb RAM by the munt project, in two flavours: the MT-32's
//! own chip and the CM-32L/LAPC-I revision.
//!
//! The arithmetic is the chip's: 16-bit samples, factors applied as
//! eight-bit fractions through a 32-bit product, sums wrapping except the
//! one adder the hardware is known to saturate (the comb mix).

/// The LA32 hands the Boss chip its output a sample late; the model buys
/// the latch with one extra slot of entrance delay.
const PROCESS_DELAY: u32 = 1;

/// The tap-delay mode's own latches, on top of the entrance one.
const MODE_3_ADDITIONAL_DELAY: u32 = 1;
const MODE_3_FEEDBACK_DELAY: u32 = 1;

/// One reverb program's traced constants.
struct Settings {
    allpasses: &'static [u32],
    combs: &'static [u32],
    out_l: &'static [u32],
    out_r: &'static [u32],
    comb_factors: &'static [u8],
    comb_feedbacks: &'static [u8],
    dry_amps: &'static [u8],
    wet_levels: &'static [u8],
    lpf_amp: u8,
}

/// The CM-32L / LAPC-I chip's four programs.
static CM32L_SETTINGS: [Settings; 4] = [
    Settings {
        allpasses: &[994, 729, 78],
        combs: &[705 + PROCESS_DELAY, 2349, 2839, 3632],
        out_l: &[2349, 141, 1960],
        out_r: &[1174, 1570, 145],
        comb_factors: &[0xA0, 0x60, 0x60, 0x60],
        comb_feedbacks: &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
            0x28, 0x48, 0x60, 0x78, 0x80, 0x88, 0x90, 0x98, //
            0x28, 0x48, 0x60, 0x78, 0x80, 0x88, 0x90, 0x98, //
            0x28, 0x48, 0x60, 0x78, 0x80, 0x88, 0x90, 0x98,
        ],
        dry_amps: &[0xA0, 0xA0, 0xA0, 0xA0, 0xB0, 0xB0, 0xB0, 0xD0],
        wet_levels: &[0x10, 0x30, 0x50, 0x70, 0x90, 0xC0, 0xF0, 0xF0],
        lpf_amp: 0x60,
    },
    Settings {
        allpasses: &[1324, 809, 176],
        combs: &[961 + PROCESS_DELAY, 2619, 3545, 4519],
        out_l: &[2618, 1760, 4518],
        out_r: &[1300, 3532, 2274],
        comb_factors: &[0x80, 0x60, 0x60, 0x60],
        comb_feedbacks: &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
            0x28, 0x48, 0x60, 0x70, 0x78, 0x80, 0x90, 0x98, //
            0x28, 0x48, 0x60, 0x78, 0x80, 0x88, 0x90, 0x98, //
            0x28, 0x48, 0x60, 0x78, 0x80, 0x88, 0x90, 0x98,
        ],
        dry_amps: &[0xA0, 0xA0, 0xB0, 0xB0, 0xB0, 0xB0, 0xB0, 0xE0],
        wet_levels: &[0x10, 0x30, 0x50, 0x70, 0x90, 0xC0, 0xF0, 0xF0],
        lpf_amp: 0x60,
    },
    Settings {
        allpasses: &[969, 644, 157],
        combs: &[116 + PROCESS_DELAY, 2259, 2839, 3539],
        out_l: &[2259, 718, 1769],
        out_r: &[1136, 2128, 1],
        comb_factors: &[0, 0x20, 0x20, 0x20],
        comb_feedbacks: &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
            0x30, 0x58, 0x78, 0x88, 0xA0, 0xB8, 0xC0, 0xD0, //
            0x30, 0x58, 0x78, 0x88, 0xA0, 0xB8, 0xC0, 0xD0, //
            0x30, 0x58, 0x78, 0x88, 0xA0, 0xB8, 0xC0, 0xD0,
        ],
        dry_amps: &[0xA0, 0xA0, 0xB0, 0xB0, 0xB0, 0xB0, 0xC0, 0xE0],
        wet_levels: &[0x10, 0x30, 0x50, 0x70, 0x90, 0xC0, 0xF0, 0xF0],
        lpf_amp: 0x80,
    },
    Settings {
        allpasses: &[],
        combs: &[16000 + MODE_3_FEEDBACK_DELAY + PROCESS_DELAY + MODE_3_ADDITIONAL_DELAY],
        out_l: &[400, 624, 960, 1488, 2256, 3472, 5280, 8000],
        out_r: &[800, 1248, 1920, 2976, 4512, 6944, 10560, 16000],
        comb_factors: &[0x68],
        comb_feedbacks: &[0x68, 0x60],
        dry_amps: &[
            0x20, 0x50, 0x50, 0x50, 0x50, 0x50, 0x50, 0x50, //
            0x20, 0x50, 0x50, 0x50, 0x50, 0x50, 0x50, 0x50,
        ],
        wet_levels: &[0x18, 0x18, 0x28, 0x40, 0x60, 0x80, 0xA8, 0xF8],
        lpf_amp: 0,
    },
];

/// The MT-32's chip: same structures, its own sizes and levels.
static MT32_SETTINGS: [Settings; 4] = [
    Settings {
        allpasses: &[994, 729, 78],
        combs: &[575 + PROCESS_DELAY, 2040, 2752, 3629],
        out_l: &[2040, 687, 1814],
        out_r: &[1019, 2072, 1],
        comb_factors: &[0xB0, 0x60, 0x60, 0x60],
        comb_feedbacks: &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
            0x28, 0x48, 0x60, 0x70, 0x78, 0x80, 0x90, 0x98, //
            0x28, 0x48, 0x60, 0x78, 0x80, 0x88, 0x90, 0x98, //
            0x28, 0x48, 0x60, 0x78, 0x80, 0x88, 0x90, 0x98,
        ],
        dry_amps: &[0x80; 8],
        wet_levels: &[0x10, 0x20, 0x30, 0x40, 0x50, 0x70, 0xA0, 0xE0],
        lpf_amp: 0x80,
    },
    Settings {
        allpasses: &[1324, 809, 176],
        combs: &[961 + PROCESS_DELAY, 2619, 3545, 4519],
        out_l: &[2618, 1760, 4518],
        out_r: &[1300, 3532, 2274],
        comb_factors: &[0x90, 0x60, 0x60, 0x60],
        comb_feedbacks: &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
            0x28, 0x48, 0x60, 0x70, 0x78, 0x80, 0x90, 0x98, //
            0x28, 0x48, 0x60, 0x78, 0x80, 0x88, 0x90, 0x98, //
            0x28, 0x48, 0x60, 0x78, 0x80, 0x88, 0x90, 0x98,
        ],
        dry_amps: &[0x80; 8],
        wet_levels: &[0x10, 0x20, 0x30, 0x40, 0x50, 0x70, 0xA0, 0xE0],
        lpf_amp: 0x80,
    },
    Settings {
        allpasses: &[969, 644, 157],
        combs: &[116 + PROCESS_DELAY, 2259, 2839, 3539],
        out_l: &[2259, 718, 1769],
        out_r: &[1136, 2128, 1],
        comb_factors: &[0, 0x60, 0x60, 0x60],
        comb_feedbacks: &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
            0x28, 0x48, 0x60, 0x70, 0x78, 0x80, 0x90, 0x98, //
            0x28, 0x48, 0x60, 0x78, 0x80, 0x88, 0x90, 0x98, //
            0x28, 0x48, 0x60, 0x78, 0x80, 0x88, 0x90, 0x98,
        ],
        dry_amps: &[0x80; 8],
        wet_levels: &[0x10, 0x20, 0x30, 0x40, 0x50, 0x70, 0xA0, 0xE0],
        lpf_amp: 0x80,
    },
    Settings {
        allpasses: &[],
        combs: &[16000 + MODE_3_FEEDBACK_DELAY + PROCESS_DELAY + MODE_3_ADDITIONAL_DELAY],
        out_l: &[400, 624, 960, 1488, 2256, 3472, 5280, 8000],
        out_r: &[800, 1248, 1920, 2976, 4512, 6944, 10560, 16000],
        comb_factors: &[0x68],
        comb_feedbacks: &[0x68, 0x60],
        dry_amps: &[
            0x10, 0x10, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, //
            0x10, 0x20, 0x20, 0x10, 0x20, 0x10, 0x20, 0x10,
        ],
        wet_levels: &[0x08, 0x18, 0x28, 0x40, 0x60, 0x80, 0xA8, 0xF8],
        lpf_amp: 0,
    },
];

/// The chip's multiplier: an eight-bit factor over a 32-bit product. The
/// result always fits a sample again, so the narrowing never wraps.
fn weird_mul(sample: i16, factor: u8) -> i16 {
    ((i32::from(sample) * i32::from(factor)) >> 8) as i16
}

/// The saturating adder the hardware is known to have, on the comb mix:
/// out1 and out2 at one-and-a-half, out3 straight, clipped.
fn mix_combs(out1: i16, out2: i16, out3: i16) -> i16 {
    let (a, b, c) = (i32::from(out1), i32::from(out2), i32::from(out3));
    clip(a + (a >> 1) + b + (b >> 1) + c)
}

/// Into range, or the saturation with the sign's own limit.
fn clip(sample: i32) -> i16 {
    if (-0x8000..=0x7FFF).contains(&sample) {
        sample as i16
    } else {
        ((sample >> 31) ^ 0x7FFF) as i16
    }
}

/// A ring of samples; stepping returns what the new slot held.
struct Ring {
    buf: Vec<i16>,
    index: usize,
}

impl Ring {
    fn new(size: u32) -> Ring {
        Ring {
            buf: vec![0; size as usize],
            index: 0,
        }
    }

    fn next(&mut self) -> i16 {
        self.index += 1;
        if self.index >= self.buf.len() {
            self.index = 0;
        }
        self.buf[self.index]
    }

    /// The sample written `at` steps ago.
    fn output_at(&self, at: u32) -> i16 {
        let size = self.buf.len();
        self.buf[(size + self.index - at as usize) % size]
    }

    /// Nothing above the chip's audibility threshold left inside.
    fn is_quiet(&self) -> bool {
        self.buf.iter().all(|&s| (-8..=8).contains(&s))
    }
}

/// One allpass: feedback and feedforward at exactly a half.
struct Allpass(Ring);

impl Allpass {
    fn process(&mut self, sample: i16) -> i16 {
        let out = self.0.next();
        let stored = (i32::from(sample) - i32::from(out >> 1)) as i16;
        self.0.buf[self.0.index] = stored;
        (i32::from(out) + i32::from(stored >> 1)) as i16
    }
}

/// One comb: a low-pass in the loop, feedback set by the running time
/// parameter. The entrance delay and the tap delay are the same store
/// stepped their own ways.
struct Comb {
    ring: Ring,
    filter: u8,
    feedback: u8,
}

impl Comb {
    fn new(size: u32, filter: u8) -> Comb {
        Comb {
            ring: Ring::new(size),
            filter,
            feedback: 0,
        }
    }

    fn process(&mut self, sample: i16) {
        let last = self.ring.buf[self.ring.index];
        let fed = self.ring.next();
        let filter_in = (i32::from(sample) + i32::from(weird_mul(fed, self.feedback))) as i16;
        self.ring.buf[self.ring.index] =
            (i32::from(weird_mul(last, self.filter)) - i32::from(filter_in)) as i16;
    }

    /// The entrance: a plain delay whose loop is only the low-pass, its
    /// store scaled on the way in.
    fn process_entrance(&mut self, sample: i16, amp: u8) {
        let last = self.ring.buf[self.ring.index];
        self.ring.next();
        let lpf_out = (i32::from(weird_mul(last, self.filter)) + i32::from(sample)) as i16;
        self.ring.buf[self.ring.index] = weird_mul(lpf_out, amp);
    }

    /// The tap delay: feedback comes from just past the right output tap,
    /// wherever the time parameter has put it.
    fn process_tap(&mut self, sample: i16, out_r: u32) {
        let last = self.ring.buf[self.ring.index];
        self.ring.next();
        let fed = self.ring.output_at(out_r + MODE_3_FEEDBACK_DELAY);
        let filter_in = (i32::from(sample) + i32::from(weird_mul(fed, self.feedback))) as i16;
        self.ring.buf[self.ring.index] =
            (i32::from(weird_mul(last, self.filter)) - i32::from(filter_in)) as i16;
    }
}

/// The chip, running one of its four programs.
pub struct Reverb {
    mode: u8,
    settings: &'static Settings,
    allpasses: Vec<Allpass>,
    combs: Vec<Comb>,
    dry_amp: u8,
    wet_level: u8,
    out_l: u32,
    out_r: u32,
}

impl Reverb {
    /// A freshly powered chip on program `mode` (0..=3), with the MT-32's
    /// or the CM-32L's constants. Levels sit at zero until
    /// [Self::set_parameters] is given the running time and level.
    pub fn new(mode: u8, mt32_compatible: bool) -> Reverb {
        let settings = if mt32_compatible {
            &MT32_SETTINGS[usize::from(mode)]
        } else {
            &CM32L_SETTINGS[usize::from(mode)]
        };
        Reverb {
            mode,
            settings,
            allpasses: settings
                .allpasses
                .iter()
                .map(|&size| Allpass(Ring::new(size)))
                .collect(),
            combs: settings
                .combs
                .iter()
                .zip(settings.comb_factors)
                .map(|(&size, &filter)| Comb::new(size, filter))
                .collect(),
            dry_amp: 0,
            wet_level: 0,
            out_l: 0,
            out_r: 0,
        }
    }

    /// Which program this chip runs; a mode change means a fresh chip.
    pub fn mode(&self) -> u8 {
        self.mode
    }

    /// The system area's running time and level applied, exactly as the
    /// firmware writes them.
    pub fn set_parameters(&mut self, time: u8, level: u8) {
        let time = time & 7;
        let level = level & 7;
        if self.mode == 3 {
            self.out_l = self.settings.out_l[usize::from(time)];
            self.out_r = self.settings.out_r[usize::from(time)];
            let which = usize::from(level >= 3 && time >= 6);
            self.combs[0].feedback = self.settings.comb_feedbacks[which];
        } else {
            let feedbacks = self.settings.comb_feedbacks;
            for (i, comb) in self.combs.iter_mut().enumerate().skip(1) {
                comb.feedback = feedbacks[(i << 3) + usize::from(time)];
            }
        }
        if time == 0 && level == 0 {
            self.dry_amp = 0;
            self.wet_level = 0;
        } else {
            // The MT-32's tap delay has a quirk of its own: at the lowest
            // times, odd levels draw from a second amp row.
            let dry_index = if self.mode == 3 && (time == 0 || (time == 1 && level == 1)) {
                usize::from(level) + 8
            } else {
                usize::from(level)
            };
            self.dry_amp = self.settings.dry_amps[dry_index];
            self.wet_level = self.settings.wet_levels[usize::from(level)];
        }
    }

    /// Whether anything above the audibility threshold still rings.
    pub fn is_active(&self) -> bool {
        !self.allpasses.iter().all(|a| a.0.is_quiet())
            || !self.combs.iter().all(|c| c.ring.is_quiet())
    }

    /// One block through the chip: the dry pair in, the wet pair out.
    pub fn process(&mut self, in_l: &[i16], in_r: &[i16], out_l: &mut [i16], out_r: &mut [i16]) {
        for n in 0..in_l.len() {
            if self.mode == 3 {
                let dry_in = (i32::from(in_l[n] >> 1) + i32::from(in_r[n] >> 1)) as i16;
                let dry = weird_mul(dry_in, self.dry_amp);
                self.combs[0].process_tap(dry, self.out_r);
                let tap = PROCESS_DELAY + MODE_3_ADDITIONAL_DELAY;
                out_l[n] = weird_mul(
                    self.combs[0].ring.output_at(self.out_l + tap),
                    self.wet_level,
                );
                out_r[n] = weird_mul(
                    self.combs[0].ring.output_at(self.out_r + tap),
                    self.wet_level,
                );
            } else {
                let dry_in = (i32::from(in_l[n] >> 2) + i32::from(in_r[n] >> 2)) as i16;
                let dry = weird_mul(dry_in, self.dry_amp);
                // The oldest entrance sample leaves as the store is rewritten.
                let mut link = self.combs[0].ring.output_at(self.settings.combs[0] - 1);
                self.combs[0].process_entrance(dry, self.settings.lpf_amp);
                for allpass in self.allpasses.iter_mut() {
                    link = allpass.process(link);
                }
                // The first left tap sits a whole buffer back: read it
                // before the comb overwrites that slot.
                let out_l1 = self.combs[1].ring.output_at(self.settings.out_l[0] - 1);
                self.combs[1].process(link);
                self.combs[2].process(link);
                self.combs[3].process(link);
                let out_l2 = self.combs[2].ring.output_at(self.settings.out_l[1]);
                let out_l3 = self.combs[3].ring.output_at(self.settings.out_l[2]);
                out_l[n] = weird_mul(mix_combs(out_l1, out_l2, out_l3), self.wet_level);
                let out_r1 = self.combs[1].ring.output_at(self.settings.out_r[0]);
                let out_r2 = self.combs[2].ring.output_at(self.settings.out_r[1]);
                let out_r3 = self.combs[3].ring.output_at(self.settings.out_r[2]);
                out_r[n] = weird_mul(mix_combs(out_r1, out_r2, out_r3), self.wet_level);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An impulse through the room program rings: silence until the
    /// entrance delay elapses, sound after, and activity reported until
    /// the tail decays away.
    #[test]
    fn an_impulse_rings_and_decays() {
        let mut reverb = Reverb::new(0, true);
        reverb.set_parameters(5, 3);
        assert!(!reverb.is_active(), "a fresh chip is quiet");

        let mut impulse_l = vec![0i16; 1];
        let impulse_r = vec![0i16; 1];
        impulse_l[0] = 20000;
        let mut wet_l = vec![0i16; 1];
        let mut wet_r = vec![0i16; 1];
        reverb.process(&impulse_l, &impulse_r, &mut wet_l, &mut wet_r);
        assert!(reverb.is_active(), "the impulse is inside the chip");

        let quiet_l = vec![0i16; 4000];
        let quiet_r = vec![0i16; 4000];
        let mut heard = false;
        for _ in 0..8 {
            let mut out_l = vec![0i16; 4000];
            let mut out_r = vec![0i16; 4000];
            reverb.process(&quiet_l, &quiet_r, &mut out_l, &mut out_r);
            heard |= out_l.iter().chain(&out_r).any(|&s| s != 0);
        }
        assert!(heard, "the room answers");
        for _ in 0..60 {
            let mut out_l = vec![0i16; 4000];
            let mut out_r = vec![0i16; 4000];
            reverb.process(&quiet_l, &quiet_r, &mut out_l, &mut out_r);
        }
        assert!(!reverb.is_active(), "the tail decays below audibility");
    }

    /// Time and level zero silence the output without stopping the store:
    /// what already rings keeps circulating, only the taps go quiet.
    #[test]
    fn zero_time_and_level_close_the_taps() {
        let mut reverb = Reverb::new(1, false);
        reverb.set_parameters(4, 4);
        let loud = vec![12000i16; 256];
        let mut wet_l = vec![0i16; 256];
        let mut wet_r = vec![0i16; 256];
        reverb.process(&loud, &loud, &mut wet_l, &mut wet_r);
        reverb.set_parameters(0, 0);
        let quiet = vec![0i16; 4096];
        let mut out_l = vec![0i16; 4096];
        let mut out_r = vec![0i16; 4096];
        reverb.process(&quiet, &quiet, &mut out_l, &mut out_r);
        assert!(out_l.iter().chain(&out_r).all(|&s| s == 0), "taps closed");
        assert!(reverb.is_active(), "the store still rings inside");
    }
}
