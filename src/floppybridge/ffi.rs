// SPDX-License-Identifier: GPL-3.0-or-later

//! Raw bindings to FloppyBridge's C ABI.
//!
//! FloppyBridge (<https://amiga.robsmithdev.co.uk/winuae>) is Rob Smith's
//! driver for talking to a physical floppy drive through a DrawBridge, a
//! Greaseweazle, or a Supercard Pro. It exposes a flat C ABI in two halves:
//! `BRIDGE_*` manages the bridge and its drivers, and `DRIVER_*` drives one
//! open device through an opaque handle. Several handles
//! can be open at once, which is what lets each Amiga drive have its own.
//!
//! The implementation is vendored in `vendor/floppybridge` and compiled into
//! the emulator by `build.rs`, so these are ordinary linked calls. The
//! signatures are reproduced from upstream's `floppybridge_common.h`, which it
//! releases into the public domain (Unlicense) precisely so other projects can
//! bind to them.
//!
//! Everything in this file is `unsafe` by nature: the signatures must match the
//! C side exactly or calls are undefined behaviour. The safe wrapper in the
//! parent module is the only thing the rest of Copperline talks to.

use std::ffi::{c_char, c_int, c_uint, c_void};

/// An open device. Opaque to us; `DRIVER_*` calls carry it back to the library.
pub(super) type BridgeDriverHandle = *mut c_void;

/// How hard the driver works to stay faithful to the disk's real timing.
///
/// `Fast`, `Compatible` and `Stalling` are all read modes and Copperline
/// offers all three -- as Normal, Compatible and Stalling, which is what
/// Amiberry's drive-type list calls them. The first two differ only in whether
/// the capture waits for the index: `Compatible` does, so a revolution begins
/// where the real one does; `Fast` does not, which is quicker but leaves the
/// revolution's two ends meeting mid-sector -- handled by reading the following
/// recording rather than looping the same one. `TurboAmigaDos` is not a read
/// mode at all, answering AmigaDOS calls instead of the disk, and is never
/// asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum BridgeMode {
    Fast = 0,
    #[default]
    Compatible = 1,
    TurboAmigaDos = 2,
    Stalling = 3,
}

/// Whether to force a density rather than sensing it from the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum BridgeDensityMode {
    #[default]
    Auto = 0,
    DdOnly = 1,
    HdOnly = 2,
}

/// Which drive on the cable the interface should select. The `DriveA`/`DriveB`
/// pair is the IBM PC cable convention; `Drive0`..`Drive3` are Shugart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum DriveSelection {
    #[default]
    DriveA = 0,
    DriveB = 1,
    Drive0 = 2,
    Drive1 = 3,
    Drive2 = 4,
    Drive3 = 5,
}

/// What a driver supports beyond the common set, as a bitmask in
/// [`BridgeDriverRaw::config_options`]. Used to grey the options a given
/// interface cannot honour.
pub mod config_option {
    pub const AUTO_CACHE: u32 = 0x01;
    pub const COM_PORT: u32 = 0x02;
    pub const AUTO_DETECT_COMPORT: u32 = 0x04;
    pub const DRIVE_AB_CABLE: u32 = 0x08;
    pub const SMART_SPEED: u32 = 0x10;
    pub const SUPPORTS_SHUGART: u32 = 0x20;
}

/// Static description of one driver (DrawBridge, Greaseweazle, ...). The
/// pointers belong to the library and stay valid while it is loaded.
#[repr(C)]
pub(super) struct BridgeDriverRaw {
    pub name: *const c_char,
    pub url: *const c_char,
    pub manufacturer: *const c_char,
    pub driver_author: *const c_char,
    pub config_options: c_uint,
}

/// Version and update information for the library itself.
#[repr(C)]
pub(super) struct BridgeAboutRaw {
    pub about: *const c_char,
    pub url: *const c_char,
    pub major_version: c_uint,
    pub minor_version: c_uint,
    pub is_beta: c_uint,
    pub is_update_available: c_uint,
    pub update_major_version: c_uint,
    pub update_minor_version: c_uint,
}

