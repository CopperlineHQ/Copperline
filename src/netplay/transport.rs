// SPDX-License-Identifier: GPL-3.0-or-later

//! Nonblocking packet adapters. Reliability belongs to the shared protocol.

use super::wire::MAX_PACKET;
use anyhow::{ensure, Result};
use std::collections::VecDeque;
#[cfg(not(target_arch = "wasm32"))]
use std::{
    collections::BTreeMap,
    net::{SocketAddr, UdpSocket},
    sync::{Arc, Mutex},
};

pub trait Transport {
    fn route(&self) -> &'static str {
        "direct"
    }
    /// Connection setup may continue off-thread while the cold machine waits.
    fn ready(&mut self) -> Result<bool> {
        Ok(true)
    }
    /// Read one complete packet without blocking. None means the queue is empty;
    /// a returned length must fit the supplied buffer. Some(0) means a packet
    /// was consumed and discarded (for example, a foreign UDP source).
    fn receive(&mut self, buffer: &mut [u8]) -> Result<Option<usize>>;
    /// False means the transport is temporarily unable to accept this packet.
    fn send(&mut self, packet: &[u8]) -> Result<bool>;
}

#[cfg(not(target_arch = "wasm32"))]
pub enum NativeTransport {
    Udp(UdpTransport),
    #[cfg(feature = "netplay-internet")]
    Internet(Box<super::internet::InternetTransport>),
}

