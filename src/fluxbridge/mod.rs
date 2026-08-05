// SPDX-License-Identifier: GPL-3.0-or-later

//! Real floppy drives, through the FluxBridge library.
//!
//! A *bridge* replaces one Amiga drive's disk image with a physical 3.5" drive
//! attached over a Greaseweazle. The emulated machine is unchanged: the bridge
//! only supplies the MFM the head would be passing over, so Paula, the disk
//! DMA, and trackdisk.device all behave exactly as they do with an image.
//! FluxBridge itself also carries DrawBridge and SuperCard Pro protocols;
//! Copperline compiles in the drivers it supports, and [`drivers`] reports
//! exactly those, so enabling another later is a Cargo feature, not a UI
//! change.
//!
//! # How it attaches
//!
//! FluxBridge reads the disk continuously on its own thread and keeps the track
//! under the head captured and ready. [`Bridge::read_track`] takes a whole
//! finished revolution from it in one call, packed MFM plus the length it
//! wrapped at -- which is the shape a captured revolution already has in
//! [`crate::floppy`], so the existing rotation, PLL, and sync-word machinery
//! reads a real disk with no special case in the hot path.
//!
//! Nothing here waits on the drive. A track that has not been captured yet
//! comes back as `None` and the caller asks again, exactly as it already did
//! while the motor spun up. Blocking instead would stop the emulated Amiga --
//! CPU, sprites, pointer and all -- every time the head moved, which is the one
//! thing a real drive never does to a real Amiga.
//!
//! Because a revolution is served whole, its two ends matter. A capture taken
//! from one index pulse to the next has its ends meeting in the gap between
//! sectors, so it can turn under the head indefinitely. An index-less capture
//! is joined where the recording repeats -- a join that is not always perfect,
//! which is why FluxBridge proves each capture itself (the join by pattern
//! matching, the decode by AmigaDOS structure and checksums) and reports the
//! verdict as its [`CaptureQuality`]; the emulator serves unproven captures
//! once and re-reads rather than trusting them beyond a single pass.
//!
//! Writing goes the same way round: [`Bridge::write_track`] hands the MFM to
//! FluxBridge, which lays it down on its own thread as the disk turns, without
//! the emulated machine waiting for the platter.
//!
//! # Where it comes from
//!
//! [FluxBridge](https://github.com/CopperlineHQ/FluxBridge) is a pure-Rust
//! library, pinned by revision in `Cargo.toml`. There is no C or C++ in the
//! build, nothing vendored, and nothing for a user to install: a build that
//! says it supports a physical drive can drive one.
//!
//! FluxBridge grew from a Rust port of the runtime parts of Rob Smith's
//! [FloppyDriveBridge](https://github.com/RobSmithDev/FloppyDriveBridge), and
//! carries that provenance in its own `NOTICE.md`.
//!
//! Turning the `fluxbridge` Cargo feature off, which is on by default, compiles
//! all of this out.
//!
//! # Determinism
//!
//! A real drive spins in wall-clock time and the disk under it can be swapped
//! by hand, so a run using a bridge is *not* reproducible: save states cannot
//! capture the medium, and a replayed input recording will not line up. Nor can
//! the emulated drive-speed setting apply to one, since the data rate is the
//! disk's own.

use std::time::Duration;

use fluxbridge as fb;
/// FluxBridge's verdict on a captured revolution: index-aligned, verified
/// AmigaDOS, or unverified. Re-exported because the emulator's serve-once
/// policy and its diagnostics are built on it.
pub use fluxbridge::CaptureQuality;
use log::warn;

/// How long a stalling read may hold the emulated machine up.
const STALL_TIMEOUT: Duration = Duration::from_millis(450);

/// What a 3.5" mechanism can reach. The guest steps past it quite deliberately
/// -- trackdisk does to find track 0 -- and a real head just sits against the
/// stop.
const MAX_CYLINDER: u8 = 82;

/// Capability bits a driver may honour, as the launcher's greying and the
/// attach path ask about them.
///
/// Copperline's own bitmask, so the UI can ask "does this driver take a port
/// name?" without every caller learning FluxBridge's capability type.
pub mod config_option {
    pub const COM_PORT: u32 = 0x02;
    pub const AUTO_DETECT_COMPORT: u32 = 0x04;
    pub const DRIVE_AB_CABLE: u32 = 0x08;
    pub const SUPPORTS_SHUGART: u32 = 0x20;
}

