// SPDX-License-Identifier: GPL-3.0-or-later

//! Nonblocking packet adapters. Reliability belongs to the shared protocol.
//!
//! A guest or spectator owns one connection to the host. The host owns a
//! listener instead, which hands out one [`PeerTransport`] per admitted
//! guest and spectator; the rollback timeline then treats each of those as
//! an ordinary point-to-point transport.

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

    /// Service a host's listener: read whatever has arrived and sort it into
    /// the admitted links' queues. A listener carries no timeline of its own.
    pub(super) fn pump(&mut self) -> Result<()> {
        let mut buffer = [0; MAX_PACKET + 1];
        for _ in 0..256 {
            if self.receive(&mut buffer)?.is_none() {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Guests that connected since the last call (hosts only).
    pub(super) fn take_players(&mut self) -> Vec<PeerTransport> {
        match self {
            Self::Udp(t) => t.take_links(SlotKind::Player),
            #[cfg(feature = "netplay-internet")]
            Self::Internet(t) => t
                .take_players()
                .into_iter()
                .map(PeerTransport::Internet)
                .collect(),
        }
    }

    /// Spectators that connected since the last call (hosts only).
    pub(super) fn take_spectators(&mut self) -> Vec<PeerTransport> {
        match self {
            Self::Udp(t) => t.take_links(SlotKind::Spectator),
            #[cfg(feature = "netplay-internet")]
            Self::Internet(t) => t
                .take_spectators()
                .into_iter()
                .map(PeerTransport::Internet)
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

/// The host's side of one guest or spectator link.
#[cfg(not(target_arch = "wasm32"))]
pub(super) enum PeerTransport {
    Udp(UdpPeer),
    #[cfg(feature = "netplay-internet")]
    Internet(super::internet::PeerLink),
}

#[cfg(not(target_arch = "wasm32"))]
impl Transport for PeerTransport {
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

/// A player link, whichever side of the session holds it: a guest owns one
/// connection to the host, and the host one admitted link per guest.
#[cfg(not(target_arch = "wasm32"))]
pub(super) enum LinkTransport {
    Direct(NativeTransport),
    Peer(PeerTransport),
}

#[cfg(not(target_arch = "wasm32"))]
impl Transport for LinkTransport {
    fn route(&self) -> &'static str {
        match self {
            Self::Direct(t) => t.route(),
            Self::Peer(t) => t.route(),
        }
    }
    fn ready(&mut self) -> Result<bool> {
        match self {
            Self::Direct(t) => t.ready(),
            Self::Peer(t) => t.ready(),
        }
    }
    fn receive(&mut self, buffer: &mut [u8]) -> Result<Option<usize>> {
        match self {
            Self::Direct(t) => t.receive(buffer),
            Self::Peer(t) => t.receive(buffer),
        }
    }
    fn send(&mut self, packet: &[u8]) -> Result<bool> {
        match self {
            Self::Direct(t) => t.send(packet),
            Self::Peer(t) => t.send(packet),
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

/// Packets from one guest or spectator, demultiplexed by its source address.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
pub(super) struct Slot {
    incoming: VecDeque<Vec<u8>>,
}

/// What a source on the host's socket was admitted as.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SlotKind {
    Player,
    Spectator,
}

/// Sources sharing the host's socket. A source claims a slot with its first
/// control packet for the session; a dropped link frees it.
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct SlotTable {
    slots: BTreeMap<SocketAddr, (SlotKind, Arc<Mutex<Slot>>)>,
    pending: Vec<(SlotKind, SocketAddr, Arc<Mutex<Slot>>)>,
    /// Guest addresses the host will accept; empty admits any source that
    /// presents the session ID, as spectators always have.
    allowed: Vec<SocketAddr>,
    players: usize,
    spectators: usize,
}

#[cfg(not(target_arch = "wasm32"))]
impl SlotTable {
    fn cap(&self, kind: SlotKind) -> usize {
        match kind {
            SlotKind::Player => self.players,
            SlotKind::Spectator => self.spectators,
        }
    }

    fn count(&self, kind: SlotKind) -> usize {
        self.slots.values().filter(|(k, _)| *k == kind).count()
    }

    fn accept(&mut self, kind: SlotKind, source: SocketAddr, bytes: &[u8]) {
        let slot = match self.slots.get(&source) {
            Some((existing, slot)) if *existing == kind => slot.clone(),
            // A source cannot be both a player and a spectator.
            Some(_) => return,
            None => {
                if self.count(kind) >= self.cap(kind) {
                    return;
                }
                if kind == SlotKind::Player
                    && !self.allowed.is_empty()
                    && !self.allowed.contains(&source)
                {
                    return;
                }
                let slot = Arc::new(Mutex::new(Slot::default()));
                self.slots.insert(source, (kind, slot.clone()));
                self.pending.push((kind, source, slot.clone()));
                slot
            }
        };
        let mut slot = slot.lock().unwrap();
        if slot.incoming.len() == 64 {
            slot.incoming.pop_front();
        }
        slot.incoming.push_back(bytes.to_vec());
    }

    /// Route a datagram from an already admitted source, whatever its kind.
    fn deliver(&mut self, source: SocketAddr, bytes: &[u8]) -> bool {
        let Some((_, slot)) = self.slots.get(&source) else {
            return false;
        };
        let mut slot = slot.lock().unwrap();
        if slot.incoming.len() == 64 {
            slot.incoming.pop_front();
        }
        slot.incoming.push_back(bytes.to_vec());
        true
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub struct UdpTransport {
    pub(super) socket: UdpSocket,
    pub(super) options: super::ConnectionOptions,
    /// The single endpoint this link talks to; a listening host has none.
    peer: Option<SocketAddr>,
    session: [u8; 16],
    table: Option<Arc<Mutex<SlotTable>>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl UdpTransport {
    /// A point-to-point link: a guest to its host, or a two-player host that
    /// was given its guest's address.
    pub(super) fn connect(options: super::Options) -> Result<Self> {
        let peer = options.peers[0];
        let spectators = usize::from(options.spectators);
        let transport = Self::bind(
            options.bind,
            Some(peer),
            options.session,
            Vec::new(),
            0,
            spectators,
            super::ConnectionOptions::Direct(options.clone()),
        )?;
        log::info!(
            "netplay: listening on {}, peer {}, player {}; waiting for matching machine",
            transport.socket.local_addr()?,
            peer,
            options.player + 1
        );
        Ok(transport)
    }

    /// A host's listener: it admits each guest and spectator on one socket.
    pub(super) fn listen(options: super::Options) -> Result<Self> {
        let players = options.settings().expected_links();
        let spectators = usize::from(options.spectators);
        let transport = Self::bind(
            options.bind,
            None,
            options.session,
            options.peers.clone(),
            players,
            spectators,
            super::ConnectionOptions::Direct(options.clone()),
        )?;
        log::info!(
            "netplay: listening on {} for {} other player(s); waiting for matching machines",
            transport.socket.local_addr()?,
            players
        );
        Ok(transport)
    }

    pub(super) fn watch(options: super::WatchOptions) -> Result<Self> {
        let transport = Self::bind(
            options.bind,
            Some(options.host),
            options.session,
            Vec::new(),
            0,
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

    #[allow(clippy::too_many_arguments)]
    fn bind(
        bind: SocketAddr,
        peer: Option<SocketAddr>,
        session: [u8; 16],
        allowed: Vec<SocketAddr>,
        players: usize,
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
            table: (players + spectators > 0).then(|| {
                Arc::new(Mutex::new(SlotTable {
                    slots: BTreeMap::new(),
                    pending: Vec::new(),
                    allowed,
                    players,
                    spectators,
                }))
            }),
        })
    }

    pub(super) fn take_links(&mut self, kind: SlotKind) -> Vec<PeerTransport> {
        let Some(table) = &self.table else {
            return Vec::new();
        };
        let ready: Vec<_> = {
            let mut table = table.lock().unwrap();
            let (taken, rest) = std::mem::take(&mut table.pending)
                .into_iter()
                .partition(|(k, _, _)| *k == kind);
            table.pending = rest;
            taken
        };
        ready
            .into_iter()
            .filter_map(|(_, peer, slot)| match self.socket.try_clone() {
                Ok(socket) => Some(PeerTransport::Udp(UdpPeer {
                    socket,
                    peer,
                    slot,
                    table: table.clone(),
                })),
                Err(error) => {
                    log::warn!("netplay: peer socket handle failed: {error}");
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
                if self.peer == Some(source) {
                    return Ok(Some(len));
                }
                // A guest's or spectator's packets are queued for its own
                // link; every other foreign datagram is discarded as before.
                if let Some(table) = &self.table {
                    let bytes = &buffer[..len.min(buffer.len())];
                    let mut table = table.lock().unwrap();
                    if !table.deliver(source, bytes) {
                        for (kind, role) in [
                            (SlotKind::Player, super::control::ROLE_GUEST),
                            (SlotKind::Spectator, super::control::ROLE_SPECTATOR),
                        ] {
                            if super::control::is_control_packet(bytes, &self.session, role) {
                                table.accept(kind, source, bytes);
                                break;
                            }
                        }
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
        let Some(peer) = self.peer else {
            // A listener has nobody to answer; its links do the talking.
            return Ok(true);
        };
        match self.socket.send_to(packet, peer) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(e) if is_reset(&e) => Ok(true),
            Err(e) => Err(e.into()),
        }
    }
}

/// One guest's or spectator's packets on the host's shared socket.
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct UdpPeer {
    socket: UdpSocket,
    peer: SocketAddr,
    slot: Arc<Mutex<Slot>>,
    table: Arc<Mutex<SlotTable>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for UdpPeer {
    fn drop(&mut self) {
        self.table.lock().unwrap().slots.remove(&self.peer);
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Transport for UdpPeer {
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
            // A peer that went away is dropped by the status timeout.
            Err(e) if is_reset(&e) => Ok(true),
            Err(e) => Err(e.into()),
        }
    }
}
