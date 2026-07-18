// SPDX-License-Identifier: GPL-3.0-or-later

//! BOOTP/DHCP responder for the virtual segment. Answers both real DHCP
//! (DISCOVER -> OFFER, REQUEST/INFORM -> ACK) and plain BOOTP requests --
//! AmiTCP-era guest stacks often speak the older protocol -- always handing
//! out the one fixed lease: guest 10.0.2.15/24, router 10.0.2.2, DNS
//! 10.0.2.3. Pure frame-in/frame-out, no state.

use smoltcp::wire::{EthernetAddress, EthernetFrame, Ipv4Address, Ipv4Packet, UdpPacket};

use super::{frames, DNS_IP, GATEWAY_IP, GUEST_IP};

pub const SERVER_PORT: u16 = 67;
const CLIENT_PORT: u16 = 68;
/// RFC 1048 vendor-extension / DHCP option magic. The same cookie covers
/// plain BOOTP replies, whose clients read the same option format.
const OPTION_MAGIC: [u8; 4] = [0x63, 0x82, 0x53, 0x63];
const BOOTP_HDR: usize = 236;

/// Handle one guest frame already known to be UDP to port 67. Returns the
/// complete reply frame, if the request deserves one.
pub fn handle_frame(frame: &[u8]) -> Option<Vec<u8>> {
    let eth = EthernetFrame::new_checked(frame).ok()?;
    let ip = Ipv4Packet::new_checked(eth.payload()).ok()?;
    let udp = UdpPacket::new_checked(ip.payload()).ok()?;
    let p = udp.payload();
    if p.len() < BOOTP_HDR {
        return None;
    }
    // BOOTREQUEST over Ethernet with a 6-byte hardware address.
    if p[0] != 1 || p[1] != 1 || p[2] != 6 {
        return None;
    }
    let xid: [u8; 4] = p[4..8].try_into().ok()?;
    let flags = u16::from_be_bytes([p[10], p[11]]);
    let chaddr: [u8; 6] = p[28..34].try_into().ok()?;
    let msg_type = if p.len() > BOOTP_HDR + 4 && p[BOOTP_HDR..BOOTP_HDR + 4] == OPTION_MAGIC {
        find_option(&p[BOOTP_HDR + 4..], 53).and_then(|v| v.first().copied())
    } else {
        None
    };
    let reply_type = match msg_type {
        None => None,                 // plain BOOTP: a BOOTREPLY, no option 53
        Some(1) => Some(2u8),         // DISCOVER -> OFFER
        Some(3) | Some(8) => Some(5), // REQUEST / INFORM -> ACK
        Some(_) => return None,       // DECLINE, RELEASE, ...: nothing to say
    };

    let mut b = vec![0u8; BOOTP_HDR];
    b[0] = 2; // BOOTREPLY
    b[1] = 1;
    b[2] = 6;
    b[4..8].copy_from_slice(&xid);
    b[10..12].copy_from_slice(&flags.to_be_bytes());
    b[16..20].copy_from_slice(&GUEST_IP.octets()); // yiaddr: the one lease
    b[20..24].copy_from_slice(&GATEWAY_IP.octets()); // siaddr
    b[28..34].copy_from_slice(&chaddr);
    b.extend_from_slice(&OPTION_MAGIC);
    if let Some(t) = reply_type {
        b.extend_from_slice(&[53, 1, t]);
        b.extend_from_slice(&[54, 4]); // server identifier
        b.extend_from_slice(&GATEWAY_IP.octets());
        b.extend_from_slice(&[51, 4]); // lease time
        b.extend_from_slice(&86_400u32.to_be_bytes());
    }
    b.extend_from_slice(&[1, 4, 255, 255, 255, 0]); // subnet mask
    b.extend_from_slice(&[3, 4]); // router
    b.extend_from_slice(&GATEWAY_IP.octets());
    b.extend_from_slice(&[6, 4]); // DNS server
    b.extend_from_slice(&DNS_IP.octets());
    b.push(255);

    // Unicast to the offered address unless the client set the broadcast
    // flag (it does not have an IP to receive unicast on yet).
    let broadcast = flags & 0x8000 != 0;
    let dst_mac = if broadcast {
        EthernetAddress::BROADCAST
    } else {
        EthernetAddress(chaddr)
    };
    let dst_ip = if broadcast {
        Ipv4Address::BROADCAST
    } else {
        GUEST_IP
    };
    Some(frames::build_udp(
        dst_mac,
        GATEWAY_IP,
        SERVER_PORT,
        dst_ip,
        CLIENT_PORT,
        &b,
    ))
}

