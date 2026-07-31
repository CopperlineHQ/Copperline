// SPDX-License-Identifier: GPL-3.0-or-later

//! Real floppy drives, through Rob Smith's FloppyBridge library.
//!
//! A *bridge* replaces one Amiga drive's disk image with a physical 3.5"
//! drive attached over a DrawBridge, a Greaseweazle, or a Supercard Pro. The
//! emulated machine is unchanged: the bridge only supplies the MFM the head
//! would be passing over, so Paula, the disk DMA, and trackdisk.device all
//! behave exactly as they do with an image.
//!
//! # How it attaches
//!
//! The library reads the disk continuously on its own thread and keeps the
//! track under the head captured and ready. [`Bridge::read_track`] takes a
//! whole finished revolution from it in one call, packed MFM plus the length
//! it wrapped at -- which is the shape a captured revolution already has in
//! [`crate::floppy`], so the existing rotation, PLL, and sync-word machinery
//! reads a real disk with no special case in the hot path.
//!
//! Nothing here waits on the drive. A track that has not been captured yet
//! comes back as `None` and the caller asks again, exactly as it already did
//! while the motor spun up. Blocking instead would stop the emulated Amiga --
//! CPU, sprites, pointer and all -- every time the head moved, which is the
//! one thing a real drive never does to a real Amiga.
//!
//! Because a revolution is served whole, its two ends matter. A capture made
//! in the library's `Compatible` mode starts at the index, so its ends meet in
//! the gap between sectors and it can turn under the head indefinitely. The
//! default `normal` mode captures wherever the head happens to be and the
//! driver joins the ends where the recording repeats -- a join that is not
//! always perfect, which is why [`scan`] verifies each such capture before the
//! emulator trusts it beyond a single pass.
//!
//! Writing goes the same way round: [`Bridge::write_track`] hands the MFM over
//! a word at a time at the rotational position the head would be passing, as a
//! real drive lays cells down, and commits it to the platter -- without waiting
//! for the platter to take it, for the same reason reads do not wait.
//!
//! # Where it comes from
//!
//! FloppyBridge is compiled into the emulator from `vendor/floppybridge`, so a
//! build that says it supports a physical drive can actually drive one, with
//! nothing to download and nothing to install. Upstream ships it as a shared
//! library to be loaded at run time; Copperline links it instead, because a
//! user should not have to fetch a second file to use a feature the build
//! claims to have.
//!
//! Keeping upstream current is a maintainer's job and close to a wholesale
//! copy: `vendor/floppybridge/README.md` records the commit vendored and the
//! handful of local changes, all of them Windows or Linux build differences
//! rather than behaviour.
//!
//! Turning the `floppybridge` Cargo feature off, which is on by default,
//! compiles all of this out -- the C++ included.
//!
//! # Determinism
//!
//! A real drive spins in wall-clock time and the disk under it can be swapped
//! by hand, so a run using a bridge is *not* reproducible: save states cannot
//! capture the medium, and a replayed input recording will not line up. Nor can
//! the machine be run faster than the platter turns, so a bridged machine is
//! paced to wall-clock time even in a headless run that would otherwise be
//! unthrottled -- see `main`. As with NAT networking, the constraint is
//! documented rather than enforced by a flag: the emulated core is as
//! deterministic as ever, it is the disk under it that is not.

mod ffi;
pub mod scan;

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::sync::{Mutex, OnceLock};

use log::warn;

pub use ffi::{config_option, BridgeDensityMode, BridgeMode, DriveSelection};

/// The largest string the library will write into a caller-provided buffer.
/// Upstream's own `BRIDGE_STRING_MAX_LENGTH`.
const STRING_MAX: usize = 255;

/// How close to the index a write has to start for the driver to place it
/// there. Upstream's own figure, from `commitWriteBuffer`.
const INDEX_WRITE_SLACK_BITS: usize = 30;

/// Bytes to allow for one captured revolution. Upstream sizes its own track
/// buffers at `MFM_BUFFER_MAX_TRACK_LENGTH` (0x3A00 * 2), so nothing longer
/// than this can come back: a DD revolution is around 12.7K of MFM and an HD
/// one twice that, leaving plenty of room for a drive running off-speed.
const MAX_TRACK_BYTES: usize = 0x3A00 * 2;

