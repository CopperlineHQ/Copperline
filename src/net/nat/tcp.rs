// SPDX-License-Identifier: GPL-3.0-or-later

//! TCP flow NAT. The guest's TCP is terminated by smoltcp on the virtual
//! gateway: when a SYN to a new destination arrives, a listening socket is
//! created for exactly that (address, port) -- the interface accepts any
//! IPv4 destination -- and a host connection is dialed in parallel on a
//! short-lived worker (std has no non-blocking connect). Once both sides
//! are up, the flow is a plain byte splice; smoltcp's receive window gives
//! guest-side backpressure for free when the host socket stops accepting
//! writes.

use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::tcp as tsock;
use smoltcp::wire::{IpAddress, IpListenEndpoint, Ipv4Address, Ipv4Packet, TcpPacket};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::frames;

const MAX_FLOWS: usize = 128;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A flow whose host dial never reported back is reaped after this.
const CONNECT_REAP: Duration = Duration::from_secs(30);
const SOCKET_BUF: usize = 64 * 1024;

enum HostState {
    /// Worker thread still dialing; guest bytes pile up in the socket
    /// buffer, bounded by its size and then the receive window.
    Connecting,
    Up {
        stream: TcpStream,
        /// The guest side has left the handshake (reached ESTABLISHED or
        /// beyond). Until then `may_recv()` reads false the same as it does
        /// for a half-closed connection, so the guest-FIN half-close must
        /// wait for this before firing -- otherwise a host dial that beats
        /// the guest's handshake ACK (a loopback-mapped service) would shut
        /// the host write side down mid-handshake.
        established: bool,
        /// Host sent EOF; its FIN has been forwarded as `close()`.
        read_eof: bool,
        /// Guest sent FIN; forwarded as a write shutdown.
        wrote_shutdown: bool,
    },
    /// Host side failed or was torn down; the smoltcp socket got an abort
    /// (RST to the guest) or close and is draining out.
    Gone,
}

struct TcpFlow {
    id: u64,
    key: (u16, Ipv4Address, u16),
    handle: SocketHandle,
    host: HostState,
    opened: Instant,
}

pub struct TcpNat {
    flows: Vec<TcpFlow>,
    connect_tx: mpsc::Sender<(u64, std::io::Result<TcpStream>)>,
    connect_rx: mpsc::Receiver<(u64, std::io::Result<TcpStream>)>,
    next_id: u64,
}

impl Default for TcpNat {
    fn default() -> Self {
        let (connect_tx, connect_rx) = mpsc::channel();
        Self {
            flows: Vec::new(),
            connect_tx,
            connect_rx,
            next_id: 0,
        }
    }
}

impl TcpNat {
    /// Called for every guest TCP segment before it enters the stack: a SYN
    /// to a destination with no flow yet opens one.
    pub fn maybe_open(&mut self, ip: &Ipv4Packet<&[u8]>, sockets: &mut SocketSet<'static>) {
        let Ok(tcp) = TcpPacket::new_checked(ip.payload()) else {
            return;
        };
        if !tcp.syn() || tcp.ack() {
            return;
        }
        log::debug!(
            "nat tcp: SYN {}:{} -> {}:{}",
            ip.src_addr(),
            tcp.src_port(),
            ip.dst_addr(),
            tcp.dst_port()
        );
        let key = (tcp.src_port(), ip.dst_addr(), tcp.dst_port());
        if self.flows.iter().any(|f| f.key == key) {
            return; // SYN retransmit for a flow already opening
        }
        if self.flows.len() >= MAX_FLOWS {
            return; // dropped SYN: the guest gets a connect timeout
        }
        let mut sock = tsock::Socket::new(
            tsock::SocketBuffer::new(vec![0u8; SOCKET_BUF]),
            tsock::SocketBuffer::new(vec![0u8; SOCKET_BUF]),
        );
        let listen_on = IpListenEndpoint {
            addr: Some(IpAddress::Ipv4(ip.dst_addr())),
            port: tcp.dst_port(),
        };
        if sock.listen(listen_on).is_err() {
            log::debug!("nat tcp: listen failed for {listen_on}");
            return;
        }
        let handle = sockets.add(sock);
        let id = self.next_id;
        self.next_id += 1;
        let target = SocketAddr::new(frames::map_host_ip(ip.dst_addr()), tcp.dst_port());
        log::debug!("nat tcp: dialing {target} for flow {id}");
        let tx = self.connect_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("a2065-nat-dial".into())
            .spawn(move || {
                let r = TcpStream::connect_timeout(&target, CONNECT_TIMEOUT);
                let _ = tx.send((id, r));
            });
        if spawned.is_err() {
            sockets.remove(handle);
            return;
        }
        self.flows.push(TcpFlow {
            id,
            key,
            handle,
            host: HostState::Connecting,
            opened: Instant::now(),
        });
    }

