// SPDX-License-Identifier: GPL-3.0-or-later

//! `[cpu] speed = "fastest"`: the host-adaptive JIT clock governor.
//!
//! WinUAE's "fastest possible" CPU keeps the chipset and emulated time in
//! real time and hands the CPU whatever host time each frame leaves over.
//! Copperline gets the same effect without a second timing model: the JIT
//! keeps billing CPU clocks exactly as it does at a fixed clock, and this
//! governor only decides how fast that clock is (`M68kMachine::
//! set_jit_speed_factor`). Beam, DMA, CIA and Paula therefore stay as
//! consistent as at any fixed `clock_mhz`; only the clock moves.
//!
//! The governor is fed one sample per real-time quantum (`step_real_until`)
//! and, every `WINDOW_SECONDS` of emulated or host time, compares the host
//! time the main thread spent -- emulating plus the frontend's work between
//! quanta, pacing sleep excluded -- with the emulated time that advanced.
//! That load ratio is 1.0 when the host only just keeps up. Above
//! `LOWER_ABOVE_LOAD` the clock drops, below `RAISE_BELOW_LOAD` it rises
//! (at most `MAX_RAISE_STEP` per window), each time by the square root of
//! the ratio between the target and the measured load, but it only rises
//! when the CPU is actually clock-bound: a CPU idling in
//! STOP, or one starved by chip-bus waits, retires far fewer instructions
//! than its clock allows, and a faster clock would buy it nothing but a
//! hiccup when the workload changes. Host time is wall time, so a busy host
//! (other processes, a slower core) shows up as load and the clock backs
//! off.
//!
//! Nothing here runs unless the mode is configured, and the governor never
//! adapts outside paced quanta: headless captures and benchmarks have no
//! wall clock to keep up with, so they stay at the configured floor (and are
//! as deterministic as the fixed-clock JIT), and warp holds whatever clock
//! the paced run had reached.

use crate::timebase::Duration;

/// Share of the main thread's host time the governor aims to fill: the rest
/// is headroom for host jitter, so frames still land on time.
const TARGET_LOAD: f64 = 0.85;
/// Raise the clock only when the load is below this...
const RAISE_BELOW_LOAD: f64 = 0.80;
/// ...and lower it when above this; in between, hold (the dead band stops
/// measurement noise from hunting).
const LOWER_ABOVE_LOAD: f64 = 0.93;
/// How hard one window corrects: the clock moves by `(TARGET_LOAD /
/// load)^STEP_GAIN`. A full proportional step (gain 1) overshoots near
/// saturation, where frame cost rises faster than the clock (late frames,
/// scheduler effects), and makes the clock hunt between two values; half
/// the log-gain settles instead. Far from the target the step caps below
/// dominate either way.
const STEP_GAIN: f64 = 0.5;
/// Largest clock increase per window. Host cost grows less than in
/// proportion to the clock (chipset work is fixed per emulated second), so
/// the steps approach the target from below; the cap bounds how far one
/// optimistic window can overshoot.
const MAX_RAISE_STEP: f64 = 1.25;
/// Largest clock decrease per window, for a sudden heavy workload at a high
/// clock. The window also closes on host time, so an overloaded clock is
/// cut within `WINDOW_SECONDS` of wall time, not of (slowed) emulated time.
const MAX_LOWER_STEP: f64 = 0.25;
/// Evaluation window, in emulated or host seconds, whichever fills first.
const WINDOW_SECONDS: f64 = 0.1;
/// A gap between quanta longer than this is a host stall outside the
/// emulator (paused machine, modal dialog, debugger break) rather than
/// frontend load; the window it lands in is discarded.
const HITCH_GAP: Duration = Duration::from_millis(100);
/// The CPU counts as clock-bound when it retires at least this fraction of
/// one instruction per billed clock (the JIT's ideal rate). Below it, the
/// CPU spends most of its time in STOP or in chip-bus waits.
const CLOCK_BOUND_FRACTION: f64 = 0.25;
/// Highest effective clock the governor will reach, in MHz: far above what
/// any current host sustains, there only as a sanity bound.
pub(crate) const CEILING_MHZ: f64 = 2000.0;

