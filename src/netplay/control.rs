// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded reliable messages beside the input datagrams, over either native
//! transport. Selective repeat uses the same socket and peer as netplay.

use super::{Transport, MAX_PACKET};
use crate::timebase::{Duration, Instant};
use anyhow::{ensure, Result};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

const MAGIC: &[u8; 4] = b"CLNC";
const HEADER: usize = 4 + 1 + 16 + 1 + 8 + 8 + 4 + 1;
const CHUNK: usize = MAX_PACKET - HEADER;
const WINDOW: u64 = 32;
const RESEND: Duration = Duration::from_millis(200);
/// Complete messages held for the session before further chunks wait in the
/// receive window; queued messages awaiting transmission are bounded alike.
const QUEUE: usize = 4;

/// Role bytes carried by every control packet.
pub(super) const ROLE_HOST: u8 = 0;
pub(super) const ROLE_GUEST: u8 = 1;
pub(super) const ROLE_SPECTATOR: u8 = 2;

/// Whether a datagram is a control packet of `session` sent by `role`.
pub(super) fn is_control_packet(bytes: &[u8], session: &[u8; 16], role: u8) -> bool {
    bytes.len() >= HEADER
        && bytes.len() <= MAX_PACKET
        && bytes.starts_with(MAGIC)
        && bytes[4] == 1
        && bytes[5..21] == *session
        && bytes[21] == role
}

struct Outgoing {
    bytes: Vec<u8>,
    sent: Option<Instant>,
}

pub(super) struct Control<T> {
    pub inner: T,
    session: [u8; 16],
    role: u8,
    peer_role: u8,
    next_send: u64,
    next_receive: u64,
    sending: VecDeque<VecDeque<Arc<Vec<u8>>>>,
    send_offset: usize,
    outgoing: BTreeMap<u64, Outgoing>,
    incoming: BTreeMap<u64, Vec<u8>>,
    assembling: Vec<u8>,
    expected: Option<usize>,
    messages: VecDeque<Vec<u8>>,
    game: VecDeque<Vec<u8>>,
    ack_pending: bool,
}

impl<T: Transport> Control<T> {
    /// A player's control link addresses the opposite player; the host also
    /// opens one link per spectator, and a spectator one link to the host.
    pub fn new(inner: T, session: [u8; 16], role: u8, peer_role: u8) -> Self {
        Self {
            inner,
            session,
            role,
            peer_role,
            next_send: 0,
            next_receive: 0,
            sending: VecDeque::new(),
            send_offset: 0,
            outgoing: BTreeMap::new(),
            incoming: BTreeMap::new(),
            assembling: Vec::new(),
            expected: None,
            messages: VecDeque::new(),
            game: VecDeque::new(),
            ack_pending: false,
        }
    }

    pub fn send_message(&mut self, bytes: Vec<u8>) -> Result<()> {
        self.send_parts(vec![Arc::new(bytes)])
    }

    /// Queue shared media buffers, copying only packet-sized chunks. The same
    /// buffers can be queued on several links without duplicating them.
    pub fn send_parts(&mut self, parts: Vec<Arc<Vec<u8>>>) -> Result<()> {
        let len = parts
            .iter()
            .try_fold(0usize, |len, part| len.checked_add(part.len()))
            .context("netplay control message length overflow")?;
        ensure!(
            len > 0 && len <= super::setup::MAX_BUNDLE + 1 && self.can_send(),
            "netplay control send queue is full"
        );
        // Parts are chunked separately, so a small leading part shares its
        // packet with the length prefix instead of costing one of its own:
        // a feed message or setup reply then travels as a single packet.
        let mut parts: VecDeque<_> = parts.into_iter().filter(|part| !part.is_empty()).collect();
        let prefix = (len as u32).to_le_bytes();
        let mut framed = VecDeque::new();
        match parts.front() {
            Some(first) if first.len() + prefix.len() <= CHUNK => {
                let mut merged = Vec::with_capacity(prefix.len() + first.len());
                merged.extend_from_slice(&prefix);
                merged.extend_from_slice(first);
                parts.pop_front();
                framed.push_back(Arc::new(merged));
            }
            _ => framed.push_back(Arc::new(prefix.to_vec())),
        }
        framed.extend(parts);
        self.sending.push_back(framed);
        Ok(())
    }

    /// Whether another message may be queued without an error.
    pub fn can_send(&self) -> bool {
        self.sending.len() < QUEUE
    }

