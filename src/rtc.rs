// SPDX-License-Identifier: GPL-3.0-or-later

//! Battery-backed real-time clock emulation.
//!
//! Classic big-box Amigas expose the Oki MSM6242 at $DC0000. The chip
//! has sixteen four-bit registers; on Amiga each register is visible as
//! a 32-bit word, so register N lives at base + N * 4. Copperline exposes a
//! read-only wall-clock view: guest writes can control the HOLD latch,
//! but they never change the host clock.
//!
//! The clock is reported in the host's *local* time zone (matching the
//! auto-generated filename stamps in `timestamp.rs`), since AmigaOS has no
//! real notion of time zones and a UTC clock just confuses users. The
//! deterministic `COPPERLINE_RTC_FIXED_SECS` override stays UTC so it
//! remains host-independent.
//!
//! A configured seed (`[machine] rtc_time` / `--rtc-time`) replaces the
//! host clock entirely: the chip powers on reading the seed and ticks
//! forward with *emulated* time, like a battery clock that was set before
//! the machine was switched on. Reads are then reproducible byte-for-byte,
//! which is what a guest program validating time-dependent behaviour
//! (TOTP vectors, timestamped logs) needs. `rtc_frozen` additionally stops
//! the tick, as if the chip's STOP bit were wired permanently high.

use crate::timebase::{SystemTime, UNIX_EPOCH};