/// Serialises calls into the library itself.
///
/// The `BRIDGE_*` half works through state the library owns process-wide: the
/// port scan fills a file-scope vector on the sizing call and reads it back on
/// the next, and the driver query hands out pointers into storage that belongs
/// to the library.
/// None of that survives two threads at once -- reliably enough that the test
/// suite aborted with SIGABRT whenever two of these ran together -- and the
/// launcher does exactly that, asking what is plugged in while a machine is
/// running.
///
/// Per-drive `DRIVER_*` calls are deliberately not covered: each belongs to one
/// [`Bridge`] owned by the emulation thread, and the library serialises its own
/// worker behind them. Only [`Bridge::open`] takes this, because creating a
/// driver goes through the `BRIDGE_*` half.
static LIB_LOCK: Mutex<()> = Mutex::new(());

/// Take the library lock, ignoring poisoning: a panic in another thread's
/// query leaves the library no more broken than it already was, and refusing
/// to look at the hardware ever again would be the worse failure.
fn lib_lock() -> std::sync::MutexGuard<'static, ()> {
    LIB_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Copy a bridge-owned C string. The pointers handed back belong to the bridge
/// and several are documented as invalid after the next call, so everything
/// crossing into Copperline is copied immediately.
///
/// # Safety
/// `ptr` must be null or a valid NUL-terminated string.
unsafe fn owned_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    CStr::from_ptr(ptr).to_string_lossy().into_owned()
}

/// The bridge's version, as `(major, minor)`.
///
/// Asked once and remembered: what is linked in cannot change under us, and
/// the launcher puts this in a heading.
pub fn version() -> Option<(u32, u32)> {
    static VERSION: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *VERSION.get_or_init(|| {
        let _guard = lib_lock();
        let mut out: *mut ffi::BridgeAboutRaw = std::ptr::null_mut();
        // `false`: never let the bridge phone home for an update check.
        unsafe { ffi::BRIDGE_About(false, &mut out) };
        if out.is_null() {
            return None;
        }
        let about = unsafe { &*out };
        Some((about.major_version as u32, about.minor_version as u32))
    })
}

/// The physical drive an interface reports being attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveType {
    /// A plain 3.5" double-density drive: the normal Amiga case.
    Dd35,
    /// A 3.5" high-density drive, which can also read DD media.
    Dd35Hd,
    /// A 5.25" single-density drive.
    Sd525,
}

/// One driver the library offers, by index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverInfo {
    pub index: u32,
    pub name: String,
    pub manufacturer: String,
    pub url: String,
    /// Bitmask of [`config_option`] flags this driver honours.
    pub config_options: u32,
}

impl DriverInfo {
    pub fn supports(&self, option: u32) -> bool {
        self.config_options & option != 0
    }
}

/// Every driver the loaded library offers (DrawBridge, Greaseweazle, ...).
/// Empty only if the bridge reports no drivers at all, which should not
/// happen: the library is linked in, not looked for.
pub fn drivers() -> Vec<DriverInfo> {
    let _guard = lib_lock();
    let count = unsafe { ffi::BRIDGE_NumDrivers() };
    (0..count)
        .filter_map(|index| {
            let mut raw: *mut ffi::BridgeDriverRaw = std::ptr::null_mut();
            if !unsafe { ffi::BRIDGE_GetDriverInfo(index, &mut raw) } || raw.is_null() {
                return None;
            }
            let d = unsafe { &*raw };
            Some(DriverInfo {
                index,
                name: unsafe { owned_string(d.name) },
                manufacturer: unsafe { owned_string(d.manufacturer) },
                url: unsafe { owned_string(d.url) },
                config_options: d.config_options as u32,
            })
        })
        .collect()
}

/// Whether anything that could be an interface is plugged in right now.
///
/// The port scan below reports the host's USB serial devices, not boards it
/// has identified -- no current interface can be told apart without opening
/// it -- so a non-empty list means "possibly", and an empty one means there
/// is nothing to drive a real disk with. Scanning walks the host's serial
/// bus, so this is sampled at deliberate moments -- a bay being switched
/// over to a real drive -- rather than every frame.
pub fn interface_connected() -> bool {
    let _guard = lib_lock();
    !com_ports_locked().is_empty()
}

