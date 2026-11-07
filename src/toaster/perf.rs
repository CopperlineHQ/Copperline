// SPDX-License-Identifier: GPL-3.0-or-later

//! Host cost of the card's work, stage by stage
//! (`COPPERLINE_DIAG_TOASTER=perf`): once a second of emulated time the
//! card logs how many times each stage ran and how long it took, on
//! whichever thread ran it. `submit`, `compose_wait`, `par_read`, `scan`
//! and `port_byte` are the emulation thread's; the rest is the compose
//! thread's. Off, the timers cost a flag test.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use std::sync::OnceLock;

/// The stages, in the order they are reported.
pub(crate) const FIELD_RENDER: usize = 0;
pub(crate) const FIELD_COMPLETE: usize = 1;
pub(crate) const HEADER_SCAN: usize = 2;
pub(crate) const WRITE_LINES: usize = 3;
pub(crate) const RECORD_INPUT: usize = 4;
pub(crate) const OUTPUTS: usize = 5;
pub(crate) const DVE_MAP: usize = 6;
pub(crate) const BUS_A: usize = 7;
pub(crate) const BUS_B: usize = 8;
pub(crate) const SRC_FLYER: usize = 9;
pub(crate) const SRC_INPUT: usize = 10;
pub(crate) const SRC_DAC: usize = 11;
pub(crate) const SRC_AMIGA: usize = 12;
pub(crate) const LATCH: usize = 13;
pub(crate) const KEY: usize = 14;
pub(crate) const MIX: usize = 15;
pub(crate) const PREVIEW: usize = 16;
pub(crate) const SOUND: usize = 17;
pub(crate) const FLYER_SOUND: usize = 18;
pub(crate) const PUBLISH: usize = 19;
pub(crate) const PAR_READ: usize = 20;
pub(crate) const SCAN: usize = 21;
pub(crate) const PORT_BYTE: usize = 22;
pub(crate) const COMPOSE_WAIT: usize = 23;
pub(crate) const SUBMIT: usize = 24;
pub(crate) const GRAB_ENCODE: usize = 25;
const STAGES: usize = 26;

const NAMES: [&str; STAGES] = [
    "field_render",
    "field_complete",
    "header_scan",
    "write_lines",
    "record_input",
    "outputs",
    "dve_map",
    "bus_a",
    "bus_b",
    "src_flyer",
    "src_input",
    "src_dac",
    "src_amiga",
    "latch",
    "key",
    "mix",
    "preview",
    "sound",
    "flyer_sound",
    "publish",
    "par_read",
    "scan",
    "port_byte",
    "compose_wait",
    "submit",
    "grab_encode",
];

static NS: [AtomicU64; STAGES] = [const { AtomicU64::new(0) }; STAGES];
static COUNT: [AtomicU64; STAGES] = [const { AtomicU64::new(0) }; STAGES];

/// Whether the timers are on.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        crate::envcfg::var_os("COPPERLINE_DIAG_TOASTER").is_some_and(|v| v == "perf")
    })
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub(crate) fn on() -> bool {
    false
}

/// A running timer: `stop` adds its time to a stage.
#[must_use]
pub(crate) struct Timer(
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))] Option<std::time::Instant>,
);

pub(crate) fn start() -> Timer {
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        Timer(on().then(std::time::Instant::now))
    }
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        Timer()
    }
}

impl Timer {
    pub(crate) fn stop(self, stage: usize) {
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        if let Some(t) = self.0 {
            NS[stage].fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
            COUNT[stage].fetch_add(1, Relaxed);
        }
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        let _ = stage;
    }
}

/// Count an event without timing it.
pub(crate) fn count(stage: usize, n: u64) {
    if on() {
        COUNT[stage].fetch_add(n, Relaxed);
    }
}

/// The stages' totals since the last report, as one log line, and reset.
pub(crate) fn report() -> String {
    let mut out = String::new();
    for (i, name) in NAMES.iter().enumerate() {
        let ns = NS[i].swap(0, Relaxed);
        let n = COUNT[i].swap(0, Relaxed);
        if n == 0 && ns == 0 {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&format!("{name}={:.1}ms/{n}", ns as f64 / 1e6));
    }
    out
}
