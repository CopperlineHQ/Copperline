// SPDX-License-Identifier: GPL-3.0-or-later

//! Battery-backed real-time clock emulation.
//!
//! Classic big-box Amigas expose a four-bit battery clock at $DC0000 with
//! sixteen register selects; on Amiga each register is visible as a 32-bit
//! word, so register N lives at base + N * 4. Two different parts fill
//! that socket: the Oki MSM6242 (A500+/A2000/CDTV boards and the clock
//! expansions), and the Ricoh RP5C01 on the A3000/A4000 motherboards --
//! a different register layout, banked register blocks, and 26 nibbles of
//! battery-backed RAM. AmigaOS probes for either part, but Linux/m68k
//! drives the chip its machine model dictates, so an A3000/A4000 has to
//! answer the RP5C01 protocol for the guest clock to work. [`RtcChip`]
//! names the fitted part (`[machine] rtc_chip`) and [`Rtc`] dispatches
//! guest traffic to it.
//!
//! Until the guest touches the counters, a chip mirrors its time source
//! (the host's wall clock, or the configured seed). Stopping the clock or
//! writing a counter detaches it: from then on the chip keeps its own BCD
//! digit counters, which carry once per second of the time source by the
//! part's own rules (day-of-week as a free-running counter, the leap year
//! from the year digits on the MSM6242 and from the leap-year counter on
//! the RP5C01, hours in the selected 12/24 form). The host clock itself is
//! never changed, and a guest-set time lasts for the session and its save
//! states, not across a power cycle.
//!
//! The RP5C01's battery-backed registers can persist across runs to a
//! backing file (`[machine] battmem`) in the same `.nvram` layout
//! WinUAE and Amiberry use, so files interchange between emulators.
//! This is how AmigaOS `battmem.resource` settings -- the SCSI host
//! options an A3000's or A4091's `scsi.device` stores, among others --
//! survive a power cycle like they do on a real battery-fitted board.
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

/// Which battery-clock part sits in the $DC0000 socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RtcChip {
    /// Oki MSM6242: the A500+/A2000/CDTV part and what the aftermarket
    /// clock expansions carry.
    Msm6242,
    /// Ricoh RP5C01: the A3000/A4000 motherboard part.
    Rp5c01,
}

impl RtcChip {
    /// The part name shown to the user (status output, control protocol).
    pub fn label(self) -> &'static str {
        match self {
            RtcChip::Msm6242 => "MSM6242",
            RtcChip::Rp5c01 => "RP5C01",
        }
    }
}

/// The fitted clock chip. Guest register traffic and host-side queries all
/// funnel through here so the two parts stay interchangeable behind the one
/// bus field.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Rtc {
    Msm6242(Msm6242Rtc),
    Rp5c01(Rp5c01Rtc),
}

impl Default for Rtc {
    fn default() -> Self {
        Rtc::Msm6242(Msm6242Rtc::default())
    }
}

impl Rtc {
    pub fn new(chip: RtcChip) -> Self {
        match chip {
            RtcChip::Msm6242 => Rtc::Msm6242(Msm6242Rtc::default()),
            RtcChip::Rp5c01 => Rtc::Rp5c01(Rp5c01Rtc::default()),
        }
    }

    pub fn chip(&self) -> RtcChip {
        match self {
            Rtc::Msm6242(_) => RtcChip::Msm6242,
            Rtc::Rp5c01(_) => RtcChip::Rp5c01,
        }
    }

    pub fn read(&mut self, addr: u64, size: usize, emulated_secs: f64) -> u64 {
        match self {
            Rtc::Msm6242(chip) => chip.read(addr, size, emulated_secs),
            Rtc::Rp5c01(chip) => chip.read(addr, size, emulated_secs),
        }
    }

    pub fn write(&mut self, addr: u64, size: usize, val: u64, emulated_secs: f64) {
        match self {
            Rtc::Msm6242(chip) => chip.write(addr, size, val, emulated_secs),
            Rtc::Rp5c01(chip) => chip.write(addr, size, val, emulated_secs),
        }
    }

    /// A CPU reset does not reach the battery-backed chip; see the concrete
    /// chips' `reset` for what little state drops back to power-on defaults.
    pub fn reset(&mut self) {
        match self {
            Rtc::Msm6242(chip) => chip.reset(),
            Rtc::Rp5c01(chip) => chip.reset(),
        }
    }

    /// Configure the power-on clock value (`None` restores the live host
    /// clock). The seed is the value the clock reads at emulated time zero.
    pub fn set_seed(&mut self, seed_unix: Option<u64>, frozen: bool) {
        self.clock_mut().set_seed(seed_unix, frozen);
    }

    /// Persist the RP5C01's battery-backed registers to (and preload them
    /// from) `path`, in the WinUAE/Amiberry `.nvram` file layout. No-op
    /// for the MSM6242, which carries no battery RAM.
    pub fn set_battmem_path(&mut self, path: std::path::PathBuf) {
        if let Rtc::Rp5c01(chip) = self {
            chip.set_battmem_path(path);
        }
    }

    pub fn seed(&self) -> Option<u64> {
        self.clock().seed()
    }

    pub fn frozen(&self) -> bool {
        self.clock().frozen()
    }

    /// Whether repeated speculative execution and rewind is self-contained.
    /// A seeded clock advances only in emulated time. An RP5C01 is also safe
    /// only without a battery-RAM file, because guest writes flush that file
    /// immediately and a host write cannot be rewound with the machine.
    pub fn runahead_safe(&self) -> bool {
        if self.seed().is_none() {
            return false;
        }
        match self {
            Rtc::Msm6242(_) => true,
            Rtc::Rp5c01(chip) => chip.battmem_path.is_none(),
        }
    }

    /// The Unix-seconds instant register reads decompose right now.
    pub fn current_unix(&self, emulated_secs: f64) -> u64 {
        self.clock().current_unix(emulated_secs, self.rules())
    }

    /// The broken-down time register reads expose right now, formatted as
    /// `YYYY-MM-DDTHH:MM:SS` (for status reporting, not the guest).
    pub fn current_display(&self, emulated_secs: f64) -> String {
        self.clock().current_display(emulated_secs, self.rules())
    }

    fn rules(&self) -> ChipRules {
        match self {
            Rtc::Msm6242(chip) => chip.rules(),
            Rtc::Rp5c01(chip) => chip.rules(),
        }
    }

    fn clock(&self) -> &ClockSource {
        match self {
            Rtc::Msm6242(chip) => &chip.clock,
            Rtc::Rp5c01(chip) => &chip.clock,
        }
    }

    fn clock_mut(&mut self) -> &mut ClockSource {
        match self {
            Rtc::Msm6242(chip) => &mut chip.clock,
            Rtc::Rp5c01(chip) => &mut chip.clock,
        }
    }
}

/// Logical digit order of the calendar counters: seconds, minutes, hours,
/// day, month and year as a ones digit followed by its tens digit, then
/// the day of the week. The MSM6242 exposes them in exactly this register
/// order; the RP5C01 moves the day of the week.
const S1: usize = 0;
const MI1: usize = 2;
const H1: usize = 4;
const H10: usize = 5;
const D1: usize = 6;
const MO1: usize = 8;
const Y1: usize = 10;
const WD: usize = 12;
const DIGITS: usize = 13;

/// How a chip counts: the form of its hour digits and where its leap year
/// comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChipRules {
    /// 24-hour mode selected.
    twenty_four: bool,
    /// The bit of the ten-hours digit that carries PM in 12-hour mode.
    pm_bit: u8,
    /// The leap year follows the chip's leap-year counter (RP5C01) and not
    /// the year digits (MSM6242: years divisible by four).
    leap_counter: bool,
}

/// The calendar counters as the chip holds them: one BCD digit each, not
/// validated, so a digit written on its own never disturbs its neighbours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
struct Counters {
    digits: [u8; DIGITS],
    /// RP5C01 leap-year counter (0 = leap year), counted with the years.
    leap: u8,
}

impl Counters {
    fn from_datetime(time: RtcDateTime, rules: ChipRules) -> Self {
        let mut counters = Self {
            digits: [0; DIGITS],
            leap: (time.year % 4) as u8,
        };
        counters.set_pair(S1, time.second);
        counters.set_pair(MI1, time.minute);
        counters.set_hour(time.hour, rules);
        counters.set_pair(D1, time.day);
        counters.set_pair(MO1, time.month);
        counters.set_pair(Y1, (time.year % 100) as u8);
        counters.digits[WD] = time.weekday;
        counters
    }

    fn pair(&self, ones: usize) -> u8 {
        self.digits[ones + 1] * 10 + self.digits[ones]
    }

    fn set_pair(&mut self, ones: usize, value: u8) {
        self.digits[ones] = value % 10;
        self.digits[ones + 1] = value / 10;
    }

