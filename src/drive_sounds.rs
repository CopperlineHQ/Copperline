// SPDX-License-Identifier: GPL-3.0-or-later

//! Synthesized 3.5" floppy drive sound effects.
//!
//! Nothing here is sampled from real hardware (or borrowed from other
//! emulators); the motor, seek and read noises are generated from
//! scratch with cheap oscillator/noise/filter voices. The synthesis
//! targets were derived by spectral analysis of freely licensed (CC0)
//! recordings of real Amiga drive mechanisms (A600 Chinon FZ-354 and an
//! A500 internal drive); only the measured numbers live here, no sample
//! data:
//!
//! - A head step is a "clack", not a tick: a low carriage thump around
//!   300 Hz, a dominant carriage-resonance ring at 1.0-1.2 kHz (~70% of
//!   a real click's energy sits in the 1-2 kHz octave, 1/e decay
//!   ~3.5 ms), a weaker higher partial that carries the 15-25 ms tail,
//!   and a ~1 ms 2-5 kHz contact tick for mechanical crispness.
//! - An isolated step (the classic empty-drive poll click, or the
//!   track-to-track advance while loading) is followed by one or two
//!   quieter carriage-rebound transients ~5 and ~10 ms behind the main
//!   impact; real recordings show every lone step as such a cluster.
//! - A fast multi-track seek is a harmonic comb of the guest's step
//!   rate (trackdisk's 3 ms stepping puts the comb at ~333 Hz) whose
//!   envelope peaks at the 1.0-1.4 kHz carriage resonance. Measured
//!   seek spectra hold under 3% of their energy below 500 Hz, so rapid
//!   stepping ducks the per-click thump and suppresses the rebound
//!   clatter (the carriage never settles between steps); the overlapped
//!   rings then form the comb by themselves.
//! - Reading adds no noise bed of its own. Reference recordings show
//!   the loading sound as the step rhythm over the spinning platter;
//!   an earlier read-gated hiss/whir layer was removed on expert
//!   listening feedback ("keep the click, not the shhhh"), so disk
//!   DMA is deliberately silent here.
//! - Spinning is cyclic, not washy: the spindle hum is a harmonic
//!   stack on ~145 Hz (2nd and 4th partials strongest, as measured),
//!   and the rumble is a noise pattern looped exactly once per 200 ms
//!   platter revolution -- real spin recordings autocorrelate at the
//!   revolution lag because the media drags a fixed pattern of jacket
//!   liner contact around with it. Pitch, pattern rate and level all
//!   glide with the spin level the floppy model reports, so
//!   spin-up/spin-down sweep naturally.
//!
//! The generator runs at the host mixer rate and is mixed into Paula's
//! stereo output after the LED filter (the drive is an acoustic source
//! next to the machine, not part of Paula's filtered audio path). When
//! idle it emits exact 0.0 so the live sink's leading-silence skip
//! keeps working.

use crate::audio::MIX_SAMPLE_RATE;

pub const NUM_DRIVES: usize = 4;
const NUM_CLICK_VOICES: usize = 12;
const NUM_PENDING_CLICKS: usize = 4;

// Master gain staging, relative to Paula's roughly [-1, 1] music mix.
// The drive should read as a mechanism in the background, not an
// instrument: clicks peak around 0.2, the motor sits well below that.
const CLICK_GAIN: f32 = 0.16;
const MOTOR_HUM_GAIN: f32 = 0.014;
const MOTOR_RUMBLE_GAIN: f32 = 0.038;

// Motor hum: a harmonic stack measured off a real spinning drive. The
// spindle's pole/slot structure hums in harmonics of ~145 Hz with the
// 2nd and 4th strongest (measured 145/290/435/577/867 series holding
// ~half the sub-1.5 kHz power in narrow peaks). Ratios are slightly
// inharmonic so the stack beats like a real motor instead of sounding
// like an organ tone.
const MOTOR_HUM_BASE_HZ: f32 = 144.6;
const MOTOR_HUM_PARTIALS: [(f32, f32); 4] =
    [(1.0, 0.55), (2.004, 1.0), (3.013, 0.16), (4.009, 0.42)];
