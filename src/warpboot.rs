//! General warp boot: run the machine unpaced from power-on until a
//! condition holds, then hand back to real-time pacing.
//!
//! `--run`'s warp launch (src/runprog.rs) does this for one specific
//! condition -- the guest loading a known executable. This is the same
//! idea for ordinary configurations that boot from whatever media they
//! have (a Workbench disk, a hard-disk setup like AmigaVision, a CD):
//!
//! - `--warp-until SECS` warps until an absolute emulated timestamp.
//!   Deterministic: the same config lands at the same place every run,
//!   so once a user knows their setup shows its menu at 12s they get it
//!   in a couple of wall seconds, every time.
//! - `--warp-boot` warps until the boot storage (floppy and hard-disk
//!   activity, as shown by the front-panel LEDs) has been idle for a
//!   threshold of emulated seconds (`[emulation] warp_boot_idle`,
//!   default 10). Overshoot is free: the extra idle seconds pass at warp
//!   speed, and landing a few emulated seconds after the menu settled is
//!   invisible. CD audio deliberately does not count as activity -- CD
//!   *data* traffic rides the HDD LED, while a menu's background CD-DA
//!   must not hold the warp forever. The idle threshold exists because
//!   boots have storage-quiet stretches that are not the end of the
//!   boot: a big-RAM machine's SetPatch MMU table build keeps the disk
//!   idle for seconds while the CPU walks every page of fitted RAM
//!   (timing-test/accelprobe.asm measures that walk), so a threshold
//!   shorter than the longest such stretch would disengage mid-boot.
//!
//! Like the warp launch, this is pure bookkeeping over emulated time so
//! it tests without an emulator; the owner (src/video/window.rs) does
//! the actual pacing and audio calls, engages the gate at power-on, and
//! polls it once per retired emulated frame. Headless capture runs are
//! already unpaced end to end and never carry a gate. The manual warp
//! toggle cancels the gate, same as it cancels a warp launch: one press
//! means normal-speed, audible emulation.

/// Hard cap for the storage-idle mode, in emulated seconds from
/// engagement: a boot that is still churning storage after this long is
/// one the user should be watching at normal speed to see what is wrong.
/// The `--warp-until` mode needs no cap -- its condition *is* a time.
pub const WARP_BOOT_TIMEOUT_SECS: f64 = 600.0;

/// What ends the warp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WarpBootCondition {
    /// Absolute emulated timestamp, in seconds.
    Until(f64),
    /// Floppy+HDD idle for this many consecutive emulated seconds.
    StorageIdle(f64),
}

/// Why a poll ended the warp (or didn't).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WarpBootOutcome {
    /// Keep warping.
    Waiting,
    /// The condition holds: resume real-time pacing.
    Done,
    /// Storage never went idle within [`WARP_BOOT_TIMEOUT_SECS`].
    TimedOut,
}

/// Build the gate a resolved configuration's settings ask for.
/// `warp_until` wins; the both-set case is rejected by shared config
/// validation (and the CLI path re-checks the merged flag/TOML combo).
pub fn gate_from_settings(
    warp_boot: bool,
    warp_boot_idle: f64,
    warp_until: Option<f64>,
) -> Option<WarpBootGate> {
    match (warp_boot, warp_until) {
        (_, Some(secs)) => Some(WarpBootGate::new(WarpBootCondition::Until(secs))),
        (true, None) => Some(WarpBootGate::new(WarpBootCondition::StorageIdle(
            warp_boot_idle,
        ))),
        (false, None) => None,
    }
}

/// The boot-phase state machine: warp from power-on until the condition
/// holds, then hand back to real-time pacing.
pub struct WarpBootGate {
    condition: WarpBootCondition,
    /// Emulated time of the last observed storage activity (idle mode).
    last_active_secs: f64,
    /// Hard-cap deadline, set at engagement (idle mode only).
    deadline_secs: Option<f64>,
    /// Whether [`WarpBootGate::engage`] has run.
    started: bool,
    /// Whether the machine actually entered warp (audio is muted only
    /// while this is true; a machine that refuses to unpace -- a bridged
    /// physical drive -- still runs the gate at normal speed).
    pub engaged: bool,
}

impl WarpBootGate {
    pub fn new(condition: WarpBootCondition) -> Self {
        Self {
            condition,
            last_active_secs: 0.0,
            deadline_secs: None,
            started: false,
            engaged: false,
        }
    }

    pub fn condition(&self) -> WarpBootCondition {
        self.condition
    }

    /// A short description for the log and the OSD.
    pub fn describe(&self) -> String {
        match self.condition {
            WarpBootCondition::Until(secs) => format!("until {secs:.1}s emulated"),
            WarpBootCondition::StorageIdle(idle) => {
                format!("until storage is idle for {idle:.0}s")
            }
        }
    }

