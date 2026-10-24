// SPDX-License-Identifier: GPL-3.0-or-later

//! Period hard-drive mechanics: an opt-in model of what a real drive of the
//! late 1980s and early 1990s does between a command arriving and its data
//! being ready (`HDD-TIMING-PLAN.md`, `docs/internals/peripherals.md`).
//!
//! A request costs the drive's command overhead, a seek, the rotational wait
//! for its first sector, and the media time of every sector, with the track
//! and cylinder skew of the low-level format absorbing head switches and
//! single-track seeks on sequential transfers. Reads leave a read-ahead
//! stream behind them that later requests can hit; writes go through the
//! drive's buffer and, on a drive with write cache, complete before they
//! reach the platter. Spin-up, standby and idle head parking are modelled
//! for the drives whose manuals describe them.
//!
//! Everything here is a pure function of the device clock (colour clocks
//! accumulated from the owner's ticks) and the request stream: no host
//! clock, no I/O. The controllers (`crate::ata`, `crate::scsi`,
//! `crate::a4091`) ask when data becomes ready and present it on their own
//! bus at that time. Only correctly rounded floating-point operations
//! (`+ - * /`, `sqrt`) are used, so two hosts on different platforms compute
//! identical times -- netplay peers depend on it.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// One recording zone: `cylinders` consecutive physical cylinders that all
/// carry `sectors_per_track` 512-byte sectors on every track. Zones are
/// listed outermost first; a drive without zoned recording has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Zone {
    pub cylinders: u32,
    pub sectors_per_track: u32,
}

/// A drive mechanism as its manual describes it. Built-in profiles live in
/// [`super::profiles`]; `[[drive_profile]]` tables define more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveProfile {
    /// Identifier used by `timing = "..."` (`conner-cp2064`).
    pub id: String,
    /// One line for logs and `--list-drive-profiles`.
    pub label: String,
    pub rpm: u32,
    /// Physical read/write heads (data surfaces).
    pub heads: u32,
    /// Recording zones, outermost first.
    pub zones: Vec<Zone>,
    /// Read seeks in microseconds: track-to-track, average over random
    /// seeks, full stroke.
    pub seek_us: [u32; 3],
    /// Write seeks, when the manual quotes them separately (a write settles
    /// more tightly than a read).
    pub write_seek_us: Option<[u32; 3]>,
    /// Time to switch to another head on the same cylinder.
    pub head_switch_us: u32,
    /// (track, cylinder) skew in sectors, when the manual publishes the
    /// low-level format's values; otherwise derived from the head-switch
    /// and track-to-track times.
    pub skew_sectors: Option<(u32, u32)>,
    pub command_overhead_us: u32,
    /// Buffer the drive reads ahead into and stages writes through.
    pub buffer_kib: u32,
    /// Sectors read ahead after a read, when the cache is segmented or the
    /// manual gives a prefetch limit; `None` fills the buffer.
    pub prefetch_sectors: Option<u32>,
    pub read_ahead: bool,
    /// Whether a request whose head is buffered but whose tail is not yet
    /// read is served from the read-ahead stream. Some early firmware only
    /// served complete hits and went back to the media otherwise.
    pub partial_hits: bool,
    /// Writes complete once they are in the buffer; the drive flushes them
    /// in the background.
    pub write_cache: bool,
    /// Power-on to ready.
    pub spin_up_ms: u32,
    /// Standby to ready.
    pub standby_spin_up_ms: u32,
    /// Idle time after which the heads park off the data zone while the
    /// spindle keeps turning (the Seagate 2.5" idle mode).
    pub idle_park_ms: Option<u32>,
    /// Asynchronous SCSI data rate in KB/s (1000 bytes); zero for ATA.
    pub scsi_kb_per_s: u32,
}

impl DriveProfile {
    /// Physical cylinders across every zone.
    pub fn cylinders(&self) -> u32 {
        self.zones.iter().map(|z| z.cylinders).sum()
    }

    /// The mechanism's native capacity in 512-byte sectors.
    pub fn capacity_sectors(&self) -> u64 {
        self.zones
            .iter()
            .map(|z| u64::from(z.cylinders) * u64::from(z.sectors_per_track))
            .sum::<u64>()
            * u64::from(self.heads)
    }

    /// User-data media rate of the outermost and innermost zones, bytes per
    /// second.
    pub fn media_rate_range(&self) -> (u64, u64) {
        let rate = |z: &Zone| u64::from(z.sectors_per_track) * 512 * u64::from(self.rpm) / 60;
        let outer = self.zones.first().map(rate).unwrap_or(0);
        let inner = self.zones.last().map(rate).unwrap_or(0);
        (outer, inner)
    }

    /// The exponent of the read seek curve (see [`seek_exponent`]).
    pub fn seek_exponent(&self) -> f64 {
        seek_exponent(self.seek_us)
    }

