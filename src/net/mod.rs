// SPDX-License-Identifier: GPL-3.0-or-later

//! Host networking backends for emulated Ethernet boards (the a2065 LANCE and
//! WASM NIC plugins).
//!
//! An emulated NIC owns a [`NetBackend`]: it pushes the Ethernet frames the
//! guest transmits with [`NetBackend::send`] and pulls inbound frames with
//! [`NetBackend::poll`]. A frame is a complete Ethernet frame (destination MAC
//! through payload, no FCS).
//!
//! Networking is inherently non-deterministic: inbound frames arrive on the
//! host's schedule, not the emulated clock, so a NIC board breaks Copperline's
//! byte-identical replay / save-state determinism while traffic flows. Backends
//! are therefore host resources, not serialized state -- a save state records
//! only the board's chosen backend ([`NetConfig`]) and brings up a fresh
//! backend on load (in-flight frames are dropped; the guest's TCP retransmits).
//!
//! Three backends are built in: [`LoopbackBackend`] (frames echo back to the
//! sender, for tests and a self-contained two-station demo) and, behind the
//! `net-nat` build feature, the userspace NAT in [`nat`] -- a slirp-style
//! virtual gateway (guest 10.0.2.15, gateway 10.0.2.2, DNS 10.0.2.3) that
//! gives the guest outbound IPv4 through ordinary host sockets, with no host
//! privileges. Behind `net-bridge`, [`bridge`] attaches complete Ethernet
//! frames directly to a selected host adapter. [`NetConfig::None`] brings up
//! no backend at all, which is how an isolated NIC (one with the capability
//! but no host connectivity) is expressed.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[cfg(all(feature = "net-bridge", not(target_arch = "wasm32")))]
pub mod bridge;
#[cfg(all(feature = "net-nat", not(target_arch = "wasm32")))]
pub mod nat;

/// A host networking backend an emulated NIC sends and receives frames through.
/// `Send` so it can live in a wasmtime store's data (which the bus owns).
pub trait NetBackend: Send {
    /// Transmit one Ethernet frame from the guest to the network.
    fn send(&mut self, frame: &[u8]);

    /// Return the next inbound Ethernet frame for the guest, if any.
    fn poll(&mut self) -> Option<Vec<u8>>;

    /// Update the station address used by a backend's receive filter. The
    /// LANCE learns this from its initialization block after the host backend
    /// has already opened.
    fn set_guest_mac(&mut self, _mac: [u8; 6]) {}
}

/// Which host backend a NIC board uses. Recorded in the board's config (and
/// save state) so the board is self-contained; the live backend it names is a
/// host resource brought up fresh by [`make_backend`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NetConfig {
    /// No connectivity: transmits are dropped, nothing is ever received.
    #[default]
    None,
    /// Frames transmitted are queued straight back as received frames. Lets a
    /// guest see its own broadcasts and supports a self-contained demo without
    /// touching the host network; also the deterministic backend for tests.
    Loopback,
    /// Userspace NAT: a virtual gateway NATs the guest's IPv4 onto ordinary
    /// host sockets (no privileges, outbound only). Needs the `net-nat` build
    /// feature; without it the variant still parses and saves, but no backend
    /// comes up.
    Nat,
    /// Direct layer-2 attachment to a host adapter. Frames retain their guest
    /// source MAC; therefore wireless adapters are best-effort because many
    /// access points reject multiple source addresses behind one station.
    Bridge {
        /// Stable platform adapter name (for example `eth0`, `en0`, or an
        /// Npcap `\Device\NPF_{...}` identifier).
        interface: String,
    },
}

/// Whether this build can bring up the userspace NAT backend: the same
/// condition [`make_backend`]'s `NetConfig::Nat` arms are compiled under
/// (the `net-nat` feature, and never on wasm32). Pickers and warnings key
/// off this so they track what `make_backend` will actually do.
pub const NAT_AVAILABLE: bool = cfg!(all(feature = "net-nat", not(target_arch = "wasm32")));
/// Whether this build includes a native direct-adapter bridge backend.
pub const BRIDGE_AVAILABLE: bool = cfg!(all(feature = "net-bridge", not(target_arch = "wasm32")));