    pub fn take_message(&mut self) -> Option<Vec<u8>> {
        self.messages.pop_front()
    }
    pub fn sending(&self) -> bool {
        !self.sending.is_empty() || !self.outgoing.is_empty()
    }
    pub fn has_game_packets(&self) -> bool {
        self.game.iter().any(|bytes| {
            super::wire::Packet::decode(bytes).is_some_and(|packet| packet.session == self.session)
        })
    }
    pub fn received_bytes(&self) -> usize {
        self.assembling.len()
    }

    pub fn poll(&mut self) -> Result<()> {
        self.poll_at(Instant::now())
    }

    fn poll_at(&mut self, now: Instant) -> Result<()> {
        if !self.inner.ready()? {
            return Ok(());
        }
        let mut buffer = [0; MAX_PACKET + 1];
        for _ in 0..128 {
            let Some(len) = self.inner.receive(&mut buffer)? else {
                break;
            };
            let Some(bytes) = buffer.get(..len) else {
                continue;
            };
            if !bytes.starts_with(MAGIC) {
                super::wire::Packet::check_version(bytes, &self.session)?;
                if self.game.len() < 64 && !bytes.is_empty() {
                    self.game.push_back(bytes.to_vec());
                }
                continue;
            }
            if bytes.len() < HEADER || bytes.len() > MAX_PACKET || bytes[5..21] != self.session {
                continue;
            }
            ensure!(
                bytes[4] == 1,
                "incompatible desktop setup protocol; use the same build"
            );
            ensure!(
                bytes[21] == self.peer_role,
                "netplay peers must use matching roles"
            );
            let seq = u64::from_le_bytes(bytes[22..30].try_into()?);
            let ack = u64::from_le_bytes(bytes[30..38].try_into()?);
            let mask = u32::from_le_bytes(bytes[38..42].try_into()?);
            ensure!(ack <= self.next_send, "peer acknowledged unsent setup data");
            self.outgoing
                .retain(|&n, _| n >= ack && (n - ack >= WINDOW || mask & (1 << (n - ack)) == 0));
            if bytes[42] == 0 {
                ensure!(bytes.len() == HEADER, "invalid setup acknowledgement");
                continue;
            }
            ensure!(
                bytes[42] == 1 && bytes.len() > HEADER,
                "invalid setup data packet"
            );
            self.ack_pending = true;
            if seq < self.next_receive {
                continue;
            }
            ensure!(
                seq - self.next_receive < WINDOW,
                "setup data exceeds receive window"
            );
            if let Some(previous) = self.incoming.get(&seq) {
                ensure!(
                    previous.as_slice() == &bytes[HEADER..],
                    "peer changed pending setup data"
                );
            } else {
                self.incoming.insert(seq, bytes[HEADER..].to_vec());
            }
        }
        // Chunks past a full message queue stay in the receive window,
        // selectively acknowledged, until the session drains messages and
        // a later poll assembles them.
        while self.messages.len() < QUEUE {
            let Some(chunk) = self.incoming.remove(&self.next_receive) else {
                break;
            };
            self.next_receive = self
                .next_receive
                .checked_add(1)
                .context("setup sequence exhausted")?;
            self.assemble(&chunk)?;
            self.ack_pending = true;
        }
        if self.ack_pending && self.inner.send(&self.packet(0, &[]))? {
            self.ack_pending = false;
        }
        // Keep the window anchored at the oldest unacknowledged sequence,
        // including selectively acknowledged packets after a missing chunk.
        let base = self
            .outgoing
            .first_key_value()
            .map_or(self.next_send, |(&n, _)| n);
        while self.next_send - base < WINDOW {
            let Some(message) = self.sending.front_mut() else {
                break;
            };
            let part = message.front().unwrap();
            let end = (self.send_offset + CHUNK).min(part.len());
            let chunk = part[self.send_offset..end].to_vec();
            self.outgoing.insert(
                self.next_send,
                Outgoing {
                    bytes: chunk,
                    sent: None,
                },
            );
            self.next_send = self
                .next_send
                .checked_add(1)
                .context("setup sequence exhausted")?;
            self.send_offset = end;
            if end == part.len() {
                message.pop_front();
                if message.is_empty() {
                    self.sending.pop_front();
                }
                self.send_offset = 0;
            }
        }
        let due: Vec<_> = self
            .outgoing
            .iter()
            .filter(|(_, packet)| {
                packet
                    .sent
                    .is_none_or(|sent| now.duration_since(sent) >= RESEND)
            })
            .map(|(&seq, _)| seq)
            .collect();
        for seq in due {
            let packet = self.packet(seq, &self.outgoing[&seq].bytes);
            if !self.inner.send(&packet)? {
                break;
            }
            self.outgoing.get_mut(&seq).unwrap().sent = Some(now);
        }
        Ok(())
    }

