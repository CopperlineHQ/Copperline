// SPDX-License-Identifier: GPL-3.0-or-later

//! The card monitors' own sounds, made rather than sampled: the power
//! switch, the degaussing coil's thump and hum, the static crackle as the
//! tube's high voltage comes up or drains away, the line output
//! transformer's 15.734 kHz whine, and the click of a front-panel button.
//! Played through [`crate::audio::fx`], so they are host sounds: no part
//! of the machine, its recordings or its captures.

use std::sync::{Arc, OnceLock};

use crate::audio::fx::{self, FX_RATE};

const RATE: f32 = FX_RATE as f32;
/// NTSC's line rate: what the flyback transformer sings at.
const LINE_HZ: f32 = 15_734.264;
/// The mains, which the degaussing coil hums at.
const MAINS_HZ: f32 = 60.0;

/// A small, fixed noise source, so the sounds are the same every time.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

fn samples(seconds: f32) -> Vec<f32> {
    vec![0.0; (seconds * RATE) as usize]
}

/// Add `f(t)` (seconds from `at`) into `out` from `at` for `len` seconds.
fn add(out: &mut [f32], at: f32, len: f32, mut f: impl FnMut(f32) -> f32) {
    let start = (at * RATE) as usize;
    let end = (((at + len) * RATE) as usize).min(out.len());
    for (k, s) in out.iter_mut().enumerate().take(end).skip(start) {
        *s += f((k - start) as f32 / RATE);
    }
}

/// A mechanical click: a crack of noise, a ring of the plastic, a thump.
fn click(out: &mut [f32], at: f32, level: f32, ring_hz: f32, thump_hz: f32, noise: &mut Noise) {
    let mut last = 0.0;
    add(out, at, 0.06, |t| {
        let n = noise.next();
        // First difference: the crack's highs only.
        let crack = (n - last) * (-t / 0.0012).exp();
        last = n;
        let ring = (std::f32::consts::TAU * ring_hz * t).sin() * (-t / 0.004).exp();
        let thump = (std::f32::consts::TAU * thump_hz * t).sin() * (-t / 0.014).exp();
        level * (0.35 * crack + 0.25 * ring + 0.45 * thump)
    });
}

/// The degaussing coil: a thump as the relay drops the mains across it,
/// then a buzzing hum that dies away as the thermistor heats.
fn degauss(out: &mut [f32], at: f32, level: f32) {
    // One cycle of the hum, harmonics and all (exactly 800 samples at
    // 48 kHz), made once and looked up.
    let period = (RATE / MAINS_HZ) as usize;
    let cycle: Vec<f32> = (0..period)
        .map(|k| {
            let ph = std::f32::consts::TAU * k as f32 / period as f32;
            let mut v = 0.0;
            for n in 1..=24 {
                let w = 1.0 / (n as f32).powf(0.85);
                v += w * (n as f32 * ph + 0.7 * n as f32).sin();
            }
            // The coil and the shadow mask rattle: a little clipping.
            (v * 0.9).tanh()
        })
        .collect();
    let mut lp = 0.0;
    let mut k = 0usize;
    add(out, at, 1.7, |t| {
        let v = cycle[k % period];
        k += 1;
        // Soften the top end a touch.
        lp += (v - lp) * 0.35;
        let attack = (t / 0.006).min(1.0);
        let env = attack * (0.25 * (-t / 0.05).exp() + 0.75 * (-t / 0.32).exp());
        level * env * lp
    });
}

/// The static on the glass as the high voltage comes up or drains away:
/// pops, thinning out over `len` seconds.
fn crackle(out: &mut [f32], at: f32, len: f32, level: f32, density: f32, noise: &mut Noise) {
    let mut pop = 0.0f32;
    let mut last = 0.0;
    let mut chance = Noise(noise.0 ^ 0x9E37_79B9);
    add(out, at, len, |t| {
        let p = density * (-t / (len * 0.2)).exp() / RATE;
        if (chance.next() * 0.5 + 0.5) < p {
            pop = pop.max(0.3 + 0.7 * (chance.next() * 0.5 + 0.5));
        }
        pop *= 0.982;
        let n = noise.next();
        let v = (n - last) * pop;
        last = n;
        level * v
    });
}

