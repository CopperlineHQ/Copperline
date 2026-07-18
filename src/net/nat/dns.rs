// SPDX-License-Identifier: GPL-3.0-or-later

//! DNS forwarder at 10.0.2.3: answers the guest's A queries through the
//! host's own resolver (getaddrinfo via `ToSocketAddrs`), so whatever the
//! host uses -- resolv.conf, VPN, DoH -- just works, with no per-OS resolver
//! discovery. Synthesized answers hold a single A record and stay far under
//! 512 bytes, so the guest never falls back to DNS over TCP.
//!
//! getaddrinfo blocks, so each lookup runs on a short-lived worker thread
//! (capped); results come back over a channel and are turned into reply
//! frames by the engine.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

pub const PORT: u16 = 53;
/// Lookups in flight before the forwarder answers SERVFAIL (the guest
/// resolver retries).
const MAX_OUTSTANDING: usize = 8;
const TTL_SECS: u32 = 60;

const QTYPE_A: u16 = 1;
const QTYPE_PTR: u16 = 12;
#[cfg(test)]
const QTYPE_AAAA: u16 = 28;

const RCODE_OK: u8 = 0;
const RCODE_SERVFAIL: u8 = 2;
const RCODE_NXDOMAIN: u8 = 3;

/// Reverse names for the virtual segment's own addresses. Vintage BSD
/// resolvers treat NOTIMP/SERVFAIL as "this server is unusable" and stop
/// querying it altogether (AmiTCP's kernel resolver caches that verdict),
/// so every PTR gets a well-formed answer: a name for our addresses,
/// NXDOMAIN for everything else, never a server error.
const PTR_NAMES: [(&str, &str); 3] = [
    ("15.2.0.10.in-addr.arpa", "amiga.local"),
    ("2.2.0.10.in-addr.arpa", "gateway.local"),
    ("3.2.0.10.in-addr.arpa", "dns.local"),
];

pub struct DnsResolver {
    results_tx: mpsc::Sender<(u16, Vec<u8>)>,
    results_rx: mpsc::Receiver<(u16, Vec<u8>)>,
    outstanding: Arc<AtomicUsize>,
}