    fn packet(&self, sequence: u64, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER + payload.len());
        bytes.extend(MAGIC);
        bytes.push(1);
        bytes.extend(self.session);
        bytes.push(self.role);
        bytes.extend(sequence.to_le_bytes());
        bytes.extend(self.next_receive.to_le_bytes());
        let mask = self
            .incoming
            .keys()
            .fold(0u32, |mask, n| mask | (1 << (n - self.next_receive)));
        bytes.extend(mask.to_le_bytes());
        bytes.push(u8::from(!payload.is_empty()));
        bytes.extend(payload);
        bytes
    }

    fn assemble(&mut self, mut bytes: &[u8]) -> Result<()> {
        while !bytes.is_empty() {
            let target = self.expected.unwrap_or(4);
            let take = (target - self.assembling.len()).min(bytes.len());
            if self.assembling.len() + take > self.assembling.capacity() {
                let capacity = self
                    .assembling
                    .capacity()
                    .max(CHUNK)
                    .saturating_mul(2)
                    .min(target);
                self.assembling
                    .reserve_exact(capacity - self.assembling.len());
            }
            self.assembling.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.assembling.len() != target {
                continue;
            }
            if self.expected.is_none() {
                let len = u32::from_le_bytes(self.assembling[..4].try_into()?) as usize;
                ensure!(
                    len <= super::setup::MAX_BUNDLE + 1 && len > 0,
                    "invalid setup message length"
                );
                // Grow only as data arrives, capped at the validated length.
                // A near-limit message type byte must not double allocation.
                self.assembling.clear();
                self.expected = Some(len);
            } else {
                // One chunk may complete several small messages; the window
                // guard in `poll_at` bounds how far past QUEUE this can go.
                self.messages
                    .push_back(std::mem::take(&mut self.assembling));
                self.expected = None;
            }
        }
        Ok(())
    }
}

use anyhow::Context;

