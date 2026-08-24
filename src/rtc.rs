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
//! Copperline exposes a read-only wall-clock view: guest writes drive the
//! chips' latch/bank/control state (and the RP5C01's battery RAM), but
//! they never change the host clock.
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
        self.clock().current_unix(emulated_secs)
    }

    /// The broken-down time register reads expose right now, formatted as
    /// `YYYY-MM-DDTHH:MM:SS` (for status reporting, not the guest).
    pub fn current_display(&self, emulated_secs: f64) -> String {
        self.clock().current_display(emulated_secs)
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

/// The time source both chips decompose into registers: the host's local
/// wall clock by default, or the deterministic seed (`[machine] rtc_time`)
/// ticking forward in emulated time.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct ClockSource {
    /// Power-on clock value in Unix seconds. When set, register reads
    /// derive from seed + elapsed emulated seconds instead of the host
    /// wall clock, making the guest-visible time deterministic.
    seed_unix: Option<u64>,
    /// Stop the seeded clock: reads always decompose the seed itself.
    frozen: bool,
    #[cfg(test)]
    test_time: Option<SystemTime>,
}

impl ClockSource {
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

    fn set_seed(&mut self, seed_unix: Option<u64>, frozen: bool) {
        self.seed_unix = seed_unix;
        self.frozen = frozen && seed_unix.is_some();
    }

    fn seed(&self) -> Option<u64> {
        self.seed_unix
    }

    fn frozen(&self) -> bool {
        self.frozen
    }