    /// Reject a profile the model cannot use. Built-in profiles are checked
    /// by a unit test; `[[drive_profile]]` tables by config validation.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("profile id must not be empty".into());
        }
        if self.rpm == 0 || self.rpm > 20_000 {
            return Err(format!("rpm {} is out of range (1-20000)", self.rpm));
        }
        if self.heads == 0 || self.heads > 64 {
            return Err(format!("heads {} is out of range (1-64)", self.heads));
        }
        if self.zones.is_empty() {
            return Err("at least one zone is needed".into());
        }
        for zone in &self.zones {
            if zone.cylinders == 0 || zone.sectors_per_track == 0 || zone.sectors_per_track > 1024 {
                return Err(format!(
                    "zone {} cylinders x {} sectors per track is out of range",
                    zone.cylinders, zone.sectors_per_track
                ));
            }
        }
        if self.cylinders() < 2 {
            return Err("a drive needs at least two cylinders to seek between".into());
        }
        for (what, seeks) in [
            ("seek", Some(self.seek_us)),
            ("write seek", self.write_seek_us),
        ] {
            let Some([t2t, avg, full]) = seeks else {
                continue;
            };
            if !(t2t < avg && avg < full) {
                return Err(format!(
                    "{what} times must satisfy track-to-track < average < full stroke \
                     ({t2t} / {avg} / {full} us)"
                ));
            }
        }
        if self.buffer_kib == 0 {
            return Err("buffer_kib must be at least 1".into());
        }
        Ok(())
    }
}

/// The exponent `p` of the seek curve
/// `seek(d) = t2t + (full - t2t) * d^p`, chosen so the mean over seeks
/// between uniformly random cylinders equals the datasheet average: with
/// `d` distributed as `2(1 - d)`, `E[d^p] = 2 / ((p + 1)(p + 2))`.
pub fn seek_exponent([t2t, avg, full]: [u32; 3]) -> f64 {
    let span = f64::from(full) - f64::from(t2t);
    let k = (f64::from(avg) - f64::from(t2t)) / span;
    if !(k > 0.0 && k < 1.0) {
        return 1.0;
    }
    ((1.0 + 8.0 / k).sqrt() - 3.0) / 2.0
}

/// `d^p` for `d` in `[0, 1]` using only multiplication and square roots,
/// which IEEE 754 rounds exactly, so every platform gets the same bits
/// (`f64::powf` is a libm call with no such guarantee).
fn frac_pow(d: f64, p: f64) -> f64 {
    if d <= 0.0 {
        return 0.0;
    }
    let whole = p.floor();
    let mut result = 1.0;
    for _ in 0..(whole as u32).min(16) {
        result *= d;
    }
    let mut frac = p - whole;
    let mut root = d;
    for _ in 0..30 {
        root = root.sqrt();
        frac *= 2.0;
        if frac >= 1.0 {
            result *= root;
            frac -= 1.0;
        }
    }
    result
}

fn us_to_cck(us: u64, cck_hz: u32) -> u64 {
    (us * u64::from(cck_hz) + 500_000) / 1_000_000
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct SeekCurve {
    t2t: u64,
    full: u64,
    exponent: f64,
}

impl SeekCurve {
    fn new(seek_us: [u32; 3], cck_hz: u32) -> Self {
        Self {
            t2t: us_to_cck(u64::from(seek_us[0]), cck_hz),
            full: us_to_cck(u64::from(seek_us[2]), cck_hz),
            exponent: seek_exponent(seek_us),
        }
    }

    /// Seek time over `distance` physical cylinders (at least one) on a
    /// drive of `cylinders`: track-to-track at one cylinder, full stroke
    /// at `cylinders - 1`.
    fn time(&self, distance: f64, cylinders: u32) -> u64 {
        let span = f64::from(cylinders.max(3) - 2);
        let d = ((distance - 1.0).max(0.0) / span).min(1.0);
        let extra = (self.full - self.t2t) as f64 * frac_pow(d, self.exponent);
        self.t2t + extra.round() as u64
    }
}

/// One zone as laid out over the image: the image's LBA space is shared
/// between the zones in proportion to their native capacity, and each zone
/// keeps its native sectors per track and head count, so the image runs at
/// the drive's media rates whatever its size; only the number of cylinders
/// differs, and seek distances scale onto the drive's real stroke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ZoneMap {
    first_lba: u64,
    end_lba: u64,
    spt: u64,
    /// The zone's first physical cylinder and physical cylinder count.
    first_cyl: u32,
    cylinders: u32,
    /// Cylinders the image needs at this zone's track size.
    image_cyls: u64,
    head_skew: u64,
    cyl_skew: u64,
}

/// A head position: zone, image cylinder within the zone, head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
struct Track {
    zone: u32,
    cyl: u64,
    head: u32,
}