/// Serial ports the library can see -- the host's USB serial devices, which
/// on macOS means the `tty.usb*`-prefixed ones only. The launcher widens
/// this with the host's own list for the chips that convention misses.
pub fn com_ports() -> Vec<String> {
    let _guard = lib_lock();
    com_ports_locked()
}

/// The scan itself. Split out because the caller may already hold the lock,
/// which is not re-entrant.
fn com_ports_locked() -> Vec<String> {
    // Upstream fills a caller-provided buffer with NUL-separated names and
    // reports the size it wanted; ask twice so a long list is never truncated.
    let mut size: c_uint = 0;
    unsafe { ffi::BRIDGE_EnumComports(std::ptr::null_mut(), &mut size) };
    let cap = (size as usize).max(STRING_MAX * 8);
    let mut buffer = vec![0u8; cap];
    let mut size = cap as c_uint;
    if !unsafe { ffi::BRIDGE_EnumComports(buffer.as_mut_ptr() as *mut c_char, &mut size) } {
        return Vec::new();
    }
    buffer
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Everything needed to open one real drive.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeConfig {
    /// Which driver to open, by the index [`drivers`] reported it at.
    pub driver: u32,
    pub mode: BridgeMode,
    pub density: BridgeDensityMode,
    pub drive: DriveSelection,
    /// Which serial port the interface is on. `None` -- the default -- lets the
    /// driver find its own device, which every current driver supports
    /// ([`config_option::AUTO_DETECT_COMPORT`]); name one to pin it, which
    /// matters when two interfaces are plugged in at once.
    pub port: Option<String>,
    pub auto_cache: bool,
}

/// One open real drive.
///
/// Not `Clone`: the handle owns a physical device. Dropping it closes and frees
/// the driver, which parks the motor.
pub struct Bridge {
    handle: ffi::BridgeDriverHandle,
    max_cylinder: u8,
    /// Scratch for whole-track reads, kept between calls so polling for a
    /// track does not churn a 29K allocation each time round.
    capture: Vec<u8>,
    /// Whether captures begin at the index. When they do, a revolution's two
    /// ends meet in the sector gap and it can be turned under the head over
    /// and over, as an image's track is. When they do not, the join falls
    /// mid-sector and the only safe way through it is forwards, into the
    /// recording that actually followed.
    index_aligned: bool,
}

// SAFETY: the handle is only ever touched through `&mut self` from the
// emulation thread; the library serialises its own worker internally.
unsafe impl Send for Bridge {}

impl Bridge {
    /// Open a real drive. `Err` carries the library's own message, which names
    /// the actual fault (no such port, device not responding, ...).
    ///
    /// No `Bridge` exists until the device is open, deliberately: dropping one
    /// takes the same lock this holds, so a half-built bridge going out of
    /// scope on the error path would deadlock against it.
    pub fn open(config: &BridgeConfig) -> Result<Self, String> {
        // Creating a driver goes through the bridge's own shared state.
        let _guard = lib_lock();
        let mut handle: ffi::BridgeDriverHandle = std::ptr::null_mut();

        let created = unsafe { ffi::BRIDGE_CreateDriver(config.driver as c_uint, &mut handle) };
        if !created || handle.is_null() {
            return Err("FloppyBridge refused the configuration".to_string());
        }

        // Settings have to be in place before the device is opened.
        // The drive select decides which physical drive on the cable is
        // spoken to, so a driver that will not take it must not be opened
        // anyway -- it would quietly use Drive A instead.
        let refused = apply(handle, config);
        if refused.contains(&"drive select") {
            unsafe { ffi::BRIDGE_FreeDriver(handle) };
            return Err(format!(
                "this interface does not support the {:?} drive select",
                config.drive
            ));
        }
        if !refused.is_empty() {
            warn!(
                "floppybridge: the interface would not take {}; its own default stands instead",
                refused.join(", "),
            );
        }

        let mut err: *mut c_char = std::ptr::null_mut();
        if !unsafe { ffi::BRIDGE_Open(handle, &mut err) } {
            let message = unsafe { owned_string(err) };
            // The driver exists even though the device would not open, so hand
            // it back rather than leaving it stranded in the bridge.
            unsafe { ffi::BRIDGE_FreeDriver(handle) };
            return Err(if message.is_empty() {
                "could not open the drive".to_string()
            } else {
                message
            });
        }

        Ok(Self {
            handle,
            // Only meaningful once the device is open, and it decides how far
            // the head is allowed to travel.
            max_cylinder: unsafe { ffi::DRIVER_getMaxCylinder(handle) },
            capture: vec![0u8; MAX_TRACK_BYTES],
            index_aligned: matches!(
                config.mode,
                ffi::BridgeMode::Compatible | ffi::BridgeMode::Stalling
            ),
        })
    }

    /// Whether the device is still responding. False once it is unplugged,
    /// which is how a bridge drive reports itself broken rather than hanging.
    pub fn is_working(&self) -> bool {
        unsafe { ffi::DRIVER_isStillWorking(self.handle) }
    }

    pub fn is_ready(&self) -> bool {
        unsafe { ffi::DRIVER_isReady(self.handle) }
    }

    pub fn disk_in_drive(&self) -> bool {
        unsafe { ffi::DRIVER_isDiskInDrive(self.handle) }
    }

    /// Consumes the library's disk-changed latch.
    pub fn take_disk_changed(&mut self) -> bool {
        unsafe { ffi::DRIVER_hasDiskChanged(self.handle) }
    }

    /// Whether the disk's write-protect tab is closed. Answered from the
    /// driver's last reading, which it keeps across the motor stopping, so
    /// this does not need the drive spun up to be meaningful.
    pub fn write_protected(&self) -> bool {
        unsafe { ffi::DRIVER_isWriteProtected(self.handle) }
    }

    /// The head's real cylinder, which is what the status bar's track counter
    /// shows for a bridged drive.
    pub fn current_cylinder(&self) -> u8 {
        unsafe { ffi::DRIVER_getCurrentCylinderNumber(self.handle) }
    }

    /// Keep the head inside what the drive can actually reach. The guest can
    /// step past the last cylinder -- trackdisk does it to find track 0, and a
    /// confused loader can do it anywhere -- and a real head just sits against
    /// the stop rather than travelling somewhere that does not exist.
    fn clamp_cylinder(&self, cylinder: u8) -> u8 {
        cylinder.min(self.max_cylinder.saturating_sub(1))
    }

    /// The kind of drive the interface reports, which is what decides whether
    /// HD media can be read at all.
    pub fn drive_type(&self) -> DriveType {
        match unsafe { ffi::DRIVER_getDriveTypeID(self.handle) } {
            1 => DriveType::Dd35Hd,
            2 => DriveType::Sd525,
            _ => DriveType::Dd35,
        }
    }

    pub fn motor_running(&self) -> bool {
        unsafe { ffi::DRIVER_isMotorRunning(self.handle) }
    }

    /// Spin the real drive up or down to follow the emulated motor line.
    pub fn set_motor(&mut self, side: bool, on: bool) {
        unsafe { ffi::DRIVER_setMotorStatus(self.handle, side, on) }
    }

    /// Move the head, following the emulated drive's stepper.
    pub fn seek(&mut self, cylinder: u8, side: bool) {
        let cylinder = self.clamp_cylinder(cylinder);
        unsafe { ffi::DRIVER_gotoCylinder(self.handle, cylinder as i32, side) }
    }

    /// Whether a captured revolution can be turned under the head more than
    /// once. Index-aligned captures can: their two ends meet in the gap
    /// between sectors, as an image's track does. A capture that began
    /// wherever the head happened to be cannot, and has to be followed by the
    /// recording that came after it.
    pub fn index_aligned(&self) -> bool {
        self.index_aligned
    }

    /// Retire the revolution just consumed so the next read returns the next
    /// recording of the track, making successive revolutions continuous the
    /// way the cells coming off a real head are.
    pub fn switch_buffer(&mut self, side: bool) {
        unsafe { ffi::DRIVER_mfmSwitchBuffer(self.handle, side) }
    }

    /// Take the current revolution of `cylinder`/`side`, if the driver has one.
    ///
    /// Returns the bit stream packed MSB-first into 16-bit words, plus its
    /// length in bits, where the head loops back round. That is exactly one
    /// captured revolution as [`crate::floppy`] stores it. In `Compatible` that
    /// boundary is the index; in `Fast` it is wherever the capture began.
    ///
    /// Never waits. The driver reads the disk continuously in the background,
    /// so either a finished revolution is sitting there or it is not, and
    /// `None` simply means "not yet": the caller asks again a moment later
    /// while the machine keeps running. Blocking here instead would stop the
    /// emulated Amiga -- mouse, screen and all -- for as long as the platter
    /// takes to come round, which is exactly the freeze a real drive does not
    /// cause. (The one exception is the `Stalling` bridge mode, where holding
    /// the caller up until data arrives is the mode's entire purpose.)
    pub fn read_track(&mut self, cylinder: u8, side: bool) -> Option<(Vec<u16>, usize)> {
        let cylinder = self.clamp_cylinder(cylinder);
        // The driver positions the head itself as part of this call, so there
        // is no separate seek to get wrong, and asking for a track it is
        // already on costs nothing.
        let bits = unsafe {
            ffi::DRIVER_getTrack(
                self.handle,
                side,
                cylinder as c_uint,
                false,
                self.capture.len() as c_int,
                self.capture.as_mut_ptr() as *mut c_void,
            )
        };
        if bits <= 0 {
            return None;
        }
        // Trust the buffer over the reported length: the driver copies only
        // what fits and still reports the track's full length, so a revolution
        // longer than anything the library itself can hold would otherwise be
        // read off the end of the capture.
        let bits = (bits as usize).min(self.capture.len() * 8);
        let bytes = &self.capture[..bits.div_ceil(8)];
        let words = bytes
            .chunks(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair.get(1).copied().unwrap_or(0)]))
            .collect();
        Some((words, bits))
    }

    /// Lay `words` of MFM down on the real disk, starting at bit `start_bit`
    /// of the track.
    ///
    /// The words go over one at a time at the rotational position the head
    /// would be passing, which is how a real drive writes and what the driver
    /// expects; committing then flushes them to the platter. `start_bit` is
    /// the emulated head position where the guest began writing, so a partial
    /// write lands exactly where it was aimed rather than at the index.
    ///
    /// Returns false if the disk cannot be written, leaving the platter
    /// untouched.
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
        if self.write_protected() {
            return false;
        }
        let cylinder = self.clamp_cylinder(cylinder);
        // The driver takes a rotational position per word, but only the first
        // survives: `commitWriteBuffer` reduces it to a `writeFromIndex`
        // boolean -- true within 30 bits of the index -- and the worker lays
        // the cells down either from the index pulse or from wherever the head
        // happens to be when it runs. There is no third option, so a write
        // that has to start anywhere else cannot be placed, and letting it go
        // would put the guest's sector on top of an unrelated one.
        //
        // Two shapes are safe. A write from the index is placed exactly. A
        // whole revolution overwrites the track entire, so where it begins
        // does not matter -- which is what AmigaDOS does, writing all eleven
        // sectors at once. Anything else is refused rather than gambled with:
        // this is somebody's real disk.
        let track_bits = unsafe { ffi::DRIVER_maxMFMBitPosition(self.handle) }.max(0) as usize;
        let from_index =
            start_bit <= INDEX_WRITE_SLACK_BITS || start_bit + INDEX_WRITE_SLACK_BITS >= track_bits;
        let whole_revolution = track_bits > 0 && words.len() * 16 + 16 >= track_bits;
        if !from_index && !whole_revolution {
            warn!(
                "floppybridge: refusing a partial write of {} words at bit {start_bit} of \
                 {track_bits} on cylinder {cylinder}: the interface can only start a write at \
                 the index or wherever the head is, so this one would land on another sector",
                words.len(),
            );
            return false;
        }
        unsafe {
            ffi::DRIVER_setSurface(self.handle, side);
            for (i, word) in words.iter().enumerate() {
                ffi::DRIVER_writeShortToBuffer(
                    self.handle,
                    side,
                    cylinder as c_uint,
                    *word,
                    (start_bit + i * 16) as c_int,
                );
            }
            // Nothing has touched the disk yet: this is what commits it.
            let new_len = ffi::DRIVER_commitWriteBuffer(self.handle, side, cylinder as c_uint);
            if new_len == 0 {
                warn!("floppybridge: the drive rejected the write of cylinder {cylinder}");
                return false;
            }
        }
        // Deliberately not waiting for the platter to take it. The driver lays
        // the cells down on its own thread as the disk turns, exactly as a real
        // drive does, and the caller drops its cached copy of the track: the
        // next read asks the driver again, and gets nothing back until the
        // freshly written track has been re-captured. Waiting here would stop
        // the emulated machine dead for a whole revolution on every write.
        true
    }
}