/// What the clock's byte lane reads back with no chip in the socket.
///
/// An empty socket does not leave the lane floating: it settles on a fixed
/// pattern, measured as `$40` on real A500 hardware (vAmiga reports the same
/// value from the same measurement). The low nibble reading zero is the part
/// that matters. Every OS clock probe -- AROS `battclock.resource`, 1.3's
/// `SetClock`, 2.0+'s `battclock.resource` -- decides a clock is there by
/// writing a control nibble and reading it back, so a lane that floated to
/// the last value on the data bus would sooner or later echo the written
/// nibble and invent a clock (and then a date) that the machine does not have.
pub const EMPTY_SOCKET_LANE: u8 = 0x40;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Msm6242Rtc {
    control_d: u8,
    control_e: u8,
    latched: Option<RtcDateTime>,
    /// Power-on clock value in Unix seconds. When set, register reads
    /// derive from seed + elapsed emulated seconds instead of the host
    /// wall clock, making the guest-visible time deterministic.
    seed_unix: Option<u64>,
    /// Stop the seeded clock: reads always decompose the seed itself.
    frozen: bool,
    #[cfg(test)]
    test_time: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct RtcDateTime {
    year: u16,
    month: u8,
    day: u8,
    weekday: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

impl Msm6242Rtc {
    const CD_HOLD: u8 = 1 << 0;
    const CD_IRQ_FLAG: u8 = 1 << 2;
    const CF_24H: u8 = 1 << 2;

    pub fn read(&mut self, addr: u64, _size: usize, emulated_secs: f64) -> u64 {
        self.read_register(register_from_offset(addr), emulated_secs) as u64
    }

    pub fn write(&mut self, addr: u64, _size: usize, val: u64, emulated_secs: f64) {
        let reg = register_from_offset(addr);
        let val = (val & 0x0F) as u8;
        match reg {
            0xD => {
                if val & Self::CD_HOLD != 0 {
                    if self.latched.is_none() {
                        self.latched = Some(self.current_time(emulated_secs));
                    }
                    self.control_d = Self::CD_HOLD;
                } else {
                    self.latched = None;
                    self.control_d = 0;
                }
            }
            0xE => {
                self.control_e = val;
            }
            0xF => {
                // Keep the clock running in 24-hour mode. STOP, RESET
                // and TEST writes are deliberately not persistent.
            }
            _ => {}
        }
    }

    fn read_register(&mut self, reg: u8, emulated_secs: f64) -> u8 {
        let time = self
            .latched
            .unwrap_or_else(|| self.current_time(emulated_secs));
        (match reg {
            0x0 => time.second % 10,
            0x1 => time.second / 10,
            0x2 => time.minute % 10,
            0x3 => time.minute / 10,
            0x4 => time.hour % 10,
            0x5 => time.hour / 10,
            0x6 => time.day % 10,
            0x7 => time.day / 10,
            0x8 => time.month % 10,
            0x9 => time.month / 10,
            0xA => (time.year % 10) as u8,
            0xB => ((time.year / 10) % 10) as u8,
            0xC => time.weekday,
            0xD => self.control_d | Self::CD_IRQ_FLAG,
            0xE => self.control_e,
            0xF => Self::CF_24H,
            _ => 0,
        }) & 0x0F
    }

    fn current_time(&self, emulated_secs: f64) -> RtcDateTime {
        #[cfg(test)]
        if let Some(time) = self.test_time {
            return RtcDateTime::from_system_time(time);
        }
        // COPPERLINE_RTC_FIXED_SECS pins the clock to a fixed Unix-seconds
        // value, making RTC reads deterministic across runs (otherwise the
        // host wall-clock differs run-to-run, which pollutes differential
        // traces with spurious timestamp divergences). As a diagnostic
        // override it wins over the configured seed.
        if let Some(secs) = crate::envcfg::var("COPPERLINE_RTC_FIXED_SECS")
            .and_then(|s| s.trim().parse::<u64>().ok())
        {
            return RtcDateTime::from_unix_seconds(secs);
        }
        if let Some(seed) = self.seed_unix {
            return RtcDateTime::from_unix_seconds(self.seeded_unix(seed, emulated_secs));
        }
        RtcDateTime::from_system_time_local(SystemTime::now())
    }

    fn seeded_unix(&self, seed: u64, emulated_secs: f64) -> u64 {
        if self.frozen {
            seed
        } else {
            seed + emulated_secs as u64
        }
    }

    /// Configure the power-on clock value (`None` restores the live host
    /// clock). The seed is the value the clock reads at emulated time zero.
    pub fn set_seed(&mut self, seed_unix: Option<u64>, frozen: bool) {
        self.seed_unix = seed_unix;
        self.frozen = frozen && seed_unix.is_some();
    }

    pub fn seed(&self) -> Option<u64> {
        self.seed_unix
    }

    pub fn frozen(&self) -> bool {
        self.frozen
    }

    /// The Unix-seconds instant register reads decompose right now,
    /// following the same source precedence as `current_time` (the
    /// host-local path reports plain host Unix seconds).
    pub fn current_unix(&self, emulated_secs: f64) -> u64 {
        if let Some(secs) = crate::envcfg::var("COPPERLINE_RTC_FIXED_SECS")
            .and_then(|s| s.trim().parse::<u64>().ok())
        {
            return secs;
        }
        if let Some(seed) = self.seed_unix {
            return self.seeded_unix(seed, emulated_secs);
        }
        RtcDateTime::unix_secs(SystemTime::now())
    }

    /// The broken-down time register reads expose right now, formatted as
    /// `YYYY-MM-DDTHH:MM:SS` (for status reporting, not the guest).
    pub fn current_display(&self, emulated_secs: f64) -> String {
        self.current_time(emulated_secs).iso8601()
    }

    /// A CPU reset does not reach the battery-backed chip, so the time
    /// source keeps running; only the bus-visible latch state drops back
    /// to power-on defaults.
    pub fn reset(&mut self) {
        self.control_d = 0;
        self.control_e = 0;
        self.latched = None;
    }

    #[cfg(test)]
    fn set_test_time(&mut self, time: SystemTime) {
        self.test_time = Some(time);
    }
}

/// Parse a `[machine] rtc_time` / `--rtc-time` value: either a bare
/// integer (Unix seconds, UTC) or a calendar timestamp
/// `YYYY-MM-DD HH:MM[:SS]` (a `T` date/time separator is also accepted).
/// The calendar form is exactly the wall-clock time the guest reads at
/// power-on, independent of the host time zone.
pub fn parse_rtc_time(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty rtc_time value".into());
    }
    if s.bytes().all(|b| b.is_ascii_digit()) {
        return s
            .parse::<u64>()
            .map_err(|_| format!("Unix-seconds value {s:?} is out of range"));
    }
    let form = "expected Unix seconds or \"YYYY-MM-DD HH:MM[:SS]\"";
    let (date, time) = s
        .split_once(['T', ' '])
        .ok_or_else(|| format!("cannot parse rtc_time {s:?}: {form}"))?;
    let mut date_parts = date.splitn(3, '-');
    let num = |part: Option<&str>| -> Result<u64, String> {
        part.filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|p| p.parse::<u64>().ok())
            .ok_or_else(|| format!("cannot parse rtc_time {s:?}: {form}"))
    };
    let year = num(date_parts.next())?;
    let month = num(date_parts.next())?;
    let day = num(date_parts.next())?;
    let mut time_parts = time.splitn(3, ':');
    let hour = num(time_parts.next())?;
    let minute = num(time_parts.next())?;
    let second = match time_parts.next() {
        Some(sec) => num(Some(sec))?,
        None => 0,
    };
    if year < 1970 {
        return Err(format!("rtc_time {s:?} is before 1970 (Unix epoch)"));
    }
    // Explicit bounds before the casts below: a year above i32::MAX or a
    // month/day above u32::MAX would wrap identically in the computation
    // and in the round-trip check, slipping past it as a wrong date.
    if year > 9999 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(format!("rtc_time {s:?} is not a valid calendar date"));
    }
    if hour > 23 || minute > 59 || second > 59 {
        return Err(format!("rtc_time {s:?} has an out-of-range time of day"));
    }
    let days = days_from_civil(year as i64, month as u32, day as u32);
    // Round-tripping through the decomposition rejects the impossible
    // dates the bounds above cannot (Feb 30, Apr 31) without a
    // hand-written calendar table.
    if civil_from_days(days) != (year as i32, month as u32, day as u32) {
        return Err(format!("rtc_time {s:?} is not a valid calendar date"));
    }
    Ok(days as u64 * 86_400 + hour * 3_600 + minute * 60 + second)
}

