// SPDX-License-Identifier: GPL-3.0-or-later

//! Spectator feed: the host's confirmed two-player input history, streamed in
//! order to lockstep observers over a reliable channel. Records use the same
//! fixed layouts as the input datagrams. Decoding is bounded and never
//! deserializes machine state: a spectator rebuilds the game from the cold
//! bundle and the inputs alone.

use super::rollback::{Machine, HASH_INTERVAL};
use super::{digest, Input};
use crate::emulator::Emulator;
use anyhow::{bail, ensure, Result};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

/// Replacement floppy images, including any decompressed expansion.
pub const SWAP_LIMIT: usize = 16 * 1024 * 1024;
/// Frames carried by one `Frames` message.
pub const MAX_BATCH: usize = 1024;
/// Confirmed frames a spectator may hold ahead of execution.
pub const MAX_PENDING_FRAMES: usize = 1 << 20;
/// Retained history on a native host before new spectators are refused.
pub const NATIVE_FEED_LIMIT: usize = 256 * 1024 * 1024;
/// Retained history on a browser host before new spectators are refused.
pub const WEB_FEED_LIMIT: usize = 64 * 1024 * 1024;
/// Spectators a host serves at most.
pub const MAX_SPECTATORS: usize = 8;

const INPUT_BYTES: usize = 2 + 16 + 2 + 2 + 1;
const RECORD: usize = 2 * INPUT_BYTES;
const FRAMES_HEADER: usize = 8 + 2;
const CHECKPOINT_LEN: usize = 8 + 32;
const SWAP_HEADER: usize = 8 + 1 + 1 + 32 + 32 + 4;
const HEADER: usize = 1 + 4;
const MAX_MESSAGE: usize = SWAP_HEADER + SWAP_LIMIT;
const MAX_CHECKPOINTS: usize = 64;

const KIND_FRAMES: u8 = 1;
const KIND_CHECKPOINT: u8 = 2;
const KIND_SWAP: u8 = 3;
const KIND_HEAD: u8 = 4;
const KIND_VERIFIED: u8 = 5;
const KIND_STATUS: u8 = 6;

/// One host-applied floppy change, replayed by spectators at the same frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapRecord {
    /// The change applies before this frame executes.
    pub frame: u64,
    pub drive: usize,
    pub writable: bool,
    /// Full machine digest at the boundary, before and after the change.
    pub before: [u8; 32],
    pub after: [u8; 32],
    /// Empty means eject.
    pub bytes: Arc<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedMessage {
    Frames { first: u64, inputs: Vec<[Input; 2]> },
    Checkpoint { frame: u64, hash: [u8; 32] },
    Swap(SwapRecord),
    Head { frame: u64 },
    Verified { identity: [u8; 32] },
    Status { frame: u64 },
}

fn encode_input(out: &mut Vec<u8>, input: &Input) {
    out.extend_from_slice(&input.buttons.to_le_bytes());
    out.extend_from_slice(&input.keys);
    out.extend_from_slice(&input.mouse_dx.to_le_bytes());
    out.extend_from_slice(&input.mouse_dy.to_le_bytes());
    out.push(input.mouse_buttons);
}

fn decode_input(bytes: &[u8]) -> Result<Input> {
    ensure!(bytes.len() == INPUT_BYTES, "invalid spectator input record");
    let input = Input {
        buttons: u16::from_le_bytes([bytes[0], bytes[1]]),
        keys: bytes[2..18].try_into()?,
        mouse_dx: i16::from_le_bytes([bytes[18], bytes[19]]),
        mouse_dy: i16::from_le_bytes([bytes[20], bytes[21]]),
        mouse_buttons: bytes[22],
    };
    ensure!(
        input.buttons & !Input::BUTTONS == 0 && input.mouse_buttons & !7 == 0,
        "invalid spectator controller input"
    );
    Ok(input)
}

