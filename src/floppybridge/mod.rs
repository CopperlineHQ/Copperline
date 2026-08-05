// SPDX-License-Identifier: GPL-3.0-or-later

//! Physical floppy drives through the safe Rust [`fluxbridge`] crate.
//!
//! FluxBridge is a Rust port of the runtime portions of Rob Smith's
//! [FloppyDriveBridge](https://github.com/RobSmithDev/FloppyDriveBridge).
//! Copperline keeps this module as a small integration seam so its historical
//! `floppybridge` Cargo feature and configuration keys remain compatible.
//!
//! The crate owns each interface on one worker thread. Reads inspect completed
//! captures without waiting on hardware, writes report their eventual result
//! through [`BridgeEvent`], and [`DriveStatus`] is a nonblocking snapshot.

pub use fluxbridge::{
    Bridge, BridgeConfig, BridgeEvent, Capabilities, CaptureQuality, DensityMode, DriveSelect,
    DriveStatus, DriveType, DriverInfo, DriverKind, PortId, PortSelection, ReadMode, Side,
    TrackAddress, TrackCapture, WriteId, WriteRequest, VERSION,
};

/// Returns the hardware drivers compiled into FluxBridge.
pub fn drivers() -> &'static [DriverInfo] {
    fluxbridge::drivers()
}

/// Returns the stable identifiers of serial and direct-FTDI ports visible now.
pub fn com_ports() -> Vec<String> {
    fluxbridge::ports()
        .unwrap_or_default()
        .into_iter()
        .map(|port| port.id.to_string())
        .collect()
}

/// Whether the host currently exposes a possible bridge interface.
///
/// As in the previous integration, this is deliberately only a port-presence
/// test: identifying a board requires opening it, which would take ownership
/// away from a running emulator or another application.
pub fn interface_connected() -> bool {
    !com_ports().is_empty()
}