impl<T: Transport> Transport for Control<T> {
    fn route(&self) -> &'static str {
        self.inner.route()
    }
    fn ready(&mut self) -> Result<bool> {
        // A disconnect can follow the last input/ack in the same poll. Let
        // the timeline consume those packets before surfacing the close.
        if self.game.is_empty() {
            self.inner.ready()
        } else {
            Ok(true)
        }
    }
    fn receive(&mut self, buffer: &mut [u8]) -> Result<Option<usize>> {
        let Some(bytes) = self.game.pop_front() else {
            return Ok(None);
        };
        ensure!(bytes.len() <= buffer.len(), "input packet too large");
        buffer[..bytes.len()].copy_from_slice(&bytes);
        Ok(Some(bytes.len()))
    }
    fn send(&mut self, packet: &[u8]) -> Result<bool> {
        self.inner.send(packet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netplay::PacketQueue;

    #[test]
    fn receive_allocation_grows_with_data_and_stays_within_message_length() -> Result<()> {
        let mut control = Control::new(PacketQueue::default(), [3; 16], ROLE_HOST, ROLE_GUEST);
        let limit = (super::super::setup::MAX_BUNDLE + 1) as u32;
        control.assemble(&limit.to_le_bytes())?;
        control.assemble(&[1])?;
        assert!(control.assembling.capacity() <= 2 * CHUNK);

        let mut control = Control::new(PacketQueue::default(), [3; 16], ROLE_HOST, ROLE_GUEST);
        let len = CHUNK * 3 + 1;
        control.assemble(&(len as u32).to_le_bytes())?;
        for chunk in vec![7; len].chunks(CHUNK) {
            control.assemble(chunk)?;
            assert!(control.assembling.capacity() <= len);
        }
        let message = control.take_message().unwrap();
        assert_eq!(message, vec![7; len]);
        assert!(message.capacity() <= len);
        Ok(())
    }

    #[test]
    fn transfer_survives_loss_reordering_duplicates_and_backpressure() -> Result<()> {
        let mut a = Control::new(PacketQueue::default(), [3; 16], ROLE_HOST, ROLE_GUEST);
        let mut b = Control::new(PacketQueue::default(), [3; 16], ROLE_GUEST, ROLE_HOST);
        let payload: Vec<_> = (0..120_000).map(|n| (n % 251) as u8).collect();
        a.send_parts(vec![
            Arc::new(payload[..123].to_vec()),
            Arc::new(vec![]),
            Arc::new(payload[123..].to_vec()),
        ])?;
        b.send_message(b"reply".to_vec())?;
        let mut now = Instant::now();
        let mut received = None;
        let mut reply = None;
        for tick in 0..500 {
            a.poll_at(now)?;
            b.poll_at(now)?;
            for reverse in [false, true] {
                let (from, to) = if reverse {
                    (&mut a.inner, &mut b.inner)
                } else {
                    (&mut b.inner, &mut a.inner)
                };
                let mut packets = Vec::new();
                while let Some(packet) = from.pop() {
                    packets.push(packet);
                }
                for (i, packet) in packets.into_iter().rev().enumerate() {
                    if (tick + i) % 7 != 0 {
                        to.push(&packet)?;
                        if i % 5 == 0 {
                            to.push(&packet)?;
                        }
                    }
                }
            }
            if let Some(message) = a.take_message() {
                ensure!(reply.is_none(), "duplicate delivery");
                reply = Some(message);
            }
            if let Some(message) = b.take_message() {
                ensure!(received.is_none(), "duplicate delivery");
                received = Some(message);
            }
            now += Duration::from_millis(50);
            if received.is_some() && reply.is_some() && !a.sending() && !b.sending() {
                break;
            }
        }
        assert_eq!(received, Some(payload));
        assert_eq!(reply, Some(b"reply".to_vec()));
        assert!(!a.sending() && !b.sending());
        Ok(())
    }

    #[test]
    fn links_check_role_pairs_and_hold_chunks_while_messages_wait() -> Result<()> {
        let session = [3; 16];
        let mut host = Control::new(PacketQueue::default(), session, ROLE_HOST, ROLE_SPECTATOR);
        let mut spectator =
            Control::new(PacketQueue::default(), session, ROLE_SPECTATOR, ROLE_HOST);
        let mut guest = Control::new(PacketQueue::default(), session, ROLE_GUEST, ROLE_HOST);
        host.send_message(b"feed".to_vec())?;
        host.poll()?;
        let packet = host.inner.pop().unwrap();
        assert!(host.inner.pop().is_none(), "a small message is one packet");
        assert!(is_control_packet(&packet, &session, ROLE_HOST));
        assert!(!is_control_packet(&packet, &session, ROLE_SPECTATOR));
        assert!(!is_control_packet(&packet, &[4; 16], ROLE_HOST));
        assert!(!is_control_packet(
            &packet[..HEADER - 1],
            &session,
            ROLE_HOST
        ));
        spectator.inner.push(&packet)?;
        spectator.poll()?;
        assert_eq!(spectator.take_message(), Some(b"feed".to_vec()));
        // A guest's packet on a spectator link is a role mismatch.
        guest.send_message(b"hello".to_vec())?;
        guest.poll()?;
        let wrong = guest.inner.pop().unwrap();
        spectator.inner.push(&wrong)?;
        assert!(spectator.poll().is_err());
        // Eight small messages: the receiver keeps QUEUE complete and
        // leaves the rest in its window until the session drains them.
        let mut host = Control::new(PacketQueue::default(), session, ROLE_HOST, ROLE_SPECTATOR);
        let mut spectator =
            Control::new(PacketQueue::default(), session, ROLE_SPECTATOR, ROLE_HOST);
        let mut sent = 0u8;
        for round in 0..2 {
            while host.can_send() {
                host.send_message(vec![sent; 3])?;
                sent += 1;
            }
            assert!(host.send_message(vec![0]).is_err(), "queue bound holds");
            host.poll()?;
            while let Some(packet) = host.inner.pop() {
                spectator.inner.push(&packet)?;
            }
            spectator.poll()?;
            assert_eq!(spectator.messages.len(), QUEUE, "round {round}");
            assert_eq!(spectator.incoming.len(), round * QUEUE, "round {round}");
        }
        assert_eq!(sent, 8);
        for expected in 0..8u8 {
            let message = spectator.take_message().unwrap();
            assert_eq!(message, vec![expected; 3]);
            if expected == 3 {
                spectator.poll()?;
                assert!(spectator.incoming.is_empty());
            }
        }
        assert!(spectator.take_message().is_none());
        Ok(())
    }
}
