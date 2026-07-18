// SPDX-License-Identifier: GPL-3.0-or-later

//! Hand-built Ethernet/IPv4 frame helpers for the NAT's frame-level
//! protocols (DHCP, DNS, UDP NAT, ICMP echo). The stateful protocols (ARP,
//! TCP) go through the smoltcp interface instead and never come here.

use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{
    EthernetAddress, EthernetFrame, EthernetProtocol, Icmpv4Message, Icmpv4Packet, Icmpv4Repr,
    IpProtocol, Ipv4Address, Ipv4Packet, Ipv4Repr, UdpPacket, UdpRepr,
};

use super::GATEWAY_MAC;

const ETH_HDR: usize = 14;
const IPV4_HDR: usize = 20;
const UDP_HDR: usize = 8;

/// Build a complete Ethernet + IPv4 + UDP frame carrying `payload`.
pub fn build_udp(
    dst_mac: EthernetAddress,
    src_ip: Ipv4Address,
    src_port: u16,
    dst_ip: Ipv4Address,
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let checksum = ChecksumCapabilities::default();
    let mut buf = vec![0u8; ETH_HDR + IPV4_HDR + UDP_HDR + payload.len()];
    let mut eth = EthernetFrame::new_unchecked(&mut buf[..]);
    eth.set_src_addr(GATEWAY_MAC);
    eth.set_dst_addr(dst_mac);
    eth.set_ethertype(EthernetProtocol::Ipv4);
    let ip_repr = Ipv4Repr {
        src_addr: src_ip,
        dst_addr: dst_ip,
        next_header: IpProtocol::Udp,
        payload_len: UDP_HDR + payload.len(),
        hop_limit: 64,
    };
    let mut ip = Ipv4Packet::new_unchecked(eth.payload_mut());
    ip_repr.emit(&mut ip, &checksum);
    let udp_repr = UdpRepr { src_port, dst_port };
    let mut udp = UdpPacket::new_unchecked(ip.payload_mut());
    udp_repr.emit(
        &mut udp,
        &src_ip.into(),
        &dst_ip.into(),
        payload.len(),
        |b| b.copy_from_slice(payload),
        &checksum,
    );
    buf
}

/// Answer an ICMP echo request (to any destination) as the destination
/// itself. The gateway has no raw-socket path to really ping for the guest,
/// so like classic slirp a reply only proves the NAT is alive.
pub fn icmp_echo_reply(guest_frame: &[u8]) -> Option<Vec<u8>> {
    let checksum = ChecksumCapabilities::default();
    let eth = EthernetFrame::new_checked(guest_frame).ok()?;
    let ip = Ipv4Packet::new_checked(eth.payload()).ok()?;
    let icmp = Icmpv4Packet::new_checked(ip.payload()).ok()?;
    if icmp.msg_type() != Icmpv4Message::EchoRequest {
        return None;
    }
    let repr = Icmpv4Repr::parse(&icmp, &checksum).ok()?;
    let Icmpv4Repr::EchoRequest {
        ident,
        seq_no,
        data,
    } = repr
    else {
        return None;
    };
    let reply = Icmpv4Repr::EchoReply {
        ident,
        seq_no,
        data,
    };
    let mut buf = vec![0u8; ETH_HDR + IPV4_HDR + reply.buffer_len()];
    let mut reth = EthernetFrame::new_unchecked(&mut buf[..]);
    reth.set_src_addr(GATEWAY_MAC);
    reth.set_dst_addr(eth.src_addr());
    reth.set_ethertype(EthernetProtocol::Ipv4);
    let ip_repr = Ipv4Repr {
        src_addr: ip.dst_addr(),
        dst_addr: ip.src_addr(),
        next_header: IpProtocol::Icmp,
        payload_len: reply.buffer_len(),
        hop_limit: 64,
    };
    let mut rip = Ipv4Packet::new_unchecked(reth.payload_mut());
    ip_repr.emit(&mut rip, &checksum);
    let mut ricmp = Icmpv4Packet::new_unchecked(rip.payload_mut());
    reply.emit(&mut ricmp, &checksum);
    Some(buf)
}

/// Map a virtual destination to the host address the NAT actually dials:
/// the gateway itself stands in for the host's loopback.
pub fn map_host_ip(dst: Ipv4Address) -> std::net::IpAddr {
    if dst == super::GATEWAY_IP {
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    } else {
        std::net::IpAddr::V4(dst)
    }
}