impl FeedMessage {
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        let start = out.len();
        out.push(0);
        out.extend_from_slice(&[0; 4]);
        match self {
            Self::Frames { first, inputs } => {
                assert!(!inputs.is_empty() && inputs.len() <= MAX_BATCH);
                out[start] = KIND_FRAMES;
                out.extend_from_slice(&first.to_le_bytes());
                out.extend_from_slice(&(inputs.len() as u16).to_le_bytes());
                for pair in inputs {
                    encode_input(out, &pair[0]);
                    encode_input(out, &pair[1]);
                }
            }
            Self::Checkpoint { frame, hash } => {
                out[start] = KIND_CHECKPOINT;
                out.extend_from_slice(&frame.to_le_bytes());
                out.extend_from_slice(hash);
            }
            Self::Swap(swap) => {
                assert!(swap.bytes.len() <= SWAP_LIMIT);
                out[start] = KIND_SWAP;
                out.extend_from_slice(&swap.frame.to_le_bytes());
                out.push(swap.drive as u8);
                out.push(u8::from(swap.writable));
                out.extend_from_slice(&swap.before);
                out.extend_from_slice(&swap.after);
                out.extend_from_slice(&(swap.bytes.len() as u32).to_le_bytes());
                out.extend_from_slice(&swap.bytes);
            }
            Self::Head { frame } => {
                out[start] = KIND_HEAD;
                out.extend_from_slice(&frame.to_le_bytes());
            }
            Self::Verified { identity } => {
                out[start] = KIND_VERIFIED;
                out.extend_from_slice(identity);
            }
            Self::Status { frame } => {
                out[start] = KIND_STATUS;
                out.extend_from_slice(&frame.to_le_bytes());
            }
        }
        let len = (out.len() - start - HEADER) as u32;
        out[start + 1..start + HEADER].copy_from_slice(&len.to_le_bytes());
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    fn check_header(kind: u8, len: usize) -> Result<()> {
        let valid = match kind {
            KIND_FRAMES => {
                len >= FRAMES_HEADER + RECORD
                    && (len - FRAMES_HEADER).is_multiple_of(RECORD)
                    && (len - FRAMES_HEADER) / RECORD <= MAX_BATCH
            }
            KIND_CHECKPOINT => len == CHECKPOINT_LEN,
            KIND_SWAP => len >= SWAP_HEADER && len - SWAP_HEADER <= SWAP_LIMIT,
            KIND_HEAD | KIND_STATUS => len == 8,
            KIND_VERIFIED => len == 32,
            _ => bail!("unknown spectator feed message"),
        };
        ensure!(valid, "invalid spectator feed message length");
        Ok(())
    }

    fn decode(kind: u8, payload: &[u8]) -> Result<Self> {
        fn u64_at(bytes: &[u8], at: usize) -> u64 {
            u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
        }
        Ok(match kind {
            KIND_FRAMES => {
                let first = u64_at(payload, 0);
                let count = usize::from(u16::from_le_bytes([payload[8], payload[9]]));
                ensure!(
                    count * RECORD == payload.len() - FRAMES_HEADER,
                    "spectator frame count mismatch"
                );
                let mut inputs = Vec::with_capacity(count);
                for record in payload[FRAMES_HEADER..].chunks_exact(RECORD) {
                    inputs.push([
                        decode_input(&record[..INPUT_BYTES])?,
                        decode_input(&record[INPUT_BYTES..])?,
                    ]);
                }
                Self::Frames { first, inputs }
            }
            KIND_CHECKPOINT => {
                let frame = u64_at(payload, 0);
                ensure!(
                    frame > 0 && frame.is_multiple_of(HASH_INTERVAL),
                    "invalid spectator checkpoint frame"
                );
                Self::Checkpoint {
                    frame,
                    hash: payload[8..40].try_into()?,
                }
            }
            KIND_SWAP => {
                let frame = u64_at(payload, 0);
                let drive = usize::from(payload[8]);
                let writable = payload[9];
                let size = u32::from_le_bytes(payload[74..78].try_into()?) as usize;
                ensure!(
                    drive < 4
                        && writable <= 1
                        && size == payload.len() - SWAP_HEADER
                        && (size > 0 || writable == 0),
                    "invalid spectator disk change"
                );
                Self::Swap(SwapRecord {
                    frame,
                    drive,
                    writable: writable == 1,
                    before: payload[10..42].try_into()?,
                    after: payload[42..74].try_into()?,
                    bytes: Arc::new(payload[SWAP_HEADER..].to_vec()),
                })
            }
            KIND_HEAD => Self::Head {
                frame: u64_at(payload, 0),
            },
            KIND_VERIFIED => Self::Verified {
                identity: payload[..32].try_into()?,
            },
            KIND_STATUS => Self::Status {
                frame: u64_at(payload, 0),
            },
            _ => bail!("unknown spectator feed message"),
        })
    }
}

