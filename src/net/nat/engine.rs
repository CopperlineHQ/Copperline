// SPDX-License-Identifier: GPL-3.0-or-later

//! The NAT engine: classifies guest frames, runs the smoltcp interface that
//! terminates ARP and TCP on the virtual gateway, and pumps the frame-level
//! responders (DHCP, DNS, UDP NAT, ICMP echo).
//!
//! The engine is a plain synchronous struct with no thread of its own --
//! `handle_guest_frame` ingests one frame, `step` advances everything once,
//! `pop_output` drains guest-bound frames -- so tests drive it directly and
//! the `a2065-nat` thread in the parent module is a thin pump around it.

use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use smoltcp::wire::{
    ArpOperation, ArpPacket, ArpRepr, EthernetAddress, EthernetFrame, EthernetProtocol,
    HardwareAddress, IpAddress, IpCidr, IpProtocol, Ipv4Packet, UdpPacket,
};
use std::collections::VecDeque;

use super::{dhcp, dns, frames, tcp, udp};
use super::{DNS_IP, GATEWAY_IP, GATEWAY_MAC, PREFIX_LEN};

/// A guest-bound Ethernet frame shorter than this is padded with zeros, as
/// the wire minimum (64 with FCS) guarantees to a real receiver.
const MIN_FRAME: usize = 60;

/// Largest UDP payload that fits one un-fragmented frame on the segment
/// (1500 MTU minus 20-byte IPv4 and 8-byte UDP headers). There is no IP
/// fragmentation, so a larger host reply is dropped rather than framed into
/// a length-inconsistent packet the guest would discard anyway.
const MAX_UDP_PAYLOAD: usize = 1500 - 20 - 8;

/// Cap on guest-bound frames staged ahead of the (bounded) output channel.
/// A guest that stops draining its RX ring while UDP keeps arriving would
/// otherwise grow this without limit; dropping the oldest matches a real
/// NIC dropping frames on a full ring.
const MAX_OUT_QUEUED: usize = 1024;

/// Zero-copy-ish frame pipe between the classifier and the smoltcp
/// interface: `rx` holds guest frames routed into the stack, `tx` collects
/// what the stack emits toward the guest.
#[derive(Default)]
pub struct PipeDevice {
    pub rx: VecDeque<Vec<u8>>,
    pub tx: VecDeque<Vec<u8>>,
}

pub struct PipeRxToken(Vec<u8>);

impl RxToken for PipeRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.0)
    }
}

pub struct PipeTxToken<'a>(&'a mut VecDeque<Vec<u8>>);

impl TxToken for PipeTxToken<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let r = f(&mut buf);
        self.0.push_back(buf);
        r
    }
}

impl Device for PipeDevice {
    type RxToken<'a> = PipeRxToken;
    type TxToken<'a> = PipeTxToken<'a>;

    fn receive(&mut self, _now: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let frame = self.rx.pop_front()?;
        Some((PipeRxToken(frame), PipeTxToken(&mut self.tx)))
    }

    fn transmit(&mut self, _now: Instant) -> Option<Self::TxToken<'_>> {
        Some(PipeTxToken(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps
    }
}

pub struct Engine {
    device: PipeDevice,
    iface: Interface,
    sockets: SocketSet<'static>,
    tcp: tcp::TcpNat,
    udp: udp::UdpNat,
    dns: dns::DnsResolver,
    /// Learned from guest traffic; broadcast until the first frame arrives.
    guest_mac: EthernetAddress,
    /// Guest-bound frames ready for the emulated NIC.
    out: VecDeque<Vec<u8>>,
    epoch: std::time::Instant,
}

impl Engine {
    pub fn new() -> Self {
        let mut device = PipeDevice::default();
        let mut config = Config::new(HardwareAddress::Ethernet(GATEWAY_MAC));
        // Fixed seed: the ISN sequence does not need host entropy, and a
        // stable engine helps when diffing NAT traces.
        config.random_seed = 0x0a00_0202;
        let mut iface = Interface::new(config, &mut device, Instant::ZERO);
        iface.update_ip_addrs(|addrs| {
            // The gateway owns both on-segment service addresses, so ARP for
            // either resolves to the gateway MAC.
            let _ = addrs.push(IpCidr::new(IpAddress::Ipv4(GATEWAY_IP), PREFIX_LEN));
            let _ = addrs.push(IpCidr::new(IpAddress::Ipv4(DNS_IP), 32));
        });
        // Accept IPv4 for arbitrary external destinations: outbound TCP flows
        // are terminated on whatever address the guest dialed. AnyIP only
        // accepts destinations covered by a route whose gateway is one of the
        // interface's own addresses, so a default route through the gateway
        // covers the whole IPv4 space.
        iface.set_any_ip(true);
        iface
            .routes_mut()
            .add_default_ipv4_route(GATEWAY_IP)
            .expect("fresh route table takes a default route");
        Self {
            device,
            iface,
            sockets: SocketSet::new(vec![]),
            tcp: tcp::TcpNat::default(),
            udp: udp::UdpNat::default(),
            dns: dns::DnsResolver::default(),
            guest_mac: EthernetAddress::BROADCAST,
            out: VecDeque::new(),
            epoch: std::time::Instant::now(),
        }
    }

