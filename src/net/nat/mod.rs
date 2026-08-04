// SPDX-License-Identifier: GPL-3.0-or-later

//! Userspace NAT backend: a slirp-style virtual gateway.
//!
//! The guest machine sees an ordinary Ethernet segment with a gateway and a
//! DNS server on it (the QEMU/slirp convention):
//!
//! - network `10.0.2.0/24`
//! - guest address `10.0.2.15` (static or via the built-in BOOTP/DHCP server)
//! - gateway / NAT router `10.0.2.2` (TCP/UDP to it are mapped to the host's
//!   `127.0.0.1`, so the guest can reach host-local services)
//! - DNS forwarder `10.0.2.3` (resolved through the host's own resolver)
//!
//! Outbound IPv4 is NATed onto ordinary host sockets, so no host privileges,
//! drivers, or per-OS configuration are needed and behavior is identical on
//! Linux, macOS, and Windows. There is no inbound path (no host port
//! forwards yet) and no IPv6; ICMP echo is answered locally by the gateway
//! for any destination, which proves the NAT is up, not that the target is
//! reachable (raw sockets would need privileges, as in classic slirp).
//!
//! Threading: one `a2065-nat` thread owns the whole engine -- the smoltcp
//! interface that terminates ARP and the guest's TCP, the UDP flow table,
//! the DHCP/DNS responders, and every host socket. Frames cross to and from
//! the emulated NIC over bounded channels that drop on overflow, so a
//! stalled host can never block the emulator thread (a dropped frame is
//! ordinary Ethernet loss; the guest's protocols retransmit). See
//! [`crate::net`] for the determinism contract: NAT traffic is host-paced
//! and breaks byte-identical replay while it flows, and a save state brings
//! the backend up fresh (flows die, guest TCP retries).

mod dhcp;
// pub(crate): src/wasmboard.rs reuses resolve_a for the WASM plugin ABI's
// `resolve` capability (host-OS-resolver lookups for a plugin, not just
// this NAT engine's own DNS forwarding).
pub(crate) mod dns;
mod engine;
mod frames;
mod tcp;
mod udp;

use crate::net::NetBackend;
use smoltcp::wire::{EthernetAddress, Ipv4Address};
use std::sync::mpsc;
use std::time::Duration;

/// The guest's address on the virtual segment.
pub const GUEST_IP: Ipv4Address = Ipv4Address::new(10, 0, 2, 15);
/// The virtual gateway / NAT router.
pub const GATEWAY_IP: Ipv4Address = Ipv4Address::new(10, 0, 2, 2);
/// The virtual DNS forwarder.
pub const DNS_IP: Ipv4Address = Ipv4Address::new(10, 0, 2, 3);
/// The segment's directed broadcast (guest datagrams here are not NATed).
pub const SEGMENT_BROADCAST: Ipv4Address = Ipv4Address::new(10, 0, 2, 255);
/// Prefix length of the virtual segment (255.255.255.0).
pub const PREFIX_LEN: u8 = 24;
/// The gateway's MAC (the slirp convention: 52:55 then the gateway IP).
pub const GATEWAY_MAC: EthernetAddress = EthernetAddress([0x52, 0x55, 0x0A, 0x00, 0x02, 0x02]);

/// Frames the emulator can queue toward the engine before drops (TX loss).
const TO_ENGINE_CAPACITY: usize = 256;
/// Frames the engine can queue toward the guest before drops (RX loss).
const TO_GUEST_CAPACITY: usize = 512;

/// The emulator-side handle: hands guest frames to the NAT thread and pulls
/// guest-bound frames back. Both directions are non-blocking.
pub struct NatBackend {
    to_engine: Option<mpsc::SyncSender<Vec<u8>>>,
    from_engine: mpsc::Receiver<Vec<u8>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NatBackend {
    pub fn new() -> Self {
        let (in_tx, in_rx) = mpsc::sync_channel(TO_ENGINE_CAPACITY);
        let (out_tx, out_rx) = mpsc::sync_channel(TO_GUEST_CAPACITY);
        let thread = std::thread::Builder::new()
            .name("a2065-nat".into())
            .spawn(move || run_engine(in_rx, out_tx))
            .map_err(|e| log::warn!("NAT thread failed to start: {e}; NIC left isolated"))
            .ok();
        // With no engine thread the receiver is already dropped, so keep no
        // sender either: `send()` then skips the per-frame allocation and the
        // board behaves like a cleanly isolated NIC.
        let to_engine = thread.is_some().then_some(in_tx);
        Self {
            to_engine,
            from_engine: out_rx,
            thread,
        }
    }
}

impl Default for NatBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NetBackend for NatBackend {
    fn send(&mut self, frame: &[u8]) {
        if let Some(tx) = &self.to_engine {
            // Full queue = TX loss, never a stall on the emulator thread.
            let _ = tx.try_send(frame.to_vec());
        }
    }

    fn poll(&mut self) -> Option<Vec<u8>> {
        self.from_engine.try_recv().ok()
    }
}

impl Drop for NatBackend {
    fn drop(&mut self) {
        // Dropping the sender wakes the engine loop (Disconnected) within one
        // recv timeout; join so its sockets and workers wind down with it.
        self.to_engine = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The NAT thread body: ingest guest frames, step the engine, ship output.
fn run_engine(in_rx: mpsc::Receiver<Vec<u8>>, out_tx: mpsc::SyncSender<Vec<u8>>) {
    let mut engine = engine::Engine::new();
    loop {
        // The 1 ms receive timeout doubles as the engine tick: host sockets
        // are all non-blocking and get pumped once per iteration.
        match in_rx.recv_timeout(Duration::from_millis(1)) {
            Ok(frame) => {
                engine.handle_guest_frame(&frame);
                while let Ok(frame) = in_rx.try_recv() {
                    engine.handle_guest_frame(&frame);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        engine.step();
        while let Some(frame) = engine.pop_output() {
            if out_tx.try_send(frame).is_err() {
                // Guest-bound queue full (RX loss) or the backend is gone;
                // drop the remainder of this batch either way.
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_starts_and_shuts_down() {
        let mut b = NatBackend::new();
        b.send(&[0u8; 60]); // malformed junk must be tolerated
        assert!(b.poll().is_none() || b.poll().is_none());
        drop(b); // must not hang on join
    }
}