/// How a capture is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BridgeMode {
    /// Capture wherever the head happens to be, without waiting for the index.
    /// Quicker by most of a revolution, at the cost of a join that has to be
    /// proved before the capture can be turned twice.
    #[default]
    Fast,
    /// Capture from one index pulse to the next, so the revolution's ends meet
    /// in the sector gap and it can be replayed indefinitely.
    Compatible,
    /// As `Compatible`, and hold the caller until a capture is ready rather
    /// than letting the emulated machine carry on without one.
    Stalling,
}

/// Which media density to assume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BridgeDensityMode {
    #[default]
    Auto,
    DdOnly,
    HdOnly,
}

/// Which drive on the interface's cable to speak to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DriveSelection {
    #[default]
    DriveA,
    DriveB,
    Drive0,
    Drive1,
    Drive2,
    Drive3,
}

/// The kind of mechanism the interface reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveType {
    Dd35,
    Dd35Hd,
    Sd525,
}

/// One driver FluxBridge offers, by the index it was reported at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverInfo {
    pub index: u32,
    pub name: String,
    pub manufacturer: String,
    pub url: String,
    /// The library's own stable configuration token for this driver, the
    /// spelling config files and [`driver_named`] resolve by.
    pub token: &'static str,
    /// Bitmask of [`config_option`] flags this driver honours.
    pub config_options: u32,
}

impl DriverInfo {
    pub fn supports(&self, option: u32) -> bool {
        self.config_options & option != 0
    }
}

/// FluxBridge's own version, for anything naming what is driving the
/// hardware. Comes from the library itself, so it can never drift from what
/// is actually linked in.
pub fn version() -> &'static str {
    fb::VERSION
}

/// Every driver FluxBridge offers. Never empty: the library is linked in, not
/// looked for.
pub fn drivers() -> Vec<DriverInfo> {
    fb::drivers()
        .iter()
        .enumerate()
        .map(|(index, driver)| DriverInfo {
            index: index as u32,
            name: driver.name.to_string(),
            manufacturer: driver.manufacturer.to_string(),
            url: driver.url.to_string(),
            token: driver.kind.as_str(),
            config_options: capability_options(driver.capabilities),
        })
        .collect()
}

/// The compiled driver a configuration token names, if this build has it.
///
/// Resolution is FluxBridge's own token parsing -- the library that defines
/// the drivers decides what their names are -- so config spellings can never
/// drift from what the library answers to.
pub fn driver_named(token: &str) -> Option<DriverInfo> {
    let kind: fb::DriverKind = token.parse().ok()?;
    drivers()
        .into_iter()
        .find(|driver| driver.token == kind.as_str())
}

fn capability_options(capabilities: fb::Capabilities) -> u32 {
    // Every interface FluxBridge drives is reached over a serial port, so the
    // port name always applies; the rest are genuinely per-driver.
    let mut options = config_option::COM_PORT;
    for (capability, option) in [
        (
            fb::Capabilities::AUTO_DETECT_PORT,
            config_option::AUTO_DETECT_COMPORT,
        ),
        (
            fb::Capabilities::PC_DRIVE_SELECT,
            config_option::DRIVE_AB_CABLE,
        ),
        (
            fb::Capabilities::SHUGART_DRIVE_SELECT,
            config_option::SUPPORTS_SHUGART,
        ),
    ] {
        if capabilities.contains(capability) {
            options |= option;
        }
    }
    options
}