    /// Move bytes between every flow's smoltcp socket and host socket.
    pub fn pump(&mut self, sockets: &mut SocketSet<'static>) {
        while let Ok((id, result)) = self.connect_rx.try_recv() {
            let Some(flow) = self.flows.iter_mut().find(|f| f.id == id) else {
                continue; // flow already reaped; a success is just dropped
            };
            match result {
                Ok(stream) => {
                    let ok = stream.set_nonblocking(true).is_ok();
                    let _ = stream.set_nodelay(true);
                    if ok {
                        flow.host = HostState::Up {
                            stream,
                            established: false,
                            read_eof: false,
                            wrote_shutdown: false,
                        };
                        continue;
                    }
                    sockets.get_mut::<tsock::Socket>(flow.handle).abort();
                    flow.host = HostState::Gone;
                }
                Err(e) => {
                    // Connection refused/unreachable: RST the guest.
                    log::debug!("nat tcp: dial for flow {id} failed: {e}");
                    sockets.get_mut::<tsock::Socket>(flow.handle).abort();
                    flow.host = HostState::Gone;
                }
            }
        }

        for flow in &mut self.flows {
            let sock = sockets.get_mut::<tsock::Socket>(flow.handle);
            let HostState::Up {
                stream,
                established,
                read_eof,
                wrote_shutdown,
            } = &mut flow.host
            else {
                continue;
            };
            // The guest has cleared the handshake once the socket is no
            // longer listening or half-open.
            if !*established
                && !matches!(
                    sock.state(),
                    tsock::State::Listen | tsock::State::SynReceived
                )
            {
                *established = true;
            }
            let mut broken = false;

            // Guest -> host. WouldBlock consumes nothing, so unsent bytes
            // stay in the socket buffer and close the guest's window.
            while !broken && sock.can_recv() {
                let mut moved = 0usize;
                let r = sock.recv(|data| match stream.write(data) {
                    Ok(n) => {
                        moved = n;
                        (n, false)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (0, false),
                    Err(_) => (0, true),
                });
                match r {
                    Ok(true) => broken = true,
                    Ok(false) if moved == 0 => break,
                    Ok(false) => {}
                    Err(_) => break,
                }
            }

            // Guest FIN, fully drained: half-close toward the host. Gated on
            // `established` so a not-yet-handshaked socket (may_recv() false)
            // is not mistaken for a half-closed one.
            if !broken && *established && !*wrote_shutdown && !sock.may_recv() && !sock.can_recv() {
                let _ = stream.shutdown(std::net::Shutdown::Write);
                *wrote_shutdown = true;
            }

            // Host -> guest, only as much as the send buffer can take so a
            // read never overruns `send_slice`.
            let mut tmp = [0u8; 4096];
            while !broken && !*read_eof && sock.can_send() {
                let room = sock.send_capacity() - sock.send_queue();
                if room == 0 {
                    break;
                }
                let want = room.min(tmp.len());
                match stream.read(&mut tmp[..want]) {
                    Ok(0) => {
                        *read_eof = true;
                        sock.close(); // forward the host's FIN
                    }
                    Ok(n) => {
                        let _ = sock.send_slice(&tmp[..n]);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => broken = true,
                }
            }

            if broken {
                sock.abort();
                flow.host = HostState::Gone;
            }
        }

        // Reap finished flows (socket fully closed), plus flows whose socket
        // never left the handshake within the reap window -- a guest SYN that
        // the interface dropped (e.g. a bad checksum) leaves a listening
        // socket that would otherwise sit forever and, after MAX_FLOWS of
        // them, wedge all TCP.
        self.flows.retain(|flow| {
            let state = sockets.get::<tsock::Socket>(flow.handle).state();
            let reap = state == tsock::State::Closed
                || (matches!(state, tsock::State::Listen | tsock::State::SynReceived)
                    && flow.opened.elapsed() > CONNECT_REAP);
            if reap {
                sockets.remove(flow.handle);
            }
            !reap
        });
    }
}