    fn now(&self) -> Instant {
        Instant::from_micros(self.epoch.elapsed().as_micros() as i64)
    }

    /// Classify and ingest one frame transmitted by the guest.
    pub fn handle_guest_frame(&mut self, frame: &[u8]) {
        let Ok(eth) = EthernetFrame::new_checked(frame) else {
            return;
        };
        let src = eth.src_addr();
        if src.is_unicast() {
            self.guest_mac = src;
        }
        match eth.ethertype() {
            // ARP is the interface's business (it owns 10.0.2.2 and .3) --
            // but only for those two addresses. With any_ip enabled the
            // stack would answer ARP for ANY address, including the guest's
            // own duplicate-address probe at configuration time, which makes
            // vintage guest stacks log "duplicate IP address" and abort
            // their interface setup.
            EthernetProtocol::Arp => {
                let Ok(arp) = ArpPacket::new_checked(eth.payload()) else {
                    return;
                };
                let forward = match ArpRepr::parse(&arp) {
                    Ok(ArpRepr::EthernetIpv4 {
                        operation: ArpOperation::Request,
                        target_protocol_addr,
                        ..
                    }) => target_protocol_addr == GATEWAY_IP || target_protocol_addr == DNS_IP,
                    // Replies feed the neighbor cache.
                    Ok(_) => true,
                    Err(_) => false,
                };
                if forward {
                    self.device.rx.push_back(frame.to_vec());
                }
            }
            EthernetProtocol::Ipv4 => {
                let Ok(ip) = Ipv4Packet::new_checked(eth.payload()) else {
                    return;
                };
                match ip.next_header() {
                    IpProtocol::Udp => {
                        let Ok(udp) = UdpPacket::new_checked(ip.payload()) else {
                            return;
                        };
                        if udp.dst_port() == dhcp::SERVER_PORT {
                            if let Some(reply) = dhcp::handle_frame(frame) {
                                self.push_out(reply);
                            }
                        } else if ip.dst_addr() == DNS_IP && udp.dst_port() == dns::PORT {
                            self.dns.handle_query(udp.src_port(), udp.payload());
                        } else {
                            self.udp.handle_datagram(
                                udp.src_port(),
                                ip.dst_addr(),
                                udp.dst_port(),
                                udp.payload(),
                            );
                        }
                    }
                    IpProtocol::Icmp => {
                        if let Some(reply) = frames::icmp_echo_reply(frame) {
                            self.push_out(reply);
                        }
                    }
                    IpProtocol::Tcp => {
                        // A new SYN grows the flow table (and its listening
                        // socket) before the segment enters the stack.
                        log::trace!(
                            "nat engine: tcp segment {} -> {}",
                            ip.src_addr(),
                            ip.dst_addr()
                        );
                        self.tcp.maybe_open(&ip, &mut self.sockets);
                        self.device.rx.push_back(frame.to_vec());
                    }
                    _ => {}
                }
            }
            // No IPv6 on this segment.
            _ => {}
        }
    }