/// The head as it moves over the media: the next LBA it will reach, the
/// track it is on, and the time it is free there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
struct Walker {
    next_lba: u64,
    t: u64,
    track: Track,
}

/// The read-ahead stream left behind by the last read: sectors from
/// `first_lba` up to the walker are in the buffer; reading ahead stops at
/// `limit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Stream {
    first_lba: u64,
    limit: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct WriteState {
    walker: Walker,
    /// Commit times of buffered sectors not yet known to be on the media,
    /// oldest first (pruned as the clock passes them).
    commits: VecDeque<u64>,
}

/// The profile converted to colour clocks and laid out over one image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Timing {
    id: String,
    cck_hz: u32,
    total: u64,
    rev: u64,
    heads: u64,
    cylinders: u32,
    zones: Vec<ZoneMap>,
    read_seek: SeekCurve,
    write_seek: SeekCurve,
    head_switch: u64,
    overhead: u64,
    buffer_sectors: u64,
    prefetch_sectors: u64,
    read_ahead: bool,
    partial_hits: bool,
    write_cache: bool,
    standby_spin_up: u64,
    idle_park: Option<u64>,
    scsi_bytes_per_sec: u64,
}

/// One drive's mechanism and its state over time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriveMechanics {
    timing: Timing,
    label: String,
    /// Device clock: colour clocks since power-on, accumulated from ticks.
    now: u64,
    head: Walker,
    stream: Option<Stream>,
    write: Option<WriteState>,
    /// The mechanism is occupied (a seek, a transfer, a write-cache flush)
    /// until this time; the next command starts no earlier.
    busy_until: u64,
    /// The spindle is up to speed from this time; the platters' angle is
    /// measured from it.
    spindle_ready: u64,
    /// Standby: the spindle is stopped until the next media access.
    standby: bool,
    /// End of the last media activity, for the idle head park.
    last_activity: u64,
}

impl DriveMechanics {
    /// The mechanism of `profile` behind an image of `total_sectors`, timed
    /// in colour clocks of `cck_hz`. With `spin_up` the drive starts at rest
    /// and is not ready until the profile's spin-up time has passed.
    pub fn new(profile: &DriveProfile, total_sectors: u64, cck_hz: u32, spin_up: bool) -> Self {
        let rev = (60 * u64::from(cck_hz) + u64::from(profile.rpm.max(1)) / 2)
            / u64::from(profile.rpm.max(1));
        let heads = u64::from(profile.heads.max(1));
        let read_seek = SeekCurve::new(profile.seek_us, cck_hz);
        let write_seek = profile
            .write_seek_us
            .map(|seek| SeekCurve::new(seek, cck_hz))
            .unwrap_or(read_seek);
        let head_switch = us_to_cck(u64::from(profile.head_switch_us), cck_hz);
        let native: u128 = u128::from(profile.capacity_sectors().max(1));
        let total = total_sectors.max(1);
        let mut zones = Vec::with_capacity(profile.zones.len());
        let mut cum: u128 = 0;
        let mut first_cyl = 0u32;
        for zone in &profile.zones {
            let spt = u64::from(zone.sectors_per_track.max(1));
            let first_lba = (u128::from(total) * cum / native) as u64;
            cum += u128::from(zone.cylinders) * u128::from(spt) * u128::from(heads);
            let end_lba = (u128::from(total) * cum / native) as u64;
            let per_cyl = heads * spt;
            let image_cyls = (end_lba - first_lba).div_ceil(per_cyl).max(1);
            // Skews in whole sectors, as the low-level format lays them out:
            // published values where the manual gives them, otherwise just
            // long enough to cover a head switch / a track-to-track seek.
            let sectors_for = |cck: u64| (cck * spt).div_ceil(rev).min(spt - 1);
            let (head_skew, cyl_skew) = match profile.skew_sectors {
                Some((track, cyl)) => (u64::from(track) % spt, u64::from(cyl) % spt),
                None => (sectors_for(head_switch), sectors_for(read_seek.t2t)),
            };
            zones.push(ZoneMap {
                first_lba,
                end_lba,
                spt,
                first_cyl,
                cylinders: zone.cylinders,
                image_cyls,
                head_skew,
                cyl_skew,
            });
            first_cyl += zone.cylinders;
        }
        let ms = |ms: u32| us_to_cck(u64::from(ms) * 1000, cck_hz);
        let buffer_sectors = (u64::from(profile.buffer_kib) * 2).max(1);
        let timing = Timing {
            id: profile.id.clone(),
            cck_hz,
            total,
            rev: rev.max(1),
            heads,
            cylinders: profile.cylinders().max(2),
            zones,
            read_seek,
            write_seek,
            head_switch,
            overhead: us_to_cck(u64::from(profile.command_overhead_us), cck_hz),
            buffer_sectors,
            prefetch_sectors: profile
                .prefetch_sectors
                .map(u64::from)
                .unwrap_or(buffer_sectors)
                .min(buffer_sectors),
            read_ahead: profile.read_ahead,
            partial_hits: profile.partial_hits,
            write_cache: profile.write_cache,
            standby_spin_up: ms(profile.standby_spin_up_ms),
            idle_park: profile.idle_park_ms.map(ms),
            scsi_bytes_per_sec: u64::from(profile.scsi_kb_per_s) * 1000,
        };
        let spindle_ready = if spin_up { ms(profile.spin_up_ms) } else { 0 };
        let mut mechanics = Self {
            timing,
            label: profile.label.clone(),
            now: 0,
            head: Walker::default(),
            stream: None,
            write: None,
            busy_until: 0,
            spindle_ready,
            standby: false,
            last_activity: spindle_ready,
        };
        mechanics.head.track = mechanics.locate(0).0;
        mechanics.head.t = spindle_ready;
        mechanics
    }

