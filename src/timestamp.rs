// SPDX-License-Identifier: GPL-3.0-or-later

//! Compact wall-clock timestamps for auto-generated filenames.
//!
//! Screenshots, save states, input scripts and video captures all stamp
//! their default filenames with the host time. We render it as a sortable
//! `YYYYMMDDHHmmSS` string (no separators) so the names order
//! chronologically and stay filesystem-safe on every platform. The stamp
//! is in the host's local time zone where the platform exposes one (via
//! `localtime_r`), falling back to UTC otherwise.

use crate::timebase::{SystemTime, UNIX_EPOCH};

/// Current host time formatted as `YYYYMMDDHHmmSS`, in local time where
/// available.
pub fn compact_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    local_compact(secs).unwrap_or_else(|| utc_compact(secs))
}

/// Format a Unix-seconds value as a local-time `YYYYMMDDHHmmSS` string.
/// Returns `None` if the platform exposes no thread-safe local-time
/// conversion (then the caller falls back to UTC).
///
/// We never call `tzset`/`setenv` at runtime (envcfg snapshots the
/// environment once), so the thread-unsafe TZ races that plague the C
/// `localtime` family do not apply even with the audio thread running.
#[cfg(unix)]
fn local_compact(secs: u64) -> Option<String> {
    // SAFETY: localtime_r writes a fully-initialized `tm` into our local
    // and does not retain the pointers.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = secs as libc::time_t;
    if unsafe { libc::localtime_r(&t, &mut tm).is_null() } {
        return None;
    }
    Some(fmt_tm(&tm))
}

#[cfg(windows)]
fn local_compact(secs: u64) -> Option<String> {
    // The Windows CRT spells it localtime_s, with the arguments reversed
    // relative to POSIX localtime_r and an errno_t return (0 = success).
    // SAFETY: localtime_s fully initializes `tm` and retains no pointers.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = secs as libc::time_t;
    if unsafe { libc::localtime_s(&mut tm, &t) } != 0 {
        return None;
    }
    Some(fmt_tm(&tm))
}

#[cfg(not(any(unix, windows)))]
fn local_compact(_secs: u64) -> Option<String> {
    None
}

/// Render a broken-down local time as `YYYYMMDDHHmmSS`.
#[cfg(any(unix, windows))]
fn fmt_tm(tm: &libc::tm) -> String {
    format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
    )
}

/// Format a Unix-seconds value as a UTC `YYYYMMDDHHmmSS` string.
fn utc_compact(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let second_of_day = (secs % 86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    let hour = second_of_day / 3600;
    let minute = (second_of_day / 60) % 60;
    let second = second_of_day % 60;
    format!("{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}")
}

/// Convert days since the Unix epoch to a (year, month, day) civil date.
/// Howard Hinnant's days-from-civil inverse; valid for the Gregorian
/// calendar across the full range of host clocks.
fn civil_from_days(days_since_unix_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (year as i32, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formats_known_instant() {
        // 2000-01-02 03:04:05 UTC.
        assert_eq!(utc_compact(946_782_245), "20000102030405");
    }

    #[test]
    fn utc_epoch_is_zero_padded() {
        assert_eq!(utc_compact(0), "19700101000000");
    }

    #[test]
    fn now_has_expected_shape() {
        let s = compact_now();
        assert_eq!(s.len(), 14);
        assert!(s.chars().all(|c| c.is_ascii_digit()));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn local_stamp_has_expected_shape() {
        let s = local_compact(946_782_245).expect("local-time conversion available");
        assert_eq!(s.len(), 14);
        assert!(s.chars().all(|c| c.is_ascii_digit()));
        // 2000-01-02 03:04:05 UTC stays on 2000-01-01 or -02 across every
        // real time zone (offsets are within +-14h), so the local stamp
        // always lands in that two-day window regardless of the test host.
        assert!(s.starts_with("200001"));
    }
}