fn register_from_offset(addr: u64) -> u8 {
    ((addr >> 2) & 0x0F) as u8
}

impl RtcDateTime {
    /// UTC decomposition for the deterministic test path, where a
    /// host-independent (time-zone-free) result keeps the asserted BCD
    /// digits stable across CI hosts.
    #[cfg(test)]
    fn from_system_time(time: SystemTime) -> Self {
        Self::from_unix_seconds(Self::unix_secs(time))
    }

    /// Local-time decomposition for the live clock, mirroring
    /// `timestamp.rs` so the RTC and the auto-generated filename stamps
    /// agree on the time zone. Falls back to UTC where the platform has no
    /// thread-safe local conversion (or it fails).
    fn from_system_time_local(time: SystemTime) -> Self {
        let secs = Self::unix_secs(time);
        Self::from_local(secs).unwrap_or_else(|| Self::from_unix_seconds(secs))
    }

    fn unix_secs(time: SystemTime) -> u64 {
        time.duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn iso8601(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    fn from_unix_seconds(secs: u64) -> Self {
        let days = (secs / 86_400) as i64;
        let second_of_day = (secs % 86_400) as u32;
        let (year, month, day) = civil_from_days(days);
        Self {
            year: year as u16,
            month: month as u8,
            day: day as u8,
            weekday: ((days + 4).rem_euclid(7)) as u8,
            hour: (second_of_day / 3600) as u8,
            minute: ((second_of_day / 60) % 60) as u8,
            second: (second_of_day % 60) as u8,
        }
    }

    /// Decompose a Unix-seconds value into the host's *local* broken-down
    /// time. Returns `None` when the platform exposes no thread-safe local
    /// conversion so the caller can fall back to UTC.
    ///
    /// As in `timestamp.rs`, this is sound only because we never mutate the
    /// TZ environment at runtime (envcfg snapshots it once), so `localtime_r`
    /// cannot race the audio thread.
    #[cfg(unix)]
    fn from_local(secs: u64) -> Option<Self> {
        // SAFETY: localtime_r fully initializes `tm` and retains no pointers.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let t = secs as libc::time_t;
        if unsafe { libc::localtime_r(&t, &mut tm).is_null() } {
            return None;
        }
        Some(Self::from_tm(&tm))
    }

    #[cfg(windows)]
    fn from_local(secs: u64) -> Option<Self> {
        // localtime_s reverses the POSIX argument order and returns errno_t
        // (0 = success).
        // SAFETY: localtime_s fully initializes `tm` and retains no pointers.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let t = secs as libc::time_t;
        if unsafe { libc::localtime_s(&mut tm, &t) } != 0 {
            return None;
        }
        Some(Self::from_tm(&tm))
    }

    #[cfg(not(any(unix, windows)))]
    fn from_local(_secs: u64) -> Option<Self> {
        None
    }

    /// Map a libc broken-down local time onto the RTC fields. `tm_wday`
    /// already uses the 0 = Sunday convention the weekday register expects.
    #[cfg(any(unix, windows))]
    fn from_tm(tm: &libc::tm) -> Self {
        Self {
            year: (tm.tm_year + 1900) as u16,
            month: (tm.tm_mon + 1) as u8,
            day: tm.tm_mday as u8,
            weekday: tm.tm_wday as u8,
            hour: tm.tm_hour as u8,
            minute: tm.tm_min as u8,
            second: tm.tm_sec as u8,
        }
    }
}

/// Inverse of `civil_from_days` (Howard Hinnant's civil-calendar
/// algorithm): days since the Unix epoch for a Gregorian date.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

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
    use std::time::Duration;

    fn read_reg(rtc: &mut Msm6242Rtc, reg: u8) -> u8 {
        rtc.read((reg as u64) * 4, 4, 0.0) as u8
    }

    fn read_reg_at(rtc: &mut Msm6242Rtc, reg: u8, emulated_secs: f64) -> u8 {
        rtc.read((reg as u64) * 4, 4, emulated_secs) as u8
    }

    #[test]
    fn registers_expose_bcd_host_time() {
        let mut rtc = Msm6242Rtc::default();
        rtc.set_test_time(UNIX_EPOCH + Duration::from_secs(946_782_245));

        assert_eq!(read_reg(&mut rtc, 0x0), 5);
        assert_eq!(read_reg(&mut rtc, 0x1), 0);
        assert_eq!(read_reg(&mut rtc, 0x2), 4);
        assert_eq!(read_reg(&mut rtc, 0x3), 0);
        assert_eq!(read_reg(&mut rtc, 0x4), 3);
        assert_eq!(read_reg(&mut rtc, 0x5), 0);
        assert_eq!(read_reg(&mut rtc, 0x6), 2);
        assert_eq!(read_reg(&mut rtc, 0x7), 0);
        assert_eq!(read_reg(&mut rtc, 0x8), 1);
        assert_eq!(read_reg(&mut rtc, 0x9), 0);
        assert_eq!(read_reg(&mut rtc, 0xA), 0);
        assert_eq!(read_reg(&mut rtc, 0xB), 0);
        assert_eq!(read_reg(&mut rtc, 0xC), 0);
        assert_eq!(
            read_reg(&mut rtc, 0xF) & Msm6242Rtc::CF_24H,
            Msm6242Rtc::CF_24H
        );
    }

    #[test]
    fn hold_write_latches_time_without_setting_host_clock() {
        let mut rtc = Msm6242Rtc::default();
        rtc.set_test_time(UNIX_EPOCH + Duration::from_secs(946_782_245));
        rtc.write(0xD * 4, 4, Msm6242Rtc::CD_HOLD as u64, 0.0);
        assert_eq!(
            read_reg(&mut rtc, 0xD) & Msm6242Rtc::CD_HOLD,
            Msm6242Rtc::CD_HOLD
        );

        rtc.set_test_time(UNIX_EPOCH + Duration::from_secs(946_782_245 + 55));
        assert_eq!(read_reg(&mut rtc, 0x0), 5);

        rtc.write(0xD * 4, 4, 0, 0.0);
        assert_eq!(read_reg(&mut rtc, 0x0), 0);
    }

    // RFC 6238 test-vector instant: 1111111109 = 2005-03-18T01:58:29Z,
    // a Friday. The seeded clock must expose exactly this decomposition
    // regardless of the host clock or time zone.
    const VECTOR_UNIX: u64 = 1_111_111_109;

    #[test]
    fn seeded_clock_reads_seed_and_advances_with_emulated_time() {
        let mut rtc = Msm6242Rtc::default();
        rtc.set_seed(Some(VECTOR_UNIX), false);

        assert_eq!(read_reg(&mut rtc, 0x0), 9); // seconds ones
        assert_eq!(read_reg(&mut rtc, 0x1), 2); // seconds tens
        assert_eq!(read_reg(&mut rtc, 0x2), 8); // minutes ones
        assert_eq!(read_reg(&mut rtc, 0x3), 5); // minutes tens
        assert_eq!(read_reg(&mut rtc, 0x4), 1); // hours ones
        assert_eq!(read_reg(&mut rtc, 0x5), 0); // hours tens
        assert_eq!(read_reg(&mut rtc, 0x6), 8); // day ones
        assert_eq!(read_reg(&mut rtc, 0x7), 1); // day tens
        assert_eq!(read_reg(&mut rtc, 0x8), 3); // month ones
        assert_eq!(read_reg(&mut rtc, 0x9), 0); // month tens
        assert_eq!(read_reg(&mut rtc, 0xA), 5); // year ones
        assert_eq!(read_reg(&mut rtc, 0xB), 0); // year tens
        assert_eq!(read_reg(&mut rtc, 0xC), 5); // Friday

        // 31 emulated seconds later the clock reads :00 of the next minute.
        assert_eq!(read_reg_at(&mut rtc, 0x0, 31.0), 0);
        assert_eq!(read_reg_at(&mut rtc, 0x2, 31.0), 9);
        assert_eq!(rtc.current_unix(31.9), VECTOR_UNIX + 31);
    }

    #[test]
    fn frozen_clock_never_advances() {
        let mut rtc = Msm6242Rtc::default();
        rtc.set_seed(Some(VECTOR_UNIX), true);
        assert_eq!(read_reg_at(&mut rtc, 0x0, 3600.0), 9);
        assert_eq!(rtc.current_unix(3600.0), VECTOR_UNIX);
        assert_eq!(rtc.current_display(3600.0), "2005-03-18T01:58:29");
    }

    #[test]
    fn reset_keeps_the_seed_but_drops_the_hold_latch() {
        let mut rtc = Msm6242Rtc::default();
        rtc.set_seed(Some(VECTOR_UNIX), false);
        rtc.write(0xD * 4, 4, Msm6242Rtc::CD_HOLD as u64, 0.0);
        rtc.reset();
        assert_eq!(read_reg(&mut rtc, 0xD) & Msm6242Rtc::CD_HOLD, 0);
        assert_eq!(read_reg_at(&mut rtc, 0x0, 5.0), 4); // still ticking from the seed
        assert_eq!(rtc.seed(), Some(VECTOR_UNIX));
    }

    #[test]
    fn parse_accepts_unix_seconds_and_calendar_forms() {
        assert_eq!(parse_rtc_time("1111111109"), Ok(VECTOR_UNIX));
        assert_eq!(parse_rtc_time("2005-03-18 01:58:29"), Ok(VECTOR_UNIX));
        assert_eq!(parse_rtc_time("2005-03-18T01:58:29"), Ok(VECTOR_UNIX));
        assert_eq!(parse_rtc_time("2005-03-18T01:58"), Ok(VECTOR_UNIX - 29));
        assert_eq!(parse_rtc_time("1970-01-01 00:00:00"), Ok(0));
    }

    #[test]
    fn parse_rejects_malformed_and_impossible_values() {
        assert!(parse_rtc_time("").is_err());
        assert!(parse_rtc_time("yesterday").is_err());
        assert!(parse_rtc_time("2005-03-18").is_err()); // no time of day
        assert!(parse_rtc_time("2005-13-01 00:00:00").is_err());
        assert!(parse_rtc_time("2005-02-30 00:00:00").is_err());
        assert!(parse_rtc_time("2005-03-00 00:00:00").is_err());
        assert!(parse_rtc_time("2005-03-18 24:00:00").is_err());
        assert!(parse_rtc_time("1969-12-31 23:59:59").is_err());
        assert!(parse_rtc_time("-100").is_err());
        // Values that would wrap the internal casts (u32 month, i32 year)
        // must fail loudly, not alias onto a nearby valid date.
        assert!(parse_rtc_time("2005-4294967299-18 00:00:00").is_err());
        assert!(parse_rtc_time("4294969296-03-18 00:00:00").is_err());
        assert!(parse_rtc_time("99999999999-01-01 00:00:00").is_err());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn live_path_uses_local_time() {
        // 2000-01-02 03:04:05 UTC stays within 2000-01-01..02 across every
        // real time zone (offsets are within +-14h), so the local
        // decomposition always lands in that window regardless of the test
        // host.
        let dt = RtcDateTime::from_system_time_local(UNIX_EPOCH + Duration::from_secs(946_782_245));
        assert_eq!(dt.year, 2000);
        assert_eq!(dt.month, 1);
        assert!(dt.day == 1 || dt.day == 2);
    }
}