    /// Advance the stack and every host-socket pump once.
    pub fn step(&mut self) {
        let now = self.now();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        self.tcp.pump(&mut self.sockets);
        // Pumping moved bytes in and out of socket buffers; poll again so
        // segments and window updates leave in the same step.
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        for (guest_port, remote_ip, remote_port, payload) in self.udp.poll() {
            if payload.len() > MAX_UDP_PAYLOAD {
                // No IP fragmentation: an oversized reply is dropped whole.
                continue;
            }
            let f = frames::build_udp(
                self.guest_mac,
                remote_ip,
                remote_port,
                super::GUEST_IP,
                guest_port,
                &payload,
            );
            self.push_out(f);
        }
        for (guest_port, payload) in self.dns.poll_results() {
            let f = frames::build_udp(
                self.guest_mac,
                DNS_IP,
                dns::PORT,
                super::GUEST_IP,
                guest_port,
                &payload,
            );
            self.push_out(f);
        }
        while let Some(frame) = self.device.tx.pop_front() {
            self.push_out_unpadded(frame);
        }
    }

    pub fn pop_output(&mut self) -> Option<Vec<u8>> {
        self.out.pop_front()
    }

    fn push_out(&mut self, frame: Vec<u8>) {
        self.push_out_unpadded(frame);
    }