// The platter turns at 5 Hz (300 RPM). The rumble texture is a noise
// pattern looped once per revolution (the media dragging its jacket
// liner is a fixed pattern on the disk), lightly refreshed so it does
// not read as a perfect loop, and its level wobbles once per turn.
// Free-running noise here is what made the old spin sound "washy":
// real recordings show the spin envelope autocorrelating at exactly
// the 200 ms revolution lag.
const MOTOR_REV_HZ: f32 = 5.0;
const MOTOR_WOBBLE_DEPTH: f32 = 0.25;
const MOTOR_PATTERN_FRESH_MIX: f32 = 0.15;
// Rumble noise lowpass sweeps up as the platter comes to speed; two
// cascaded poles keep the wash dark (a single pole leaks the hiss that
// reads as "washy").
const MOTOR_RUMBLE_LP_MIN_HZ: f32 = 140.0;
const MOTOR_RUMBLE_LP_MAX_HZ: f32 = 420.0;
// Smooths externally supplied spin levels (updated at chipset-tick
// granularity) to avoid zipper noise.
const MOTOR_SPIN_SMOOTH_HZ: f32 = 18.0;

// Step clack components (see the module comment for the measured
// targets each of these encodes).
//
// Low carriage-mass thump.
const CLICK_THUMP_MIN_HZ: f32 = 260.0;
const CLICK_THUMP_HZ_SPREAD: f32 = 60.0;
const CLICK_THUMP_DECAY_S: f32 = 0.007;
const CLICK_THUMP_LEVEL: f32 = 0.55;
// Main carriage-resonance ring; the dominant component.
const CLICK_BODY_MIN_HZ: f32 = 1000.0;
const CLICK_BODY_HZ_SPREAD: f32 = 170.0;
const CLICK_BODY_DECAY_S: f32 = 0.0035;
const CLICK_BODY_LEVEL: f32 = 1.0;
// Weak higher partial carrying the long audible tail.
const CLICK_RING_FREQ_RATIO: f32 = 1.45;
const CLICK_RING_DECAY_S: f32 = 0.011;
const CLICK_RING_LEVEL: f32 = 0.16;
// Short bandpassed contact tick.
const CLICK_TICK_LP_HZ: f32 = 4800.0;
const CLICK_TICK_HP_HZ: f32 = 2200.0;
const CLICK_TICK_DECAY_S: f32 = 0.0012;
const CLICK_TICK_LEVEL: f32 = 1.6;
// A voice this quiet is retired.
const CLICK_DEAD_LEVEL: f32 = 1.0e-4;

// A step at least this long after the previous one is "isolated": the
// carriage had settled, so the impact carries its full thump and is
// followed by rebound clatter. Steps arriving faster form a seek buzz
// with neither.
const STEP_ISOLATED_GAP_S: f32 = 0.012;
const SEEK_THUMP_DUCK: f32 = 0.30;
// Rebound transients after an isolated step: delay, level and pitch
// scale relative to the main impact, plus per-event jitter.
const REBOUND_DELAY_S: [f32; 2] = [0.0045, 0.0095];
const REBOUND_DELAY_JITTER_S: f32 = 0.0012;
const REBOUND_LEVEL: [f32; 2] = [0.42, 0.16];
const REBOUND_FREQ_SCALE: [f32; 2] = [1.05, 1.12];

const SILENT_LEVEL: f32 = 1.0e-4;
const TWO_PI: f32 = std::f32::consts::TAU;