/// Reassembles feed messages from a byte stream split at arbitrary points.
/// Headers are validated before their payload is buffered.
#[derive(Default)]
pub struct FeedDecoder {
    buffer: Vec<u8>,
    offset: usize,
}

impl FeedDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.buffer.len() - self.offset + bytes.len() <= 2 * (HEADER + MAX_MESSAGE),
            "spectator feed exceeds the receive buffer"
        );
        if self.offset > 0 && self.offset >= self.buffer.len() / 2 {
            self.buffer.drain(..self.offset);
            self.offset = 0;
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    pub fn next_message(&mut self) -> Result<Option<FeedMessage>> {
        let available = &self.buffer[self.offset..];
        if available.len() < HEADER {
            return Ok(None);
        }
        let kind = available[0];
        let len = u32::from_le_bytes(available[1..HEADER].try_into()?) as usize;
        FeedMessage::check_header(kind, len)?;
        if available.len() < HEADER + len {
            return Ok(None);
        }
        let message = FeedMessage::decode(kind, &available[HEADER..HEADER + len])?;
        self.offset += HEADER + len;
        if self.offset == self.buffer.len() {
            self.buffer.clear();
            self.offset = 0;
        }
        Ok(Some(message))
    }
}

/// Where a spectator link is in the host's history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeedCursor {
    pub frame: u64,
    pub swap: usize,
    pub checkpoint: usize,
}

/// The host's complete confirmed history for the session.
pub struct Feed {
    frames: Vec<[Input; 2]>,
    checkpoints: Vec<(u64, [u8; 32])>,
    swaps: Vec<SwapRecord>,
    bytes: usize,
    limit: usize,
}

impl Feed {
    pub fn new(limit: usize) -> Self {
        Self {
            frames: Vec::new(),
            checkpoints: Vec::new(),
            swaps: Vec::new(),
            bytes: 0,
            limit,
        }
    }

    /// Confirmed frames recorded so far.
    pub fn frames(&self) -> u64 {
        self.frames.len() as u64
    }

    /// Late joiners would need more history than the host retains.
    pub fn full(&self) -> bool {
        self.bytes > self.limit
    }

    pub fn record_frame(&mut self, frame: u64, inputs: [Input; 2]) -> Result<()> {
        ensure!(
            frame == self.frames(),
            "confirmed frames must be recorded in order"
        );
        self.frames.push(inputs);
        self.bytes += RECORD;
        Ok(())
    }

    pub fn record_checkpoint(&mut self, frame: u64, hash: [u8; 32]) -> Result<()> {
        ensure!(
            frame > 0
                && frame.is_multiple_of(HASH_INTERVAL)
                && frame <= self.frames()
                && self.checkpoints.last().is_none_or(|(f, _)| *f < frame),
            "checkpoints must be recorded in order"
        );
        self.checkpoints.push((frame, hash));
        self.bytes += CHECKPOINT_LEN;
        Ok(())
    }

    pub fn record_swap(&mut self, swap: SwapRecord) -> Result<()> {
        ensure!(
            swap.frame == self.frames()
                && swap.drive < 4
                && swap.bytes.len() <= SWAP_LIMIT
                && self.swaps.last().is_none_or(|s| s.frame <= swap.frame),
            "disk changes must be recorded at the confirmed frame"
        );
        self.bytes += SWAP_HEADER + swap.bytes.len();
        self.swaps.push(swap);
        Ok(())
    }

    /// The next message a spectator at `cursor` needs, in replay order: a
    /// checkpoint or disk change due at its frame, then a batch of frames that
    /// never crosses the next checkpoint or disk change.
    pub fn next_message(
        &self,
        cursor: FeedCursor,
        max_frames: usize,
    ) -> Option<(FeedMessage, FeedCursor)> {
        let mut next = cursor;
        if let Some(&(frame, hash)) = self.checkpoints.get(cursor.checkpoint) {
            if frame <= cursor.frame {
                next.checkpoint += 1;
                return Some((FeedMessage::Checkpoint { frame, hash }, next));
            }
        }
        if let Some(swap) = self.swaps.get(cursor.swap) {
            if swap.frame <= cursor.frame {
                next.swap += 1;
                return Some((FeedMessage::Swap(swap.clone()), next));
            }
        }
        let total = self.frames();
        if cursor.frame >= total {
            return None;
        }
        let mut end = total.min(cursor.frame + max_frames.clamp(1, MAX_BATCH) as u64);
        if let Some(swap) = self.swaps.get(cursor.swap) {
            end = end.min(swap.frame);
        }
        end = end.min((cursor.frame / HASH_INTERVAL + 1) * HASH_INTERVAL);
        next.frame = end;
        Some((
            FeedMessage::Frames {
                first: cursor.frame,
                inputs: self.frames[cursor.frame as usize..end as usize].to_vec(),
            },
            next,
        ))
    }