    /// Hour of the day, 0-23, from the digits in the selected form.
    fn hour(&self, rules: ChipRules) -> u8 {
        if rules.twenty_four {
            return (self.digits[H10] & 0x3) * 10 + self.digits[H1];
        }
        let hour = (self.digits[H10] & 0x1) * 10 + self.digits[H1];
        hour % 12
            + if self.digits[H10] & rules.pm_bit != 0 {
                12
            } else {
                0
            }
    }

    /// Twelve-hour mode reads 12, 1 .. 11 with the PM flag in the
    /// ten-hours digit.
    fn set_hour(&mut self, hour: u8, rules: ChipRules) {
        if rules.twenty_four {
            self.set_pair(H1, hour);
            return;
        }
        let twelve = match hour % 12 {
            0 => 12,
            h => h,
        };
        self.digits[H1] = twelve % 10;
        self.digits[H10] = (twelve / 10) | if hour >= 12 { rules.pm_bit } else { 0 };
    }

    fn leap_year(&self, rules: ChipRules) -> bool {
        if rules.leap_counter {
            self.leap & 0x3 == 0
        } else {
            self.pair(Y1).is_multiple_of(4)
        }
    }

    fn days_in_month(&self, rules: ChipRules) -> u8 {
        match self.pair(MO1) {
            2 if self.leap_year(rules) => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        }
    }

    /// One carry out of the divider: a second.
    fn tick(&mut self, rules: ChipRules) {
        let second = self.pair(S1) + 1;
        if second < 60 {
            return self.set_pair(S1, second);
        }
        self.set_pair(S1, 0);
        let minute = self.pair(MI1) + 1;
        if minute < 60 {
            return self.set_pair(MI1, minute);
        }
        self.set_pair(MI1, 0);
        let hour = self.hour(rules) + 1;
        if hour < 24 {
            return self.set_hour(hour, rules);
        }
        self.set_hour(0, rules);
        self.digits[WD] = (self.digits[WD] + 1) % 7;
        let day = self.pair(D1) + 1;
        if day <= self.days_in_month(rules) {
            return self.set_pair(D1, day);
        }
        self.set_pair(D1, 1);
        let month = self.pair(MO1) + 1;
        if month <= 12 {
            return self.set_pair(MO1, month);
        }
        self.set_pair(MO1, 1);
        self.set_pair(Y1, (self.pair(Y1) + 1) % 100);
        self.leap = (self.leap + 1) & 0x3;
    }

    /// The counters as a date, for status reporting. The two year digits
    /// take the AmigaOS century window (78-99 are 19xx).
    fn to_datetime(self, rules: ChipRules) -> RtcDateTime {
        let year = u16::from(self.pair(Y1));
        RtcDateTime {
            year: if year >= 78 { 1900 + year } else { 2000 + year },
            month: self.pair(MO1),
            day: self.pair(D1),
            weekday: self.digits[WD],
            hour: self.hour(rules),
            minute: self.pair(MI1),
            second: self.pair(S1),
        }
    }
}

/// Counters the guest has taken over from the time source.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
struct ChipTime {
    counters: Counters,
    /// Time-source seconds at the last carry applied to `counters`;
    /// `None` while the clock is stopped.
    since: Option<f64>,
    /// What the divider below the seconds held when the clock stopped, as
    /// a fraction of a second. A divider reset clears it.
    frac: f64,
}

/// The time source both chips count from: the host's local wall clock by
/// default, or the deterministic seed (`[machine] rtc_time`) ticking
/// forward in emulated time.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct ClockSource {
    /// Power-on clock value in Unix seconds. When set, register reads
    /// derive from seed + elapsed emulated seconds instead of the host
    /// wall clock, making the guest-visible time deterministic.
    seed_unix: Option<u64>,
    /// Stop the seeded clock: reads always decompose the seed itself.
    frozen: bool,
    /// The chip's own counters once the guest has stopped or set the
    /// clock; `None` while the chip still mirrors the time source.
    #[serde(default)]
    chip: Option<ChipTime>,
    #[cfg(test)]
    test_time: Option<SystemTime>,
}