/// Push the settings onto a driver that has been created but not yet opened. A free function because it runs before
/// there is a [`Bridge`] to call it on.
///
/// Returns the settings the driver would not take. Most of these are
/// preferences, and a driver that cannot honour one carries on sensibly with
/// its default -- but the drive select is not a preference. Every driver
/// advertises the cable conventions it supports, and one that refuses the
/// requested selection silently keeps Drive A: the open then succeeds against
/// a different physical drive than the one asked for, which is the sort of
/// thing that is only noticed after writing to the wrong disk.
fn apply(handle: ffi::BridgeDriverHandle, config: &BridgeConfig) -> Vec<&'static str> {
    let mut refused = Vec::new();
    unsafe {
        if !ffi::BRIDGE_DriverSetMode(handle, config.mode as u8) {
            refused.push("read mode");
        }
        if !ffi::BRIDGE_DriverSetDensityMode(handle, config.density as u8) {
            refused.push("density");
        }
        if !ffi::BRIDGE_DriverSetCable2(handle, config.drive as u8) {
            refused.push("drive select");
        }
        if !ffi::BRIDGE_DriverSetAutoCache(handle, config.auto_cache) {
            refused.push("auto-cache");
        }
        // Auto-detect unless a port is named, so the common case is
        // plug-in-and-go and an explicit port always wins.
        ffi::BRIDGE_DriverSetAutoDetectComPort(handle, config.port.is_none());
        if let Some(port) = config.port.as_deref() {
            if let Ok(port) = CString::new(port) {
                ffi::BRIDGE_DriverSetCurrentComPort(handle, port.as_ptr() as *mut c_char);
            }
        }
    }
    refused
}