fn find_option(mut opts: &[u8], code: u8) -> Option<&[u8]> {
    loop {
        match *opts.first()? {
            0 => opts = &opts[1..], // pad
            255 => return None,     // end
            c => {
                let len = *opts.get(1)? as usize;
                let val = opts.get(2..2 + len)?;
                if c == code {
                    return Some(val);
                }
                opts = &opts[2 + len..];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(msg_type: Option<u8>, flags: u16) -> Vec<u8> {
        let mut p = vec![0u8; BOOTP_HDR];
        p[0] = 1; // BOOTREQUEST
        p[1] = 1;
        p[2] = 6;
        p[4..8].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        p[10..12].copy_from_slice(&flags.to_be_bytes());
        p[28..34].copy_from_slice(&[2, 0, 0x10, 0, 0, 1]);
        if let Some(t) = msg_type {
            p.extend_from_slice(&OPTION_MAGIC);
            p.extend_from_slice(&[53, 1, t, 255]);
        }
        frames::build_udp(
            EthernetAddress::BROADCAST,
            Ipv4Address::UNSPECIFIED,
            CLIENT_PORT,
            Ipv4Address::BROADCAST,
            SERVER_PORT,
            &p,
        )
    }

    fn reply_payload(frame: &[u8]) -> Vec<u8> {
        let eth = EthernetFrame::new_checked(frame).unwrap();
        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        let udp = UdpPacket::new_checked(ip.payload()).unwrap();
        udp.payload().to_vec()
    }

    #[test]
    fn discover_gets_offer_with_the_fixed_lease() {
        let reply = handle_frame(&request(Some(1), 0)).expect("offer");
        let p = reply_payload(&reply);
        assert_eq!(p[0], 2, "BOOTREPLY");
        assert_eq!(&p[4..8], &[0xDE, 0xAD, 0xBE, 0xEF], "xid echoed");
        assert_eq!(&p[16..20], &GUEST_IP.octets(), "yiaddr");
        let opts = &p[BOOTP_HDR + 4..];
        assert_eq!(find_option(opts, 53), Some(&[2u8][..]), "OFFER");
        assert_eq!(find_option(opts, 3), Some(&GATEWAY_IP.octets()[..]));
        assert_eq!(find_option(opts, 6), Some(&DNS_IP.octets()[..]));
        assert_eq!(find_option(opts, 1), Some(&[255, 255, 255, 0][..]));
    }

    #[test]
    fn request_gets_ack_and_bootp_gets_plain_reply() {
        let ack = reply_payload(&handle_frame(&request(Some(3), 0)).unwrap());
        assert_eq!(
            find_option(&ack[BOOTP_HDR + 4..], 53),
            Some(&[5u8][..]),
            "ACK"
        );
        let bootp = reply_payload(&handle_frame(&request(None, 0)).unwrap());
        assert_eq!(find_option(&bootp[BOOTP_HDR + 4..], 53), None);
        assert_eq!(&bootp[16..20], &GUEST_IP.octets());
    }

    #[test]
    fn broadcast_flag_addresses_the_reply_to_broadcast() {
        let reply = handle_frame(&request(Some(1), 0x8000)).unwrap();
        let eth = EthernetFrame::new_checked(&reply[..]).unwrap();
        assert_eq!(eth.dst_addr(), EthernetAddress::BROADCAST);
        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        assert_eq!(ip.dst_addr(), Ipv4Address::BROADCAST);
    }
}