/// Bring up the live backend a [`NetConfig`] names. `None` means the board has
/// no host networking (its NIC still works, it just never sees traffic).
///
/// Unlike optional NAT support, selecting a bridge is an explicit request for
/// a particular host resource. Failure is returned to the caller and must stop
/// startup/state restoration instead of silently changing network semantics.
pub fn make_backend(
    cfg: &NetConfig,
    guest_mac: Option<[u8; 6]>,
) -> Result<Option<Box<dyn NetBackend>>> {
    #[cfg(not(all(feature = "net-bridge", not(target_arch = "wasm32"))))]
    let _ = guest_mac;
    match cfg {
        NetConfig::None => Ok(None),
        NetConfig::Loopback => Ok(Some(Box::new(LoopbackBackend::default()))),
        #[cfg(all(feature = "net-nat", not(target_arch = "wasm32")))]
        NetConfig::Nat => Ok(Some(Box::new(nat::NatBackend::new()))),
        #[cfg(not(all(feature = "net-nat", not(target_arch = "wasm32"))))]
        NetConfig::Nat => {
            log::warn!("net = \"nat\" needs the net-nat build feature; NIC left isolated");
            Ok(None)
        }
        #[cfg(all(feature = "net-bridge", not(target_arch = "wasm32")))]
        NetConfig::Bridge { interface } => bridge::BridgeBackend::new(interface, guest_mac)
            .map(|backend| Some(Box::new(backend) as Box<dyn NetBackend>)),
        #[cfg(not(all(feature = "net-bridge", not(target_arch = "wasm32"))))]
        NetConfig::Bridge { .. } => {
            bail!("net = \"bridge\" needs a native build with the net-bridge feature")
        }
    }
}

/// A backend that queues each transmitted frame straight back for receipt.
#[derive(Default)]
pub struct LoopbackBackend {
    queue: VecDeque<Vec<u8>>,
}

impl NetBackend for LoopbackBackend {
    fn send(&mut self, frame: &[u8]) {
        self.queue.push_back(frame.to_vec());
    }

    fn poll(&mut self) -> Option<Vec<u8>> {
        self.queue.pop_front()
    }
}

/// Parse a `net`/`net_backend` config string into a [`NetConfig`]. A bridge
/// needs its adapter name in the adjacent `interface`/`net_interface` field.
pub fn parse_net_config(s: &str, interface: Option<&str>) -> Result<NetConfig> {
    match s.trim().to_ascii_lowercase().as_str() {
        "none" | "off" | "" => Ok(NetConfig::None),
        "loopback" | "loop" => Ok(NetConfig::Loopback),
        "nat" => Ok(NetConfig::Nat),
        "bridge" | "bridged" => {
            let interface = interface.map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| {
                anyhow::anyhow!("net = \"bridge\" needs an interface name")
            })?;
            Ok(NetConfig::Bridge {
                interface: interface.to_string(),
            })
        }
        _ => bail!(
            "net = {s:?} is not a known backend (expected \"none\", \"loopback\", \"nat\", or \"bridge\")"
        ),
    }
}

/// The canonical config-file spelling of a [`NetConfig`]: the inverse of
/// [`parse_net_config`], used when emitting a config from the launcher.
pub fn net_config_name(cfg: &NetConfig) -> &'static str {
    match cfg {
        NetConfig::None => "none",
        NetConfig::Loopback => "loopback",
        NetConfig::Nat => "nat",
        NetConfig::Bridge { .. } => "bridge",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_returns_sent_frames_in_order() {
        let mut b = LoopbackBackend::default();
        assert!(b.poll().is_none());
        b.send(&[1, 2, 3]);
        b.send(&[4, 5]);
        assert_eq!(b.poll(), Some(vec![1, 2, 3]));
        assert_eq!(b.poll(), Some(vec![4, 5]));
        assert!(b.poll().is_none());
    }

    #[test]
    fn config_parses_known_backends() {
        assert_eq!(
            parse_net_config("loopback", None).unwrap(),
            NetConfig::Loopback
        );
        assert_eq!(parse_net_config("None", None).unwrap(), NetConfig::None);
        assert_eq!(parse_net_config("", None).unwrap(), NetConfig::None);
        assert_eq!(parse_net_config("nat", None).unwrap(), NetConfig::Nat);
        assert_eq!(
            parse_net_config("bridge", Some("en0")).unwrap(),
            NetConfig::Bridge {
                interface: "en0".into()
            }
        );
        assert!(parse_net_config("bridge", None).is_err());
        assert!(parse_net_config("tap0", None).is_err());
    }

    #[test]
    fn make_backend_brings_up_named_backend() {
        assert!(make_backend(&NetConfig::None, None).unwrap().is_none());
        let mut b = make_backend(&NetConfig::Loopback, None)
            .unwrap()
            .expect("loopback backend");
        b.send(&[9]);
        assert_eq!(b.poll(), Some(vec![9]));
    }
}