impl Drop for Bridge {
    fn drop(&mut self) {
        // Closing and freeing are `BRIDGE_*` calls, so they take the same lock
        // the rest of that half does. Both null-check, which is what makes the
        // half-built bridge left by a failed `open` safe to drop.
        let _guard = lib_lock();
        unsafe {
            ffi::BRIDGE_Close(self.handle);
            ffi::BRIDGE_FreeDriver(self.handle);
        }
    }
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge")
            .field("max_cylinder", &self.max_cylinder)
            .finish_non_exhaustive()
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
        assert!(version().is_some(), "and reports its version");
        // These simply describe a host with nothing attached.
        let _ = com_ports();
        let _ = interface_connected();
    }

    /// Print what a real library reports. Ignored by default because it needs
    /// one installed; run it to check a setup:
    ///     cargo test --release floppybridge_inventory -- --ignored --nocapture
    #[test]
    #[ignore = "lists what the built-in bridge offers; run it to see the drivers"]
    fn floppybridge_inventory() {
        println!("bridge: compiled in from vendor/floppybridge");
        if let Some((major, minor)) = version() {
            println!("version: {major}.{minor}");
        }
        for d in drivers() {
            println!(
                "driver {}: {} by {} (options {:#04x})",
                d.index, d.name, d.manufacturer, d.config_options
            );
        }
        println!("ports: {:?}", com_ports());
    }

    /// Read cylinder 0 off a real disk and report what the MFM looks like:
    /// how many AmigaDOS sync marks are in it and how they are spaced. That
    /// separates "the stream is good, the emulator's timing is wrong" from
    /// "the bytes crossing the FFI are not what we think they are".
    ///
    ///     cargo test --release floppybridge_track_probe -- --ignored --nocapture
    /// Watch the drive's status lines with the motor stopped and running, to
    /// see what the driver actually reports rather than what it is assumed to.
    /// Write protection is the one that matters: Copperline gates real writes
    /// on it, and a reading taken at the wrong moment either refuses a disk
    /// that is writable or, worse, allows one that is not.
    ///
    ///     cargo test --release floppybridge_status_probe -- --ignored --nocapture
    #[test]
    #[ignore = "needs a FloppyBridge device attached"]
    fn floppybridge_status_probe() {
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

    #[test]
    #[ignore = "needs a FloppyBridge device with a disk in the drive"]
    fn floppybridge_track_probe() {
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

    /// The library keeps its `BRIDGE_*` state process-wide -- one shared port
    /// vector, one profile cache -- so the queries have to be serialised on
    /// this side. Without that, this does not fail: it aborts the process,
    /// which is how the problem was found.
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