/// One real-time quantum, as `step_real_until` saw it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct QuantumSample {
    /// Host time spent emulating the quantum, pacing sleep excluded.
    pub busy: Duration,
    /// Host time between the previous quantum's end and this one's start:
    /// the frontend's share of the main thread.
    pub gap: Duration,
    /// Emulated seconds the quantum advanced.
    pub emulated_seconds: f64,
    /// Instructions the CPU retired in it.
    pub instructions: u64,
    /// Whether the CPU ran on the JIT batch path. A debug hook forcing the
    /// precise core makes the clock irrelevant, so such quanta are ignored.
    pub jit_active: bool,
}

/// What one evaluated window decided, for the log.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Decision {
    pub load: f64,
    pub achieved_mips: f64,
    pub from_mhz: f64,
    pub to_mhz: f64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Window {
    host_seconds: f64,
    emulated_seconds: f64,
    instructions: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct SpeedGovernor {
    floor_mhz: f64,
    ceiling_mhz: f64,
    effective_mhz: f64,
    window: Window,
}

impl SpeedGovernor {
    /// A governor starting at, and never dropping below, `floor_mhz` (the
    /// configured CPU clock).
    pub(crate) fn new(floor_mhz: f64) -> Self {
        Self {
            floor_mhz,
            ceiling_mhz: CEILING_MHZ.max(floor_mhz),
            effective_mhz: floor_mhz,
            window: Window::default(),
        }
    }

    /// The effective JIT clock, in MHz.
    pub(crate) fn effective_mhz(&self) -> f64 {
        self.effective_mhz
    }

    /// The JIT speed factor to apply: effective clock over the floor.
    pub(crate) fn factor(&self) -> f64 {
        self.effective_mhz / self.floor_mhz
    }

    /// Forget the partly filled window. For anything that breaks the link
    /// between host and emulated time: warp, a pause, a timeline jump.
    pub(crate) fn discard_window(&mut self) {
        self.window = Window::default();
    }

    /// Move the floor (a loaded save state brought a differently clocked
    /// CPU), keeping the effective clock where the new bounds allow.
    pub(crate) fn rebase(&mut self, floor_mhz: f64) {
        self.floor_mhz = floor_mhz;
        self.ceiling_mhz = CEILING_MHZ.max(floor_mhz);
        self.effective_mhz = self.effective_mhz.clamp(floor_mhz, self.ceiling_mhz);
        self.discard_window();
    }

    /// Feed one paced quantum. Returns the decision when the window closed
    /// and the clock changed.
    pub(crate) fn observe(&mut self, sample: QuantumSample) -> Option<Decision> {
        if sample.gap > HITCH_GAP || !sample.jit_active {
            self.discard_window();
            return None;
        }
        self.window.host_seconds += (sample.busy + sample.gap).as_secs_f64();
        self.window.emulated_seconds += sample.emulated_seconds.max(0.0);
        self.window.instructions = self.window.instructions.saturating_add(sample.instructions);
        if self.window.emulated_seconds < WINDOW_SECONDS
            && self.window.host_seconds < WINDOW_SECONDS
        {
            return None;
        }
        let window = std::mem::take(&mut self.window);
        if window.emulated_seconds <= 0.0 {
            return None;
        }
        let load = window.host_seconds / window.emulated_seconds;
        let achieved_mips = window.instructions as f64 / window.emulated_seconds / 1.0e6;
        let clock_bound = achieved_mips >= CLOCK_BOUND_FRACTION * self.effective_mhz;
        let correction = (TARGET_LOAD / load.max(f64::EPSILON)).powf(STEP_GAIN);
        let step = if load > LOWER_ABOVE_LOAD {
            correction.max(MAX_LOWER_STEP)
        } else if load < RAISE_BELOW_LOAD && clock_bound {
            correction.min(MAX_RAISE_STEP)
        } else {
            return None;
        };
        let from_mhz = self.effective_mhz;
        let to_mhz = (from_mhz * step).clamp(self.floor_mhz, self.ceiling_mhz);
        if to_mhz == from_mhz {
            return None;
        }
        self.effective_mhz = to_mhz;
        Some(Decision {
            load,
            achieved_mips,
            from_mhz,
            to_mhz,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A simulated host: emulating one emulated second costs `fixed` host
    /// seconds of chipset work plus `per_mips` host seconds per MIPS the CPU
    /// retires, and the frontend adds `gap` per quantum. The CPU retires
    /// `clock_bound` of its effective clock in MIPS.
    struct Host {
        fixed: f64,
        per_mips: f64,
        gap: Duration,
        clock_bound: f64,
    }

    const QUANTUM: f64 = 1.0 / 60.0;

    impl Host {
        fn sample(&self, governor: &SpeedGovernor) -> QuantumSample {
            let mips = governor.effective_mhz() * self.clock_bound;
            let busy = (self.fixed + self.per_mips * mips) * QUANTUM;
            QuantumSample {
                busy: Duration::from_secs_f64(busy),
                gap: self.gap,
                emulated_seconds: QUANTUM,
                instructions: (mips * 1.0e6 * QUANTUM) as u64,
                jit_active: true,
            }
        }

        fn run(&self, governor: &mut SpeedGovernor, quanta: usize) {
            for _ in 0..quanta {
                let sample = self.sample(governor);
                governor.observe(sample);
            }
        }

        /// The clock at which this host's load is exactly `load`.
        fn clock_for_load(&self, load: f64) -> f64 {
            let frontend = self.gap.as_secs_f64() / QUANTUM;
            (load - self.fixed - frontend) / (self.per_mips * self.clock_bound)
        }
    }

    fn load_at(host: &Host, governor: &SpeedGovernor) -> f64 {
        let s = host.sample(governor);
        (s.busy + s.gap).as_secs_f64() / s.emulated_seconds
    }

    #[test]
    fn converges_into_the_dead_band_below_the_host_ceiling() {
        // 25 MHz floor; the host sustains about 95 MIPS flat out
        // (0.05 + 0.01 * 95 = 1.0): fastest should settle within the
        // dead band, well above the floor, and stay there.
        let host = Host {
            fixed: 0.05,
            per_mips: 0.01,
            gap: Duration::from_millis(1),
            clock_bound: 1.0,
        };
        let mut governor = SpeedGovernor::new(25.0);
        host.run(&mut governor, 60 * 10);
        let load = load_at(&host, &governor);
        assert!(
            (RAISE_BELOW_LOAD..=LOWER_ABOVE_LOAD).contains(&load),
            "settled load {load:.3} at {:.1} MHz",
            governor.effective_mhz()
        );
        assert!(governor.effective_mhz() > host.clock_for_load(RAISE_BELOW_LOAD) - 1.0);
        let settled = governor.effective_mhz();
        host.run(&mut governor, 60 * 10);
        assert_eq!(governor.effective_mhz(), settled, "no hunting once settled");
    }

    #[test]
    fn ramps_from_the_floor_within_a_couple_of_seconds() {
        let host = Host {
            fixed: 0.05,
            per_mips: 0.002,
            gap: Duration::ZERO,
            clock_bound: 1.0,
        };
        let mut governor = SpeedGovernor::new(25.0);
        host.run(&mut governor, 60 * 2);
        assert!(
            governor.effective_mhz() > 200.0,
            "{:.1} MHz after 2 s",
            governor.effective_mhz()
        );
    }

    #[test]
    fn backs_off_when_the_host_gets_slower() {
        let fast = Host {
            fixed: 0.05,
            per_mips: 0.005,
            gap: Duration::from_millis(1),
            clock_bound: 1.0,
        };
        let mut governor = SpeedGovernor::new(25.0);
        fast.run(&mut governor, 60 * 10);
        let before = governor.effective_mhz();
        // Another process takes half the core: everything costs twice the
        // wall time.
        let loaded = Host {
            fixed: fast.fixed * 2.0,
            per_mips: fast.per_mips * 2.0,
            ..fast
        };
        loaded.run(&mut governor, 60);
        let after = governor.effective_mhz();
        assert!(after < before * 0.6, "{before:.1} -> {after:.1} MHz");
        assert!(load_at(&loaded, &governor) <= LOWER_ABOVE_LOAD);
    }

    #[test]
    fn idle_or_bus_bound_cpu_does_not_ramp_the_clock() {
        // A CPU sitting in STOP (or waiting on the chip bus) retires a
        // small fraction of its clock: the host has headroom, but a faster
        // clock would buy nothing, so it holds at the floor.
        let host = Host {
            fixed: 0.10,
            per_mips: 0.002,
            gap: Duration::ZERO,
            clock_bound: 0.05,
        };
        let mut governor = SpeedGovernor::new(25.0);
        host.run(&mut governor, 60 * 10);
        assert_eq!(governor.effective_mhz(), 25.0);
    }

    #[test]
    fn overload_at_the_floor_holds_the_floor() {
        // A host too slow for even the configured clock runs below real
        // time, exactly as the fixed-clock JIT would; never below the floor.
        let host = Host {
            fixed: 0.9,
            per_mips: 0.02,
            gap: Duration::ZERO,
            clock_bound: 1.0,
        };
        let mut governor = SpeedGovernor::new(25.0);
        host.run(&mut governor, 60 * 5);
        assert_eq!(governor.effective_mhz(), 25.0);
        assert_eq!(governor.factor(), 1.0);
    }

    #[test]
    fn sudden_overload_is_cut_on_host_time() {
        let mut governor = SpeedGovernor::new(25.0);
        governor.effective_mhz = 1000.0;
        // One quantum that takes 400 ms of host time for 1/60 s of
        // emulated time (24x overloaded) closes the window on host time
        // and cuts as hard as allowed.
        let decision = governor
            .observe(QuantumSample {
                busy: Duration::from_millis(400),
                gap: Duration::ZERO,
                emulated_seconds: QUANTUM,
                instructions: 1_000_000,
                jit_active: true,
            })
            .expect("window closes on host time");
        assert_eq!(decision.from_mhz, 1000.0);
        assert_eq!(decision.to_mhz, 1000.0 * MAX_LOWER_STEP);
    }

    #[test]
    fn hitches_and_precise_quanta_discard_the_window() {
        let mut governor = SpeedGovernor::new(25.0);
        let heavy = QuantumSample {
            busy: Duration::from_millis(30),
            gap: Duration::ZERO,
            emulated_seconds: QUANTUM,
            instructions: 400_000,
            jit_active: true,
        };
        // Half a window of overload, then a long frontend stall (a pause),
        // then the rest of the window: the stall must not count, and the
        // overload before it is forgotten with it.
        for _ in 0..3 {
            assert_eq!(governor.observe(heavy), None);
        }
        let stall = QuantumSample {
            gap: Duration::from_secs(2),
            ..heavy
        };
        assert_eq!(governor.observe(stall), None);
        assert_eq!(governor.window.host_seconds, 0.0);
        // A precise-core quantum (debugger hook armed) likewise says
        // nothing about the JIT clock.
        assert_eq!(governor.observe(heavy), None);
        let precise = QuantumSample {
            jit_active: false,
            ..heavy
        };
        assert_eq!(governor.observe(precise), None);
        assert_eq!(governor.window.instructions, 0);
        assert_eq!(governor.effective_mhz(), 25.0);
    }

    #[test]
    fn rebase_keeps_the_effective_clock_within_the_new_bounds() {
        let mut governor = SpeedGovernor::new(25.0);
        governor.effective_mhz = 300.0;
        governor.rebase(50.0);
        assert_eq!(governor.effective_mhz(), 300.0);
        assert_eq!(governor.factor(), 6.0);
        governor.rebase(400.0);
        assert_eq!(governor.effective_mhz(), 400.0);
        assert_eq!(governor.factor(), 1.0);
        let mut high = SpeedGovernor::new(CEILING_MHZ * 2.0);
        assert_eq!(high.factor(), 1.0);
        let saturated = QuantumSample {
            busy: Duration::from_millis(1),
            gap: Duration::ZERO,
            emulated_seconds: WINDOW_SECONDS,
            instructions: u64::MAX / 2,
            jit_active: true,
        };
        assert_eq!(
            high.observe(saturated),
            None,
            "a floor above the cap stays fixed"
        );
    }
}