impl ClockSource {
    fn current_time(&self, emulated_secs: f64) -> RtcDateTime {
        #[cfg(test)]
        if let Some(time) = self.test_time {
            return RtcDateTime::from_system_time(time);
        }
        if let Some(secs) = fixed_secs_override() {
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

    /// The time source as a running count of seconds, fraction included:
    /// what the chip's divider counts once the guest has detached it.
    fn base_secs(&self, emulated_secs: f64) -> f64 {
        #[cfg(test)]
        if let Some(time) = self.test_time {
            return time
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
        }
        if let Some(secs) = fixed_secs_override() {
            return secs as f64;
        }
        if let Some(seed) = self.seed_unix {
            return if self.frozen {
                seed as f64
            } else {
                seed as f64 + emulated_secs
            };
        }
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    }

    /// A new seed (or a return to the host clock) replaces whatever the
    /// guest had set: the chip mirrors the time source again.
    fn set_seed(&mut self, seed_unix: Option<u64>, frozen: bool) {
        self.seed_unix = seed_unix;
        self.frozen = frozen && seed_unix.is_some();
        self.chip = None;
    }

    fn seed(&self) -> Option<u64> {
        self.seed_unix
    }

    fn frozen(&self) -> bool {
        self.frozen
    }

    /// Apply the carries the time source has produced since the last one.
    fn sync(&mut self, emulated_secs: f64, rules: ChipRules) {
        let now = self.base_secs(emulated_secs);
        if let Some(chip) = &mut self.chip {
            if let Some(since) = chip.since {
                let carries = (now - since).max(0.0).floor();
                for _ in 0..carries as u64 {
                    chip.counters.tick(rules);
                }
                chip.since = Some(since + carries);
            }
        }
    }

    /// The counters right now, carries applied.
    fn counters(&mut self, emulated_secs: f64, rules: ChipRules) -> Counters {
        self.sync(emulated_secs, rules);
        match &self.chip {
            Some(chip) => chip.counters,
            None => Counters::from_datetime(self.current_time(emulated_secs), rules),
        }
    }

    /// As [`counters`](Self::counters), without storing the carries.
    fn peek_counters(&self, emulated_secs: f64, rules: ChipRules) -> Counters {
        self.clone().counters(emulated_secs, rules)
    }

    /// Detach the chip from the time source, keeping the phase of its
    /// carries: the divider continues from the source's own fraction.
    fn detach(&mut self, emulated_secs: f64, rules: ChipRules) -> &mut ChipTime {
        self.sync(emulated_secs, rules);
        if self.chip.is_none() {
            let counters = Counters::from_datetime(self.current_time(emulated_secs), rules);
            self.chip = Some(ChipTime {
                counters,
                since: Some(self.base_secs(emulated_secs).floor()),
                frac: 0.0,
            });
        }
        self.chip.as_mut().expect("chip time was just set")
    }

    /// Change the counters in place (a digit write, an adjust).
    fn edit(&mut self, emulated_secs: f64, rules: ChipRules, edit: impl FnOnce(&mut Counters)) {
        edit(&mut self.detach(emulated_secs, rules).counters);
    }

    /// Stop the carries. The divider keeps what it had counted.
    fn stop(&mut self, emulated_secs: f64, rules: ChipRules) {
        let now = self.base_secs(emulated_secs);
        let chip = self.detach(emulated_secs, rules);
        if let Some(since) = chip.since.take() {
            chip.frac = (now - since).clamp(0.0, 1.0);
        }
    }

    /// Let the carries run again from where the divider stood.
    fn start(&mut self, emulated_secs: f64) {
        let now = self.base_secs(emulated_secs);
        if let Some(chip) = &mut self.chip {
            if chip.since.is_none() {
                chip.since = Some(now - chip.frac);
            }
        }
    }

    /// Clear the divider stages below the seconds: the next carry is one
    /// whole second after this (or after the clock next starts).
    fn reset_divider(&mut self, emulated_secs: f64, rules: ChipRules) {
        let now = self.base_secs(emulated_secs);
        let chip = self.detach(emulated_secs, rules);
        chip.frac = 0.0;
        if chip.since.is_some() {
            chip.since = Some(now);
        }
    }

    /// How far the divider is into the current second, or `None` while
    /// the clock is stopped.
    fn phase(&self, emulated_secs: f64) -> Option<f64> {
        let now = self.base_secs(emulated_secs);
        match &self.chip {
            Some(chip) => chip.since.map(|since| (now - since).max(0.0).fract()),
            None => Some(now.fract()),
        }
    }

    /// The Unix-seconds instant register reads decompose right now,
    /// following the same source precedence as `current_time` (the
    /// host-local path reports plain host Unix seconds). Counters the
    /// guest has set report their own date instead.
    fn current_unix(&self, emulated_secs: f64, rules: ChipRules) -> u64 {
        if self.chip.is_some() {
            let time = self.peek_counters(emulated_secs, rules).to_datetime(rules);
            let days = days_from_civil(i64::from(time.year), time.month.into(), time.day.into());
            let secs = days * 86_400
                + i64::from(time.hour) * 3600
                + i64::from(time.minute) * 60
                + i64::from(time.second);
            return secs.max(0) as u64;
        }
        if let Some(secs) = fixed_secs_override() {
            return secs;
        }
        if let Some(seed) = self.seed_unix {
            return self.seeded_unix(seed, emulated_secs);
        }
        RtcDateTime::unix_secs(SystemTime::now())
    }

    fn current_display(&self, emulated_secs: f64, rules: ChipRules) -> String {
        if self.chip.is_some() {
            return self
                .peek_counters(emulated_secs, rules)
                .to_datetime(rules)
                .iso8601();
        }
        self.current_time(emulated_secs).iso8601()
    }

    #[cfg(test)]
    fn set_test_time(&mut self, time: SystemTime) {
        self.test_time = Some(time);
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Msm6242Rtc {
    control_d: u8,
    control_e: u8,
    /// CF: REST, STOP and the 24/12 select. A chip that never had CF
    /// written (and a state saved before the register was modelled)
    /// counts in 24-hour mode.
    #[serde(default = "msm6242_cf_default")]
    control_f: u8,
    /// The counters as HOLD froze them for reading.
    #[serde(default)]
    held: Option<Counters>,
    clock: ClockSource,
}

fn msm6242_cf_default() -> u8 {
    Msm6242Rtc::CF_24H
}

impl Default for Msm6242Rtc {
    fn default() -> Self {
        Self {
            control_d: 0,
            control_e: 0,
            control_f: Self::CF_24H,
            held: None,
            clock: ClockSource::default(),
        }
    }
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
    const CD_BUSY: u8 = 1 << 1;
    const CD_IRQ_FLAG: u8 = 1 << 2;
    const CD_ADJ: u8 = 1 << 3;
    const CF_REST: u8 = 1 << 0;
    const CF_STOP: u8 = 1 << 1;
    const CF_24H: u8 = 1 << 2;
    /// BUSY is up for this long after each carry (data sheet: 190 us).
    const BUSY_SECS: f64 = 190e-6;
    /// Bits each counter digit implements; the rest read zero.
    const DIGIT_MASK: [u8; DIGITS] = [
        0xF, 0x7, 0xF, 0x7, 0xF, 0x7, 0xF, 0x3, 0xF, 0x1, 0xF, 0xF, 0x7,
    ];

    fn rules(&self) -> ChipRules {
        ChipRules {
            twenty_four: self.control_f & Self::CF_24H != 0,
            pm_bit: 1 << 2,
            leap_counter: false,
        }
    }

    /// The clock counts unless STOP holds the carries or REST holds the
    /// divider in reset.
    fn counting(control_f: u8) -> bool {
        control_f & (Self::CF_STOP | Self::CF_REST) == 0
    }

    pub fn read(&mut self, addr: u64, _size: usize, emulated_secs: f64) -> u64 {
        self.read_register(register_from_offset(addr), emulated_secs) as u64
    }

    pub fn write(&mut self, addr: u64, _size: usize, val: u64, emulated_secs: f64) {
        let reg = register_from_offset(addr);
        let val = (val & 0x0F) as u8;
        let rules = self.rules();
        match reg {
            0x0..=0xC => {
                // The counters are read/write. A digit is loaded as
                // written; software stops the clock or sets HOLD around a
                // multi-digit update.
                let digit = usize::from(reg);
                let val = val & Self::DIGIT_MASK[digit];
                self.clock.edit(emulated_secs, rules, |counters| {
                    counters.digits[digit] = val
                });
                if let Some(held) = &mut self.held {
                    held.digits[digit] = val;
                }
            }
            0xD => {
                if val & Self::CD_ADJ != 0 {
                    // 30-second adjust: seconds 00-29 fall back to 00,
                    // 30-59 carry into the next minute, and the divider
                    // restarts. The bit clears itself.
                    self.clock.edit(emulated_secs, rules, |counters| {
                        let up = counters.pair(S1) >= 30;
                        counters.set_pair(S1, if up { 59 } else { 0 });
                        if up {
                            counters.tick(rules);
                        }
                    });
                    self.clock.reset_divider(emulated_secs, rules);
                    self.held = None;
                }
                if val & Self::CD_HOLD != 0 {
                    if self.held.is_none() {
                        self.held = Some(self.clock.counters(emulated_secs, rules));
                    }
                    self.control_d = Self::CD_HOLD;
                } else {
                    // A carry that fell inside the hold shows as soon as
                    // HOLD drops: the divider never stopped.
                    self.held = None;
                    self.control_d = 0;
                }
            }
            0xE => {
                self.control_e = val;
            }
            0xF => {
                // TEST (bit 3) is not modelled and reads back zero.
                let new = val & (Self::CF_REST | Self::CF_STOP | Self::CF_24H);
                let was_counting = Self::counting(self.control_f);
                // The 24/12 select does not convert the hour digits; the
                // data sheet has software rewrite them.
                self.control_f = new;
                let rules = self.rules();
                if was_counting && !Self::counting(new) {
                    self.clock.stop(emulated_secs, rules);
                }
                if new & Self::CF_REST != 0 {
                    self.clock.reset_divider(emulated_secs, rules);
                }
                if !was_counting && Self::counting(new) {
                    self.clock.start(emulated_secs);
                }
            }
            _ => {}
        }
    }

    fn read_register(&mut self, reg: u8, emulated_secs: f64) -> u8 {
        (match reg {
            0x0..=0xC => {
                let counters = match self.held {
                    Some(held) => held,
                    None => self.clock.counters(emulated_secs, self.rules()),
                };
                counters.digits[usize::from(reg)]
            }
            0xD => {
                let busy = self
                    .clock
                    .phase(emulated_secs)
                    .is_some_and(|phase| phase < Self::BUSY_SECS);
                self.control_d | Self::CD_IRQ_FLAG | if busy { Self::CD_BUSY } else { 0 }
            }
            0xE => self.control_e,
            0xF => self.control_f,
            _ => 0,
        }) & 0x0F
    }

    /// A CPU reset does not reach the battery-backed chip, so the counters
    /// and CF (stop, 24/12) keep their state; only the bus-visible latch
    /// state drops back to power-on defaults.
    pub fn reset(&mut self) {
        self.control_d = 0;
        self.control_e = 0;
        self.held = None;
    }
}

/// The Ricoh RP5C01, the A3000/A4000 battery clock.
///
/// Same four-bit bus presence as the MSM6242, entirely different register
/// model: the MODE register (D) selects one of four register blocks for
/// selects 0-C -- block 0 is the time/calendar counters (note the layout
/// differs from the Oki part: day-of-week at 6, day at 7-8, month at 9-A,
/// year at B-C), block 1 the alarm digits plus the 12/24 select and the
/// leap-year counter, and blocks 2/3 are 13 battery-backed RAM nibbles
/// each (low and high halves of an internal byte, respectively), which
/// AmigaOS uses via battmem.resource on these machines. MODE bit 3 gates
/// the timer -- Linux's rtc-rp5c01 driver clears it around every read and
/// write for a tearing-free window, and the counters stand still while it
/// is clear -- and bit 2 arms the alarm (stored
/// here, but no interrupt line is wired on the Amiga boards). TEST (E) and
/// RESET (F) are write-only and read back zero, which together with MODE
/// reading its power-on $8 is exactly how battclock.resource-style probes
/// (AROS names it "RF5C01A") tell this part from an MSM6242.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Rp5c01Rtc {
    /// MODE register (select D): TIMER_EN | ALARM_EN | block select.
    mode: u8,
    /// Block-1 register storage (alarm digits, 12/24 select). Slots the
    /// write mask zeroes never hold anything.
    bank1: [u8; 13],
    /// The 26 battery-backed RAM nibbles, one byte per select: block 2
    /// reads/writes the low nibble, block 3 the high one.
    ram: [u8; 13],
    clock: ClockSource,
    /// Backing file for the battery-backed registers (`[machine]
    /// battmem`), in the WinUAE/Amiberry `.nvram` layout. `None` keeps
    /// them session-only, like a board with a flat battery.
    battmem_path: Option<std::path::PathBuf>,
    /// Battery state changed since the last flush to `battmem_path`.
    battmem_dirty: bool,
}

impl Default for Rp5c01Rtc {
    fn default() -> Self {
        Self {
            // Power-on: timer running, block 0. The probes match on MODE
            // reading exactly this.
            mode: Self::MODE_TIMER_EN,
            // Battery-fresh block 1 with the 12/24 select in 24-hour mode,
            // matching what AmigaOS and Linux configure and expect.
            bank1: Self::bank1_default(),
            ram: [0; 13],
            clock: ClockSource::default(),
            battmem_path: None,
            battmem_dirty: false,
        }
    }
}

impl Rp5c01Rtc {
    const MODE_TIMER_EN: u8 = 1 << 3;
    const MODE_BLOCK_MASK: u8 = 0x03;
    const BLOCK_TIME: u8 = 0;
    const BLOCK_ALARM: u8 = 1;
    const BLOCK_RAM_LO: u8 = 2;
    const RESET_ALARM: u8 = 1 << 0;
    /// RESET bit 1: clear the divider stages for the seconds and below.
    const RESET_DIVIDER: u8 = 1 << 1;
    /// Block-0 select of each counter digit, in logical digit order: the
    /// day of the week sits at 6, ahead of the day digits.
    const TIME_REGS: [u8; DIGITS] = [
        0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x7, 0x8, 0x9, 0xA, 0xB, 0xC, 0x6,
    ];
    /// Bits each counter digit implements; the rest read zero.
    const DIGIT_MASK: [u8; DIGITS] = [
        0xF, 0x7, 0xF, 0x7, 0xF, 0x3, 0xF, 0x3, 0xF, 0x1, 0xF, 0xF, 0x7,
    ];
    /// Block-1 selects: the 12/24 flag (bit 0: 1 = 24-hour) and the
    /// leap-year counter.
    const REG_1224: usize = 0xA;
    const REG_LEAP: u8 = 0xB;
    /// Alarm digit selects within block 1 (1-minute through 10-day).
    const ALARM_REGS: std::ops::RangeInclusive<usize> = 0x2..=0x8;

    /// Writable bits per block-1 select, mirroring the digit widths the
    /// part implements (a 10-minute alarm digit is three bits, and so on).
    /// Selects 0, 1, 9 and C hold no register at all. The leap-year
    /// counter (B) is not stored here: it lives with the calendar
    /// counters, which carry into it.
    const BANK1_WRITE_MASK: [u8; 13] = [
        0x0, 0x0, 0xF, 0x7, 0xF, 0x3, 0x7, 0xF, 0x3, 0x0, 0x1, 0x0, 0x0,
    ];

    /// The WinUAE/Amiberry RP5C01 `.nvram` file layout, kept bit-for-bit
    /// compatible so backing files interchange between emulators: three
    /// 16-byte blocks of one register value per byte -- the time digits
    /// as of the last save plus MODE/TEST/RESET at selects D-F, then the
    /// block-1 (alarm) registers, then the 13 battery RAM bytes (block 2
    /// low nibbles, block 3 high).
    const BATTMEM_FILE_SIZE: usize = 48;
    const BATTMEM_ALARM_OFFSET: usize = 16;
    const BATTMEM_RAM_OFFSET: usize = 32;

    fn bank1_default() -> [u8; 13] {
        let mut bank1 = [0u8; 13];
        bank1[Self::REG_1224] = 1;
        bank1
    }

    fn rules(&self) -> ChipRules {
        ChipRules {
            twenty_four: self.bank1[Self::REG_1224] & 1 != 0,
            pm_bit: 1 << 1,
            leap_counter: true,
        }
    }

    /// The logical counter digit a block-0 select addresses.
    fn time_digit(reg: u8) -> Option<usize> {
        Self::TIME_REGS.iter().position(|&select| select == reg)
    }

    pub fn read(&mut self, addr: u64, _size: usize, emulated_secs: f64) -> u64 {
        let reg = register_from_offset(addr);
        let val = match reg {
            0xD => self.mode,
            // TEST and RESET are write-only; the lane reads back zero.
            0xE | 0xF => 0,
            _ => match self.mode & Self::MODE_BLOCK_MASK {
                Self::BLOCK_TIME => self.read_time_register(reg, emulated_secs),
                Self::BLOCK_ALARM => self.read_alarm_register(reg, emulated_secs),
                Self::BLOCK_RAM_LO => self.ram[reg as usize],
                _ => self.ram[reg as usize] >> 4,
            },
        };
        u64::from(val & 0x0F)
    }

    pub fn write(&mut self, addr: u64, _size: usize, val: u64, emulated_secs: f64) {
        let reg = register_from_offset(addr);
        let val = (val & 0x0F) as u8;
        match reg {
            0xD => {
                // TIMER_EN low stops the carries -- the tearing-free
                // window Linux's driver opens around every access, and how
                // software sets the time. The divider keeps its count, so
                // a short window costs only its own length.
                let was_enabled = self.mode & Self::MODE_TIMER_EN != 0;
                let enabled = val & Self::MODE_TIMER_EN != 0;
                if was_enabled && !enabled {
                    self.clock.stop(emulated_secs, self.rules());
                } else if !was_enabled && enabled {
                    self.clock.start(emulated_secs);
                }
                self.mode = val;
                // Every battery-RAM access sequence brackets its nibble
                // writes in MODE writes (bank select in, bank restore
                // out -- battmem.resource and Linux's nvram driver both
                // do), so a MODE write is the transaction boundary that
                // flushes changed battery state to the backing file.
                if self.battmem_dirty {
                    self.flush_battmem(emulated_secs);
                }
            }
            0xE => {
                // TEST: not modelled, deliberately not persistent.
            }
            0xF => {
                if val & Self::RESET_ALARM != 0 {
                    if self.bank1[Self::ALARM_REGS].iter().any(|&digit| digit != 0) {
                        self.battmem_dirty = true;
                    }
                    self.bank1[Self::ALARM_REGS].fill(0);
                }
                if val & Self::RESET_DIVIDER != 0 {
                    self.clock.reset_divider(emulated_secs, self.rules());
                }
                // The 16 Hz / 1 Hz output gates go nowhere: no output pin
                // is wired on the Amiga boards.
            }
            _ => match self.mode & Self::MODE_BLOCK_MASK {
                // The counters are read/write: a digit is loaded as
                // written.
                Self::BLOCK_TIME => {
                    if let Some(digit) = Self::time_digit(reg) {
                        let val = val & Self::DIGIT_MASK[digit];
                        self.clock.edit(emulated_secs, self.rules(), |counters| {
                            counters.digits[digit] = val;
                        });
                    }
                }
                Self::BLOCK_ALARM if reg == Self::REG_LEAP => {
                    self.clock.edit(emulated_secs, self.rules(), |counters| {
                        counters.leap = val & 0x3
                    });
                }
                Self::BLOCK_ALARM => {
                    self.store_battery(reg, val & Self::BANK1_WRITE_MASK[reg as usize], true);
                }
                Self::BLOCK_RAM_LO => {
                    self.store_battery(reg, (self.ram[reg as usize] & 0xF0) | val, false);
                }
                _ => {
                    self.store_battery(reg, (val << 4) | (self.ram[reg as usize] & 0x0F), false);
                }
            },
        }
    }

    /// Store into a battery-backed register (`bank1` when `alarm`, the
    /// RAM byte otherwise), marking the backing file stale only when the
    /// value actually changed.
    fn store_battery(&mut self, reg: u8, val: u8, alarm: bool) {
        let slot = if alarm {
            &mut self.bank1[reg as usize]
        } else {
            &mut self.ram[reg as usize]
        };
        if *slot != val {
            *slot = val;
            self.battmem_dirty = true;
        }
    }

    fn read_time_register(&mut self, reg: u8, emulated_secs: f64) -> u8 {
        let counters = self.clock.counters(emulated_secs, self.rules());
        Self::time_digit(reg).map_or(0, |digit| counters.digits[digit])
    }

    fn read_alarm_register(&mut self, reg: u8, emulated_secs: f64) -> u8 {
        if reg == Self::REG_LEAP {
            // Counted alongside the years (0 = leap year).
            return self.clock.counters(emulated_secs, self.rules()).leap;
        }
        self.bank1[reg as usize]
    }

    /// Persist the battery-backed registers to (and preload them from)
    /// `path`, in the WinUAE/Amiberry `.nvram` layout. Only the alarm
    /// block (behind the hardware write masks) and the RAM bytes load
    /// back: the stored time digits are ignored because the clock
    /// starts each session from its time source, and the stored MODE is ignored
    /// because restoring a parked selector would defeat the power-on
    /// probe tell every battclock probe matches on (MODE reads `$8`).
    pub fn set_battmem_path(&mut self, path: std::path::PathBuf) {
        match std::fs::read(&path) {
            Ok(data) if data.len() >= Self::BATTMEM_FILE_SIZE => {
                for (reg, slot) in self.bank1.iter_mut().enumerate() {
                    *slot = data[Self::BATTMEM_ALARM_OFFSET + reg] & Self::BANK1_WRITE_MASK[reg];
                }
                for (reg, slot) in self.ram.iter_mut().enumerate() {
                    *slot = data[Self::BATTMEM_RAM_OFFSET + reg];
                }
            }
            Ok(data) if data.is_empty() => {}
            // A 16-byte file is WinUAE's MSM6242 flavour; anything else
            // short is a truncated write. Neither holds RP5C01 RAM, so
            // start battery-fresh rather than misread it.
            Ok(data) => log::warn!(
                "rp5c01 battmem: {} is {} bytes, not a {}-byte RP5C01 nvram file; starting fresh",
                path.display(),
                data.len(),
                Self::BATTMEM_FILE_SIZE
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("rp5c01 battmem: reading {}: {e}", path.display()),
        }
        self.battmem_path = Some(path);
        self.battmem_dirty = false;
    }

    fn flush_battmem(&mut self, emulated_secs: f64) {
        if let Some(path) = &self.battmem_path {
            // The directory is Copperline's own and may not exist yet; it
            // is made here rather than at bind time so a guest that never
            // writes the clock RAM leaves no empty folder behind.
            if let Err(e) = crate::paths::ensure_parent(path)
                .and_then(|()| std::fs::write(path, self.battmem_bytes(emulated_secs)))
            {
                // Stay dirty so the next MODE write retries: a transient
                // host error must not lose the battery state until the
                // guest happens to change it again.
                log::warn!("rp5c01 battmem: writing {}: {e}", path.display());
                return;
            }
        }
        self.battmem_dirty = false;
    }

    /// The full `.nvram` image: WinUAE also stores the time digits and
    /// control registers as of the save, so write them for file
    /// compatibility even though only the alarm and RAM blocks load back.
    fn battmem_bytes(&self, emulated_secs: f64) -> [u8; Self::BATTMEM_FILE_SIZE] {
        let mut bytes = [0u8; Self::BATTMEM_FILE_SIZE];
        let counters = self.clock.peek_counters(emulated_secs, self.rules());
        for (digit, &reg) in Self::TIME_REGS.iter().enumerate() {
            bytes[reg as usize] = counters.digits[digit];
        }
        bytes[0xD] = self.mode;
        // Selects E/F (TEST, RESET) are write-only and hold nothing.
        bytes[Self::BATTMEM_ALARM_OFFSET..][..self.bank1.len()].copy_from_slice(&self.bank1);
        bytes[Self::BATTMEM_ALARM_OFFSET + Self::REG_LEAP as usize] = counters.leap;
        bytes[Self::BATTMEM_RAM_OFFSET..][..self.ram.len()].copy_from_slice(&self.ram);
        bytes
    }

    /// A CPU reset does not reach the battery-backed chip: the counters,
    /// alarm registers, 12/24 select, and battery RAM all survive. Only
    /// the volatile-looking selector state returns to its power-on
    /// default, which is also the first thing any OS probe
    /// re-establishes; a clock the reset found stopped runs again.
    pub fn reset(&mut self) {
        self.mode = Self::MODE_TIMER_EN;
        self.clock.start(0.0);
    }
}

/// `COPPERLINE_RTC_FIXED_SECS`: pins the clock to a fixed Unix-seconds
/// value, making RTC reads deterministic across runs (otherwise the host
/// wall clock differs run-to-run, which pollutes differential traces with
/// spurious timestamp divergences). As a diagnostic override it wins over
/// the configured seed.
fn fixed_secs_override() -> Option<u64> {
    crate::envcfg::var("COPPERLINE_RTC_FIXED_SECS").and_then(|s| s.trim().parse::<u64>().ok())
}

/// The guest clock's power-on instant in Unix seconds when it is pinned
/// rather than following the host clock: the `COPPERLINE_RTC_FIXED_SECS`
/// override, else the configured `seed_unix` (`[machine] rtc_time` /
/// `--rtc-time`). `None` means the guest clock reads the host wall clock.
/// Host-side state the guest reads back as a date -- the volume dates of
/// host filesystem mounts -- takes it too, so a run with a pinned clock
/// boots the same way every time.
pub fn pinned_power_on_unix(seed_unix: Option<u64>) -> Option<u64> {
    fixed_secs_override().or(seed_unix)
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

    impl Msm6242Rtc {
        fn set_test_time(&mut self, time: SystemTime) {
            self.clock.set_test_time(time);
        }
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
        rtc.clock.set_seed(Some(VECTOR_UNIX), false);

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
        assert_eq!(rtc.clock.current_unix(31.9, rtc.rules()), VECTOR_UNIX + 31);
    }

    #[test]
    fn frozen_clock_never_advances() {
        let mut rtc = Rtc::new(RtcChip::Msm6242);
        rtc.set_seed(Some(VECTOR_UNIX), true);
        assert_eq!(rtc.read(0x0, 4, 3600.0), 9);
        assert_eq!(rtc.current_unix(3600.0), VECTOR_UNIX);
        assert_eq!(rtc.current_display(3600.0), "2005-03-18T01:58:29");
    }

    #[test]
    fn reset_keeps_the_seed_but_drops_the_hold_latch() {
        let mut rtc = Msm6242Rtc::default();
        rtc.clock.set_seed(Some(VECTOR_UNIX), false);
        rtc.write(0xD * 4, 4, Msm6242Rtc::CD_HOLD as u64, 0.0);
        rtc.reset();
        assert_eq!(read_reg(&mut rtc, 0xD) & Msm6242Rtc::CD_HOLD, 0);
        assert_eq!(read_reg_at(&mut rtc, 0x0, 5.0), 4); // still ticking from the seed
        assert_eq!(rtc.clock.seed(), Some(VECTOR_UNIX));
    }

    fn seeded_msm(seed: u64) -> Msm6242Rtc {
        let mut rtc = Msm6242Rtc::default();
        rtc.clock.set_seed(Some(seed), false);
        rtc
    }

    fn msm_write(rtc: &mut Msm6242Rtc, reg: u8, val: u8, at: f64) {
        rtc.write(u64::from(reg) * 4, 4, u64::from(val), at);
    }

    fn msm_digits(rtc: &mut Msm6242Rtc, at: f64) -> [u8; 13] {
        std::array::from_fn(|reg| read_reg_at(rtc, reg as u8, at))
    }

    /// STOP | REST, write the counters, release: the first carry is one
    /// second after the release.
    fn msm_set_and_start(rtc: &mut Msm6242Rtc, digits: [u8; 13], cf: u8, at: f64) {
        msm_write(rtc, 0xF, cf | Msm6242Rtc::CF_STOP | Msm6242Rtc::CF_REST, at);
        for (reg, &digit) in digits.iter().enumerate() {
            msm_write(rtc, reg as u8, digit, at);
        }
        msm_write(rtc, 0xF, cf, at);
    }

    #[test]
    fn msm6242_stop_holds_the_counters_and_rest_restarts_the_second() {
        let mut rtc = seeded_msm(VECTOR_UNIX);
        let before = msm_digits(&mut rtc, 0.5);
        msm_write(&mut rtc, 0xF, 0x4 | 0x2 | 0x1, 0.5);
        assert_eq!(read_reg_at(&mut rtc, 0xF, 0.5), 0x7);
        assert_eq!(msm_digits(&mut rtc, 2.0), before);

        msm_write(&mut rtc, 0xF, 0x4, 2.0);
        assert_eq!(msm_digits(&mut rtc, 2.9), before);
        assert_eq!(read_reg_at(&mut rtc, 0x0, 3.1), 0); // :30
        assert_eq!(read_reg_at(&mut rtc, 0x1, 3.1), 3);
    }

    #[test]
    fn msm6242_counters_take_a_written_time_and_roll_over_by_the_year_digits() {
        let mut rtc = seeded_msm(VECTOR_UNIX);
        let written = digits(42, 37, 1, 3, 14, 7, 95, 5);
        msm_set_and_start(&mut rtc, written, 0x4, 0.0);
        assert_eq!(msm_digits(&mut rtc, 0.9), written);
        assert_eq!(
            msm_digits(&mut rtc, 1.0),
            digits(43, 37, 1, 3, 14, 7, 95, 5)
        );

        // Year 24 is a leap year, year 23 is not.
        msm_set_and_start(&mut rtc, digits(59, 59, 2, 3, 28, 2, 24, 3), 0x4, 2.0);
        assert_eq!(msm_digits(&mut rtc, 3.0), digits(0, 0, 0, 0, 29, 2, 24, 4));
        msm_set_and_start(&mut rtc, digits(59, 59, 2, 3, 28, 2, 23, 3), 0x4, 4.0);
        assert_eq!(msm_digits(&mut rtc, 5.0), digits(0, 0, 0, 0, 1, 3, 23, 4));
        msm_set_and_start(&mut rtc, digits(59, 59, 2, 3, 31, 12, 99, 6), 0x4, 6.0);
        assert_eq!(msm_digits(&mut rtc, 7.0), digits(0, 0, 0, 0, 1, 1, 0, 0));
    }

    #[test]
    fn msm6242_twelve_hour_mode_carries_the_pm_flag_in_bit_2_of_the_ten_hours() {
        let mut rtc = seeded_msm(VECTOR_UNIX);
        msm_set_and_start(&mut rtc, digits(59, 59, 1, 1, 14, 7, 95, 5), 0x0, 0.0);
        // 11:59:59 AM -> 12:00:00 PM
        assert_eq!(
            msm_digits(&mut rtc, 1.0),
            digits(0, 0, 4 | 1, 2, 14, 7, 95, 5)
        );
        msm_set_and_start(&mut rtc, digits(59, 59, 4 | 1, 1, 14, 7, 95, 5), 0x0, 2.0);
        // 11:59:59 PM -> 12:00:00 AM of the next day
        assert_eq!(msm_digits(&mut rtc, 3.0), digits(0, 0, 1, 2, 15, 7, 95, 6));
    }

    #[test]
    fn msm6242_thirty_second_adjust_rounds_to_the_minute_and_clears_itself() {
        let mut rtc = seeded_msm(VECTOR_UNIX);
        msm_set_and_start(&mut rtc, digits(20, 37, 1, 3, 14, 7, 95, 5), 0x4, 0.0);
        msm_write(&mut rtc, 0xD, Msm6242Rtc::CD_ADJ, 0.02);
        assert_eq!(
            msm_digits(&mut rtc, 0.03),
            digits(0, 37, 1, 3, 14, 7, 95, 5)
        );
        assert_eq!(read_reg_at(&mut rtc, 0xD, 0.03) & Msm6242Rtc::CD_ADJ, 0);

        msm_set_and_start(&mut rtc, digits(40, 37, 1, 3, 14, 7, 95, 5), 0x4, 1.0);
        msm_write(&mut rtc, 0xD, Msm6242Rtc::CD_ADJ, 1.02);
        assert_eq!(
            msm_digits(&mut rtc, 1.03),
            digits(0, 38, 1, 3, 14, 7, 95, 5)
        );
    }

    #[test]
    fn msm6242_hold_freezes_the_registers_and_keeps_the_carry_phase() {
        let mut rtc = seeded_msm(VECTOR_UNIX);
        msm_write(&mut rtc, 0xD, Msm6242Rtc::CD_HOLD, 0.45);
        let held = msm_digits(&mut rtc, 0.45);
        assert_eq!(msm_digits(&mut rtc, 1.3), held);
        msm_write(&mut rtc, 0xD, 0, 1.3);
        // The carry that fell inside the hold shows on release, and the
        // next one comes at the old phase.
        assert_eq!(read_reg_at(&mut rtc, 0x0, 1.36), 0);
        assert_eq!(read_reg_at(&mut rtc, 0x0, 1.99), 0);
        assert_eq!(read_reg_at(&mut rtc, 0x0, 2.0), 1);
    }

    #[test]
    fn msm6242_busy_marks_the_carry_once_a_second() {
        let mut rtc = seeded_msm(VECTOR_UNIX);
        let busy = |rtc: &mut Msm6242Rtc, at: f64| read_reg_at(rtc, 0xD, at) & Msm6242Rtc::CD_BUSY;
        assert_ne!(busy(&mut rtc, 1.0001), 0);
        assert_eq!(busy(&mut rtc, 1.001), 0);
        assert_eq!(busy(&mut rtc, 1.5), 0);
        // A stopped clock has no carry to announce.
        msm_write(&mut rtc, 0xF, 0x4 | 0x2, 1.6);
        assert_eq!(busy(&mut rtc, 2.0001), 0);
    }

    fn seeded_rp5c01(seed: u64) -> Rp5c01Rtc {
        let mut rtc = Rp5c01Rtc::default();
        rtc.clock.set_seed(Some(seed), false);
        rtc
    }

    fn rp_read_at(rtc: &mut Rp5c01Rtc, reg: u8, emulated_secs: f64) -> u8 {
        rtc.read((reg as u64) * 4, 4, emulated_secs) as u8
    }

    fn rp_read(rtc: &mut Rp5c01Rtc, reg: u8) -> u8 {
        rp_read_at(rtc, reg, 0.0)
    }

    fn rp_write(rtc: &mut Rp5c01Rtc, reg: u8, val: u8) {
        rtc.write((reg as u64) * 4, 4, val as u64, 0.0);
    }

    /// The power-on tell every battclock probe matches on: MODE reads $8
    /// (timer running, block 0) while the write-only TEST and RESET
    /// selects read zero. AROS accepts exactly D == 8 && F == 0 as an
    /// "RF5C01A"; an MSM6242 shows control F's 24-hour bit instead.
    #[test]
    fn rp5c01_powers_on_as_the_clock_probes_expect() {
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        assert_eq!(rp_read(&mut rtc, 0xD), 0x8);
        assert_eq!(rp_read(&mut rtc, 0xE), 0x0);
        assert_eq!(rp_read(&mut rtc, 0xF), 0x0);
        // The "not a clock" early-outs: 10-second and 10-minute digits
        // never show bit 3.
        assert_eq!(rp_read(&mut rtc, 0x1) & 8, 0);
        assert_eq!(rp_read(&mut rtc, 0x3) & 8, 0);
    }

    /// Block 0 has the Ricoh layout: weekday at 6, day at 7-8, month at
    /// 9-A, year at B-C -- shifted one select up from the Oki map by the
    /// weekday register moving to the front.
    #[test]
    fn rp5c01_time_registers_expose_the_ricoh_layout() {
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        assert_eq!(rp_read(&mut rtc, 0x0), 9); // seconds ones
        assert_eq!(rp_read(&mut rtc, 0x1), 2); // seconds tens
        assert_eq!(rp_read(&mut rtc, 0x2), 8); // minutes ones
        assert_eq!(rp_read(&mut rtc, 0x3), 5); // minutes tens
        assert_eq!(rp_read(&mut rtc, 0x4), 1); // hours ones
        assert_eq!(rp_read(&mut rtc, 0x5), 0); // hours tens
        assert_eq!(rp_read(&mut rtc, 0x6), 5); // Friday
        assert_eq!(rp_read(&mut rtc, 0x7), 8); // day ones
        assert_eq!(rp_read(&mut rtc, 0x8), 1); // day tens
        assert_eq!(rp_read(&mut rtc, 0x9), 3); // month ones
        assert_eq!(rp_read(&mut rtc, 0xA), 0); // month tens
        assert_eq!(rp_read(&mut rtc, 0xB), 5); // year ones
        assert_eq!(rp_read(&mut rtc, 0xC), 0); // year tens
    }

    /// The Linux rtc-rp5c01 access pattern: clear TIMER_EN (MODE = block
    /// 0) for a tearing-free window, read the digits, then park the chip
    /// in MODE = $9. The held view must not advance, and re-enabling the
    /// timer must resume the running clock without losing the interval.
    #[test]
    fn rp5c01_counters_stand_still_while_the_timer_is_disabled() {
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        // Half a second into :29, TIMER_EN goes low: the tearing-free
        // window Linux opens around an access.
        rtc.write(0xD * 4, 4, 0x0, 0.5);
        assert_eq!(rp_read_at(&mut rtc, 0x0, 31.0), 9); // still :29, not :00
        assert_eq!(rp_read_at(&mut rtc, 0x2, 31.0), 8);

        rtc.write(0xD * 4, 4, 0x9, 31.0); // timer on, block 1
        assert_eq!(rtc.read(0xD * 4, 4, 31.0), 0x9);
        rtc.write(0xD * 4, 4, 0x8, 31.0);
        // The stopped interval is lost, as on the part, and the divider
        // carries on from the half second it held.
        assert_eq!(rp_read_at(&mut rtc, 0x0, 31.4), 9);
        assert_eq!(rp_read_at(&mut rtc, 0x0, 31.6), 0); // :30
        assert_eq!(rp_read_at(&mut rtc, 0x1, 31.6), 3);
        assert_eq!(rp_read_at(&mut rtc, 0x2, 31.6), 8); // of minute 58
    }

    /// Stop, reset the divider, write a time, start: the first carry is
    /// one second after the start and advances the written time by one.
    fn rp_set_and_start(rtc: &mut Rp5c01Rtc, digits: [u8; 13], leap: u8, at: f64) {
        rtc.write(0xD * 4, 4, 0x0, at);
        rtc.write(0xF * 4, 4, 0x2, at);
        rtc.write(0xD * 4, 4, 0x1, at);
        rtc.write(0xB * 4, 4, u64::from(leap), at);
        rtc.write(0xD * 4, 4, 0x0, at);
        for (digit, &reg) in Rp5c01Rtc::TIME_REGS.iter().enumerate() {
            rtc.write(u64::from(reg) * 4, 4, u64::from(digits[digit]), at);
        }
        rtc.write(0xD * 4, 4, 0x8, at);
        rtc.write(0xF * 4, 4, 0x0, at);
    }

    fn rp_digits(rtc: &mut Rp5c01Rtc, at: f64) -> [u8; 13] {
        Rp5c01Rtc::TIME_REGS.map(|reg| rp_read_at(rtc, reg, at))
    }

    /// `[s1, s10, mi1, mi10, h1, h10, d1, d10, mo1, mo10, y1, y10, wd]`.
    fn digits(s: u8, mi: u8, h10: u8, h1: u8, d: u8, mo: u8, y: u8, wd: u8) -> [u8; 13] {
        [
            s % 10,
            s / 10,
            mi % 10,
            mi / 10,
            h1,
            h10,
            d % 10,
            d / 10,
            mo % 10,
            mo / 10,
            y % 10,
            y / 10,
            wd,
        ]
    }

    #[test]
    fn rp5c01_counters_take_a_written_time_and_count_from_it() {
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        let written = digits(42, 37, 1, 3, 14, 7, 95, 5);
        rp_set_and_start(&mut rtc, written, 3, 10.25);

        assert_eq!(rp_digits(&mut rtc, 10.25), written);
        // The divider was reset: nothing carries for a whole second.
        assert_eq!(rp_digits(&mut rtc, 11.2), written);
        assert_eq!(
            rp_digits(&mut rtc, 11.3),
            digits(43, 37, 1, 3, 14, 7, 95, 5)
        );
        assert_eq!(
            rtc.clock.current_display(11.3, rtc.rules()),
            "1995-07-14T13:37:43"
        );
        // The seed is still what a new machine would start from.
        assert_eq!(rtc.clock.seed(), Some(VECTOR_UNIX));
    }

    #[test]
    fn rp5c01_leap_year_follows_the_leap_counter_not_the_year_digits() {
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        let feb28 = digits(59, 59, 2, 3, 28, 2, 23, 3);
        // Leap counter 0: February has 29 days whatever the year reads.
        rp_set_and_start(&mut rtc, feb28, 0, 0.0);
        assert_eq!(rp_digits(&mut rtc, 1.0), digits(0, 0, 0, 0, 29, 2, 23, 4));
        // Leap counter 3: a common year.
        rp_set_and_start(&mut rtc, feb28, 3, 2.0);
        assert_eq!(rp_digits(&mut rtc, 3.0), digits(0, 0, 0, 0, 1, 3, 23, 4));

        // The year carry counts the leap counter with it, and the day of
        // the week is a counter of its own.
        rp_set_and_start(&mut rtc, digits(59, 59, 2, 3, 31, 12, 99, 6), 3, 4.0);
        assert_eq!(rp_digits(&mut rtc, 5.0), digits(0, 0, 0, 0, 1, 1, 0, 0));
        rtc.write(0xD * 4, 4, 0x9, 5.0);
        assert_eq!(rp_read_at(&mut rtc, 0xB, 5.0), 0);
    }

    #[test]
    fn rp5c01_twelve_hour_mode_carries_the_pm_flag_in_the_ten_hours_digit() {
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        // 12-hour select, then 3 PM written as PM | 0, 3.
        rtc.write(0xD * 4, 4, 0x1, 0.0);
        rtc.write(0xA * 4, 4, 0x0, 0.0);
        rp_set_and_start(&mut rtc, digits(42, 37, 2, 3, 14, 7, 95, 5), 3, 0.0);
        assert_eq!(rp_digits(&mut rtc, 0.0), digits(42, 37, 2, 3, 14, 7, 95, 5));

        // 11:59:59 AM carries to 12:00:00 PM.
        rp_set_and_start(&mut rtc, digits(59, 59, 1, 1, 14, 7, 95, 5), 3, 1.0);
        assert_eq!(
            rp_digits(&mut rtc, 2.0),
            digits(0, 0, 2 | 1, 2, 14, 7, 95, 5)
        );
        // 11:59:59 PM carries to 12:00:00 AM of the next day.
        rp_set_and_start(&mut rtc, digits(59, 59, 2 | 1, 1, 14, 7, 95, 5), 3, 3.0);
        assert_eq!(rp_digits(&mut rtc, 4.0), digits(0, 0, 1, 2, 15, 7, 95, 6));
    }

    /// Block 1 stores alarm digits behind their hardware write masks, and
    /// RESET bit 0 clears them all.
    #[test]
    fn rp5c01_alarm_registers_store_masked_digits_until_alarm_reset() {
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        rp_write(&mut rtc, 0xD, 0x9); // block 1
        for reg in 0x2..=0x8 {
            rp_write(&mut rtc, reg, 0xF);
        }
        assert_eq!(rp_read(&mut rtc, 0x2), 0xF); // 1-minute alarm
        assert_eq!(rp_read(&mut rtc, 0x3), 0x7); // 10-minute: 3 bits
        assert_eq!(rp_read(&mut rtc, 0x5), 0x3); // 10-hour: 2 bits
        assert_eq!(rp_read(&mut rtc, 0x8), 0x3); // 10-day: 2 bits
                                                 // Selects with no register behind them stay empty.
        rp_write(&mut rtc, 0x0, 0xF);
        rp_write(&mut rtc, 0x9, 0xF);
        assert_eq!(rp_read(&mut rtc, 0x0), 0x0);
        assert_eq!(rp_read(&mut rtc, 0x9), 0x0);

        rp_write(&mut rtc, 0xF, Rp5c01Rtc::RESET_ALARM);
        for reg in 0x2..=0x8 {
            assert_eq!(rp_read(&mut rtc, reg), 0, "alarm select {reg:#X}");
        }
    }

    /// The 12/24 select (block 1, select A) reshapes the hour digits: in
    /// 12-hour mode the ten-hours register carries PM in bit 1 and hours
    /// read 12, 1..11.
    #[test]
    fn rp5c01_12_hour_mode_sets_the_pm_flag() {
        // 13:58:29 (VECTOR + 12h): 24-hour mode first.
        let mut rtc = seeded_rp5c01(VECTOR_UNIX + 12 * 3600);
        assert_eq!(rp_read(&mut rtc, 0x4), 3);
        assert_eq!(rp_read(&mut rtc, 0x5), 1);

        rp_write(&mut rtc, 0xD, 0x9); // block 1
        rp_write(&mut rtc, 0xA, 0x0); // 12-hour mode
        rp_write(&mut rtc, 0xD, 0x8); // back to time
        assert_eq!(rp_read(&mut rtc, 0x4), 1); // 1 PM
        assert_eq!(rp_read(&mut rtc, 0x5), 2); // PM flag, no tens

        // Midnight reads as 12 with the PM flag clear.
        let mut rtc = seeded_rp5c01(VECTOR_UNIX - 3600 - 58 * 60 - 29);
        rp_write(&mut rtc, 0xD, 0x9);
        rp_write(&mut rtc, 0xA, 0x0);
        rp_write(&mut rtc, 0xD, 0x8);
        assert_eq!(rp_read(&mut rtc, 0x4), 2);
        assert_eq!(rp_read(&mut rtc, 0x5), 1);
    }

    /// The leap-year counter tracks the year counter: 0 in a leap year,
    /// counting up to 3. 2005 % 4 = 1; one year earlier it reads 0.
    #[test]
    fn rp5c01_leap_year_counter_tracks_the_year() {
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        rp_write(&mut rtc, 0xD, 0x9);
        assert_eq!(rp_read(&mut rtc, 0xB), 1);

        let mut rtc = seeded_rp5c01(VECTOR_UNIX - 365 * 86_400);
        rp_write(&mut rtc, 0xD, 0x9);
        assert_eq!(rp_read(&mut rtc, 0xB), 0); // 2004
    }

    /// Blocks 2 and 3 are the 26 battery RAM nibbles (battmem.resource's
    /// storage on the A3000/A4000): block 2 holds the low nibble of each
    /// of 13 bytes, block 3 the high one, and both survive a CPU reset
    /// like everything battery-powered.
    #[test]
    fn rp5c01_ram_blocks_hold_26_battery_nibbles_across_reset() {
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        rp_write(&mut rtc, 0xD, 0xA); // block 2: low nibbles
        rp_write(&mut rtc, 0x0, 0x5);
        rp_write(&mut rtc, 0xC, 0xF);
        rp_write(&mut rtc, 0xD, 0xB); // block 3: high nibbles
        rp_write(&mut rtc, 0x0, 0xA);

        rp_write(&mut rtc, 0xD, 0xA);
        assert_eq!(rp_read(&mut rtc, 0x0), 0x5);
        assert_eq!(rp_read(&mut rtc, 0xC), 0xF);
        rp_write(&mut rtc, 0xD, 0xB);
        assert_eq!(rp_read(&mut rtc, 0x0), 0xA);
        assert_eq!(rp_read(&mut rtc, 0xC), 0x0);

        rtc.reset();
        assert_eq!(rp_read(&mut rtc, 0xD), 0x8); // selector back to power-on
        rp_write(&mut rtc, 0xD, 0xB);
        assert_eq!(rp_read(&mut rtc, 0x0), 0xA); // RAM kept
        rp_write(&mut rtc, 0xD, 0x9);
        assert_eq!(rp_read(&mut rtc, 0xA), 0x1); // 12/24 select kept too
    }

    /// Writes to the time counters never move the read-only clock view
    /// (module policy: the guest cannot set the host or seeded clock).
    #[test]
    fn a_new_seed_replaces_a_time_the_guest_set() {
        let mut rtc = Rtc::new(RtcChip::Rp5c01);
        rtc.set_seed(Some(VECTOR_UNIX), false);
        rtc.write(0x0, 4, 0x7, 0.0);
        assert_eq!(rtc.read(0x0, 4, 0.0), 7);
        assert_eq!(rtc.current_display(0.0), "2005-03-18T01:58:27");

        rtc.set_seed(Some(VECTOR_UNIX), false);
        assert_eq!(rtc.read(0x0, 4, 0.0), 9);
        assert_eq!(rtc.current_unix(0.0), VECTOR_UNIX);
    }

    /// A per-test battmem backing path in the host temp directory,
    /// removed up front so each test starts battery-fresh.
    fn battmem_file(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "copperline-battmem-{}-{name}.nvram",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// A battery-RAM write sequence (bank select in, nibble writes, bank
    /// restore out) must land on disk in the WinUAE/Amiberry layout:
    /// time digits + MODE in the first 16-byte block, the alarm bank in
    /// the second, the 13 combined RAM bytes in the third. The restore
    /// of MODE is the flush boundary.
    #[test]
    fn rp5c01_battmem_flushes_the_winuae_nvram_layout_on_mode_restore() {
        let path = battmem_file("flush");
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        rtc.set_battmem_path(path.clone());

        rp_write(&mut rtc, 0xD, 0xA); // block 2: low nibbles
        rp_write(&mut rtc, 0x0, 0x5);
        rp_write(&mut rtc, 0xD, 0xB); // block 3: high nibbles
        rp_write(&mut rtc, 0x0, 0xA);
        rp_write(&mut rtc, 0xD, 0x8); // restore: transaction over

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 48);
        // Time digits of the seeded 2005-03-18T01:58:29 (a Friday) in the
        // Ricoh block-0 layout, then MODE as restored.
        assert_eq!(&bytes[0x0..=0xC], &[9, 2, 8, 5, 1, 0, 5, 8, 1, 3, 0, 5, 0]);
        assert_eq!(bytes[0xD], 0x8);
        assert_eq!(bytes[0xE], 0);
        assert_eq!(bytes[0xF], 0);
        assert_eq!(bytes[0x1A], 1); // 12/24 select in the alarm block: 24h
        assert_eq!(bytes[0x20], 0xA5); // RAM byte 0: high and low nibbles

        // A fresh chip preloads the same battery state from the file.
        let mut resumed = seeded_rp5c01(VECTOR_UNIX);
        resumed.set_battmem_path(path.clone());
        rp_write(&mut resumed, 0xD, 0xA);
        assert_eq!(rp_read(&mut resumed, 0x0), 0x5);
        rp_write(&mut resumed, 0xD, 0xB);
        assert_eq!(rp_read(&mut resumed, 0x0), 0xA);
        let _ = std::fs::remove_file(&path);
    }

    /// Loading an Amiberry/WinUAE file takes only the battery payload:
    /// alarm digits (behind the hardware write masks) and RAM bytes. The
    /// stored time never touches the read-only clock, and the stored
    /// MODE never overrides the power-on value the chip probes match on.
    #[test]
    fn rp5c01_battmem_load_takes_ram_and_alarm_but_not_clock_or_mode() {
        let path = battmem_file("load");
        let mut file = [0u8; 48];
        // Time block from a foreign save, MODE parked at $9.
        file[..16].copy_from_slice(&[7, 4, 4, 5, 4, 1, 4, 3, 2, 7, 0, 6, 2, 9, 0, 0]);
        file[0x12] = 0xF; // 1-minute alarm digit (4 bits wide)
        file[0x13] = 0xFF; // 10-minute alarm digit: masked to 3 bits
        file[0x1A] = 1; // 24-hour select
        file[0x20] = 0x07;
        file[0x2C] = 0x89;
        std::fs::write(&path, file).unwrap();

        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        rtc.set_battmem_path(path.clone());
        assert_eq!(rp_read(&mut rtc, 0xD), 0x8); // power-on probe tell kept
        assert_eq!(rp_read(&mut rtc, 0x0), 9); // seeded clock, not the file's
        rp_write(&mut rtc, 0xD, 0x9); // block 1
        assert_eq!(rp_read(&mut rtc, 0x2), 0xF);
        assert_eq!(rp_read(&mut rtc, 0x3), 0x7); // hardware mask applied
        rp_write(&mut rtc, 0xD, 0xA); // block 2
        assert_eq!(rp_read(&mut rtc, 0x0), 0x7);
        assert_eq!(rp_read(&mut rtc, 0xC), 0x9);
        rp_write(&mut rtc, 0xD, 0xB); // block 3
        assert_eq!(rp_read(&mut rtc, 0xC), 0x8);
        let _ = std::fs::remove_file(&path);
    }

    /// Clock reads bracket their digit reads in MODE writes too; those
    /// must not rewrite the backing file. Only a write that changes
    /// battery state arms the flush -- rewriting an identical value does
    /// not count as a change.
    #[test]
    fn rp5c01_battmem_flushes_only_after_a_real_battery_change() {
        let path = battmem_file("clean");
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        rtc.set_battmem_path(path.clone());

        rp_write(&mut rtc, 0xD, 0x0); // lock for a clock read
        rp_write(&mut rtc, 0xD, 0x8); // unlock
        rp_write(&mut rtc, 0xD, 0xA); // select RAM
        rp_write(&mut rtc, 0x3, 0x0); // rewrite the value already there
        rp_write(&mut rtc, 0xD, 0x8);
        assert!(!path.exists(), "no battery change, no file");

        rp_write(&mut rtc, 0xD, 0xA);
        rp_write(&mut rtc, 0x3, 0x6);
        rp_write(&mut rtc, 0xD, 0x8);
        assert!(path.exists(), "a changed nibble flushes on MODE restore");
        let _ = std::fs::remove_file(&path);
    }

    /// A failed backing-file write must leave the dirty bit armed so a
    /// later MODE write retries; clearing it up front would silently
    /// drop the battery state on a transient host error.
    #[test]
    fn rp5c01_battmem_retries_the_flush_after_a_failed_write() {
        let dir = battmem_file("retry-dir");
        let path = dir.join("battmem.nvram");
        // A directory where the file goes: the write fails, and unlike a
        // missing parent it stays failing, since the flush makes its own
        // directories now.
        std::fs::create_dir_all(&path).unwrap();
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        rtc.set_battmem_path(path.clone());

        rp_write(&mut rtc, 0xD, 0xA);
        rp_write(&mut rtc, 0x0, 0x5);
        rp_write(&mut rtc, 0xD, 0x8); // flush fails: a directory is in the way
        assert!(path.is_dir(), "nothing should have been written over it");

        std::fs::remove_dir(&path).unwrap();
        rp_write(&mut rtc, 0xD, 0x8); // still dirty: retried and lands
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes[0x20], 0x05);

        rp_write(&mut rtc, 0xD, 0xA); // clean again: no further writes
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that is not a 48-byte RP5C01 image (WinUAE's 16-byte
    /// MSM6242 flavour, or a truncated write) is ignored: the chip
    /// starts battery-fresh instead of misreading it.
    #[test]
    fn rp5c01_battmem_ignores_files_of_the_wrong_shape() {
        let path = battmem_file("short");
        std::fs::write(&path, [0xAB; 16]).unwrap();
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        rtc.set_battmem_path(path.clone());
        rp_write(&mut rtc, 0xD, 0xA);
        for reg in 0x0..=0xC {
            assert_eq!(rp_read(&mut rtc, reg), 0, "RAM select {reg:#X}");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rtc_wrapper_selects_and_reports_the_chip() {
        let mut rtc = Rtc::new(RtcChip::Rp5c01);
        assert_eq!(rtc.chip(), RtcChip::Rp5c01);
        assert_eq!(rtc.chip().label(), "RP5C01");
        rtc.set_seed(Some(VECTOR_UNIX), true);
        assert_eq!(rtc.seed(), Some(VECTOR_UNIX));
        assert!(rtc.frozen());
        assert_eq!(rtc.read(0xD * 4, 4, 0.0), 0x8);
        assert_eq!(Rtc::default().chip(), RtcChip::Msm6242);
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
