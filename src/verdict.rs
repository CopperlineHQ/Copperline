// SPDX-License-Identifier: GPL-3.0-or-later

//! The process exit status of a session.
//!
//! A run can end for several reasons that a caller (a CI job, a test
//! harness, a Makefile) wants to tell apart without parsing the log:
//!
//! | status | meaning |
//! |---|---|
//! | 0 | the run completed; every expectation held |
//! | 1 | Copperline itself failed (configuration, assets, host errors) |
//! | 3 | a `--expect-screenshot` comparison failed ([`crate::expect`]) |
//! | 4 | `--exit-on-return` was given but the guest program had not returned when the run ended ([`crate::runprog`]) |
//! | 0-255 | the guest program's AmigaDOS return code, under `--exit-on-return` |
//!
//! The rules combine as [`RunVerdict::exit_status`] documents: a guest
//! return code the user asked for wins whenever it is non-zero, a failed
//! expectation is never hidden behind a success, and a guest that stops
//! the emulator through uaelib `ExitEmu` ends the run as a clean exit.

use crate::expect::EXIT_STATUS_MISMATCH;
use crate::runprog::EXIT_STATUS_NO_RETURN;

/// What the session concluded, accumulated while it runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunVerdict {
    /// How many `--expect-screenshot` checks failed.
    pub expect_failures: u32,
    /// `--exit-on-return` was given: the exit status reports the guest's
    /// return code, or [`EXIT_STATUS_NO_RETURN`] if it never came.
    pub exit_on_return: bool,
    /// The guest program's AmigaDOS return code, once its completion
    /// marker was read.
    pub guest_return: Option<i32>,
    /// The guest stopped the emulator through the uaelib trap (WinUAE's
    /// `ExitEmu`, function 13).
    pub guest_exit: bool,
}

impl RunVerdict {
    /// The process exit status this verdict maps to.
    pub fn exit_status(&self) -> i32 {
        let expectations = if self.expect_failures > 0 {
            EXIT_STATUS_MISMATCH
        } else {
            0
        };
        if !self.exit_on_return {
            return expectations;
        }
        match self.guest_return {
            // A non-zero return code is what the caller asked to see; a
            // zero one must still not hide a failed expectation.
            Some(rc) if rc != 0 => rc.clamp(0, 255),
            Some(_) => expectations,
            // ExitEmu is the guest's own clean stop: not a missing return.
            None if self.guest_exit => expectations,
            None => EXIT_STATUS_NO_RETURN,
        }
    }

    /// Whether the guest program's return code has been seen.
    pub fn returned(&self) -> bool {
        self.guest_return.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_run_exits_zero_or_three() {
        let mut v = RunVerdict::default();
        assert_eq!(v.exit_status(), 0);
        v.expect_failures = 2;
        assert_eq!(v.exit_status(), EXIT_STATUS_MISMATCH);
        // A guest return code is ignored unless it was asked for.
        v.guest_return = Some(20);
        assert_eq!(v.exit_status(), EXIT_STATUS_MISMATCH);
        v.expect_failures = 0;
        assert_eq!(v.exit_status(), 0);
    }

    #[test]
    fn exit_on_return_reports_the_guest_code_clamped() {
        let mut v = RunVerdict {
            exit_on_return: true,
            ..Default::default()
        };
        assert_eq!(v.exit_status(), EXIT_STATUS_NO_RETURN, "never returned");
        v.guest_return = Some(0);
        assert_eq!(v.exit_status(), 0);
        v.guest_return = Some(20);
        assert_eq!(v.exit_status(), 20);
        v.guest_return = Some(1000);
        assert_eq!(v.exit_status(), 255);
        v.guest_return = Some(-1);
        assert_eq!(v.exit_status(), 0, "negative codes clamp to 0");
    }

    #[test]
    fn a_zero_return_never_hides_a_failed_expectation() {
        let v = RunVerdict {
            exit_on_return: true,
            guest_return: Some(0),
            expect_failures: 1,
            ..Default::default()
        };
        assert_eq!(v.exit_status(), EXIT_STATUS_MISMATCH);
        // A non-zero guest code is the more specific answer.
        let v = RunVerdict {
            guest_return: Some(10),
            ..v
        };
        assert_eq!(v.exit_status(), 10);
    }

    #[test]
    fn guest_exit_is_a_clean_stop() {
        let v = RunVerdict {
            exit_on_return: true,
            guest_exit: true,
            ..Default::default()
        };
        assert_eq!(v.exit_status(), 0);
        let v = RunVerdict {
            expect_failures: 1,
            ..v
        };
        assert_eq!(v.exit_status(), EXIT_STATUS_MISMATCH);
    }
}