/// Advance a sine phase accumulator and keep it in `[0, TWO_PI)`. Every
/// increment here is a small fraction of a cycle per host sample, so wrapping
/// is a single subtraction rather than a floating-point modulo -- `% TWO_PI`
/// lowered to `fmodf`, which showed up as several percent of the whole
/// emulator on the per-sample audio path. The loop covers the (rare) case of a
/// click body pitch high enough to advance more than a full cycle per sample.
#[inline]
fn wrap_phase(phase: f32, inc: f32) -> f32 {
    let mut next = phase + inc;
    while next >= TWO_PI {
        next -= TWO_PI;
    }
    next
}

/// One-pole lowpass coefficient for a cutoff at the mixer rate. The
/// linear approximation is fine for the sub-5 kHz cutoffs used here.
fn lowpass_coeff(cutoff_hz: f32) -> f32 {
    (TWO_PI * cutoff_hz / MIX_SAMPLE_RATE as f32).min(1.0)
}

/// Per-sample decay multiplier for an exponential envelope with the
/// given time constant.
fn decay_per_sample(time_constant_s: f32) -> f32 {
    (-1.0 / (time_constant_s * MIX_SAMPLE_RATE as f32)).exp()
}

fn seconds_to_samples(s: f32) -> u32 {
    (s * MIX_SAMPLE_RATE as f32) as u32
}

#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct MotorVoice {
    /// Smoothed spin level actually driving the synthesis, 0..1.
    spin: f32,
    /// Spin level last reported by the floppy model.
    spin_target: f32,
    /// One phase accumulator per hum partial: the ratios are not
    /// integers, so sharing one accumulator would glitch each wrap.
    hum_phases: [f32; MOTOR_HUM_PARTIALS.len()],
    /// Platter revolution phase, 0..1; drives the looped rumble
    /// pattern and the once-per-turn level wobble.
    rev_phase: f32,
    rumble_lp: f32,
    rumble_lp2: f32,
    /// Per-drive pattern seed so two spinning drives do not phase-lock.
    seed: u32,
}

#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct ClickVoice {
    thump_env: f32,
    thump_phase: f32,
    thump_phase_inc: f32,
    thump_level: f32,
    body_env: f32,
    body_phase: f32,
    body_phase_inc: f32,
    ring_env: f32,
    ring_phase: f32,
    ring_phase_inc: f32,
    tick_env: f32,
    tick_lp: f32,
    tick_hp_lp: f32,
    amp: f32,
}

impl ClickVoice {
    fn active(&self) -> bool {
        self.body_env > CLICK_DEAD_LEVEL
            || self.thump_env > CLICK_DEAD_LEVEL
            || self.ring_env > CLICK_DEAD_LEVEL
            || self.tick_env > CLICK_DEAD_LEVEL
    }
}

/// A rebound transient scheduled behind an isolated step's main impact.
#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct PendingClick {
    live: bool,
    delay_samples: u32,
    amp: f32,
    freq_scale: f32,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct DriveSounds {
    enabled: bool,
    volume: f32,
    motors: [MotorVoice; NUM_DRIVES],
    clicks: [ClickVoice; NUM_CLICK_VOICES],
    next_click_voice: usize,
    pending: [PendingClick; NUM_PENDING_CLICKS],
    /// Host samples since the last head step, saturating; classifies a
    /// step as an isolated clack or part of a seek buzz.
    since_step: u32,
    rng: u32,
}

impl Default for DriveSounds {
    fn default() -> Self {
        Self::new()
    }
}