#[cfg(not(target_arch = "wasm32"))]
impl Transport for NativeTransport {
    fn route(&self) -> &'static str {
        match self {
            Self::Udp(_) => "UDP",
            #[cfg(feature = "netplay-internet")]
            Self::Internet(t) => t.route(),
        }
    }
    fn ready(&mut self) -> Result<bool> {
        match self {
            Self::Udp(t) => t.ready(),
            #[cfg(feature = "netplay-internet")]
            Self::Internet(t) => t.ready(),
        }
    }
    fn receive(&mut self, buffer: &mut [u8]) -> Result<Option<usize>> {
        match self {
            Self::Udp(t) => t.receive(buffer),
            #[cfg(feature = "netplay-internet")]
            Self::Internet(t) => t.receive(buffer),
        }
    }
    fn send(&mut self, packet: &[u8]) -> Result<bool> {
        match self {
            Self::Udp(t) => t.send(packet),
            #[cfg(feature = "netplay-internet")]
            Self::Internet(t) => t.send(packet),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeTransport {
    pub(super) fn options(&self) -> super::ConnectionOptions {
        match self {
            Self::Udp(t) => t.options.clone(),
            #[cfg(feature = "netplay-internet")]
            Self::Internet(t) => t.connection_options(),
        }
    }

    /// Spectators that connected since the last call (hosts only).
    pub(super) fn take_spectators(&mut self) -> Vec<SpectatorTransport> {
        match self {
            Self::Udp(t) => t.take_spectators(),
            #[cfg(feature = "netplay-internet")]
            Self::Internet(t) => t
                .take_spectators()
                .into_iter()
                .map(SpectatorTransport::Internet)
                .collect(),
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
impl NativeTransport {
    pub(super) fn socket(&self) -> &std::net::UdpSocket {
        match self {
            Self::Udp(t) => &t.socket,
            #[cfg(feature = "netplay-internet")]
            _ => panic!("expected UDP transport"),
        }
    }
}

/// The host's side of one spectator link.
#[cfg(not(target_arch = "wasm32"))]
pub(super) enum SpectatorTransport {
    Udp(UdpSpectator),
    #[cfg(feature = "netplay-internet")]
    Internet(super::internet::SpectatorPeer),
}

#[cfg(not(target_arch = "wasm32"))]
impl Transport for SpectatorTransport {
    fn route(&self) -> &'static str {
        match self {
            Self::Udp(_) => "UDP",
            #[cfg(feature = "netplay-internet")]
            Self::Internet(t) => t.route(),
        }
    }
    fn ready(&mut self) -> Result<bool> {
        match self {
            Self::Udp(t) => t.ready(),
            #[cfg(feature = "netplay-internet")]
            Self::Internet(t) => t.ready(),
        }
    }
    fn receive(&mut self, buffer: &mut [u8]) -> Result<Option<usize>> {
        match self {
            Self::Udp(t) => t.receive(buffer),
            #[cfg(feature = "netplay-internet")]
            Self::Internet(t) => t.receive(buffer),
        }
    }
    fn send(&mut self, packet: &[u8]) -> Result<bool> {
        match self {
            Self::Udp(t) => t.send(packet),
            #[cfg(feature = "netplay-internet")]
            Self::Internet(t) => t.send(packet),
        }
    }
}

/// The browser feeds incoming data-channel packets and drains outgoing packets.
/// Both queues are bounded independently of how often JavaScript services them.
#[derive(Default)]
pub struct PacketQueue {
    incoming: VecDeque<Vec<u8>>,
    outgoing: VecDeque<Vec<u8>>,
}

impl PacketQueue {
    #[cfg(all(feature = "netplay-internet", not(target_arch = "wasm32")))]
    pub(super) fn has_incoming(&self) -> bool {
        !self.incoming.is_empty()
    }

    pub fn push(&mut self, packet: &[u8]) -> Result<()> {
        ensure!(packet.len() <= MAX_PACKET, "netplay packet is too large");
        if self.incoming.len() == 64 {
            self.incoming.pop_front();
        }
        self.incoming.push_back(packet.to_vec());
        Ok(())
    }

    pub fn pop(&mut self) -> Option<Vec<u8>> {
        self.outgoing.pop_front()
    }
}

impl Transport for PacketQueue {
    fn receive(&mut self, buffer: &mut [u8]) -> Result<Option<usize>> {
        let Some(packet) = self.incoming.pop_front() else {
            return Ok(None);
        };
        ensure!(
            packet.len() <= buffer.len(),
            "netplay receive buffer is too small"
        );
        buffer[..packet.len()].copy_from_slice(&packet);
        Ok(Some(packet.len()))
    }

    fn send(&mut self, packet: &[u8]) -> Result<bool> {
        ensure!(packet.len() <= MAX_PACKET, "netplay packet is too large");
        if self.outgoing.len() == 64 {
            return Ok(false);
        }
        self.outgoing.push_back(packet.to_vec());
        Ok(true)
    }
}

/// Packets from one spectator, demultiplexed by its source address.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
pub(super) struct Slot {
    incoming: VecDeque<Vec<u8>>,
}

/// Spectator sources sharing the host's socket. A source claims a slot with
/// its first control packet for the session; a dropped link frees it.
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct SlotTable {
    slots: BTreeMap<SocketAddr, Arc<Mutex<Slot>>>,
    pending: Vec<(SocketAddr, Arc<Mutex<Slot>>)>,
    cap: usize,
}

#[cfg(not(target_arch = "wasm32"))]
impl SlotTable {
    fn accept(&mut self, source: SocketAddr, bytes: &[u8]) {
        let slot = match self.slots.get(&source) {
            Some(slot) => slot.clone(),
            None => {
                if self.slots.len() >= self.cap {
                    return;
                }
                let slot = Arc::new(Mutex::new(Slot::default()));
                self.slots.insert(source, slot.clone());
                self.pending.push((source, slot.clone()));
                slot
            }
        };
        let mut slot = slot.lock().unwrap();
        if slot.incoming.len() == 64 {
            slot.incoming.pop_front();
        }
        slot.incoming.push_back(bytes.to_vec());
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub struct UdpTransport {
    pub(super) socket: UdpSocket,
    pub(super) options: super::ConnectionOptions,
    peer: SocketAddr,
    session: [u8; 16],
    spectators: Option<Arc<Mutex<SlotTable>>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl UdpTransport {
    pub(super) fn new(options: super::Options) -> Result<Self> {
        let spectators = usize::from(options.spectators);
        let transport = Self::bind(
            options.bind,
            options.peer,
            options.session,
            spectators,
            super::ConnectionOptions::Direct(options.clone()),
        )?;
        log::info!(
            "netplay: listening on {}, peer {}, player {}; waiting for matching machine",
            transport.socket.local_addr()?,
            options.peer,
            options.player + 1
        );
        Ok(transport)
    }

    pub(super) fn watch(options: super::WatchOptions) -> Result<Self> {
        let transport = Self::bind(
            options.bind,
            options.host,
            options.session,
            0,
            super::ConnectionOptions::Watch(options.clone()),
        )?;
        log::info!(
            "netplay: listening on {}, host {}; spectating",
            transport.socket.local_addr()?,
            options.host
        );
        Ok(transport)
    }

    fn bind(
        bind: SocketAddr,
        peer: SocketAddr,
        session: [u8; 16],
        spectators: usize,
        options: super::ConnectionOptions,
    ) -> Result<Self> {
        use anyhow::Context;
        let socket = UdpSocket::bind(bind).context("binding netplay UDP socket")?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            options,
            peer,
            session,
            spectators: (spectators > 0).then(|| {
                Arc::new(Mutex::new(SlotTable {
                    slots: BTreeMap::new(),
                    pending: Vec::new(),
                    cap: spectators,
                }))
            }),
        })
    }

    pub(super) fn take_spectators(&mut self) -> Vec<SpectatorTransport> {
        let Some(table) = &self.spectators else {
            return Vec::new();
        };
        let pending = std::mem::take(&mut table.lock().unwrap().pending);
        pending
            .into_iter()
            .filter_map(|(peer, slot)| match self.socket.try_clone() {
                Ok(socket) => Some(SpectatorTransport::Udp(UdpSpectator {
                    socket,
                    peer,
                    slot,
                    table: table.clone(),
                })),
                Err(error) => {
                    log::warn!("netplay: spectator socket handle failed: {error}");
                    table.lock().unwrap().slots.remove(&peer);
                    None
                }
            })
            .collect()
    }
}

/// Windows reports an ICMP port-unreachable for an earlier `send_to` as
/// `WSAECONNRESET` on the socket's next receive or send. On a UDP socket that
/// is not a failed connection but a destination that has gone away (a peer
/// that quit, a spectator that left); the protocol's own timeouts decide when
/// a link is dead, and a departed spectator must never fail the players'
/// link, which shares the host's socket.
#[cfg(not(target_arch = "wasm32"))]
fn is_reset(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::ConnectionReset
}

#[cfg(not(target_arch = "wasm32"))]
impl Transport for UdpTransport {
    fn receive(&mut self, buffer: &mut [u8]) -> Result<Option<usize>> {
        match self.socket.recv_from(buffer) {
            Ok((len, source)) => {
                if source == self.peer {
                    return Ok(Some(len));
                }
                // A spectator's control packets are queued for its own link;
                // every other foreign datagram is discarded as before.
                if let Some(table) = &self.spectators {
                    let bytes = &buffer[..len.min(buffer.len())];
                    if super::control::is_control_packet(
                        bytes,
                        &self.session,
                        super::control::ROLE_SPECTATOR,
                    ) {
                        table.lock().unwrap().accept(source, bytes);
                    }
                }
                Ok(Some(0))
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) if is_reset(&e) => Ok(Some(0)),
            Err(e) => Err(e.into()),
        }
    }

    fn send(&mut self, packet: &[u8]) -> Result<bool> {
        match self.socket.send_to(packet, self.peer) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(e) if is_reset(&e) => Ok(true),
            Err(e) => Err(e.into()),
        }
    }
}

/// One spectator's packets on the host's shared socket.
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct UdpSpectator {
    socket: UdpSocket,
    peer: SocketAddr,
    slot: Arc<Mutex<Slot>>,
    table: Arc<Mutex<SlotTable>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for UdpSpectator {
    fn drop(&mut self) {
        self.table.lock().unwrap().slots.remove(&self.peer);
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Transport for UdpSpectator {
    fn route(&self) -> &'static str {
        "UDP"
    }
    fn receive(&mut self, buffer: &mut [u8]) -> Result<Option<usize>> {
        let Some(packet) = self.slot.lock().unwrap().incoming.pop_front() else {
            return Ok(None);
        };
        ensure!(
            packet.len() <= buffer.len(),
            "netplay receive buffer is too small"
        );
        buffer[..packet.len()].copy_from_slice(&packet);
        Ok(Some(packet.len()))
    }
    fn send(&mut self, packet: &[u8]) -> Result<bool> {
        match self.socket.send_to(packet, self.peer) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            // A spectator that went away is dropped by the status timeout.
            Err(e) if is_reset(&e) => Ok(true),
            Err(e) => Err(e.into()),
        }
    }
}