/// Serial ports an interface might be on -- the host's USB serial devices.
///
/// No current interface can be told apart without opening it, so this is
/// "possibly one of these", not "these are interfaces".
pub fn com_ports() -> Vec<String> {
    fb::ports()
        .map(|ports| {
            ports
                .into_iter()
                .map(|port| port.id.as_str().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether anything that could be an interface is plugged in right now.
///
/// Scanning walks the host's serial bus, so this is sampled at deliberate
/// moments -- a bay being switched over to a real drive -- rather than every
/// frame.
pub fn interface_connected() -> bool {
    !com_ports().is_empty()
}

/// Everything needed to open one real drive.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeConfig {
    /// Which driver to open, by the index [`drivers`] reported it at.
    pub driver: u32,
    pub mode: BridgeMode,
    pub density: BridgeDensityMode,
    pub drive: DriveSelection,
    /// Which serial port the interface is on. `None` -- the default -- lets
    /// FluxBridge find the device itself; name one to pin it, which matters
    /// when two interfaces are plugged in at once.
    pub port: Option<String>,
}

/// One open real drive.
///
/// Not `Clone`: it owns a physical device. Dropping it closes the interface,
/// which parks the motor.
pub struct Bridge {
    inner: fb::Bridge,
    side: fb::Side,
    cylinder: u8,
    max_cylinder: u8,
    /// What FluxBridge proved about the revolution last handed over. A
    /// reusable one closes on itself -- its two ends meet, by index alignment
    /// or by a proven join -- and can be turned under the head over and over,
    /// as an image's track is. An unverified one is good for a single pass.
    quality: fb::CaptureQuality,
    /// Set when the interface reports a disk arriving or leaving, and consumed
    /// by [`Bridge::take_disk_changed`]. FluxBridge reports presence rather
    /// than a latch, so the edge is noticed here.
    disk_changed: bool,
    disk_present: bool,
}

impl Bridge {
    /// Open a real drive. `Err` carries FluxBridge's own message, which names
    /// the actual fault (no such port, device not responding, ...).
    pub fn open(config: &BridgeConfig) -> Result<Self, String> {
        let driver = fb::drivers()
            .get(config.driver as usize)
            .ok_or_else(|| format!("no driver at index {}", config.driver))?;

        let port = match config.port.as_deref().map(str::trim) {
            Some(port) if !port.is_empty() => {
                fb::PortSelection::Exact(fb::PortId::new(port).map_err(|e| e.to_string())?)
            }
            _ => fb::PortSelection::Auto,
        };

        let settings = fb::BridgeConfig {
            driver: driver.kind,
            mode: match config.mode {
                BridgeMode::Fast => fb::ReadMode::Fast,
                BridgeMode::Compatible => fb::ReadMode::Compatible,
                BridgeMode::Stalling => fb::ReadMode::Stalling,
            },
            density: match config.density {
                BridgeDensityMode::Auto => fb::DensityMode::Auto,
                BridgeDensityMode::DdOnly => fb::DensityMode::Double,
                BridgeDensityMode::HdOnly => fb::DensityMode::High,
            },
            drive: match config.drive {
                DriveSelection::DriveA => fb::DriveSelect::PcA,
                DriveSelection::DriveB => fb::DriveSelect::PcB,
                DriveSelection::Drive0 => fb::DriveSelect::Shugart0,
                DriveSelection::Drive1 => fb::DriveSelect::Shugart1,
                DriveSelection::Drive2 => fb::DriveSelect::Shugart2,
                DriveSelection::Drive3 => fb::DriveSelect::Shugart3,
            },
            port,
            stall_timeout: STALL_TIMEOUT,
        };
        settings.validate().map_err(|e| e.to_string())?;

        // Only the stalling mode holds the emulated machine up, and FluxBridge
        // applies that itself from the mode; the others take whatever is ready
        // and let the machine carry on.
        let inner = fb::Bridge::open(&settings).map_err(|e| e.to_string())?;
        let status = inner.status();
        Ok(Self {
            inner,
            side: fb::Side::Lower,
            cylinder: 0,
            max_cylinder: if status.max_cylinders == 0 {
                MAX_CYLINDER
            } else {
                status.max_cylinders
            },
            quality: if matches!(config.mode, BridgeMode::Fast) {
                fb::CaptureQuality::Unverified
            } else {
                fb::CaptureQuality::IndexAligned
            },
            disk_changed: false,
            disk_present: status.disk_present,
        })
    }

    /// Whether the device is still responding. False once it is unplugged,
    /// which is how a bridge drive reports itself broken rather than hanging.
    pub fn is_working(&self) -> bool {
        self.inner.status().working
    }

    pub fn is_ready(&self) -> bool {
        self.inner.status().ready
    }

    pub fn disk_in_drive(&self) -> bool {
        self.inner.status().disk_present
    }

    /// Consumes the disk-changed edge.
    ///
    /// FluxBridge reports whether a disk is present rather than latching the
    /// change, so the edge is noticed here and held until it is asked for.
    pub fn take_disk_changed(&mut self) -> bool {
        let present = self.inner.status().disk_present;
        if present != self.disk_present {
            self.disk_present = present;
            self.disk_changed = true;
        }
        std::mem::take(&mut self.disk_changed)
    }

    /// Whether the disk's write-protect tab is closed.
    pub fn write_protected(&self) -> bool {
        self.inner.status().write_protected
    }

    /// The head's real cylinder, which is what the status bar's track counter
    /// shows for a bridged drive.
    pub fn current_cylinder(&self) -> u8 {
        self.cylinder
    }

    /// Keep the head inside what the drive can actually reach.
    fn clamp_cylinder(&self, cylinder: u8) -> u8 {
        cylinder.min(self.max_cylinder.saturating_sub(1))
    }

    /// The kind of drive the interface reports, which is what decides whether
    /// HD media can be read at all.
    pub fn drive_type(&self) -> DriveType {
        match self.inner.status().drive_type {
            fb::DriveType::Hd35 => DriveType::Dd35Hd,
            fb::DriveType::Sd525 => DriveType::Sd525,
            _ => DriveType::Dd35,
        }
    }

    pub fn motor_running(&self) -> bool {
        self.inner.status().motor_running
    }

    /// Spin the real drive up or down to follow the emulated motor line.
    pub fn set_motor(&mut self, side: bool, on: bool) {
        self.side = side_of(side);
        if let Err(error) = self.inner.set_motor(self.side, on) {
            warn!("fluxbridge: cannot switch the drive motor: {error}");
        }
    }

    /// Move the head, following the emulated drive's stepper.
    pub fn seek(&mut self, cylinder: u8, side: bool) {
        let cylinder = self.clamp_cylinder(cylinder);
        self.side = side_of(side);
        self.cylinder = cylinder;
        let track = self.track();
        if let Err(error) = self.inner.seek(track) {
            warn!("fluxbridge: cannot step to cylinder {cylinder}: {error}");
        }
    }

    const fn track(&self) -> fb::TrackAddress {
        fb::TrackAddress {
            cylinder: self.cylinder,
            side: self.side,
        }
    }

    /// FluxBridge's verdict on the revolution last handed over: whether it
    /// can be turned under the head more than once, and why.
    pub const fn last_quality(&self) -> CaptureQuality {
        self.quality
    }

    /// The capture in flight for `cylinder`/`side`, as far as it has got.
    ///
    /// The words are the decode of the flux the head has already passed, so
    /// the caller can serve them while the rest of the revolution is still
    /// arriving. Grows monotonically until [`Bridge::read_track`] hands over
    /// the finished revolution.
    pub fn partial_track(&mut self, cylinder: u8, side: bool) -> Option<(Vec<u16>, usize)> {
        let track = fb::TrackAddress {
            cylinder: self.clamp_cylinder(cylinder),
            side: side_of(side),
        };
        self.inner.partial_track(track)
    }

    /// Retire the revolution just consumed so the next read returns the next
    /// recording of the track, making successive revolutions continuous the way
    /// the cells coming off a real head are.
    pub fn switch_buffer(&mut self, side: bool) {
        self.side = side_of(side);
        let track = self.track();
        if let Err(error) = self.inner.advance_revolution(track) {
            warn!("fluxbridge: cannot retire the spent revolution: {error}");
        }
    }

    /// Take the current revolution of `cylinder`/`side`, if one is ready.
    ///
    /// Returns the bit stream packed MSB-first into 16-bit words, plus its
    /// length in bits, where the head loops back round -- exactly one captured
    /// revolution as [`crate::floppy`] stores it.
    ///
    /// Never waits, outside the stalling mode where waiting is the point.
    /// FluxBridge reads the disk continuously in the background, so either a
    /// finished revolution is sitting there or it is not, and `None` simply
    /// means "not yet": the caller asks again a moment later while the machine
    /// keeps running.
    pub fn read_track(&mut self, cylinder: u8, side: bool) -> Option<(Vec<u16>, usize)> {
        let cylinder = self.clamp_cylinder(cylinder);
        self.side = side_of(side);
        self.cylinder = cylinder;
        let track = self.track();
        let capture = match self.inner.read_track(track) {
            Ok(capture) => capture?,
            Err(error) => {
                warn!("fluxbridge: cannot read cylinder {cylinder}: {error}");
                return None;
            }
        };
        // What the library could prove about this revolution beats the
        // configured mode: an index-less capture it has shown to be a whole
        // AmigaDOS track is as replayable as an index-aligned one.
        self.quality = capture.quality();
        let bit_len = capture.bit_len();
        Some((capture.into_words(), bit_len))
    }
}

impl Bridge {
    /// Lay `words` of MFM down on the real disk, starting at bit `start_bit` of
    /// the track.
    ///
    /// Returns whether the write was accepted, not whether the platter took it.
    /// FluxBridge lays the cells down on its own thread as the disk turns,
    /// exactly as a real drive does, and reports the outcome afterwards through
    /// [`Bridge::take_write_failure`] -- because waiting here would stop the
    /// emulated machine for as long as the write takes, which is the one thing
    /// a real drive never does to a real Amiga.
    ///
    /// Refuses rather than gambles when the write cannot be placed: this is
    /// somebody's real disk.
    pub fn write_track(
        &mut self,
        cylinder: u8,
        side: bool,
        words: &[u16],
        start_bit: usize,
    ) -> bool {
        if words.is_empty() {
            return false;
        }
        let cylinder = self.clamp_cylinder(cylinder);
        self.side = side_of(side);
        self.cylinder = cylinder;
        let request = fb::WriteRequest {
            track: self.track(),
            words: words.to_vec(),
            bit_len: words.len() * 16,
            start_bit,
        };
        match self.inner.submit_write(request) {
            Ok(_) => true,
            Err(error) => {
                warn!("fluxbridge: the drive would not take the write of cylinder {cylinder}: {error}");
                false
            }
        }
    }

    /// Whether a write that had been accepted has since failed on the platter.
    ///
    /// A real write is only known to have worked once the drive has turned, so
    /// the failure arrives after the emulator has moved on. Reporting it lets
    /// the guest be told its disk is not what it thinks, rather than the
    /// failure passing silently.
    pub fn take_write_failure(&mut self) -> Option<String> {
        let mut failure = None;
        while let Some(event) = self.inner.poll_event() {
            match event {
                fb::BridgeEvent::WriteFailed { track, .. } => {
                    failure.get_or_insert_with(|| {
                        format!(
                            "cylinder {} side {}",
                            track.cylinder,
                            u8::from(track.side == fb::Side::Upper)
                        )
                    });
                }
                fb::BridgeEvent::DiskChanged { present } if present != self.disk_present => {
                    self.disk_present = present;
                    self.disk_changed = true;
                }
                _ => {}
            }
        }
        failure
    }
}

const fn side_of(upper: bool) -> fb::Side {
    if upper {
        fb::Side::Upper
    } else {
        fb::Side::Lower
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Which interface the hardware tests below expect to be attached. They
    /// never fall back to another one: each driver speaks its own protocol,
    /// and opening a board with the wrong one can leave it needing to be
    /// unplugged before it will answer again.
    const INTERFACE: &str = "greaseweazle";

    /// Every query here is reached from config parsing and from the launcher,
    /// on machines with no interface plugged in and no intention of using one.
    /// None of them may depend on hardware being present.
    #[test]
    fn queries_are_safe_with_no_hardware() {
        // The bridge is linked in, so it always answers.
        assert!(!drivers().is_empty(), "the bridge offers its drivers");
        assert!(!version().is_empty(), "and reports its version");
        // These simply describe a host with nothing attached.
        let _ = com_ports();
        let _ = interface_connected();
    }

    /// Print what a real library reports. Ignored by default because it needs
    /// one installed; run it to check a setup:
    ///     cargo test --release fluxbridge_inventory -- --ignored --nocapture
    #[test]
    #[ignore = "lists what the built-in bridge offers; run it to see the drivers"]
    fn fluxbridge_inventory() {
        println!("bridge: FluxBridge v{}", version());
        for d in drivers() {
            println!(
                "driver {}: {} by {} (options {:#04x})",
                d.index, d.name, d.manufacturer, d.config_options
            );
        }
        println!("ports: {:?}", com_ports());
    }

    /// Watch the drive's status lines with the motor stopped and running, to
    /// see what the driver actually reports rather than what it is assumed to.
    /// Write protection is the one that matters: Copperline gates real writes
    /// on it, and a reading taken at the wrong moment either refuses a disk
    /// that is writable or, worse, allows one that is not.
    ///
    ///     cargo test --release fluxbridge_status_probe -- --ignored --nocapture
    #[test]
    #[ignore = "needs a FluxBridge interface attached"]
    fn fluxbridge_status_probe() {
        let driver = drivers()
            .into_iter()
            .find(|d| d.name.to_ascii_lowercase().contains(INTERFACE))
            .unwrap_or_else(|| panic!("no {INTERFACE} driver in this library"));
        let mut bridge = Bridge::open(&BridgeConfig {
            driver: driver.index,
            ..Default::default()
        })
        .expect("open the drive");

        let sample = |bridge: &mut Bridge, phase: &str| {
            for _ in 0..8 {
                println!(
                    "{phase:>10}  disk={:<5} ready={:<5} motor={:<5} wp={}",
                    bridge.disk_in_drive(),
                    bridge.is_ready(),
                    bridge.motor_running(),
                    bridge.write_protected(),
                );
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        };

        sample(&mut bridge, "motor off");
        bridge.set_motor(false, true);
        sample(&mut bridge, "motor on");
        bridge.seek(1, false);
        sample(&mut bridge, "after seek");
        bridge.set_motor(false, false);
        sample(&mut bridge, "stopped");
    }

    /// Read cylinder 0 off a real disk and report what the MFM looks like:
    /// how many AmigaDOS sync marks are in it and how they are spaced. That
    /// separates "the stream is good, the emulator's timing is wrong" from
    /// "the bytes crossing the library boundary are not what we think".
    ///
    ///     cargo test --release fluxbridge_track_probe -- --ignored --nocapture
    #[test]
    #[ignore = "needs a FluxBridge interface with a disk in the drive"]
    fn fluxbridge_track_probe() {
        let driver = drivers()
            .into_iter()
            .find(|d| d.name.to_ascii_lowercase().contains(INTERFACE))
            .unwrap_or_else(|| panic!("no {INTERFACE} driver in this library"));
        println!("using driver {}: {}", driver.index, driver.name);

        let mut bridge = Bridge::open(&BridgeConfig {
            driver: driver.index,
            ..Default::default()
        })
        .expect("open the drive");

        // Spin up and let the platter reach speed, as the emulator does.
        bridge.set_motor(false, true);
        std::thread::sleep(std::time::Duration::from_millis(1200));
        println!(
            "disk={} ready={} wp={} type={:?}",
            bridge.disk_in_drive(),
            bridge.is_ready(),
            bridge.write_protected(),
            bridge.drive_type()
        );

        // `read_track` never waits, so poll it the way the emulator does. A
        // capture takes a revolution; a couple of seconds is generous.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let (words, bits) = loop {
            if let Some(track) = bridge.read_track(0, false) {
                break track;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no track came back within three seconds (disk in the drive?)"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        println!("captured {bits} bits in {} words", words.len());

        // AmigaDOS marks every sector with the MFM sync word 0x4489. A good
        // DD track has 11 of them, evenly spaced about one sector apart.
        let bit_at = |i: usize| words[(i / 16) % words.len()] & (1 << (15 - (i % 16))) != 0;
        let mut syncs = Vec::new();
        let mut window: u16 = 0;
        for i in 0..bits {
            window = (window << 1) | u16::from(bit_at(i));
            if i >= 15 && window == 0x4489 {
                syncs.push(i - 15);
            }
        }
        println!("found {} sync marks (0x4489)", syncs.len());
        if let Some(first) = syncs.first() {
            println!("first sync at bit {first}");
            let gaps: Vec<usize> = syncs.windows(2).map(|w| w[1] - w[0]).collect();
            println!("gaps between syncs: {:?}", &gaps[..gaps.len().min(14)]);
        }
        // A readable AmigaDOS track has a sync per sector. Fewer means the
        // stream reaching us is not the disk's MFM.
        assert!(
            syncs.len() >= 11,
            "expected at least 11 AmigaDOS sync marks, found {} -- the MFM crossing \
             the FFI does not look like the disk's",
            syncs.len()
        );

        // Decode each sector header. AmigaDOS splits every long into its odd
        // and even MFM bits, so a header that reads back with a sane format
        // byte, track and sector number proves the stream is not just
        // sync-shaped but genuinely decodable in the order we present it.
        let long_at = |start: usize| -> u32 {
            let mut v = 0u32;
            for k in 0..32 {
                v = (v << 1) | u32::from(bit_at(start + k));
            }
            v
        };
        let mut decoded = Vec::new();
        for &s in &syncs {
            // Skip the sync pair to reach the info long.
            let info_start = s + 32;
            if info_start + 64 > bits {
                continue;
            }
            let odd = long_at(info_start);
            let even = long_at(info_start + 32);
            let info = ((odd & 0x5555_5555) << 1) | (even & 0x5555_5555);
            let [format, track_no, sector, to_gap] = info.to_be_bytes();
            if format == 0xFF {
                decoded.push((track_no, sector, to_gap));
            }
        }
        println!("decoded {} sector headers", decoded.len());
        for (t, s, g) in decoded.iter().take(13) {
            println!("  track {t} sector {s} (sectors to gap {g})");
        }
        assert!(
            decoded.len() >= 11,
            "only {} sector headers decoded; the MFM is sync-shaped but not \
             AmigaDOS-decodable as presented",
            decoded.len()
        );

        // With the tab open, a write must be refused before anything is queued.
        // Checked here rather than in its own test because it is the one part
        // of the write path that can be exercised without risking a disk.
        if bridge.write_protected() {
            let unchanged = words.clone();
            assert!(
                !bridge.write_track(0, false, &words, 0),
                "a write-protected disk must refuse the write"
            );
            let (after, _) = bridge.read_track(0, false).expect("re-read cylinder 0");
            assert_eq!(
                after, unchanged,
                "a refused write must leave the platter untouched"
            );
            println!("write-protect: write refused and the track is unchanged");
        } else {
            println!("write-protect: disk is writable, skipping the refusal check");
        }
    }

    /// Powering the machine off drops the bridge and powering it back on
    /// opens a new one, so the device has to be genuinely released by the
    /// drop -- not merely forgotten about -- or the second open is refused
    /// with the port still in use.
    #[test]
    #[ignore = "needs a real interface attached"]
    fn a_dropped_bridge_hands_the_device_back() {
        // Deliberately no fallback to another driver: opening a board with a
        // different interface's protocol can leave it wedged until it is
        // physically unplugged.
        let driver = drivers()
            .into_iter()
            .find(|d| d.name.to_ascii_lowercase().contains(INTERFACE))
            .unwrap_or_else(|| panic!("no {INTERFACE} driver in this library"));
        let open = || {
            Bridge::open(&BridgeConfig {
                driver: driver.index,
                ..Default::default()
            })
        };

        let first = open().expect("open the drive");
        drop(first);
        // Immediately, with no grace period: this is a power button being
        // pressed twice, or Run from the configuration screen.
        let second = open().expect("re-open the drive after releasing it");
        drop(second);
    }

    /// The queries walk the host's serial bus and the library's driver table
    /// concurrently from the launcher and from config validation, so they
    /// must be safe to ask from several threads at once.
    #[test]
    fn library_queries_survive_being_asked_from_several_threads() {
        let threads: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..8 {
                        let _ = com_ports();
                        let _ = drivers();
                        let _ = interface_connected();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("a query thread panicked");
        }
    }

    #[test]
    fn reported_drivers_are_well_formed() {
        for (i, d) in drivers().iter().enumerate() {
            assert_eq!(d.index as usize, i, "driver indices are dense and ordered");
            assert!(!d.name.is_empty(), "driver {i} has a name");
        }
    }
}