    /// Encode messages from `cursor` until `max_bytes` is reached. A disk
    /// change is always returned whole and alone.
    pub fn encode_from(&self, cursor: FeedCursor, max_bytes: usize) -> (Vec<u8>, FeedCursor) {
        let mut out = Vec::new();
        let mut cursor = cursor;
        while out.len() < max_bytes {
            let Some((message, next)) = self.next_message(cursor, MAX_BATCH) else {
                break;
            };
            if matches!(message, FeedMessage::Swap(_)) && !out.is_empty() {
                break;
            }
            message.encode_into(&mut out);
            cursor = next;
        }
        (out, cursor)
    }

    pub fn head(&self) -> FeedMessage {
        FeedMessage::Head {
            frame: self.frames(),
        }
    }
}

/// A confirmed-only lockstep timeline: executes the host's inputs in order,
/// verifies checkpoints, and pauses for disk changes at their frame.
pub struct Spectator {
    decoder: FeedDecoder,
    identity: [u8; 32],
    executed: u64,
    inputs: VecDeque<[Input; 2]>,
    swaps: VecDeque<SwapRecord>,
    checkpoints: BTreeMap<u64, [u8; 32]>,
    head: u64,
    previous_keys: [u8; 16],
    checked: u64,
    swap_bytes: usize,
    swaps_applied: u64,
}

impl Spectator {
    pub fn new(identity: [u8; 32]) -> Self {
        Self {
            decoder: FeedDecoder::default(),
            identity,
            executed: 0,
            inputs: VecDeque::new(),
            swaps: VecDeque::new(),
            checkpoints: BTreeMap::new(),
            head: 0,
            previous_keys: [0; 16],
            checked: 0,
            swap_bytes: 0,
            swaps_applied: 0,
        }
    }

    pub fn identity(&self) -> [u8; 32] {
        self.identity
    }