    fn push_out_unpadded(&mut self, mut frame: Vec<u8>) {
        // The Am7990 does not pad short frames on either side; a real wire
        // would never carry one under the Ethernet minimum, so pad here.
        if frame.len() < MIN_FRAME {
            frame.resize(MIN_FRAME, 0);
        }
        if self.out.len() >= MAX_OUT_QUEUED {
            self.out.pop_front();
        }
        self.out.push_back(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{frames, DNS_IP, GATEWAY_IP, GATEWAY_MAC, GUEST_IP};
    use super::*;
    use smoltcp::iface::SocketHandle;
    use smoltcp::phy::ChecksumCapabilities;
    use smoltcp::socket::tcp as tsock;
    use smoltcp::wire::{
        ArpOperation, ArpPacket, ArpRepr, Icmpv4Packet, Icmpv4Repr, Ipv4Address, Ipv4Repr,
    };
    use std::time::Duration;

    const GUEST_MAC: EthernetAddress = EthernetAddress([0x02, 0x00, 0x10, 0x00, 0x00, 0x01]);

    #[test]
    fn arp_for_the_gateway_is_answered() {
        let repr = ArpRepr::EthernetIpv4 {
            operation: ArpOperation::Request,
            source_hardware_addr: GUEST_MAC,
            source_protocol_addr: GUEST_IP,
            target_hardware_addr: EthernetAddress([0; 6]),
            target_protocol_addr: GATEWAY_IP,
        };
        let mut buf = vec![0u8; 14 + repr.buffer_len()];
        let mut eth = EthernetFrame::new_unchecked(&mut buf[..]);
        eth.set_src_addr(GUEST_MAC);
        eth.set_dst_addr(EthernetAddress::BROADCAST);
        eth.set_ethertype(EthernetProtocol::Arp);
        repr.emit(&mut ArpPacket::new_unchecked(eth.payload_mut()));

        let mut e = Engine::new();
        e.handle_guest_frame(&buf);
        e.step();
        let reply = e.pop_output().expect("ARP reply");
        let eth = EthernetFrame::new_checked(&reply[..]).unwrap();
        assert_eq!(eth.ethertype(), EthernetProtocol::Arp);
        let arp = ArpPacket::new_checked(eth.payload()).unwrap();
        match ArpRepr::parse(&arp).unwrap() {
            ArpRepr::EthernetIpv4 {
                operation: ArpOperation::Reply,
                source_hardware_addr,
                source_protocol_addr,
                ..
            } => {
                assert_eq!(source_hardware_addr, GATEWAY_MAC);
                assert_eq!(source_protocol_addr, GATEWAY_IP);
            }
            other => panic!("unexpected ARP: {other:?}"),
        }
    }

    #[test]
    fn arp_probe_for_the_guests_own_address_is_not_answered() {
        // A duplicate-address probe (ARP request for the guest's own IP)
        // must stay unanswered or the guest thinks its address is taken.
        let repr = ArpRepr::EthernetIpv4 {
            operation: ArpOperation::Request,
            source_hardware_addr: GUEST_MAC,
            source_protocol_addr: GUEST_IP,
            target_hardware_addr: EthernetAddress([0; 6]),
            target_protocol_addr: GUEST_IP,
        };
        let mut buf = vec![0u8; 14 + repr.buffer_len()];
        let mut eth = EthernetFrame::new_unchecked(&mut buf[..]);
        eth.set_src_addr(GUEST_MAC);
        eth.set_dst_addr(EthernetAddress::BROADCAST);
        eth.set_ethertype(EthernetProtocol::Arp);
        repr.emit(&mut ArpPacket::new_unchecked(eth.payload_mut()));

        let mut e = Engine::new();
        e.handle_guest_frame(&buf);
        e.step();
        assert!(e.pop_output().is_none(), "probe must go unanswered");
    }

    #[test]
    fn icmp_echo_to_an_external_address_is_answered_locally() {
        let checksum = ChecksumCapabilities::default();
        let external = Ipv4Address::new(93, 184, 216, 34);
        let echo = Icmpv4Repr::EchoRequest {
            ident: 7,
            seq_no: 3,
            data: b"ping",
        };
        let mut buf = vec![0u8; 14 + 20 + echo.buffer_len()];
        let mut eth = EthernetFrame::new_unchecked(&mut buf[..]);
        eth.set_src_addr(GUEST_MAC);
        eth.set_dst_addr(GATEWAY_MAC);
        eth.set_ethertype(EthernetProtocol::Ipv4);
        let ip_repr = Ipv4Repr {
            src_addr: GUEST_IP,
            dst_addr: external,
            next_header: IpProtocol::Icmp,
            payload_len: echo.buffer_len(),
            hop_limit: 64,
        };
        let mut ip = Ipv4Packet::new_unchecked(eth.payload_mut());
        ip_repr.emit(&mut ip, &checksum);
        echo.emit(
            &mut Icmpv4Packet::new_unchecked(ip.payload_mut()),
            &checksum,
        );

        let mut e = Engine::new();
        e.handle_guest_frame(&buf);
        e.step();
        let reply = e.pop_output().expect("echo reply");
        let eth = EthernetFrame::new_checked(&reply[..]).unwrap();
        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        assert_eq!(ip.src_addr(), external, "answered as the target");
        assert_eq!(ip.dst_addr(), GUEST_IP);
        let icmp = Icmpv4Packet::new_checked(ip.payload()).unwrap();
        match Icmpv4Repr::parse(&icmp, &checksum).unwrap() {
            Icmpv4Repr::EchoReply {
                ident,
                seq_no,
                data,
            } => {
                assert_eq!((ident, seq_no), (7, 3));
                assert_eq!(data, b"ping");
            }
            other => panic!("unexpected ICMP: {other:?}"),
        }
    }

    #[test]
    fn dns_query_through_the_engine_returns_an_a_record() {
        let query = crate::net::nat::dns::tests::build_query(0x77, "localhost", 1);
        let frame = frames::build_udp(GATEWAY_MAC, GUEST_IP, 3333, DNS_IP, dns::PORT, &query);
        let mut e = Engine::new();
        e.handle_guest_frame(&frame);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            e.step();
            if let Some(reply) = e.pop_output() {
                let eth = EthernetFrame::new_checked(&reply[..]).unwrap();
                let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
                assert_eq!(ip.src_addr(), DNS_IP);
                let udp = UdpPacket::new_checked(ip.payload()).unwrap();
                assert_eq!(udp.src_port(), dns::PORT);
                assert_eq!(udp.dst_port(), 3333);
                let p = udp.payload();
                assert_eq!(&p[p.len() - 4..], &[127, 0, 0, 1]);
                return;
            }
            assert!(std::time::Instant::now() < deadline, "no DNS reply");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn dhcp_discover_through_the_engine_is_offered() {
        // Classifier wiring only; the responder itself is tested in dhcp.rs.
        let mut p = vec![0u8; 236];
        p[0] = 1;
        p[1] = 1;
        p[2] = 6;
        p[28..34].copy_from_slice(&GUEST_MAC.0);
        p.extend_from_slice(&[0x63, 0x82, 0x53, 0x63, 53, 1, 1, 255]);
        let frame = frames::build_udp(
            EthernetAddress::BROADCAST,
            Ipv4Address::UNSPECIFIED,
            68,
            Ipv4Address::BROADCAST,
            dhcp::SERVER_PORT,
            &p,
        );
        let mut e = Engine::new();
        e.handle_guest_frame(&frame);
        e.step();
        let reply = e.pop_output().expect("DHCP offer");
        let eth = EthernetFrame::new_checked(&reply[..]).unwrap();
        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        let udp = UdpPacket::new_checked(ip.payload()).unwrap();
        assert_eq!(udp.dst_port(), 68);
        assert_eq!(&udp.payload()[16..20], &GUEST_IP.octets());
    }

    /// A second smoltcp stack standing in for the guest machine, so the TCP
    /// splice is exercised with a real TCP state machine on both ends.
    struct GuestStack {
        device: PipeDevice,
        iface: Interface,
        sockets: SocketSet<'static>,
        handle: SocketHandle,
        epoch: std::time::Instant,
    }

    impl GuestStack {
        fn new() -> Self {
            let mut device = PipeDevice::default();
            let mut config = Config::new(HardwareAddress::Ethernet(GUEST_MAC));
            config.random_seed = 42;
            let mut iface = Interface::new(config, &mut device, Instant::ZERO);
            iface.update_ip_addrs(|addrs| {
                let _ = addrs.push(IpCidr::new(IpAddress::Ipv4(GUEST_IP), PREFIX_LEN));
            });
            iface
                .routes_mut()
                .add_default_ipv4_route(GATEWAY_IP)
                .unwrap();
            let mut sockets = SocketSet::new(vec![]);
            let sock = tsock::Socket::new(
                tsock::SocketBuffer::new(vec![0u8; 16384]),
                tsock::SocketBuffer::new(vec![0u8; 16384]),
            );
            let handle = sockets.add(sock);
            Self {
                device,
                iface,
                sockets,
                handle,
                epoch: std::time::Instant::now(),
            }
        }

        fn sock(&mut self) -> &mut tsock::Socket<'static> {
            self.sockets.get_mut::<tsock::Socket>(self.handle)
        }

        /// One full frame exchange in both directions with the engine.
        fn exchange(&mut self, engine: &mut Engine) {
            let now = Instant::from_micros(self.epoch.elapsed().as_micros() as i64);
            self.iface.poll(now, &mut self.device, &mut self.sockets);
            while let Some(f) = self.device.tx.pop_front() {
                engine.handle_guest_frame(&f);
            }
            engine.step();
            while let Some(f) = engine.pop_output() {
                self.device.rx.push_back(f);
            }
            self.iface.poll(now, &mut self.device, &mut self.sockets);
        }
    }

    #[test]
    fn tcp_flow_splices_to_a_localhost_server() {
        // The gateway maps to the host loopback, so a local server stands in
        // for the internet and the test needs no network access.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut s, _) = listener.accept().unwrap();
            s.write_all(b"hello from host").unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 64];
            while !got.ends_with(b"hi") {
                let n = s.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
            }
            got
        });

        let mut engine = Engine::new();
        let mut guest = GuestStack::new();
        {
            let GuestStack {
                iface,
                sockets,
                handle,
                ..
            } = &mut guest;
            let cx = iface.context();
            sockets
                .get_mut::<tsock::Socket>(*handle)
                .connect(cx, (IpAddress::Ipv4(GATEWAY_IP), port), 49152)
                .unwrap();
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut received = Vec::new();
        let mut sent_reply = false;
        loop {
            guest.exchange(&mut engine);
            let sock = guest.sock();
            if sock.can_recv() {
                sock.recv(|d| {
                    received.extend_from_slice(d);
                    (d.len(), ())
                })
                .unwrap();
            }
            if !sent_reply && received == b"hello from host" && sock.can_send() {
                sock.send_slice(b"hi").unwrap();
                sent_reply = true;
            }
            if sent_reply && sock.send_queue() == 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "splice stalled: received {received:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        // A few extra exchanges flush the last ACKs, then close cleanly.
        guest.sock().close();
        for _ in 0..20 {
            guest.exchange(&mut engine);
            std::thread::sleep(Duration::from_millis(2));
        }
        let got = server.join().unwrap();
        assert_eq!(got, b"hi");
        assert_eq!(received, b"hello from host");
    }
}