    /// Whether [`WarpBootGate::engage`] has run.
    pub fn started(&self) -> bool {
        self.started
    }

    /// Start the boot phase at `now_secs` of emulated time. `unpaced`
    /// says whether the machine really went unpaced.
    pub fn engage(&mut self, now_secs: f64, unpaced: bool) {
        self.started = true;
        self.engaged = unpaced;
        self.last_active_secs = now_secs;
        if matches!(self.condition, WarpBootCondition::StorageIdle(_)) {
            self.deadline_secs = Some(now_secs + WARP_BOOT_TIMEOUT_SECS);
        }
    }

    /// Feed one poll: the current emulated time and whether the boot
    /// storage (floppy or hard disk) is active right now.
    pub fn note(&mut self, now_secs: f64, storage_active: bool) -> WarpBootOutcome {
        match self.condition {
            WarpBootCondition::Until(target) => {
                if now_secs >= target {
                    WarpBootOutcome::Done
                } else {
                    WarpBootOutcome::Waiting
                }
            }
            WarpBootCondition::StorageIdle(threshold) => {
                if storage_active {
                    self.last_active_secs = now_secs;
                }
                if now_secs - self.last_active_secs >= threshold {
                    return WarpBootOutcome::Done;
                }
                match self.deadline_secs {
                    Some(deadline) if now_secs >= deadline => WarpBootOutcome::TimedOut,
                    _ => WarpBootOutcome::Waiting,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn until_mode_ends_exactly_at_the_target_time() {
        let mut gate = WarpBootGate::new(WarpBootCondition::Until(12.0));
        gate.engage(0.5, true);
        assert!(gate.engaged);
        assert_eq!(gate.note(0.6, true), WarpBootOutcome::Waiting);
        assert_eq!(gate.note(11.99, false), WarpBootOutcome::Waiting);
        assert_eq!(gate.note(12.0, false), WarpBootOutcome::Done);
    }

    #[test]
    fn until_mode_already_past_the_target_ends_on_the_first_poll() {
        // Engaging after the target (a resumed save state, a late gate)
        // must not warp forever.
        let mut gate = WarpBootGate::new(WarpBootCondition::Until(1.0));
        gate.engage(5.0, true);
        assert_eq!(gate.note(5.0, true), WarpBootOutcome::Done);
    }

    #[test]
    fn idle_mode_needs_a_full_quiet_threshold() {
        let mut gate = WarpBootGate::new(WarpBootCondition::StorageIdle(10.0));
        gate.engage(0.0, true);
        // Boot activity keeps re-arming the window.
        assert_eq!(gate.note(1.0, true), WarpBootOutcome::Waiting);
        assert_eq!(gate.note(8.0, true), WarpBootOutcome::Waiting);
        assert_eq!(gate.note(17.9, false), WarpBootOutcome::Waiting);
        // Ten quiet seconds after the last activity at 8.0.
        assert_eq!(gate.note(18.0, false), WarpBootOutcome::Done);
    }

    #[test]
    fn idle_mode_survives_a_storage_quiet_stretch_shorter_than_threshold() {
        // The SetPatch MMU-walk shape: disk quiet for seconds mid-boot,
        // then loading resumes. A threshold longer than the stretch keeps
        // the warp engaged across it.
        let mut gate = WarpBootGate::new(WarpBootCondition::StorageIdle(10.0));
        gate.engage(0.0, true);
        assert_eq!(gate.note(2.0, true), WarpBootOutcome::Waiting);
        // 9 quiet seconds -- not enough to disengage.
        assert_eq!(gate.note(11.0, false), WarpBootOutcome::Waiting);
        // Activity resumes and re-arms the window.
        assert_eq!(gate.note(11.5, true), WarpBootOutcome::Waiting);
        assert_eq!(gate.note(21.4, false), WarpBootOutcome::Waiting);
        assert_eq!(gate.note(21.5, false), WarpBootOutcome::Done);
    }

    #[test]
    fn idle_mode_times_out_rather_than_warping_forever() {
        let mut gate = WarpBootGate::new(WarpBootCondition::StorageIdle(10.0));
        gate.engage(0.0, true);
        // Storage never goes quiet for the threshold.
        let mut now = 0.0;
        while now < WARP_BOOT_TIMEOUT_SECS {
            assert_eq!(gate.note(now, true), WarpBootOutcome::Waiting);
            now += 5.0;
        }
        assert_eq!(gate.note(now, true), WarpBootOutcome::TimedOut);
    }

    #[test]
    fn a_machine_that_refuses_to_unpace_still_runs_the_gate() {
        let mut gate = WarpBootGate::new(WarpBootCondition::Until(2.0));
        gate.engage(0.0, false);
        assert!(gate.started());
        assert!(!gate.engaged);
        assert_eq!(gate.note(2.5, false), WarpBootOutcome::Done);
    }
}