    pub fn profile_id(&self) -> &str {
        &self.timing.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// The device clock.
    pub fn now(&self) -> u64 {
        self.now
    }

    pub fn advance(&mut self, cck: u32) {
        self.now += u64::from(cck);
    }

    /// Whether the drive is still spinning up from power-on or standby.
    pub fn spinning_up(&self) -> bool {
        self.now < self.spindle_ready
    }

    /// When the spindle is (or was) up to speed.
    pub fn spindle_ready_at(&self) -> u64 {
        self.spindle_ready
    }

    pub fn in_standby(&self) -> bool {
        self.standby
    }

    pub fn write_cache(&self) -> bool {
        self.timing.write_cache
    }

    /// Spin the spindle down (STANDBY): the next media access pays the
    /// profile's standby-to-ready time.
    pub fn enter_standby(&mut self) {
        self.flush_pending_writes();
        self.stream = None;
        self.standby = true;
    }

    /// Spin up out of standby (IDLE): returns when the drive is ready.
    pub fn leave_standby(&mut self) -> u64 {
        if self.standby {
            self.standby = false;
            let start = self.now.max(self.busy_until);
            self.spindle_ready = start + self.timing.standby_spin_up;
            self.head.t = self.spindle_ready;
            self.last_activity = self.spindle_ready;
        }
        self.spindle_ready.max(self.now)
    }

    /// Time a non-data command finishes: the command overhead, after any
    /// spin-up or flush still in progress.
    pub fn overhead(&mut self) -> u64 {
        let start = self.command_start();
        self.busy_until = start;
        start
    }

    /// Move the heads to `lba`'s track (RECALIBRATE, SEEK): returns when the
    /// seek has settled.
    pub fn seek_to(&mut self, lba: u64) -> u64 {
        let start = self.command_start();
        self.advance_stream(start);
        self.stream = None;
        let (track, _) = self.locate(lba);
        let done = self.head.t.max(start) + self.move_time(self.head.track, track, false);
        self.head = Walker {
            next_lba: lba,
            t: done,
            track,
        };
        self.busy_until = done;
        self.last_activity = done;
        done
    }

    /// Read `count` sectors from `lba`: each sector's ready time on the
    /// device clock, in order.
    pub fn read(&mut self, lba: u64, count: u32) -> Vec<u64> {
        let start = self.command_start();
        let count = u64::from(count);
        let mut times = Vec::with_capacity(count as usize);
        let mut first_lba = lba;
        let mut hit = false;
        if let Some(stream) = self.stream.filter(|_| self.timing.read_ahead) {
            self.advance_stream(start);
            let buffered_end = self.head.next_lba;
            let lo = stream
                .first_lba
                .max(buffered_end.saturating_sub(self.timing.buffer_sectors));
            hit = lba >= lo
                && if self.timing.partial_hits {
                    lba <= stream.limit
                } else {
                    lba + count <= buffered_end
                };
            if hit {
                first_lba = stream.first_lba.max(lo);
                // Reading ahead had stopped at its limit: it resumes now.
                if buffered_end >= stream.limit {
                    self.head.t = self.head.t.max(start);
                }
                for l in lba..lba + count {
                    if l < self.head.next_lba {
                        times.push(start);
                        continue;
                    }
                    while self.head.next_lba < l {
                        self.head = self.step(self.head, false).1;
                    }
                    let (ready, next) = self.step(self.head, false);
                    self.head = next;
                    times.push(ready.max(start));
                }
            }
        }
        if !hit {
            let mut walker = Walker {
                next_lba: lba,
                t: self.head.t.max(start),
                track: self.head.track,
            };
            for _ in 0..count {
                let (ready, next) = self.step(walker, false);
                times.push(ready);
                walker = next;
            }
            self.head = walker;
        }
        let last = times.last().copied().unwrap_or(start);
        self.busy_until = last;
        self.last_activity = last;
        self.stream = self.timing.read_ahead.then(|| Stream {
            first_lba,
            limit: (lba + count + self.timing.prefetch_sectors).min(self.timing.total),
        });
        if crate::envcfg::flag("COPPERLINE_DIAG_HDTIMING") {
            let ms = |cck: u64| cck as f64 * 1000.0 / f64::from(self.timing.cck_hz);
            log::info!(
                "hdtiming {}: read lba {lba} x{count} {} at {:.3} ms: first {:.3} ms, last {:.3} ms",
                self.timing.id,
                if hit { "hit" } else { "miss" },
                ms(self.now),
                ms(times.first().copied().unwrap_or(start) - self.now),
                ms(last - self.now),
            );
        }
        times
    }

    /// Start a write at `lba`. Data then arrives through
    /// [`Self::write_sectors`] and the command finishes with
    /// [`Self::end_write`].
    pub fn begin_write(&mut self, lba: u64) {
        let start = self.command_start();
        self.advance_stream(start);
        self.stream = None;
        self.write = Some(WriteState {
            walker: Walker {
                next_lba: lba,
                t: self.head.t.max(start),
                track: self.head.track,
            },
            commits: VecDeque::new(),
        });
    }

    /// `count` more sectors of write data reached the drive's buffer at
    /// `available`: returns when the last of them is on the media.
    pub fn write_sectors(&mut self, count: u32, available: u64) -> u64 {
        let now = self.now;
        let Some(mut ws) = self.write.take() else {
            return available;
        };
        while ws.commits.front().is_some_and(|&t| t <= now) {
            ws.commits.pop_front();
        }
        for _ in 0..count {
            ws.walker.t = ws.walker.t.max(available);
            let (ready, next) = self.step(ws.walker, true);
            ws.walker = next;
            ws.commits.push_back(ready);
        }
        let last = ws.walker.t;
        self.write = Some(ws);
        last
    }

    /// When the buffer has room for `count` more write sectors.
    pub fn write_room_at(&self, count: u32) -> u64 {
        let Some(ws) = &self.write else {
            return self.now;
        };
        let pending = ws.commits.iter().filter(|&&t| t > self.now).count() as u64;
        let cap = self.timing.buffer_sectors;
        let count = u64::from(count).min(cap);
        if pending + count <= cap {
            return self.now;
        }
        // The buffer frees as sectors reach the media, oldest first.
        let first_pending = ws.commits.len() as u64 - pending;
        let index = first_pending + (pending + count - cap) - 1;
        ws.commits[index as usize]
    }

    /// Finish the write: returns when the last sector is on the media. The
    /// mechanism stays busy until then even when write cache let the
    /// command complete earlier.
    pub fn end_write(&mut self) -> u64 {
        let Some(ws) = self.write.take() else {
            return self.now.max(self.busy_until);
        };
        let last = ws.walker.t;
        self.head = ws.walker;
        self.busy_until = last;
        self.last_activity = last;
        if crate::envcfg::flag("COPPERLINE_DIAG_HDTIMING") {
            let ms = |cck: u64| cck as f64 * 1000.0 / f64::from(self.timing.cck_hz);
            log::info!(
                "hdtiming {}: write committed at +{:.3} ms",
                self.timing.id,
                ms(last.saturating_sub(self.now))
            );
        }
        last
    }

    /// SCSI bytes the bus can carry between two device-clock times; the bus
    /// is unlimited on a profile without a SCSI rate.
    pub fn bus_bytes_between(&self, from: u64, to: u64) -> u64 {
        if self.timing.scsi_bytes_per_sec == 0 {
            return u64::MAX;
        }
        let span = u128::from(to.saturating_sub(from));
        (span * u128::from(self.timing.scsi_bytes_per_sec) / u128::from(self.timing.cck_hz))
            .min(u128::from(u64::MAX)) as u64
    }

    /// Colour clocks the SCSI bus needs to carry `bytes`.
    pub fn bus_time_for(&self, bytes: u64) -> u64 {
        if self.timing.scsi_bytes_per_sec == 0 {
            return 0;
        }
        (u128::from(bytes) * u128::from(self.timing.cck_hz))
            .div_ceil(u128::from(self.timing.scsi_bytes_per_sec)) as u64
    }

    // ----- internals ----------------------------------------------------

    /// When a command reaching the drive now starts its media work: after
    /// anything still occupying the mechanism, after spin-up, and after the
    /// command overhead. Parks the heads first if the drive has been idle
    /// long enough to have parked them.
    fn command_start(&mut self) -> u64 {
        let mut base = self.now.max(self.busy_until);
        if self.standby {
            self.standby = false;
            self.spindle_ready = base + self.timing.standby_spin_up;
            self.head.t = self.spindle_ready;
            self.last_activity = self.spindle_ready;
        }
        base = base.max(self.spindle_ready);
        if let Some(park) = self.timing.idle_park {
            self.advance_stream(base);
            if base >= self.last_activity + park && self.write.is_none() {
                // The heads wait off the data zone at the outer edge; the
                // next access seeks in from there.
                self.stream = None;
                self.head.track = self.locate(0).0;
                self.head.next_lba = 0;
            }
        }
        base + self.timing.overhead
    }

    /// Commit the read-ahead stream's sectors that are on the buffer by
    /// `until`.
    fn advance_stream(&mut self, until: u64) {
        let Some(stream) = self.stream else {
            return;
        };
        while self.head.next_lba < stream.limit {
            let (ready, next) = self.step(self.head, false);
            if ready > until {
                break;
            }
            self.head = next;
            self.last_activity = ready;
        }
    }

    fn flush_pending_writes(&mut self) {
        if self.write.is_some() {
            self.end_write();
        }
    }

    /// The track and sector-within-track of `lba`.
    fn locate(&self, lba: u64) -> (Track, u64) {
        let zones = &self.timing.zones;
        let lba = lba.min(self.timing.total - 1);
        let index = zones
            .partition_point(|z| z.end_lba <= lba)
            .min(zones.len() - 1);
        let zone = &zones[index];
        let rel = lba - zone.first_lba;
        let per_cyl = self.timing.heads * zone.spt;
        let within = rel % per_cyl;
        (
            Track {
                zone: index as u32,
                cyl: rel / per_cyl,
                head: (within / zone.spt) as u32,
            },
            within % zone.spt,
        )
    }

    /// A track's position across the drive's physical stroke, in cylinders.
    fn position(&self, track: Track) -> f64 {
        let zone = &self.timing.zones[track.zone as usize];
        f64::from(zone.first_cyl)
            + track.cyl as f64 * f64::from(zone.cylinders) / zone.image_cyls as f64
    }

    /// Time to move the heads between two tracks.
    fn move_time(&self, from: Track, to: Track, write: bool) -> u64 {
        if from == to {
            return 0;
        }
        if from.zone == to.zone && from.cyl == to.cyl {
            return self.timing.head_switch;
        }
        let distance = (self.position(to) - self.position(from)).abs().max(1.0);
        let curve = if write {
            self.timing.write_seek
        } else {
            self.timing.read_seek
        };
        curve
            .time(distance, self.timing.cylinders)
            .max(self.timing.head_switch)
    }

    /// The next time a sector passes under the head at or after `t`: its
    /// start and its duration.
    fn sector_slot(&self, track: Track, sector: u64, t: u64) -> (u64, u64) {
        let zone = &self.timing.zones[track.zone as usize];
        let spt = zone.spt;
        let heads = self.timing.heads;
        let per_cyl_skew = ((heads - 1) * zone.head_skew + zone.cyl_skew) % spt;
        let skew =
            ((track.cyl % spt) * per_cyl_skew + u64::from(track.head) * zone.head_skew) % spt;
        let index = (sector + skew) % spt;
        let rev = self.timing.rev;
        let begin = index * rev / spt;
        let end = (index + 1) * rev / spt;
        let angle = t.saturating_sub(self.spindle_ready) % rev;
        let wait = (begin + rev - angle) % rev;
        (t + wait, end - begin)
    }

    /// Read or write the walker's next sector: when it is done, and the
    /// walker after it.
    fn step(&self, walker: Walker, write: bool) -> (u64, Walker) {
        let (track, sector) = self.locate(walker.next_lba);
        let t = walker.t + self.move_time(walker.track, track, write);
        let (start, duration) = self.sector_slot(track, sector, t);
        let ready = start + duration;
        (
            ready,
            Walker {
                next_lba: walker.next_lba + 1,
                t: ready,
                track,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harddrive::profiles::builtin_profile;

    const PAL: u32 = 3_546_895;

    fn ms(cck: u64) -> f64 {
        cck as f64 * 1000.0 / f64::from(PAL)
    }

    /// A deterministic sequence for the statistical tests.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }
    }

    #[test]
    fn frac_pow_matches_powf() {
        for &(d, p) in &[
            (0.5, 0.79),
            (0.1, 0.56),
            (0.9, 1.04),
            (0.333, 0.63),
            (1.0, 0.7),
        ] {
            let ours = frac_pow(d, p);
            let libm = f64::powf(d, p);
            assert!((ours - libm).abs() < 1e-8, "{d}^{p}: {ours} vs {libm}");
        }
        assert_eq!(frac_pow(0.0, 0.5), 0.0);
    }

    #[test]
    fn seek_curve_hits_the_three_datasheet_figures() {
        for id in ["conner-cp2064", "quantum-prodrive-80s", "seagate-st31200n"] {
            let profile = builtin_profile(id).unwrap();
            let curve = SeekCurve::new(profile.seek_us, PAL);
            let cyls = profile.cylinders();
            let [t2t, avg, full] = profile.seek_us.map(|us| us_to_cck(u64::from(us), PAL));
            assert_eq!(curve.time(1.0, cyls), t2t, "{id} track-to-track");
            assert_eq!(
                curve.time(f64::from(cyls - 1), cyls),
                full,
                "{id} full stroke"
            );
            // Mean over seeks between random cylinders.
            let mut rng = Lcg(7);
            let (mut sum, mut n) = (0.0, 0u32);
            while n < 200_000 {
                let a = rng.next() % u64::from(cyls);
                let b = rng.next() % u64::from(cyls);
                if a == b {
                    continue;
                }
                sum += curve.time(a.abs_diff(b) as f64, cyls) as f64;
                n += 1;
            }
            let mean = sum / f64::from(n);
            let error = (mean - avg as f64).abs() / avg as f64;
            assert!(error < 0.01, "{id} average seek {mean} vs {avg}");
        }
    }

    #[test]
    fn mean_rotational_latency_is_half_a_revolution() {
        let profile = builtin_profile("conner-cp2064").unwrap();
        let mut drive = DriveMechanics::new(&profile, profile.capacity_sectors(), PAL, false);
        let rev = drive.timing.rev;
        let sector = rev / 38;
        let mut rng = Lcg(3);
        let mut total = 0u64;
        let n = 20_000u64;
        for _ in 0..n {
            // Re-read one sector of the current track at a random moment:
            // no seek, so the wait is purely rotational.
            drive.stream = None;
            drive.advance((rng.next() % 1_000_000) as u32 + 100_000);
            let lba = drive.head.next_lba.saturating_sub(1);
            let start = drive.now + drive.timing.overhead;
            let ready = drive.read(lba, 1)[0];
            total += ready - start - sector;
        }
        let mean = total as f64 / n as f64;
        let error = (mean - rev as f64 / 2.0).abs() / (rev as f64 / 2.0);
        assert!(error < 0.03, "mean latency {mean} vs half rev {}", rev / 2);
    }

    #[test]
    fn sequential_reading_runs_at_the_skewed_media_rate() {
        // HDD-TIMING-PLAN.md's worked example: a CP2064 cylinder of 4 x 38
        // sectors costs 152 + 3 x 7 + 12 sector times, 929 KB/s.
        let profile = builtin_profile("conner-cp2064").unwrap();
        let mut drive = DriveMechanics::new(&profile, profile.capacity_sectors(), PAL, false);
        let per_cyl = 4 * 38;
        let times = drive.read(0, 20 * per_cyl);
        let span = times[(19 * per_cyl) as usize] - times[(per_cyl - 1) as usize];
        let rate = 18.0 * f64::from(per_cyl) * 512.0 / (ms(span) / 1000.0);
        assert!(
            (rate / 929_000.0 - 1.0).abs() < 0.01,
            "sustained {rate} B/s"
        );
    }

    #[test]
    fn st3144a_outer_zone_matches_the_period_scsispeed_figure() {
        // ScsiSpeed 4.2 on an A4000 measured 254.5 ms per 256 KB read; the
        // profile streams it in about 258.7 ms (within 2%).
        let profile = builtin_profile("seagate-st3144a").unwrap();
        let mut drive = DriveMechanics::new(&profile, profile.capacity_sectors(), PAL, false);
        let times = drive.read(0, 512);
        let first_sector_end = times[0];
        let span = ms(times[511] - first_sector_end);
        assert!((span / 258.7 - 1.0).abs() < 0.02, "256 KB took {span} ms");
    }

    #[test]
    fn read_ahead_serves_the_next_request_from_the_buffer() {
        let profile = builtin_profile("conner-cp2064").unwrap();
        let mut drive = DriveMechanics::new(&profile, profile.capacity_sectors(), PAL, false);
        drive.read(1000, 8);
        drive.advance(200_000); // ~56 ms: far more than 8 more sectors
        let start = drive.now + drive.timing.overhead;
        let times = drive.read(1008, 8);
        assert!(times.iter().all(|&t| t == start), "{times:?} vs {start}");
    }

    #[test]
    fn complete_hit_only_firmware_loses_a_revolution_on_a_partial_hit() {
        let mut profile = builtin_profile("seagate-st3144a").unwrap();
        let total = profile.capacity_sectors();
        let cost = |profile: &DriveProfile| {
            let mut drive = DriveMechanics::new(profile, total, PAL, false);
            let first = drive.read(5000, 8);
            drive.now = *first.last().unwrap() + 2000; // the next request follows at once
            let start = drive.now;
            ms(drive.read(5008, 8)[7] - start)
        };
        assert!(!profile.partial_hits);
        let miss = cost(&profile);
        profile.partial_hits = true;
        let hit = cost(&profile);
        let rev = 60_000.0 / 3211.0;
        assert!(miss - hit > rev * 0.8, "miss {miss} ms vs hit {hit} ms");
    }

    #[test]
    fn write_cache_frees_the_host_but_not_the_mechanism() {
        let profile = builtin_profile("seagate-st9655ag").unwrap();
        assert!(profile.write_cache);
        let mut drive = DriveMechanics::new(&profile, profile.capacity_sectors(), PAL, false);
        drive.begin_write(90_000);
        let last = drive.write_sectors(16, drive.now);
        assert_eq!(drive.end_write(), last);
        assert!(last > drive.now);
        // The next command waits for the flush.
        assert!(drive.overhead() >= last + drive.timing.overhead);
    }

    #[test]
    fn write_buffer_room_frees_as_sectors_commit() {
        let profile = builtin_profile("conner-cp2024").unwrap(); // 8 KB buffer
        let mut drive = DriveMechanics::new(&profile, profile.capacity_sectors(), PAL, false);
        drive.begin_write(0);
        assert_eq!(drive.write_room_at(16), drive.now);
        drive.write_sectors(16, drive.now);
        let room = drive.write_room_at(1);
        assert!(room > drive.now, "a full buffer must wait for a commit");
        drive.end_write();
    }

    #[test]
    fn zones_cover_the_whole_image_in_proportion() {
        let profile = builtin_profile("wd-ac2120").unwrap();
        for total in [50_000u64, 246_880, 4_000_000] {
            let drive = DriveMechanics::new(&profile, total, PAL, false);
            let zones = &drive.timing.zones;
            assert_eq!(zones[0].first_lba, 0);
            assert_eq!(zones.last().unwrap().end_lba, total);
            // 970 x 48 of 970 x 48 + 379 x 40 sectors per surface.
            let share = zones[0].end_lba as f64 / total as f64;
            assert!((share - 46_560.0 / 61_720.0).abs() < 1e-4);
            for lba in [0, total / 3, total - 1] {
                let (track, sector) = drive.locate(lba);
                assert!(sector < zones[track.zone as usize].spt);
            }
        }
    }

    #[test]
    fn identical_requests_give_identical_times() {
        let profile = builtin_profile("quantum-lps-52s").unwrap();
        let run = || {
            let mut drive = DriveMechanics::new(&profile, 120_000, PAL, true);
            let mut rng = Lcg(11);
            let mut out = Vec::new();
            for _ in 0..200 {
                drive.advance((rng.next() % 50_000) as u32);
                let lba = rng.next() % 119_000;
                out.extend(drive.read(lba, (rng.next() % 64) as u32 + 1));
            }
            out
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn spin_up_delays_the_first_command() {
        let profile = builtin_profile("conner-cp2064").unwrap();
        let mut drive = DriveMechanics::new(&profile, profile.capacity_sectors(), PAL, true);
        assert!(drive.spinning_up());
        let ready = drive.overhead();
        assert!(ms(ready) >= 10_000.0, "ready at {} ms", ms(ready));
    }

    #[test]
    fn standby_costs_the_standby_spin_up_time() {
        let profile = builtin_profile("seagate-st9080a").unwrap();
        let mut drive = DriveMechanics::new(&profile, profile.capacity_sectors(), PAL, false);
        drive.advance(1_000_000);
        drive.enter_standby();
        let start = drive.now;
        let ready = drive.overhead();
        assert!(ms(ready - start) >= 2000.0, "{} ms", ms(ready - start));
        assert!(!drive.in_standby());
    }

    #[test]
    fn idle_park_makes_the_next_access_seek_from_the_edge() {
        let profile = builtin_profile("seagate-st9080a").unwrap();
        let total = profile.capacity_sectors();
        let mut drive = DriveMechanics::new(&profile, total, PAL, false);
        let lba = total - 1000;
        drive.read(lba, 1);
        drive.advance(100_000); // 28 ms: heads still loaded
        assert_ne!(drive.head.track, drive.locate(0).0);
        drive.advance(20_000_000); // ~5.6 s idle: parked
        drive.command_start();
        assert_eq!(drive.head.track, drive.locate(0).0);
    }

    #[test]
    fn scsi_bus_rate_converts_both_ways() {
        let profile = builtin_profile("seagate-st125n").unwrap();
        let drive = DriveMechanics::new(&profile, profile.capacity_sectors(), PAL, false);
        let one_ms = u64::from(PAL) / 1000;
        let bytes = drive.bus_bytes_between(0, one_ms);
        assert!((1495..=1500).contains(&bytes), "{bytes} bytes in 1 ms");
        assert!(drive.bus_time_for(1500) >= one_ms);
    }
}
