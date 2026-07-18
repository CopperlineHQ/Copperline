// SPDX-License-Identifier: GPL-3.0-or-later

//! UDP flow NAT: each (guest port, destination) pair gets one connected,
//! non-blocking host socket; replies are re-framed toward the guest by the
//! engine. Flows die after an idle timeout, like any home router's NAT.

use smoltcp::wire::Ipv4Address;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

use super::frames;

const MAX_FLOWS: usize = 64;
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Largest datagram accepted back from the host (fits any guest-side MTU
/// after fragmentation is ruled out; bigger replies are truncated by recv).
const RECV_BUF: usize = 2048;

struct UdpFlow {
    guest_port: u16,
    remote_ip: Ipv4Address,
    remote_port: u16,
    sock: UdpSocket,
    last_used: Instant,
}

#[derive(Default)]
pub struct UdpNat {
    flows: Vec<UdpFlow>,
}

impl UdpNat {
    /// Forward one guest datagram (payload of a UDP packet not claimed by
    /// the DHCP or DNS responders).
    pub fn handle_datagram(
        &mut self,
        guest_port: u16,
        dst_ip: Ipv4Address,
        dst_port: u16,
        payload: &[u8],
    ) {
        // No broadcast/multicast NAT (NetBIOS chatter and friends). The
        // segment's own directed broadcast is suppressed by address rather
        // than by "ends in .255", which would wrongly drop unicast to a real
        // host ending in .255 on a wider external subnet.
        if dst_ip.is_broadcast() || dst_ip.is_multicast() || dst_ip == super::SEGMENT_BROADCAST {
            return;
        }
        log::debug!(
            "nat udp: guest :{guest_port} -> {dst_ip}:{dst_port} ({} bytes)",
            payload.len()
        );
        let now = Instant::now();
        if let Some(flow) = self.flows.iter_mut().find(|f| {
            f.guest_port == guest_port && f.remote_ip == dst_ip && f.remote_port == dst_port
        }) {
            flow.last_used = now;
            let _ = flow.sock.send(payload);
            return;
        }
        self.flows
            .retain(|f| now.duration_since(f.last_used) < IDLE_TIMEOUT);
        if self.flows.len() >= MAX_FLOWS {
            return; // drop: the guest side sees ordinary UDP loss
        }
        let Ok(sock) = UdpSocket::bind(("0.0.0.0", 0)) else {
            return;
        };
        if sock
            .connect((frames::map_host_ip(dst_ip), dst_port))
            .is_err()
            || sock.set_nonblocking(true).is_err()
        {
            return;
        }
        let _ = sock.send(payload);
        self.flows.push(UdpFlow {
            guest_port,
            remote_ip: dst_ip,
            remote_port: dst_port,
            sock,
            last_used: now,
        });
    }

    /// Drain replies from every flow: (guest port, remote ip, remote port,
    /// payload), ready for re-framing toward the guest.
    #[allow(clippy::type_complexity)]
    pub fn poll(&mut self) -> Vec<(u16, Ipv4Address, u16, Vec<u8>)> {
        let now = Instant::now();
        let mut out = Vec::new();
        for flow in &mut self.flows {
            let mut buf = [0u8; RECV_BUF];
            while let Ok(n) = flow.sock.recv(&mut buf) {
                flow.last_used = now;
                out.push((
                    flow.guest_port,
                    flow.remote_ip,
                    flow.remote_port,
                    buf[..n].to_vec(),
                ));
            }
        }
        self.flows
            .retain(|f| now.duration_since(f.last_used) < IDLE_TIMEOUT);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datagram_reaches_localhost_and_reply_returns() {
        // The gateway address maps to the host's loopback, so a local echo
        // peer stands in for "the internet" without any network access.
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = peer.local_addr().unwrap().port();
        let mut nat = UdpNat::default();
        nat.handle_datagram(5000, super::super::GATEWAY_IP, port, b"marco");

        let mut buf = [0u8; 64];
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let (n, from) = peer.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"marco");
        peer.send_to(b"polo", from).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let replies = nat.poll();
            if let Some((gp, rip, rport, data)) = replies.into_iter().next() {
                assert_eq!(gp, 5000);
                assert_eq!(rip, super::super::GATEWAY_IP);
                assert_eq!(rport, port);
                assert_eq!(data, b"polo");
                break;
            }
            assert!(Instant::now() < deadline, "no UDP reply seen");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