impl DriveSounds {
    pub fn new() -> Self {
        let mut motors = [MotorVoice::default(); NUM_DRIVES];
        for (drive, motor) in motors.iter_mut().enumerate() {
            motor.seed = 0x9E37_79B9u32.wrapping_mul(drive as u32 + 1);
        }
        Self {
            enabled: true,
            volume: 1.0,
            motors,
            clicks: [ClickVoice::default(); NUM_CLICK_VOICES],
            next_click_voice: 0,
            pending: [PendingClick::default(); NUM_PENDING_CLICKS],
            since_step: u32::MAX,
            rng: 0x2F6E_2B1D,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled && !enabled {
            // Drop in-flight voices so a later re-enable does not
            // replay the tail of old activity.
            self.clicks = [ClickVoice::default(); NUM_CLICK_VOICES];
            self.pending = [PendingClick::default(); NUM_PENDING_CLICKS];
            for motor in &mut self.motors {
                motor.spin = 0.0;
            }
        }
        self.enabled = enabled;
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_volume_percent(&mut self, percent: u8) {
        self.volume = f32::from(percent.min(100)) / 100.0;
    }

    /// Report a drive's platter spin level, 0.0 (stopped) to 1.0 (at
    /// speed). The floppy model's motor accumulator already ramps this
    /// over the real ~0.5 s spin-up/spin-down, which gives the hum its
    /// pitch/level glide.
    pub fn set_motor_spin(&mut self, drive: usize, spin: f32) {
        if let Some(motor) = self.motors.get_mut(drive) {
            motor.spin_target = spin.clamp(0.0, 1.0);
        }
    }

    /// Fire one head-step. An isolated step lands as a full clack with
    /// rebound clatter; rapid steps duck the thump and skip the
    /// rebounds so the overlapped rings form the seek buzz comb.
    pub fn step_pulse(&mut self) {
        if !self.enabled {
            return;
        }
        let isolated = self.since_step >= seconds_to_samples(STEP_ISOLATED_GAP_S);
        self.since_step = 0;
        // Small random spread keeps a seek from sounding machine-gun
        // perfect; real steppers never hit twice identically.
        let amp = 0.78 + 0.22 * next_noise_unipolar(&mut self.rng);
        let thump_scale = if isolated { 1.0 } else { SEEK_THUMP_DUCK };
        self.fire_click(amp, 1.0, thump_scale);
        if isolated {
            for i in 0..REBOUND_DELAY_S.len() {
                let delay = REBOUND_DELAY_S[i]
                    + REBOUND_DELAY_JITTER_S * (next_noise_unipolar(&mut self.rng) - 0.5);
                let level = REBOUND_LEVEL[i] * (0.75 + 0.5 * next_noise_unipolar(&mut self.rng));
                self.schedule_click(
                    seconds_to_samples(delay),
                    amp * level,
                    REBOUND_FREQ_SCALE[i],
                );
            }
        }
    }

    fn schedule_click(&mut self, delay_samples: u32, amp: f32, freq_scale: f32) {
        if let Some(slot) = self.pending.iter_mut().find(|p| !p.live) {
            *slot = PendingClick {
                live: true,
                delay_samples,
                amp,
                freq_scale,
            };
        }
    }

    /// Start a clack voice: thump + body ring + tail partial + contact
    /// tick, with per-click pitch jitter on top of the caller's scales.
    fn fire_click(&mut self, amp: f32, freq_scale: f32, thump_scale: f32) {
        let thump_hz =
            CLICK_THUMP_MIN_HZ + CLICK_THUMP_HZ_SPREAD * next_noise_unipolar(&mut self.rng);
        let body_hz = (CLICK_BODY_MIN_HZ
            + CLICK_BODY_HZ_SPREAD * next_noise_unipolar(&mut self.rng))
            * freq_scale;
        let voice = &mut self.clicks[self.next_click_voice];
        self.next_click_voice = (self.next_click_voice + 1) % NUM_CLICK_VOICES;
        voice.thump_env = 1.0;
        voice.thump_phase = 0.0;
        voice.thump_phase_inc = TWO_PI * thump_hz / MIX_SAMPLE_RATE as f32;
        voice.thump_level = CLICK_THUMP_LEVEL * thump_scale;
        voice.body_env = 1.0;
        voice.body_phase = 0.0;
        voice.body_phase_inc = TWO_PI * body_hz / MIX_SAMPLE_RATE as f32;
        voice.ring_env = 1.0;
        voice.ring_phase = 0.0;
        voice.ring_phase_inc = voice.body_phase_inc * CLICK_RING_FREQ_RATIO;
        voice.tick_env = 1.0;
        voice.tick_lp = 0.0;
        voice.tick_hp_lp = 0.0;
        voice.amp = CLICK_GAIN * amp;
    }

    /// Render the next mono sample. Exact 0.0 while idle.
    pub fn next_sample(&mut self) -> f32 {
        // Elapsed time counts even while disabled or muted (Paula
        // mixes every host sample either way), so a step fired after a
        // disable/enable toggle still classifies against the real time
        // since the last audible step instead of a frozen counter.
        self.since_step = self.since_step.saturating_add(1);
        if !self.enabled || self.volume <= 0.0 {
            return 0.0;
        }
        for i in 0..NUM_PENDING_CLICKS {
            if !self.pending[i].live {
                continue;
            }
            if self.pending[i].delay_samples > 0 {
                self.pending[i].delay_samples -= 1;
                continue;
            }
            let p = self.pending[i];
            self.pending[i].live = false;
            // A rebound is a settled-carriage clatter, full thump scale.
            self.fire_click(p.amp, p.freq_scale, 1.0);
        }
        if self.is_idle() {
            return 0.0;
        }

        let mut sample = 0.0f32;
        let spin_smooth = lowpass_coeff(MOTOR_SPIN_SMOOTH_HZ);
        for motor in &mut self.motors {
            motor.spin += spin_smooth * (motor.spin_target - motor.spin);
            if motor.spin < SILENT_LEVEL && motor.spin_target == 0.0 {
                motor.spin = 0.0;
                continue;
            }

            // Hum pitch glides up with spin so the motor audibly winds
            // up rather than fading in at full pitch.
            let pitch = 0.35 + 0.65 * motor.spin;
            let hum_inc = TWO_PI * MOTOR_HUM_BASE_HZ * pitch / MIX_SAMPLE_RATE as f32;
            let mut hum = 0.0f32;
            for (i, &(ratio, amp)) in MOTOR_HUM_PARTIALS.iter().enumerate() {
                motor.hum_phases[i] = wrap_phase(motor.hum_phases[i], hum_inc * ratio);
                hum += amp * motor.hum_phases[i].sin();
            }

            // The revolution phase paces the looped rumble pattern and
            // the once-per-turn wobble; both slow down together as the
            // platter spins up or down.
            let rev_inc = MOTOR_REV_HZ * motor.spin / MIX_SAMPLE_RATE as f32;
            motor.rev_phase += rev_inc;
            if motor.rev_phase >= 1.0 {
                motor.rev_phase -= 1.0;
            }
            let wobble = 1.0 + MOTOR_WOBBLE_DEPTH * (TWO_PI * motor.rev_phase).sin();

            let rev_samples = MIX_SAMPLE_RATE as f32 / MOTOR_REV_HZ;
            let pattern = pattern_noise(motor.seed, (motor.rev_phase * rev_samples) as u32);
            let fresh = next_noise_bipolar(&mut self.rng);
            let noise = (1.0 - MOTOR_PATTERN_FRESH_MIX) * pattern + MOTOR_PATTERN_FRESH_MIX * fresh;
            let rumble_cutoff = MOTOR_RUMBLE_LP_MIN_HZ
                + (MOTOR_RUMBLE_LP_MAX_HZ - MOTOR_RUMBLE_LP_MIN_HZ) * motor.spin;
            let rumble_coeff = lowpass_coeff(rumble_cutoff);
            motor.rumble_lp += rumble_coeff * (noise - motor.rumble_lp);
            motor.rumble_lp2 += rumble_coeff * (motor.rumble_lp - motor.rumble_lp2);

            // Level rises faster than linearly with spin so a barely
            // turning platter is barely audible.
            let level = motor.spin * motor.spin.sqrt();
            sample +=
                level * (MOTOR_HUM_GAIN * hum + MOTOR_RUMBLE_GAIN * wobble * motor.rumble_lp2);
        }

        let thump_decay = decay_per_sample(CLICK_THUMP_DECAY_S);
        let body_decay = decay_per_sample(CLICK_BODY_DECAY_S);
        let ring_decay = decay_per_sample(CLICK_RING_DECAY_S);
        let tick_decay = decay_per_sample(CLICK_TICK_DECAY_S);
        let tick_lp_coeff = lowpass_coeff(CLICK_TICK_LP_HZ);
        let tick_hp_coeff = lowpass_coeff(CLICK_TICK_HP_HZ);
        for voice in &mut self.clicks {
            if !voice.active() {
                continue;
            }
            voice.thump_phase = wrap_phase(voice.thump_phase, voice.thump_phase_inc);
            let thump = voice.thump_phase.sin() * voice.thump_env * voice.thump_level;
            voice.body_phase = wrap_phase(voice.body_phase, voice.body_phase_inc);
            let body = voice.body_phase.sin() * voice.body_env * CLICK_BODY_LEVEL;
            voice.ring_phase = wrap_phase(voice.ring_phase, voice.ring_phase_inc);
            let ring = voice.ring_phase.sin() * voice.ring_env * CLICK_RING_LEVEL;
            let noise = next_noise_bipolar(&mut self.rng);
            voice.tick_lp += tick_lp_coeff * (noise - voice.tick_lp);
            voice.tick_hp_lp += tick_hp_coeff * (voice.tick_lp - voice.tick_hp_lp);
            let tick = (voice.tick_lp - voice.tick_hp_lp) * voice.tick_env * CLICK_TICK_LEVEL;
            sample += voice.amp * (thump + body + ring + tick);
            voice.thump_env *= thump_decay;
            voice.body_env *= body_decay;
            voice.ring_env *= ring_decay;
            voice.tick_env *= tick_decay;
        }

        sample * self.volume
    }

    fn is_idle(&self) -> bool {
        self.motors
            .iter()
            .all(|m| m.spin == 0.0 && m.spin_target == 0.0)
            && self.clicks.iter().all(|v| !v.active())
            && self.pending.iter().all(|p| !p.live)
    }
}

/// Stateless hash of a revolution-pattern index into noise in [-1, 1).
/// The same (seed, index) always yields the same value, which is what
/// loops the rumble texture exactly once per platter revolution.
fn pattern_noise(seed: u32, index: u32) -> f32 {
    let mut x = seed ^ index.wrapping_mul(0x9E37_79B9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    (x as f32 / (u32::MAX as f32 / 2.0)) - 1.0
}

/// xorshift32; deterministic, no host entropy.
fn next_rng(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// Uniform noise in [-1, 1).
fn next_noise_bipolar(state: &mut u32) -> f32 {
    (next_rng(state) as f32 / (u32::MAX as f32 / 2.0)) - 1.0
}

/// Uniform noise in [0, 1).
fn next_noise_unipolar(state: &mut u32) -> f32 {
    next_rng(state) as f32 / u32::MAX as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(sounds: &mut DriveSounds, samples: usize) -> Vec<f32> {
        (0..samples).map(|_| sounds.next_sample()).collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn ms(n: f32) -> usize {
        (MIX_SAMPLE_RATE as f32 * n / 1000.0) as usize
    }

    #[test]
    fn idle_drive_sounds_emit_exact_silence() {
        let mut sounds = DriveSounds::new();
        assert!(render(&mut sounds, 4096).iter().all(|&s| s == 0.0));
    }

    #[test]
    fn step_pulse_clicks_then_decays_back_to_silence() {
        let mut sounds = DriveSounds::new();
        sounds.step_pulse();
        let click = render(&mut sounds, MIX_SAMPLE_RATE as usize / 5);
        assert!(peak(&click) > 0.01, "step produced no click");
        // After 200 ms the click and its rebounds are over and the
        // output is bit-exact 0.
        let tail = render(&mut sounds, 1024);
        assert!(tail.iter().all(|&s| s == 0.0), "click did not fully decay");
    }

    #[test]
    fn isolated_step_carries_rebound_clatter() {
        let mut sounds = DriveSounds::new();
        sounds.step_pulse();
        let out = render(&mut sounds, ms(30.0));
        // The main impact decays with tau 3.5 ms; without the rebounds
        // the 5-13 ms window would hold only a faint tail. The clatter
        // keeps it a substantial fraction of the impact window.
        let impact = rms(&out[..ms(4.0)]);
        let clatter = rms(&out[ms(5.0)..ms(13.0)]);
        assert!(
            clatter > 0.18 * impact,
            "no rebound clatter after isolated step (impact={impact}, clatter={clatter})"
        );
    }

    #[test]
    fn seek_steps_suppress_rebounds_and_thump() {
        let mut sounds = DriveSounds::new();
        // Prime: one isolated step, then let it fully decay.
        sounds.step_pulse();
        render(&mut sounds, MIX_SAMPLE_RATE as usize / 5);
        // A 3 ms burst: every step after the first is a seek step and
        // must not schedule rebounds.
        for _ in 0..10 {
            sounds.step_pulse();
            render(&mut sounds, ms(3.0));
        }
        assert!(
            sounds.pending.iter().filter(|p| p.live).count() == 0,
            "seek steps scheduled rebound clatter"
        );
    }

    #[test]
    fn step_after_disable_toggle_classifies_as_isolated() {
        // Disabling must not freeze the step-spacing clock: a step
        // fired shortly before a disable, with the re-enabled drive
        // stepping much later, is an isolated clack (with rebound
        // clatter), not the continuation of a seek burst.
        let mut sounds = DriveSounds::new();
        sounds.step_pulse();
        render(&mut sounds, ms(3.0)); // well inside the seek gap
        sounds.set_enabled(false);
        render(&mut sounds, ms(30.0)); // time passes while disabled
        sounds.set_enabled(true);
        sounds.step_pulse();
        assert_eq!(
            sounds.pending.iter().filter(|p| p.live).count(),
            REBOUND_DELAY_S.len(),
            "step after a disable toggle lost its rebound clatter"
        );
    }

    #[test]
    fn motor_spin_produces_hum_and_spindown_returns_to_silence() {
        let mut sounds = DriveSounds::new();
        sounds.set_motor_spin(0, 1.0);
        let hum = render(&mut sounds, MIX_SAMPLE_RATE as usize / 5);
        assert!(peak(&hum) > 0.005, "motor produced no hum");
        sounds.set_motor_spin(0, 0.0);
        // Let the smoothed level decay, then expect exact silence.
        render(&mut sounds, MIX_SAMPLE_RATE as usize);
        let tail = render(&mut sounds, 1024);
        assert!(tail.iter().all(|&s| s == 0.0), "motor did not spin down");
    }

    #[test]
    fn motor_spin_texture_repeats_each_revolution() {
        // Real spin recordings autocorrelate at the 200 ms revolution
        // lag; the synthesized rumble must too, or the spin reads as
        // formless noise wash. Correlate two consecutive revolutions
        // of the settled motor bed.
        let mut sounds = DriveSounds::new();
        sounds.set_motor_spin(0, 1.0);
        render(&mut sounds, MIX_SAMPLE_RATE as usize); // settle spin
        let rev = (MIX_SAMPLE_RATE as f32 / MOTOR_REV_HZ) as usize;
        let out = render(&mut sounds, 2 * rev);
        let (a, b) = out.split_at(rev);
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        let corr = dot / (na * nb).max(1.0e-12);
        assert!(
            corr > 0.5,
            "spin texture does not repeat at the revolution lag (corr={corr})"
        );
    }

    #[test]
    fn motor_bed_is_dark_not_hissy() {
        // The spinning bed must live low: first-difference (high
        // frequency) energy far below what white-ish hiss would carry.
        // Guards against regressing back to a "shhhh" wash; the
        // read-gated hiss layer this replaced was removed on expert
        // listening feedback.
        let mut sounds = DriveSounds::new();
        sounds.set_motor_spin(0, 1.0);
        render(&mut sounds, MIX_SAMPLE_RATE as usize / 2);
        let out = render(&mut sounds, 16384);
        let total: f32 = out.iter().map(|s| s * s).sum();
        let hf: f32 = out.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
        // For white noise hf/total ~= 2; the hum-and-rumble bed must
        // sit far below.
        assert!(
            hf / total < 0.2,
            "motor bed too bright (hf/total={})",
            hf / total
        );
    }

    #[test]
    fn click_energy_centers_on_the_carriage_resonance() {
        // Goertzel probes: the 1 kHz body must dominate both the low
        // thump and the 3 kHz region, matching the measured spectra of
        // real drive clicks.
        fn goertzel(samples: &[f32], hz: f32) -> f32 {
            let w = TWO_PI * hz / MIX_SAMPLE_RATE as f32;
            let coeff = 2.0 * w.cos();
            let (mut s1, mut s2) = (0.0f32, 0.0f32);
            for &x in samples {
                let s0 = x + coeff * s1 - s2;
                s2 = s1;
                s1 = s0;
            }
            (s1 * s1 + s2 * s2 - coeff * s1 * s2).sqrt()
        }
        let mut sounds = DriveSounds::new();
        sounds.step_pulse();
        let out = render(&mut sounds, ms(20.0));
        let body = goertzel(&out, 1080.0);
        let low = goertzel(&out, 300.0);
        let high = goertzel(&out, 3200.0);
        assert!(
            body > low && body > high,
            "click spectrum off target (300Hz={low}, 1080Hz={body}, 3200Hz={high})"
        );
    }

    #[test]
    fn disabled_drive_sounds_are_silent_and_drop_pending_voices() {
        let mut sounds = DriveSounds::new();
        sounds.set_motor_spin(0, 1.0);
        sounds.step_pulse();
        sounds.set_enabled(false);
        assert!(render(&mut sounds, 4096).iter().all(|&s| s == 0.0));
        // Re-enabling does not replay the old click; motor spin target
        // still applies, so silence requires the motor off too.
        sounds.set_motor_spin(0, 0.0);
        sounds.set_enabled(true);
        assert!(render(&mut sounds, 4096).iter().all(|&s| s == 0.0));
    }

    #[test]
    fn volume_percent_scales_output() {
        let mut loud = DriveSounds::new();
        loud.set_volume_percent(100);
        loud.step_pulse();
        let mut quiet = DriveSounds::new();
        quiet.set_volume_percent(25);
        quiet.step_pulse();
        // Same deterministic seed, same click; only the volume differs.
        let loud_peak = peak(&render(&mut loud, 2048));
        let quiet_peak = peak(&render(&mut quiet, 2048));
        assert!(loud_peak > 0.0);
        assert!((quiet_peak / loud_peak - 0.25).abs() < 0.01);
    }

    #[test]
    fn seek_burst_stays_within_headroom() {
        let mut sounds = DriveSounds::new();
        sounds.set_motor_spin(0, 1.0);
        // 80-track seek at ~3 ms per step, all voices overlapping.
        let step_interval = (MIX_SAMPLE_RATE as usize * 3) / 1000;
        let mut rendered = Vec::new();
        for _ in 0..80 {
            sounds.step_pulse();
            rendered.extend(render(&mut sounds, step_interval));
        }
        assert!(peak(&rendered) < 1.0, "seek burst clips");
    }
}
