// SPDX-License-Identifier: GPL-3.0-or-later

//! Paced JIT throughput: keep a bounded regular CPU workload, then spend the
//! time before presentation on CPU-only bursts. The baseline governor can
//! back off below the configured clock when chipset/frontend work is heavy.
//! Burst time is excluded from its load sample; measured throughput includes
//! all instructions. Unpaced execution never samples or adds bursts.

use crate::timebase::{Duration, Instant};
const WINDOW_SECONDS: f64 = 0.1;
const HITCH_GAP: Duration = Duration::from_millis(100);
const TARGET_LOAD: f64 = 0.50;
const RAISE_BELOW_LOAD: f64 = 0.35;
const LOWER_ABOVE_LOAD: f64 = 0.65;
const MIN_MHZ: f64 = 1.0;

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
    pub gap: Duration,
    pub emulated_seconds: f64,
    /// Includes instructions retired in spare-time bursts.
    pub instructions: u64,
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
        }
    }
    /// Equivalent one-instruction-per-clock rate, including spare CPU work.
    pub(crate) fn effective_mhz(&self) -> f64 {
        self.measured_mips.unwrap_or(self.baseline_mhz)
    }
    pub(crate) fn factor(&self) -> f64 {
        self.baseline_mhz / self.configured_mhz
    }
    pub(crate) fn discard_window(&mut self) {
        self.host_seconds = 0.0;
        self.emulated_seconds = 0.0;
        self.instructions = 0;
    }
    pub(crate) fn rebase(&mut self, configured_mhz: f64) {
        self.configured_mhz = configured_mhz;
        self.baseline_mhz = if self.adapted {
            self.baseline_mhz
                .clamp(MIN_MHZ.min(configured_mhz), configured_mhz)
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
        if self.emulated_seconds < WINDOW_SECONDS && self.host_seconds < WINDOW_SECONDS {
            return None;
        }
        let seconds = self.emulated_seconds;
        let host = self.host_seconds;
        let instructions = self.instructions;
        self.discard_window();
        if seconds <= 0.0 {
            return None;
        }
        let load = host / seconds;
        let achieved_mips = instructions as f64 / seconds / 1.0e6;
        self.measured_mips = Some(achieved_mips);
        let correction = (TARGET_LOAD / load.max(f64::EPSILON)).sqrt();
        let step = if load > LOWER_ABOVE_LOAD {
            correction.max(0.25)
        } else if load < RAISE_BELOW_LOAD {
            correction.min(1.25)
        } else {
            return None;
        };
        let from_mhz = self.baseline_mhz;
        let to_mhz = (from_mhz * step).clamp(MIN_MHZ.min(self.configured_mhz), self.configured_mhz);
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
            jit_active: true,
            ..Default::default()
        }
    }
    #[test]
    fn overload_can_reduce_the_baseline_below_the_configured_clock() {
        let mut g = SpeedGovernor::new(25.0);
        for _ in 0..20 {
            g.observe(sample(0.2));
        }
        assert_eq!(g.baseline_mhz, 1.0);
        assert_eq!(g.factor(), 0.04);
        assert_eq!(g.effective_mhz(), 500.0, "report includes spare work");
        for _ in 0..30 {
            g.observe(sample(0.01));
        }
        assert_eq!(
            g.baseline_mhz, 25.0,
            "recovery is capped; bursts spend the spare time"
        );
    }
    #[test]
    fn chipset_cost_leaves_baseline_headroom_on_a_slower_host() {
        let mut g = SpeedGovernor::new(25.0);
        for _ in 0..100 {
            g.observe(sample((0.3 + g.baseline_mhz * 0.04) * 0.1));
        }
        let load = 0.3 + g.baseline_mhz * 0.04;
        assert!(load <= LOWER_ABOVE_LOAD);
        assert!(g.factor() < 1.0);
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
        g.observe(sample(0.2));
        let baseline = g.baseline_mhz;
        g.rebase(40.0);
        assert_eq!(g.effective_mhz(), baseline);
        assert_eq!(g.factor(), baseline / 40.0);
    }
    #[test]
    fn burst_deadline_reserves_presentation_time() {
        let target = Instant::now();
        assert_eq!(
            target.duration_since(spare_deadline(target, 0.02).unwrap()),
            Duration::from_millis(2)
        );
    }
}