impl Default for DnsResolver {
    fn default() -> Self {
        let (results_tx, results_rx) = mpsc::channel();
        Self {
            results_tx,
            results_rx,
            outstanding: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl DnsResolver {
    /// Ingest one query datagram from the guest (UDP payload only).
    /// Unparseable queries are dropped silently, like a dead server.
    pub fn handle_query(&mut self, guest_port: u16, payload: &[u8]) {
        let Some(q) = Query::parse(payload) else {
            log::debug!(
                "nat dns: unparseable query ({} bytes), dropped",
                payload.len()
            );
            return;
        };
        log::debug!(
            "nat dns: query id={} qtype={} name={:?}",
            q.id,
            q.qtype,
            q.name
        );
        match q.qtype {
            QTYPE_A => {
                if self.outstanding.load(Ordering::Relaxed) >= MAX_OUTSTANDING {
                    let _ = self
                        .results_tx
                        .send((guest_port, q.response(None, RCODE_SERVFAIL)));
                    return;
                }
                self.outstanding.fetch_add(1, Ordering::Relaxed);
                let tx = self.results_tx.clone();
                let outstanding = Arc::clone(&self.outstanding);
                // Built before the closure consumes `q`, so the spawn-failure
                // path can still answer (a fast SERVFAIL beats a DNS timeout).
                let servfail = q.response(None, RCODE_SERVFAIL);
                let spawned = std::thread::Builder::new()
                    .name("a2065-nat-dns".into())
                    .spawn(move || {
                        let addr = resolve_a(&q.name);
                        let rcode = if addr.is_some() {
                            RCODE_OK
                        } else {
                            RCODE_NXDOMAIN
                        };
                        log::debug!(
                            "nat dns: answer name={:?} addr={addr:?} rcode={rcode}",
                            q.name
                        );
                        let answer = addr.map(|a| (QTYPE_A, a.octets().to_vec()));
                        let _ = tx.send((guest_port, q.response(answer, rcode)));
                        outstanding.fetch_sub(1, Ordering::Relaxed);
                    });
                if spawned.is_err() {
                    self.outstanding.fetch_sub(1, Ordering::Relaxed);
                    let _ = self.results_tx.send((guest_port, servfail));
                }
            }
            QTYPE_PTR => {
                let lname = q.name.to_ascii_lowercase();
                let hit = PTR_NAMES.iter().find(|(rev, _)| *rev == lname);
                let reply = match hit {
                    Some((_, host)) => q.response(Some((QTYPE_PTR, encode_name(host))), RCODE_OK),
                    None => q.response(None, RCODE_NXDOMAIN),
                };
                let _ = self.results_tx.send((guest_port, reply));
            }
            // No IPv6 on the segment (empty NOERROR steers dual-stack
            // resolvers straight to the A record), and unknown types get
            // the same NODATA shape rather than a server error.
            _ => {
                let _ = self
                    .results_tx
                    .send((guest_port, q.response(None, RCODE_OK)));
            }
        }
    }

    /// Completed responses as (guest source port, DNS message payload).
    pub fn poll_results(&mut self) -> Vec<(u16, Vec<u8>)> {
        let mut out = Vec::new();
        while let Ok(r) = self.results_rx.try_recv() {
            out.push(r);
        }
        out
    }
}

struct Query {
    id: u16,
    /// The raw question section, echoed verbatim into the response.
    question: Vec<u8>,
    name: String,
    qtype: u16,
}

impl Query {
    fn parse(p: &[u8]) -> Option<Self> {
        if p.len() < 12 {
            return None;
        }
        let id = u16::from_be_bytes([p[0], p[1]]);
        let flags = u16::from_be_bytes([p[2], p[3]]);
        if flags & 0x8000 != 0 {
            return None; // a response, not a query
        }
        let qdcount = u16::from_be_bytes([p[4], p[5]]);
        if qdcount == 0 {
            return None;
        }
        let mut name = String::new();
        let mut i = 12usize;
        loop {
            let len = *p.get(i)? as usize;
            if len == 0 {
                i += 1;
                break;
            }
            if len & 0xC0 != 0 {
                return None; // compressed qname: nobody sends this
            }
            let label = p.get(i + 1..i + 1 + len)?;
            if !name.is_empty() {
                name.push('.');
            }
            name.push_str(std::str::from_utf8(label).ok()?);
            if name.len() > 253 {
                return None;
            }
            i += 1 + len;
        }
        let qtype = u16::from_be_bytes([*p.get(i)?, *p.get(i + 1)?]);
        let question = p.get(12..i + 4)?.to_vec();
        Some(Self {
            id,
            question,
            name,
            qtype,
        })
    }

    /// Build the full response message for this query; `answer` is one
    /// record as (rr type, rdata) under the query name.
    fn response(&self, answer: Option<(u16, Vec<u8>)>, rcode: u8) -> Vec<u8> {
        let mut r = Vec::with_capacity(12 + self.question.len() + 16);
        r.extend_from_slice(&self.id.to_be_bytes());
        // QR + RD + RA, plus the rcode.
        r.extend_from_slice(&(0x8180u16 | u16::from(rcode)).to_be_bytes());
        r.extend_from_slice(&1u16.to_be_bytes()); // qdcount
        r.extend_from_slice(&u16::from(answer.is_some()).to_be_bytes()); // ancount
        r.extend_from_slice(&0u16.to_be_bytes()); // nscount
        r.extend_from_slice(&0u16.to_be_bytes()); // arcount
        r.extend_from_slice(&self.question);
        if let Some((rrtype, rdata)) = answer {
            r.extend_from_slice(&[0xC0, 0x0C]); // pointer to the qname
            r.extend_from_slice(&rrtype.to_be_bytes());
            r.extend_from_slice(&1u16.to_be_bytes()); // class IN
            r.extend_from_slice(&TTL_SECS.to_be_bytes());
            r.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
            r.extend_from_slice(&rdata);
        }
        r
    }
}

/// DNS wire encoding of a dotted name (labels + terminator).
fn encode_name(name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 2);
    for label in name.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}

fn resolve_a(name: &str) -> Option<std::net::Ipv4Addr> {
    // Only hostname-shaped strings reach getaddrinfo.
    let ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_');
    if !ok {
        return None;
    }
    use std::net::ToSocketAddrs;
    (name, 80u16)
        .to_socket_addrs()
        .ok()?
        .find_map(|sa| match sa {
            std::net::SocketAddr::V4(v4) => Some(*v4.ip()),
            _ => None,
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn build_query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
        let mut q = Vec::new();
        q.extend_from_slice(&id.to_be_bytes());
        q.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
        q.extend_from_slice(&1u16.to_be_bytes());
        q.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }

    fn wait_result(r: &mut DnsResolver) -> (u16, Vec<u8>) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(res) = r.poll_results().pop() {
                return res;
            }
            assert!(std::time::Instant::now() < deadline, "no DNS result");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn a_query_for_localhost_resolves_offline() {
        let mut r = DnsResolver::default();
        r.handle_query(4242, &build_query(7, "localhost", QTYPE_A));
        let (port, resp) = wait_result(&mut r);
        assert_eq!(port, 4242);
        assert_eq!(u16::from_be_bytes([resp[0], resp[1]]), 7, "id echoed");
        assert_eq!(resp[3] & 0x0F, RCODE_OK);
        let ancount = u16::from_be_bytes([resp[6], resp[7]]);
        assert_eq!(ancount, 1);
        assert_eq!(&resp[resp.len() - 4..], &[127, 0, 0, 1], "A 127.0.0.1");
    }

    #[test]
    #[ignore = "needs internet: resolves a real external name"]
    fn a_query_for_an_external_name_resolves() {
        let mut r = DnsResolver::default();
        r.handle_query(9999, &build_query(21, "frogfind.com", QTYPE_A));
        let (_, resp) = wait_result(&mut r);
        assert_eq!(resp[3] & 0x0F, RCODE_OK, "rcode: {:02x?}", &resp[..12]);
        assert_eq!(u16::from_be_bytes([resp[6], resp[7]]), 1, "one A answer");
    }

    #[test]
    fn aaaa_and_unknown_types_are_answered_nodata() {
        let mut r = DnsResolver::default();
        r.handle_query(1, &build_query(8, "localhost", QTYPE_AAAA));
        let (_, resp) = wait_result(&mut r);
        assert_eq!(resp[3] & 0x0F, RCODE_OK);
        assert_eq!(u16::from_be_bytes([resp[6], resp[7]]), 0, "no answers");

        // Never a server error: AmiTCP's resolver caches those as a dead
        // server and stops querying entirely.
        r.handle_query(2, &build_query(9, "localhost", 15 /* MX */));
        let (_, resp) = wait_result(&mut r);
        assert_eq!(resp[3] & 0x0F, RCODE_OK);
        assert_eq!(u16::from_be_bytes([resp[6], resp[7]]), 0, "no answers");
    }

    #[test]
    fn ptr_for_segment_addresses_answers_and_others_nxdomain() {
        let mut r = DnsResolver::default();
        r.handle_query(3, &build_query(10, "15.2.0.10.in-addr.arpa", QTYPE_PTR));
        let (_, resp) = wait_result(&mut r);
        assert_eq!(resp[3] & 0x0F, RCODE_OK);
        assert_eq!(u16::from_be_bytes([resp[6], resp[7]]), 1);
        let tail = &resp[resp.len() - encode_name("amiga.local").len()..];
        assert_eq!(tail, &encode_name("amiga.local")[..]);

        r.handle_query(
            4,
            &build_query(11, "255.255.255.255.in-addr.arpa", QTYPE_PTR),
        );
        let (_, resp) = wait_result(&mut r);
        assert_eq!(resp[3] & 0x0F, RCODE_NXDOMAIN);
        assert_eq!(u16::from_be_bytes([resp[6], resp[7]]), 0);
    }
}