    /// Feed bytes as they arrive; chunk boundaries are arbitrary.
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        self.decoder.push(bytes)?;
        while let Some(message) = self.decoder.next_message()? {
            self.receive(message)?;
        }
        Ok(())
    }

    pub fn receive(&mut self, message: FeedMessage) -> Result<()> {
        match message {
            FeedMessage::Frames { first, inputs } => {
                ensure!(
                    first == self.executed + self.inputs.len() as u64,
                    "spectator feed skipped or repeated frames"
                );
                ensure!(
                    self.inputs.len() + inputs.len() <= MAX_PENDING_FRAMES,
                    "spectator backlog exceeds the memory budget"
                );
                let end = first + inputs.len() as u64;
                self.inputs.extend(inputs);
                self.head = self.head.max(end);
            }
            FeedMessage::Checkpoint { frame, hash } => {
                ensure!(frame >= self.executed, "late spectator checkpoint");
                self.checkpoints.retain(|f, _| *f >= self.executed);
                ensure!(
                    self.checkpoints.len() < MAX_CHECKPOINTS,
                    "too many pending spectator checkpoints"
                );
                self.checkpoints.insert(frame, hash);
            }
            FeedMessage::Swap(swap) => {
                ensure!(
                    swap.frame >= self.executed + self.inputs.len() as u64
                        && self.swaps.back().is_none_or(|s| s.frame <= swap.frame),
                    "spectator disk change is out of order"
                );
                self.swap_bytes += swap.bytes.len();
                ensure!(
                    self.swap_bytes <= NATIVE_FEED_LIMIT,
                    "spectator disk changes exceed the memory budget"
                );
                self.swaps.push_back(swap);
            }
            FeedMessage::Head { frame } => self.head = self.head.max(frame),
            FeedMessage::Verified { .. } | FeedMessage::Status { .. } => {
                bail!("unexpected spectator message from the host")
            }
        }
        Ok(())
    }

    /// A disk change that must be applied before the next frame executes.
    pub fn due_swap(&self) -> Option<&SwapRecord> {
        self.swaps
            .front()
            .filter(|swap| swap.frame == self.executed)
    }

    pub fn swap_applied(&mut self) {
        if let Some(swap) = self.swaps.pop_front() {
            self.swap_bytes -= swap.bytes.len();
            self.swaps_applied += 1;
        }
    }

    /// Execute one frame of the emulated machine; see [`Self::step`].
    pub fn run_frame(&mut self, emu: &mut Emulator) -> Result<bool> {
        self.step(&mut super::EmulatedMachine(emu))
    }

    /// Compare the host's digest for the current frame if one is due; see
    /// [`Self::verify`]. Called after every service pass so a checkpoint
    /// that arrives once the spectator is level with the host is still
    /// checked without executing another frame. `false` means a checkpoint
    /// is due but its digest has not arrived: nothing may change the machine
    /// at this boundary yet, a disk change included.
    pub fn verify_frame(&mut self, emu: &mut Emulator) -> Result<bool> {
        self.verify(&super::EmulatedMachine(emu))
    }

    /// At a checkpoint frame, wait for the host's digest and compare it
    /// before anything else happens at that boundary. `false` means the
    /// digest is still on its way.
    fn verify(&mut self, machine: &impl Machine) -> Result<bool> {
        if self.executed == 0
            || !self.executed.is_multiple_of(HASH_INTERVAL)
            || self.checked >= self.executed
        {
            return Ok(true);
        }
        let Some(expected) = self.checkpoints.get(&self.executed) else {
            return Ok(false);
        };
        ensure!(
            digest(&machine.save()?) == *expected,
            "spectator desynchronized at frame {}",
            self.executed
        );
        self.checked = self.executed;
        self.checkpoints.retain(|f, _| *f > self.executed);
        Ok(true)
    }

    /// Execute one frame if its inputs are known and nothing is due first.
    /// At checkpoint frames the host's digest is awaited and compared before
    /// any disk change at the same boundary, matching the host's own order.
    pub(super) fn step(&mut self, machine: &mut impl Machine) -> Result<bool> {
        if !self.verify(machine)? {
            return Ok(false);
        }
        if self.due_swap().is_some() {
            return Ok(false);
        }
        let Some(inputs) = self.inputs.pop_front() else {
            return Ok(false);
        };
        machine.frame(inputs, self.previous_keys, false)?;
        self.previous_keys = Input::merged_keys(inputs);
        self.executed += 1;
        Ok(true)
    }

    pub fn executed(&self) -> u64 {
        self.executed
    }
    pub fn head(&self) -> u64 {
        self.head
    }
    pub fn behind(&self) -> u64 {
        self.head.saturating_sub(self.executed)
    }
    pub fn checked(&self) -> u64 {
        self.checked
    }
    pub fn swaps_applied(&self) -> u64 {
        self.swaps_applied
    }
    pub fn buffered(&self) -> usize {
        self.inputs.len()
    }
}

/// Insert or eject a session floppy image with canonical netplay metadata.
pub(super) fn change_floppy(
    emu: &mut Emulator,
    drive: usize,
    bytes: Vec<u8>,
    writable: bool,
) -> Result<()> {
    if bytes.is_empty() {
        emu.bus_mut().floppy.eject_disk_image(drive)
    } else {
        emu.bus_mut()
            .floppy
            .insert_memory_disk_image_bytes_with_limit(
                drive,
                bytes,
                format!("netplay-df{drive}").into(),
                !writable,
                SWAP_LIMIT,
            )
    }
}