// The bridge's exported entry points.
//
// These are compiled into the emulator from `vendor/floppybridge`, not loaded
// from anywhere, so they link like any other native call. Upstream declares
// them `extern "C"` (`_cdecl` on Windows, the platform default elsewhere),
// which is what Rust's `extern "C"` means on every target Copperline builds
// for.
//
// Only the calls Copperline makes are declared. The two Windows-only dialog
// entry points are deliberately absent, and are compiled out of the vendored
// sources entirely: bridges are configured through Copperline's own launcher
// and config file on every platform.
#[allow(non_snake_case)]
extern "C" {
    // --- library ---
    pub(super) fn BRIDGE_About(allow_check_for_updates: bool, output: *mut *mut BridgeAboutRaw);
    pub(super) fn BRIDGE_NumDrivers() -> c_uint;
    pub(super) fn BRIDGE_GetDriverInfo(index: c_uint, out: *mut *mut BridgeDriverRaw) -> bool;
    pub(super) fn BRIDGE_EnumComports(output: *mut c_char, size: *mut c_uint) -> bool;

    // --- lifecycle ---
    /// Create from a driver index directly. Preferred over synthesizing a
    /// config string, whose field layout is upstream's private business.
    pub(super) fn BRIDGE_CreateDriver(index: c_uint, out: *mut BridgeDriverHandle) -> bool;
    pub(super) fn BRIDGE_Open(handle: BridgeDriverHandle, error: *mut *mut c_char) -> bool;
    pub(super) fn BRIDGE_Close(handle: BridgeDriverHandle) -> bool;
    pub(super) fn BRIDGE_FreeDriver(handle: BridgeDriverHandle) -> bool;

    // --- per-driver settings ---
    pub(super) fn BRIDGE_DriverSetMode(handle: BridgeDriverHandle, mode: u8) -> bool;
    pub(super) fn BRIDGE_DriverSetDensityMode(handle: BridgeDriverHandle, density: u8) -> bool;
    pub(super) fn BRIDGE_DriverSetCurrentComPort(
        handle: BridgeDriverHandle,
        port: *mut c_char,
    ) -> bool;
    pub(super) fn BRIDGE_DriverSetAutoDetectComPort(
        handle: BridgeDriverHandle,
        auto_detect: bool,
    ) -> bool;
    pub(super) fn BRIDGE_DriverSetCable2(handle: BridgeDriverHandle, drive: u8) -> bool;
    pub(super) fn BRIDGE_DriverSetSmartSpeedEnabled(
        handle: BridgeDriverHandle,
        enabled: bool,
    ) -> bool;
    pub(super) fn BRIDGE_DriverSetAutoCache(handle: BridgeDriverHandle, enabled: bool) -> bool;

    // --- drive state ---
    pub(super) fn DRIVER_isStillWorking(handle: BridgeDriverHandle) -> bool;
    pub(super) fn DRIVER_isReady(handle: BridgeDriverHandle) -> bool;
    pub(super) fn DRIVER_isDiskInDrive(handle: BridgeDriverHandle) -> bool;
    pub(super) fn DRIVER_hasDiskChanged(handle: BridgeDriverHandle) -> bool;
    pub(super) fn DRIVER_isWriteProtected(handle: BridgeDriverHandle) -> bool;
    pub(super) fn DRIVER_getMaxCylinder(handle: BridgeDriverHandle) -> u8;
    pub(super) fn DRIVER_getCurrentCylinderNumber(handle: BridgeDriverHandle) -> u8;
    pub(super) fn DRIVER_gotoCylinder(handle: BridgeDriverHandle, cylinder: c_int, side: bool);
    pub(super) fn DRIVER_setMotorStatus(handle: BridgeDriverHandle, side: bool, on: bool);
    pub(super) fn DRIVER_isMotorRunning(handle: BridgeDriverHandle) -> bool;
    pub(super) fn DRIVER_setSurface(handle: BridgeDriverHandle, side: bool);
    pub(super) fn DRIVER_getDriveTypeID(handle: BridgeDriverHandle) -> u8;

    /// Copy the whole live revolution of a track out in one call: side,
    /// cylinder, whether to resync to the index (which the library ignores
    /// here -- the capture mode decides it), the buffer's size in bytes, and
    /// the buffer. The MFM lands packed MSB-first, and the return is the
    /// revolution's length in bits, or 0 when the driver has not finished
    /// capturing one yet.
    ///
    /// The driver reads the disk continuously in the background, so this never
    /// waits for the platter -- with one exception, the `Stalling` bridge mode,
    /// whose whole purpose is to hold the caller until data arrives.
    pub(super) fn DRIVER_getTrack(
        handle: BridgeDriverHandle,
        side: bool,
        cylinder: c_uint,
        resync: bool,
        buffer_bytes: c_int,
        buffer: *mut c_void,
    ) -> c_int;

    /// Retire the revolution just read and promote the next recording of the
    /// same track. Upstream calls this when a full revolution has been
    /// consumed, so a caller reading revolution after revolution gets
    /// successive recordings rather than the same one over again.
    pub(super) fn DRIVER_mfmSwitchBuffer(handle: BridgeDriverHandle, side: bool);

    /// The current track's length in bits -- the wrap point, and the largest
    /// position `getMFMBit` or a write may name.
    pub(super) fn DRIVER_maxMFMBitPosition(handle: BridgeDriverHandle) -> c_int;

    /// Hand one MFM word to the drive at the rotational position the head
    /// would be passing over, mirroring how a real write lays cells down as
    /// the platter turns.
    pub(super) fn DRIVER_writeShortToBuffer(
        handle: BridgeDriverHandle,
        side: bool,
        cylinder: c_uint,
        data: u16,
        position: c_int,
    );
    /// Commit the words fed so far to the platter. Returns the track's new
    /// length in bits.
    pub(super) fn DRIVER_commitWriteBuffer(
        handle: BridgeDriverHandle,
        side: bool,
        cylinder: c_uint,
    ) -> c_uint;
}
