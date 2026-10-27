// SPDX-License-Identifier: GPL-3.0-or-later

//! Paced JIT throughput: keep a bounded regular CPU workload, then spend the
//! time before presentation on CPU-only bursts. The baseline governor can
//! back off to the configured clock when chipset/frontend work is heavy.
//! That floor keeps STOP and bus-bound guests responsive after overload;
//! lowering CPU speed cannot cure a host overloaded by chipset work alone.
//! Burst time is excluded from its load sample; measured throughput includes
//! all instructions. Unpaced execution never samples or adds bursts.

use crate::timebase::{Duration, Instant};

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
/// Highest billed clock the governor will reach, in MHz: far above what
/// any current host sustains, there only as a sanity bound. Spare-time CPU
/// throughput is independent of this bound.
const MAX_BASELINE_MHZ: f64 = 2000.0;

/// Leave time for presentation and host scheduling jitter. This is a fixed
/// deadline for the entire burst phase, even if a bus access advances time.
pub(crate) fn spare_deadline(target: Instant, quantum_seconds: f64) -> Option<Instant> {
    let reserve = (quantum_seconds * 0.10).clamp(0.0002, 0.002);
    target.checked_sub(Duration::from_secs_f64(reserve))
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct QuantumSample {
    /// Regular emulation cost only, excluding spare CPU work and pacing sleep.
    pub busy: Duration,
    /// Frontend work between quanta; reported presentation waits are excluded.
    pub gap: Duration,
    /// Guest time advanced in this quantum. A window also closes on host time
    /// so overload is corrected before a slow guest accumulates 0.1 seconds.
    pub emulated_seconds: f64,
    /// Includes instructions retired in spare-time bursts.
    pub instructions: u64,
    /// Regular instructions only, used to distinguish clock-bound work from
    /// STOP/shared-bus waits. Spare bursts must not trigger baseline increases.
    pub regular_instructions: u64,
    /// Precise/debugged quanta cannot measure the JIT clock and are discarded.
    pub jit_active: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Decision {
    pub load: f64,
    pub achieved_mips: f64,
    pub from_mhz: f64,
    pub to_mhz: f64,
}
#[derive(Clone, Debug)]
pub(crate) struct SpeedGovernor {
    configured_mhz: f64,
    baseline_mhz: f64,
    measured_mips: Option<f64>,
    adapted: bool,
    host_seconds: f64,
    emulated_seconds: f64,
    instructions: u64,
    regular_instructions: u64,
}
impl SpeedGovernor {
    pub(crate) fn new(configured_mhz: f64) -> Self {
        Self {
            configured_mhz,
            baseline_mhz: configured_mhz,
            measured_mips: None,
            adapted: false,
            host_seconds: 0.0,
            emulated_seconds: 0.0,
            instructions: 0,
            regular_instructions: 0,
        }
    }
    /// Equivalent one-instruction-per-clock rate, including spare CPU work.
    pub(crate) fn effective_mhz(&self) -> f64 {
        self.measured_mips.unwrap_or(self.baseline_mhz)
    }
    pub(crate) fn factor(&self) -> f64 {
        self.baseline_mhz / self.configured_mhz
    }
    /// Paced quanta use the regular-work baseline. Warp holds measured total
    /// throughput, including spare work, but never drops below the configured
    /// clock. State loads and pacing transitions must use the same rule.
    pub(crate) fn factor_for_pacing(&self, paced: bool) -> f64 {
        if paced {
            self.factor()
        } else {
            self.effective_mhz().max(self.configured_mhz) / self.configured_mhz
        }
    }

    pub(crate) fn discard_window(&mut self) {
        self.host_seconds = 0.0;
        self.emulated_seconds = 0.0;
        self.instructions = 0;
        self.regular_instructions = 0;
    }
    /// A state load follows its configured clock until adaptation has occurred.
    /// Thereafter keep the adapted baseline within the new bounds and restart
    /// measured-throughput reporting. No wall-clock history enters the state.
    pub(crate) fn rebase(&mut self, configured_mhz: f64) {
        self.configured_mhz = configured_mhz;
        self.baseline_mhz = if self.adapted {
            self.baseline_mhz
                .clamp(configured_mhz, MAX_BASELINE_MHZ.max(configured_mhz))
        } else {
            configured_mhz
        };
        self.measured_mips = None;
        self.discard_window();
    }
    pub(crate) fn observe(&mut self, sample: QuantumSample) -> Option<Decision> {
        if sample.gap > HITCH_GAP || !sample.jit_active {
            self.discard_window();
            return None;
        }
        self.host_seconds += (sample.busy + sample.gap).as_secs_f64();
        self.emulated_seconds += sample.emulated_seconds.max(0.0);
        self.instructions = self.instructions.saturating_add(sample.instructions);
        self.regular_instructions = self
            .regular_instructions
            .saturating_add(sample.regular_instructions);
        if self.emulated_seconds < WINDOW_SECONDS && self.host_seconds < WINDOW_SECONDS {
            return None;
        }
        let seconds = self.emulated_seconds;
        let host = self.host_seconds;
        let instructions = self.instructions;
        let regular_instructions = self.regular_instructions;
        self.discard_window();
        if seconds <= 0.0 {
            return None;
        }
        let load = host / seconds;
        let achieved_mips = instructions as f64 / seconds / 1.0e6;
        self.measured_mips = Some(achieved_mips);
        let regular_mips = regular_instructions as f64 / seconds / 1.0e6;
        let clock_bound = regular_mips >= CLOCK_BOUND_FRACTION * self.baseline_mhz;
        let correction = (TARGET_LOAD / load.max(f64::EPSILON)).powf(STEP_GAIN);
        let step = if load > LOWER_ABOVE_LOAD {
            correction.max(MAX_LOWER_STEP)
        } else if load < RAISE_BELOW_LOAD && clock_bound {
            correction.min(MAX_RAISE_STEP)
        } else {
            return None;
        };
        let from_mhz = self.baseline_mhz;
        let to_mhz = (from_mhz * step).clamp(
            self.configured_mhz,
            MAX_BASELINE_MHZ.max(self.configured_mhz),
        );
        if from_mhz == to_mhz {
            return None;
        }
        self.baseline_mhz = to_mhz;
        self.adapted = true;
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
    fn sample(busy: f64) -> QuantumSample {
        QuantumSample {
            busy: Duration::from_secs_f64(busy),
            emulated_seconds: 0.1,
            instructions: 50_000_000,
            regular_instructions: 50_000_000,
            jit_active: true,
            ..Default::default()
        }
    }
    #[test]
    fn regular_work_that_fits_does_not_back_off_to_half_load() {
        let mut g = SpeedGovernor::new(25.0);
        for _ in 0..20 {
            g.observe(sample(0.085));
        }
        assert_eq!(g.factor(), 1.0);
    }

    #[test]
    fn spare_instructions_do_not_make_idle_regular_work_clock_bound() {
        let mut g = SpeedGovernor::new(25.0);
        for _ in 0..20 {
            g.observe(QuantumSample {
                regular_instructions: 0,
                ..sample(0.01)
            });
        }
        assert_eq!(g.factor(), 1.0);
        assert_eq!(
            g.effective_mhz(),
            500.0,
            "telemetry still includes spare work"
        );
        for _ in 0..10 {
            g.observe(sample(0.01));
        }
        assert!(g.factor() > 1.0, "regular clock-bound work can accelerate");
    }

    #[test]
    fn host_stalls_and_precise_stepping_discard_the_window() {
        let mut g = SpeedGovernor::new(25.0);
        for s in [
            QuantumSample {
                gap: Duration::from_secs(1),
                ..sample(1.0)
            },
            QuantumSample {
                jit_active: false,
                ..sample(1.0)
            },
        ] {
            assert_eq!(g.observe(s), None);
            assert_eq!(g.factor(), 1.0);
            assert_eq!(g.instructions, 0);
        }
    }
    #[test]
    fn rebase_preserves_an_adapted_baseline_but_not_measured_throughput() {
        let mut g = SpeedGovernor::new(25.0);
        g.rebase(50.0);
        assert_eq!(g.factor(), 1.0);
        g.observe(sample(0.01));
        let baseline = g.baseline_mhz;
        g.rebase(40.0);
        assert_eq!(g.effective_mhz(), baseline);
        assert_eq!(g.factor(), baseline / 40.0);
    }
    #[test]
    fn chipset_overload_and_idle_recovery_preserve_the_configured_floor() {
        let mut governor = SpeedGovernor::new(25.0);
        for _ in 0..100 {
            governor.observe(QuantumSample {
                regular_instructions: 0,
                ..sample(0.11)
            });
        }
        assert_eq!(
            governor.factor(),
            1.0,
            "chipset work alone exceeds capacity"
        );
        for _ in 0..100 {
            governor.observe(QuantumSample {
                regular_instructions: 0,
                ..sample(0.01)
            });
        }
        assert_eq!(
            governor.factor(),
            1.0,
            "idle work must not retain a reduced clock"
        );
    }

    #[test]
    fn burst_deadline_reserves_presentation_time() {
        let target = Instant::now();
        assert_eq!(
            target.duration_since(spare_deadline(target, 0.02).unwrap()),
            Duration::from_millis(2)
        );
    }

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
            let mips = governor.baseline_mhz * self.clock_bound;
            let busy = (self.fixed + self.per_mips * mips) * QUANTUM;
            QuantumSample {
                busy: Duration::from_secs_f64(busy),
                gap: self.gap,
                emulated_seconds: QUANTUM,
                instructions: (mips * 1.0e6 * QUANTUM) as u64,
                regular_instructions: (mips * 1.0e6 * QUANTUM) as u64,
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
            governor.baseline_mhz
        );
        assert!(governor.baseline_mhz > host.clock_for_load(RAISE_BELOW_LOAD) - 1.0);
        let settled = governor.baseline_mhz;
        host.run(&mut governor, 60 * 10);
        assert_eq!(governor.baseline_mhz, settled, "no hunting once settled");
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
            governor.baseline_mhz > 200.0,
            "{:.1} MHz after 2 s",
            governor.baseline_mhz
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
        let before = governor.baseline_mhz;
        // Another process takes half the core: everything costs twice the
        // wall time.
        let loaded = Host {
            fixed: fast.fixed * 2.0,
            per_mips: fast.per_mips * 2.0,
            ..fast
        };
        loaded.run(&mut governor, 60);
        let after = governor.baseline_mhz;
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
        assert_eq!(governor.baseline_mhz, 25.0);
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
        assert_eq!(governor.baseline_mhz, 25.0);
        assert_eq!(governor.factor(), 1.0);
    }

    #[test]
    fn sudden_overload_is_cut_on_host_time() {
        let mut governor = SpeedGovernor::new(25.0);
        governor.baseline_mhz = 1000.0;
        // One quantum that takes 400 ms of host time for 1/60 s of
        // emulated time (24x overloaded) closes the window on host time
        // and cuts as hard as allowed.
        let decision = governor
            .observe(QuantumSample {
                busy: Duration::from_millis(400),
                gap: Duration::ZERO,
                emulated_seconds: QUANTUM,
                instructions: 1_000_000,
                regular_instructions: 1_000_000,
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
            regular_instructions: 400_000,
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
        assert_eq!(governor.host_seconds, 0.0);
        // A precise-core quantum (debugger hook armed) likewise says
        // nothing about the JIT clock.
        assert_eq!(governor.observe(heavy), None);
        let precise = QuantumSample {
            jit_active: false,
            ..heavy
        };
        assert_eq!(governor.observe(precise), None);
        assert_eq!(governor.instructions, 0);
        assert_eq!(governor.baseline_mhz, 25.0);
    }

    #[test]
    fn rebase_follows_the_new_floor_until_the_clock_has_adapted() {
        // Never adapted (an unpaced run): the governor is the fixed clock,
        // so a state with a different clock is followed exactly, whichever
        // way it moves.
        let mut governor = SpeedGovernor::new(24.83);
        governor.rebase(14.18);
        assert_eq!(governor.baseline_mhz, 14.18);
        assert_eq!(governor.factor(), 1.0);
        governor.rebase(49.66);
        assert_eq!(governor.factor(), 1.0);
        // Once a paced window has moved it, the reached clock is kept.
        let window = QuantumSample {
            busy: Duration::from_millis(10),
            gap: Duration::ZERO,
            emulated_seconds: WINDOW_SECONDS,
            instructions: 10_000_000,
            regular_instructions: 10_000_000,
            jit_active: true,
        };
        assert!(governor.observe(window).is_some());
        let reached = governor.baseline_mhz;
        governor.rebase(24.83);
        assert_eq!(governor.baseline_mhz, reached);
    }

    #[test]
    fn rebase_keeps_the_effective_clock_within_the_new_bounds() {
        let mut governor = SpeedGovernor::new(25.0);
        governor.baseline_mhz = 300.0;
        governor.adapted = true;
        governor.rebase(50.0);
        assert_eq!(governor.baseline_mhz, 300.0);
        assert_eq!(governor.factor(), 6.0);
        governor.rebase(400.0);
        assert_eq!(governor.baseline_mhz, 400.0);
        assert_eq!(governor.factor(), 1.0);
        let mut high = SpeedGovernor::new(MAX_BASELINE_MHZ * 2.0);
        assert_eq!(high.factor(), 1.0);
        let saturated = QuantumSample {
            busy: Duration::from_millis(1),
            gap: Duration::ZERO,
            emulated_seconds: WINDOW_SECONDS,
            instructions: u64::MAX / 2,
            regular_instructions: u64::MAX / 2,
            jit_active: true,
        };
        assert_eq!(
            high.observe(saturated),
            None,
            "a floor above the cap stays fixed"
        );
    }
}