/// The line output transformer's whine, from `f0` to `f1` Hz over `len`
/// seconds (exponentially), shaped by `env(t)`.
fn whine(out: &mut [f32], at: f32, len: f32, f0: f32, f1: f32, env: impl Fn(f32) -> f32) {
    let mut ph = 0.0f32;
    add(out, at, len, |t| {
        let f = f0 * (f1 / f0).powf((t / len).min(1.0));
        ph = (ph + std::f32::consts::TAU * f / RATE) % std::f32::consts::TAU;
        env(t) * ph.sin()
    });
}

/// A monitor being switched on: the switch, the degaussing thump and hum,
/// the whine and crackle of the tube coming up.
fn power_on(seed: u32) -> Vec<f32> {
    let mut out = samples(4.5);
    let mut noise = Noise(0x1234_5678 ^ seed);
    click(&mut out, 0.0, 0.8, 2600.0, 110.0, &mut noise);
    click(&mut out, 0.035, 0.5, 1900.0, 80.0, &mut noise);
    degauss(&mut out, 0.12, 0.55);
    whine(&mut out, 0.35, 4.0, LINE_HZ, LINE_HZ, |t| {
        0.03 * (t / 0.5).min(1.0) * (-t / 1.6).exp()
    });
    crackle(&mut out, 0.45, 3.5, 0.35, 140.0, &mut noise);
    out
}

/// A monitor being switched off: the switch, the whine falling away as
/// the scan stops, the thump of the high voltage collapsing, crackle as
/// the glass discharges.
fn power_off(seed: u32) -> Vec<f32> {
    let mut out = samples(2.5);
    let mut noise = Noise(0x8765_4321 ^ seed);
    click(&mut out, 0.0, 0.8, 2400.0, 100.0, &mut noise);
    whine(&mut out, 0.01, 0.45, LINE_HZ, 900.0, |t| {
        0.06 * (-t / 0.12).exp()
    });
    add(&mut out, 0.02, 0.15, |t| {
        0.35 * (std::f32::consts::TAU * 55.0 * t).sin() * (-t / 0.03).exp()
    });
    crackle(&mut out, 0.08, 2.2, 0.3, 110.0, &mut noise);
    out
}

/// A front-panel button.
fn button() -> Vec<f32> {
    let mut out = samples(0.08);
    let mut noise = Noise(0x0BAD_F00D);
    click(&mut out, 0.0, 0.35, 3200.0, 180.0, &mut noise);
    out
}

/// Made once each: two variants of the power sounds, so two monitors
/// switched on together do not sound as one.
fn cached(slot: &'static OnceLock<Arc<[f32]>>, make: impl FnOnce() -> Vec<f32>) -> Arc<[f32]> {
    Arc::clone(slot.get_or_init(|| Arc::from(make())))
}

/// Play a monitor's power going on or off, `delay` seconds from now.
/// `variant` picks one of two takes.
pub(super) fn power(on: bool, variant: usize, delay: f32) {
    static ON: [OnceLock<Arc<[f32]>>; 2] = [OnceLock::new(), OnceLock::new()];
    static OFF: [OnceLock<Arc<[f32]>>; 2] = [OnceLock::new(), OnceLock::new()];
    let v = variant % 2;
    let sound = if on {
        cached(&ON[v], || power_on(v as u32 * 0x5555))
    } else {
        cached(&OFF[v], || power_off(v as u32 * 0x5555))
    };
    fx::play(sound, delay, 0.7);
}

/// Play a front-panel button's click.
pub(super) fn button_click() {
    static CLICK: OnceLock<Arc<[f32]>> = OnceLock::new();
    fx::play(cached(&CLICK, button), 0.0, 0.6);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sounds_are_made_within_full_scale() {
        for s in [power_on(0), power_on(0x5555), power_off(0), button()] {
            let peak = s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(peak > 0.05 && peak < 1.0, "peak {peak}");
            assert!(s.iter().all(|v| v.is_finite()));
        }
        // The power-on sound is mostly the degaussing hum's first second.
        let on = power_on(0);
        let energy = |a: f32, b: f32| {
            on[(a * RATE) as usize..(b * RATE) as usize]
                .iter()
                .map(|v| v * v)
                .sum::<f32>()
        };
        assert!(energy(0.15, 0.6) > 10.0 * energy(3.0, 3.45));
    }
}
