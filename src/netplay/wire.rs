// SPDX-License-Identifier: GPL-3.0-or-later

//! Small fixed-layout datagrams. No remote state deserialization or allocation
//! from an untrusted length. Redundant unacknowledged inputs fit below the MTU.
//!
//! A packet carries records for any player, so the host can relay the other
//! players' inputs to each guest on the same wire the two-player session
//! always used. Acknowledgements are per player for the same reason: a
//! receiver states what it still needs from every player, and the sender
//! repeats exactly that.

use super::{Input, MAX_PLAYERS};

const MAGIC: &[u8; 4] = b"CLNP";
pub const VERSION: u16 = 3;
pub const MAX_INPUTS: usize = 32;
pub const HEADER: usize = 4 + 2 + 4 + 16 + 32 + 5 + 8 * MAX_PLAYERS + 8 + 32 + 1;
pub const INPUT_RECORD: usize = 1 + 8 + 2 + 16 + 2 + 2 + 1;
pub const MAX_PACKET: usize = HEADER + MAX_INPUTS * INPUT_RECORD;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Packet {
    pub session: [u8; 16],
    pub identity: [u8; 32],
    /// The player this packet was sent by.
    pub player: usize,
    /// Controller ports the session drives, 2..=[`MAX_PLAYERS`].
    pub players: usize,
    pub ready: bool,
    pub delay: u8,
    pub window: u8,
    /// Next input needed from each player; all frames below it have been
    /// received. Entries at or above `players` are zero.
    pub acks: [u64; MAX_PLAYERS],
    /// Player, frame and controls, ordered by player then frame.
    pub inputs: Vec<(usize, u64, Input)>,
    pub checksum: Option<(u64, [u8; 32])>,
}

impl Packet {
    pub fn encode(&self) -> Vec<u8> {
        assert!(self.inputs.len() <= MAX_INPUTS);
        let mut out = Vec::with_capacity(MAX_PACKET);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&crate::savestate::SCHEMA_FINGERPRINT.to_le_bytes());
        out.extend_from_slice(&self.session);
        out.extend_from_slice(&self.identity);
        out.extend_from_slice(&[
            self.player as u8,
            u8::from(self.ready),
            self.delay,
            self.window,
            self.players as u8,
        ]);
        for ack in self.acks {
            out.extend_from_slice(&ack.to_le_bytes());
        }
        let (frame, hash) = self.checksum.unwrap_or_default();
        out.extend_from_slice(&frame.to_le_bytes());
        out.extend_from_slice(&hash);
        out.push(self.inputs.len() as u8);
        for (player, frame, input) in &self.inputs {
            out.push(*player as u8);
            out.extend_from_slice(&frame.to_le_bytes());
            out.extend_from_slice(&input.buttons.to_le_bytes());
            out.extend_from_slice(&input.keys);
            out.extend_from_slice(&input.mouse_dx.to_le_bytes());
            out.extend_from_slice(&input.mouse_dy.to_le_bytes());
            out.push(input.mouse_buttons);
        }
        out
    }

    // The magic and session prefix are stable across protocol versions. Only
    // diagnose a recognized session; unrelated datagrams remain ignorable.
    pub fn check_version(bytes: &[u8], session: &[u8; 16]) -> anyhow::Result<()> {
        if bytes.len() >= 26 && &bytes[..4] == MAGIC && &bytes[10..26] == session {
            anyhow::ensure!(
                bytes[4..6] == VERSION.to_le_bytes()
                    && bytes[6..10] == crate::savestate::SCHEMA_FINGERPRINT.to_le_bytes(),
                "netplay incompatible build: protocol version or state schema differs; use the same Copperline build"
            );
        }
        Ok(())
    }

    pub fn decode(mut bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HEADER || bytes.len() > MAX_PACKET {
            return None;
        }
        fn take<const N: usize>(bytes: &mut &[u8]) -> Option<[u8; N]> {
            let (head, rest) = bytes.split_at_checked(N)?;
            *bytes = rest;
            head.try_into().ok()
        }
        if &take::<4>(&mut bytes)? != MAGIC
            || u16::from_le_bytes(take(&mut bytes)?) != VERSION
            || u32::from_le_bytes(take(&mut bytes)?) != crate::savestate::SCHEMA_FINGERPRINT
        {
            return None;
        }
        let session = take(&mut bytes)?;
        let identity = take(&mut bytes)?;
        let [player, ready, delay, window, players] = take(&mut bytes)?;
        let players = usize::from(players);
        if !(2..=MAX_PLAYERS).contains(&players)
            || usize::from(player) >= players
            || ready > 1
            || delay > 6
            || !(1..=12).contains(&window)
        {
            return None;
        }
        let mut acks = [0; MAX_PLAYERS];
        for (index, ack) in acks.iter_mut().enumerate() {
            *ack = u64::from_le_bytes(take(&mut bytes)?);
            // Unused ports never acknowledge anything.
            if index >= players && *ack != 0 {
                return None;
            }
        }
        let frame = u64::from_le_bytes(take(&mut bytes)?);
        let hash = take(&mut bytes)?;
        if frame != 0 && !frame.is_multiple_of(super::rollback::HASH_INTERVAL) {
            return None;
        }
        let [count] = take(&mut bytes)?;
        if usize::from(count) > MAX_INPUTS || bytes.len() != usize::from(count) * INPUT_RECORD {
            return None;
        }
        let mut inputs = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let source = usize::from(take::<1>(&mut bytes)?[0]);
            let number = u64::from_le_bytes(take(&mut bytes)?);
            let buttons = u16::from_le_bytes(take(&mut bytes)?);
            if source >= players
                || buttons & !Input::BUTTONS != 0
                || inputs
                    .last()
                    .is_some_and(|&(p, f, _): &(usize, u64, Input)| (p, f) >= (source, number))
            {
                return None;
            }
            let input = Input {
                buttons,
                keys: take(&mut bytes)?,
                mouse_dx: i16::from_le_bytes(take(&mut bytes)?),
                mouse_dy: i16::from_le_bytes(take(&mut bytes)?),
                mouse_buttons: take::<1>(&mut bytes)?[0],
            };
            if input.mouse_buttons & !7 != 0 {
                return None;
            }
            inputs.push((source, number, input));
        }
        Some(Self {
            session,
            identity,
            player: usize::from(player),
            players,
            ready: ready != 0,
            delay,
            window,
            acks,
            inputs,
            checksum: (frame != 0).then_some((frame, hash)),
        })
    }
}