/// Apply a host disk change at its boundary, checking the machine digest on
/// both sides so a diverged spectator stops instead of drifting further.
pub fn apply_swap(emu: &mut Emulator, swap: &SwapRecord) -> Result<()> {
    ensure!(
        digest(&emu.netplay_snapshot()?) == swap.before,
        "spectator differs from the host before the disk change at frame {}",
        swap.frame
    );
    change_floppy(emu, swap.drive, swap.bytes.to_vec(), swap.writable)?;
    ensure!(
        digest(&emu.netplay_snapshot()?) == swap.after,
        "spectator differs from the host after the disk change at frame {}",
        swap.frame
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Toy {
        state: u64,
    }

    impl Machine for Toy {
        fn save(&self) -> Result<Vec<u8>> {
            Ok(self.state.to_le_bytes().to_vec())
        }
        fn load(&mut self, bytes: &[u8]) -> Result<()> {
            self.state = u64::from_le_bytes(bytes.try_into().unwrap());
            Ok(())
        }
        fn frame(&mut self, input: [Input; 2], previous: [u8; 16], _replay: bool) -> Result<()> {
            self.state = self
                .state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(u64::from(input[0].buttons) + 100 * u64::from(input[1].buttons))
                .wrapping_add(u64::from(input[1].mouse_dx as u16));
            for (i, key) in Input::merged_keys(input).iter().enumerate() {
                self.state = self.state.wrapping_add(u64::from(key ^ previous[i]));
            }
            Ok(())
        }
    }

    fn input(frame: u64, player: u64) -> Input {
        let mut i = Input {
            buttons: ((frame * (player + 3) / 7) % 2048) as u16,
            mouse_dx: (frame % 19) as i16 - 9,
            mouse_dy: 11 - ((frame + player) % 23) as i16,
            mouse_buttons: ((frame / 3 + player) % 8) as u8,
            ..Default::default()
        };
        i.set_key(0x40, (frame + player) % 11 < 3);
        i
    }

    fn pair(frame: u64) -> [Input; 2] {
        [input(frame, 0), input(frame, 1)]
    }

    fn swap(frame: u64, bytes: Vec<u8>) -> SwapRecord {
        SwapRecord {
            frame,
            drive: 1,
            writable: !bytes.is_empty(),
            before: [1; 32],
            after: [2; 32],
            bytes: Arc::new(bytes),
        }
    }

    #[test]
    fn feed_messages_round_trip_across_arbitrary_chunks() -> Result<()> {
        let messages = vec![
            FeedMessage::Frames {
                first: 7,
                inputs: (7..10).map(pair).collect(),
            },
            FeedMessage::Checkpoint {
                frame: 60,
                hash: [9; 32],
            },
            FeedMessage::Swap(swap(60, vec![5; 1000])),
            FeedMessage::Head { frame: 99 },
            FeedMessage::Verified { identity: [3; 32] },
            FeedMessage::Status { frame: 5 },
            FeedMessage::Swap(swap(61, Vec::new())),
        ];
        let mut bytes = Vec::new();
        for message in &messages {
            message.encode_into(&mut bytes);
        }
        for chunk in [1usize, 7, 64, 4096] {
            let mut decoder = FeedDecoder::default();
            let mut decoded = Vec::new();
            for piece in bytes.chunks(chunk) {
                decoder.push(piece)?;
                while let Some(message) = decoder.next_message()? {
                    decoded.push(message);
                }
            }
            assert_eq!(decoded, messages, "chunk size {chunk}");
        }
        Ok(())
    }

    #[test]
    fn feed_decoder_rejects_invalid_headers_and_payloads() -> Result<()> {
        let header = |kind: u8, len: usize| {
            let mut bytes = vec![kind];
            bytes.extend((len as u32).to_le_bytes());
            bytes
        };
        // Invalid lengths fail on the header alone, before any payload.
        for (kind, len) in [
            (9, 0),
            (KIND_FRAMES, FRAMES_HEADER),
            (KIND_FRAMES, FRAMES_HEADER + RECORD + 1),
            (KIND_FRAMES, FRAMES_HEADER + (MAX_BATCH + 1) * RECORD),
            (KIND_CHECKPOINT, 39),
            (KIND_SWAP, SWAP_HEADER + SWAP_LIMIT + 1),
            (KIND_HEAD, 7),
            (KIND_VERIFIED, 31),
            (KIND_STATUS, 9),
        ] {
            let mut decoder = FeedDecoder::default();
            decoder.push(&header(kind, len))?;
            assert!(decoder.next_message().is_err(), "kind {kind} len {len}");
        }
        // Payload checks: a count that disagrees with the length, an
        // off-grid checkpoint, a bad drive, and controller bits outside
        // the mask.
        let mut frames = header(KIND_FRAMES, FRAMES_HEADER + RECORD);
        frames.extend(0u64.to_le_bytes());
        frames.extend(2u16.to_le_bytes());
        frames.extend([0; RECORD]);
        let mut decoder = FeedDecoder::default();
        decoder.push(&frames)?;
        assert!(decoder.next_message().is_err());
        let mut checkpoint = header(KIND_CHECKPOINT, CHECKPOINT_LEN);
        checkpoint.extend(61u64.to_le_bytes());
        checkpoint.extend([0; 32]);
        let mut decoder = FeedDecoder::default();
        decoder.push(&checkpoint)?;
        assert!(decoder.next_message().is_err());
        let mut bad_swap = FeedMessage::Swap(swap(5, vec![1; 4])).encode();
        bad_swap[HEADER + 8] = 4;
        let mut decoder = FeedDecoder::default();
        decoder.push(&bad_swap)?;
        assert!(decoder.next_message().is_err());
        let mut bad_input = FeedMessage::Frames {
            first: 0,
            inputs: vec![pair(0)],
        }
        .encode();
        bad_input[HEADER + FRAMES_HEADER + 1] = 0xff;
        let mut decoder = FeedDecoder::default();
        decoder.push(&bad_input)?;
        assert!(decoder.next_message().is_err());
        // The receive buffer is bounded.
        let mut decoder = FeedDecoder::default();
        assert!(decoder
            .push(&vec![0; 2 * (HEADER + MAX_MESSAGE) + 1])
            .is_err());
        Ok(())
    }

    fn describe(message: &FeedMessage) -> String {
        match message {
            FeedMessage::Frames { first, inputs } => {
                format!("frames {first}..{}", first + inputs.len() as u64)
            }
            FeedMessage::Checkpoint { frame, .. } => format!("checkpoint {frame}"),
            FeedMessage::Swap(swap) => format!("swap {}", swap.frame),
            FeedMessage::Head { frame } => format!("head {frame}"),
            FeedMessage::Verified { .. } => "verified".into(),
            FeedMessage::Status { frame } => format!("status {frame}"),
        }
    }

    #[test]
    fn feed_streams_checkpoints_and_disk_changes_in_replay_order() -> Result<()> {
        let mut feed = Feed::new(1 << 20);
        for frame in 0..130 {
            feed.record_frame(frame, pair(frame))?;
            if (frame + 1).is_multiple_of(60) {
                feed.record_checkpoint(frame + 1, [frame as u8; 32])?;
            }
        }
        assert!(feed.record_swap(swap(129, vec![0; 10])).is_err());
        assert!(feed.record_frame(131, pair(131)).is_err());
        assert!(feed.record_checkpoint(120, [0; 32]).is_err());
        feed.record_swap(swap(130, vec![0; 10]))?;
        for frame in 130..200 {
            feed.record_frame(frame, pair(frame))?;
            if (frame + 1).is_multiple_of(60) {
                feed.record_checkpoint(frame + 1, [frame as u8; 32])?;
            }
        }
        let expected = [
            "frames 0..60",
            "checkpoint 60",
            "frames 60..120",
            "checkpoint 120",
            "frames 120..130",
            "swap 130",
            "frames 130..180",
            "checkpoint 180",
            "frames 180..200",
        ];
        let mut cursor = FeedCursor::default();
        let mut sequence = Vec::new();
        while let Some((message, next)) = feed.next_message(cursor, MAX_BATCH) {
            sequence.push(describe(&message));
            cursor = next;
        }
        assert_eq!(sequence, expected);
        assert_eq!(cursor.frame, 200);
        // Byte budgets split the same sequence; a disk change never shares
        // a call with earlier messages.
        let mut cursor = FeedCursor::default();
        let mut calls = Vec::new();
        loop {
            let (bytes, next) = feed.encode_from(cursor, 1500);
            if bytes.is_empty() {
                break;
            }
            let mut decoder = FeedDecoder::default();
            decoder.push(&bytes)?;
            let mut call = Vec::new();
            while let Some(message) = decoder.next_message()? {
                call.push(describe(&message));
            }
            calls.push(call);
            cursor = next;
        }
        let flat: Vec<_> = calls.iter().flatten().cloned().collect();
        assert_eq!(flat, expected);
        assert!(
            calls
                .iter()
                .all(|call| call.iter().all(|m| !m.starts_with("swap"))
                    || call[0].starts_with("swap"))
        );
        assert_eq!(feed.head(), FeedMessage::Head { frame: 200 });
        // A small budget refuses more spectators once history outgrows it.
        let mut small = Feed::new(100);
        small.record_frame(0, pair(0))?;
        assert!(!small.full());
        small.record_swap(swap(1, vec![0; 200]))?;
        assert!(small.full());
        Ok(())
    }

    #[test]
    fn spectator_replays_the_feed_and_verifies_checkpoints() -> Result<()> {
        let mut baseline = Toy::default();
        let mut previous = [0; 16];
        let mut feed = Feed::new(1 << 20);
        for frame in 0..200 {
            let inputs = pair(frame);
            baseline.frame(inputs, previous, false)?;
            previous = Input::merged_keys(inputs);
            feed.record_frame(frame, inputs)?;
            if (frame + 1).is_multiple_of(60) {
                feed.record_checkpoint(frame + 1, digest(&baseline.save()?))?;
            }
            // One change on a checkpoint boundary, one between boundaries.
            if frame + 1 == 120 || frame + 1 == 130 {
                feed.record_swap(swap(frame + 1, vec![0; 4]))?;
            }
        }
        let mut spectator = Spectator::new([0; 32]);
        let mut toy = Toy::default();
        let mut cursor = FeedCursor::default();
        let mut steps = 0;
        let mut waited = Vec::new();
        loop {
            let (bytes, next) = feed.encode_from(cursor, 500);
            cursor = next;
            for piece in bytes.chunks(13) {
                spectator.push(piece)?;
            }
            loop {
                if let Some(due) = spectator.due_swap().map(|swap| swap.frame) {
                    assert_eq!(due, spectator.executed());
                    assert!(!spectator.step(&mut toy)?, "a due change blocks the frame");
                    // On a boundary the host's digest was compared first,
                    // against the machine before the change.
                    assert!(
                        spectator.verify(&toy)?,
                        "the boundary is settled before the change"
                    );
                    assert_eq!(spectator.checked(), spectator.executed() / 60 * 60);
                    spectator.swap_applied();
                    waited.push(due);
                }
                if !spectator.step(&mut toy)? {
                    break;
                }
                steps += 1;
            }
            if bytes.is_empty() {
                break;
            }
        }
        assert_eq!(steps, 200);
        assert_eq!(waited, [120, 130]);
        assert_eq!(toy.state, baseline.state);
        assert_eq!(spectator.checked(), 180);
        assert_eq!(spectator.swaps_applied(), 2);
        assert_eq!((spectator.head(), spectator.behind()), (200, 0));

        // A checkpoint that disagrees stops the replay at its frame.
        let mut spectator = Spectator::new([0; 32]);
        let mut toy = Toy::default();
        spectator.receive(FeedMessage::Frames {
            first: 0,
            inputs: (0..61).map(pair).collect(),
        })?;
        for _ in 0..60 {
            assert!(spectator.step(&mut toy)?);
        }
        assert!(!spectator.step(&mut toy)?, "waits for the host's digest");
        spectator.receive(FeedMessage::Checkpoint {
            frame: 60,
            hash: [1; 32],
        })?;
        let error = spectator.step(&mut toy).unwrap_err().to_string();
        assert!(error.contains("desynchronized at frame 60"), "{error}");

        // Frames, checkpoints and changes must arrive in replay order.
        let mut spectator = Spectator::new([0; 32]);
        assert!(spectator
            .receive(FeedMessage::Frames {
                first: 5,
                inputs: vec![pair(5)],
            })
            .is_err());
        spectator.receive(FeedMessage::Frames {
            first: 0,
            inputs: (0..10).map(pair).collect(),
        })?;
        assert!(spectator
            .receive(FeedMessage::Swap(swap(9, Vec::new())))
            .is_err());
        spectator.receive(FeedMessage::Swap(swap(10, Vec::new())))?;
        assert!(spectator
            .receive(FeedMessage::Swap(swap(9, Vec::new())))
            .is_err());
        let mut toy = Toy::default();
        while spectator.step(&mut toy)? {}
        assert_eq!(spectator.executed(), 10);
        assert!(spectator
            .receive(FeedMessage::Checkpoint {
                frame: 0,
                hash: [0; 32]
            })
            .is_err());
        assert!(spectator.receive(FeedMessage::Status { frame: 1 }).is_err());
        Ok(())
    }
}