    /// The Unix-seconds instant register reads decompose right now,
    /// following the same source precedence as `current_time` (the
    /// host-local path reports plain host Unix seconds).
    fn current_unix(&self, emulated_secs: f64) -> u64 {
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

    fn current_display(&self, emulated_secs: f64) -> String {
        self.current_time(emulated_secs).iso8601()
    }

    #[cfg(test)]
    fn set_test_time(&mut self, time: SystemTime) {
        self.test_time = Some(time);
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Msm6242Rtc {
    control_d: u8,
    control_e: u8,
    latched: Option<RtcDateTime>,
    clock: ClockSource,
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
                        self.latched = Some(self.clock.current_time(emulated_secs));
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
            .unwrap_or_else(|| self.clock.current_time(emulated_secs));
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

    /// A CPU reset does not reach the battery-backed chip, so the time
    /// source keeps running; only the bus-visible latch state drops back
    /// to power-on defaults.
    pub fn reset(&mut self) {
        self.control_d = 0;
        self.control_e = 0;
        self.latched = None;
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
/// write for a tearing-free window -- and bit 2 arms the alarm (stored
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
    /// Time held while TIMER_EN is low. The underlying source keeps
    /// running, so unlike the real stopped chip no time is lost.
    latched: Option<RtcDateTime>,
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
            latched: None,
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
    /// Block-1 selects: the 12/24 flag (bit 0: 1 = 24-hour) and the
    /// leap-year counter.
    const REG_1224: usize = 0xA;
    const REG_LEAP: u8 = 0xB;
    /// Alarm digit selects within block 1 (1-minute through 10-day).
    const ALARM_REGS: std::ops::RangeInclusive<usize> = 0x2..=0x8;

    /// Writable bits per block-1 select, mirroring the digit widths the
    /// part implements (a 10-minute alarm digit is three bits, and so on).
    /// Selects 0, 1, 9 and C hold no register at all. The leap-year
    /// counter (B) is masked off too: reads derive it from the year
    /// counter, which is the value it can never drift from here.
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
                // Clearing TIMER_EN holds the read view still -- the
                // tearing-free window Linux's driver opens around every
                // access. The time source keeps running underneath, so
                // the stopped interval is not actually lost.
                if val & Self::MODE_TIMER_EN == 0 {
                    if self.latched.is_none() {
                        self.latched = Some(self.clock.current_time(emulated_secs));
                    }
                } else {
                    self.latched = None;
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
                // The fraction reset and the 16 Hz / 1 Hz output gates go
                // nowhere: sub-second phase derives from the time source
                // and no output pin is wired on the Amiga boards.
            }
            _ => match self.mode & Self::MODE_BLOCK_MASK {
                // Time-counter writes never move the read-only host or
                // seeded clock (module policy, same as the MSM6242).
                Self::BLOCK_TIME => {}
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

    fn time(&self, emulated_secs: f64) -> RtcDateTime {
        self.latched
            .unwrap_or_else(|| self.clock.current_time(emulated_secs))
    }

    fn read_time_register(&self, reg: u8, emulated_secs: f64) -> u8 {
        let time = self.time(emulated_secs);
        match reg {
            0x0 => time.second % 10,
            0x1 => time.second / 10,
            0x2 => time.minute % 10,
            0x3 => time.minute / 10,
            0x4 => self.hour_digits(time.hour).0,
            0x5 => self.hour_digits(time.hour).1,
            0x6 => time.weekday,
            0x7 => time.day % 10,
            0x8 => time.day / 10,
            0x9 => time.month % 10,
            0xA => time.month / 10,
            0xB => (time.year % 10) as u8,
            0xC => ((time.year / 10) % 10) as u8,
            _ => 0,
        }
    }

    /// Hour digits under the current 12/24 select: in 12-hour mode the
    /// ten-hours register carries the PM flag in bit 1 and hours read
    /// 12, 1..11.
    fn hour_digits(&self, hour: u8) -> (u8, u8) {
        if self.bank1[Self::REG_1224] & 1 != 0 {
            return (hour % 10, hour / 10);
        }
        let pm = u8::from(hour >= 12);
        let hour = match hour % 12 {
            0 => 12,
            h => h,
        };
        (hour % 10, (hour / 10) | (pm << 1))
    }

    fn read_alarm_register(&self, reg: u8, emulated_secs: f64) -> u8 {
        if reg == Self::REG_LEAP {
            // The real part counts this alongside the year counter
            // (0 = leap year); deriving both from the same source keeps
            // them agreeing by construction.
            return (self.time(emulated_secs).year % 4) as u8;
        }
        self.bank1[reg as usize]
    }

    /// Persist the battery-backed registers to (and preload them from)
    /// `path`, in the WinUAE/Amiberry `.nvram` layout. Only the alarm
    /// block (behind the hardware write masks) and the RAM bytes load
    /// back: the stored time digits are ignored because the clock is
    /// read-only (host- or seed-driven), and the stored MODE is ignored
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
        for reg in 0x0..=0xC {
            bytes[reg as usize] = self.read_time_register(reg, emulated_secs);
        }
        bytes[0xD] = self.mode;
        // Selects E/F (TEST, RESET) are write-only and hold nothing.
        bytes[Self::BATTMEM_ALARM_OFFSET..][..self.bank1.len()].copy_from_slice(&self.bank1);
        bytes[Self::BATTMEM_RAM_OFFSET..][..self.ram.len()].copy_from_slice(&self.ram);
        bytes
    }

    /// A CPU reset does not reach the battery-backed chip: the time
    /// source, alarm registers, 12/24 select, and battery RAM all
    /// survive. Only the volatile-looking selector state returns to its
    /// power-on default, which is also the first thing any OS probe
    /// re-establishes.
    pub fn reset(&mut self) {
        self.mode = Self::MODE_TIMER_EN;
        self.latched = None;
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
        assert_eq!(rtc.clock.current_unix(31.9), VECTOR_UNIX + 31);
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
    fn rp5c01_timer_enable_gates_a_tearing_free_read_window() {
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        rp_write(&mut rtc, 0xD, 0x0); // lock: timer off, block 0
        assert_eq!(rp_read_at(&mut rtc, 0x0, 31.0), 9); // still :29, not :00
        assert_eq!(rp_read_at(&mut rtc, 0x2, 31.0), 8);

        rtc.write(0xD * 4, 4, 0x9, 31.0); // unlock: timer on, block 1
        assert_eq!(rtc.read(0xD * 4, 4, 31.0), 0x9);
        // Back in block 0 the clock has kept ticking underneath the hold.
        rtc.write(0xD * 4, 4, 0x8, 31.0);
        assert_eq!(rp_read_at(&mut rtc, 0x0, 31.0), 0); // :00
        assert_eq!(rp_read_at(&mut rtc, 0x2, 31.0), 9); // of minute 59
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
    fn rp5c01_time_counter_writes_never_move_the_clock() {
        let mut rtc = seeded_rp5c01(VECTOR_UNIX);
        for reg in 0x0..=0xC {
            rp_write(&mut rtc, reg, 0x7);
        }
        assert_eq!(rp_read(&mut rtc, 0x0), 9);
        assert_eq!(rp_read(&mut rtc, 0x6), 5);
        assert_eq!(rp_read(&mut rtc, 0xB), 5);
        assert_eq!(rtc.clock.seed(), Some(VECTOR_UNIX));
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
