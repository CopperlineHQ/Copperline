// SPDX-License-Identifier: GPL-3.0-or-later

//! Small fixed-layout datagrams. No remote state deserialization or allocation
//! from an untrusted length. Redundant unacknowledged inputs fit below the MTU.

use super::Input;

const MAGIC: &[u8; 4] = b"CLNP";
const VERSION: u16 = 1;
const MAX_INPUTS: usize = 32;
const HEADER: usize = 4 + 2 + 4 + 16 + 32 + 4 + 8 + 8 + 32 + 1;
pub(super) const MAX_PACKET: usize = HEADER + MAX_INPUTS * 26;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Packet {
    pub session: [u8; 16],
    pub identity: [u8; 32],
    pub player: usize,
    pub ready: bool,
    pub delay: u8,
    pub window: u8,
    /// Next input needed; all frames below it have been received.
    pub ack: u64,
    pub inputs: Vec<(u64, Input)>,
    pub checksum: Option<(u64, [u8; 32])>,
}

impl Packet {
    pub fn encode(&self) -> Vec<u8> {
        assert!(self.inputs.len() <= MAX_INPUTS);
        let mut out = Vec::with_capacity(MAX_PACKET);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&crate::savestate::STATE_VERSION.to_le_bytes());
        out.extend_from_slice(&self.session);
        out.extend_from_slice(&self.identity);
        out.extend_from_slice(&[
            self.player as u8,
            u8::from(self.ready),
            self.delay,
            self.window,
        ]);
        out.extend_from_slice(&self.ack.to_le_bytes());
        let (frame, hash) = self.checksum.unwrap_or_default();
        out.extend_from_slice(&frame.to_le_bytes());
        out.extend_from_slice(&hash);
        out.push(self.inputs.len() as u8);
        for (frame, input) in &self.inputs {
            out.extend_from_slice(&frame.to_le_bytes());
            out.extend_from_slice(&input.buttons.to_le_bytes());
            out.extend_from_slice(&input.keys);
        }
        out
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
            || u32::from_le_bytes(take(&mut bytes)?) != crate::savestate::STATE_VERSION
        {
            return None;
        }
        let session = take(&mut bytes)?;
        let identity = take(&mut bytes)?;
        let [player, ready, delay, window] = take(&mut bytes)?;
        if player > 1 || ready > 1 || delay > 6 || !(1..=12).contains(&window) {
            return None;
        }
        let ack = u64::from_le_bytes(take(&mut bytes)?);
        let frame = u64::from_le_bytes(take(&mut bytes)?);
        let hash = take(&mut bytes)?;
        if frame != 0 && !frame.is_multiple_of(super::rollback::HASH_INTERVAL) {
            return None;
        }
        let [count] = take(&mut bytes)?;
        if usize::from(count) > MAX_INPUTS || bytes.len() != usize::from(count) * 26 {
            return None;
        }
        let mut inputs = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let number = u64::from_le_bytes(take(&mut bytes)?);
            let buttons = u16::from_le_bytes(take(&mut bytes)?);
            if buttons & !0x7ff != 0 || inputs.last().is_some_and(|(prev, _)| *prev >= number) {
                return None;
            }
            inputs.push((
                number,
                Input {
                    buttons,
                    keys: take(&mut bytes)?,
                },
            ));
        }
        Some(Self {
            session,
            identity,
            player: usize::from(player),
            ready: ready != 0,
            delay,
            window,
            ack,
            inputs,
            checksum: (frame != 0).then_some((frame, hash)),
        })
    }
}
