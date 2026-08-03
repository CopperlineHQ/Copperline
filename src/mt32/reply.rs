// SPDX-License-Identifier: GPL-3.0-or-later

//! What the MT-32 sends back down its MIDI OUT.
//!
//! The module has no keyboard, so everything on its MIDI OUT is an answer to
//! a request: a librarian or editor sends an RQ1 asking for a stretch of
//! memory, and the module returns it as one or more DT1 blocks. That round
//! trip is the whole reason a patch editor needs a MIDI IN back to the
//! Amiga.
//!
//! The synthesis engine holds the memory but does not answer for it -- its
//! `Synth::readSysex` is unimplemented -- so the reply is assembled here from
//! what the engine will hand over, in the same frame format Copperline
//! already uses to write to it.

use super::Mt32Synth;

/// What an exclusive message opens with, after `F0`: the manufacturer ID,
/// then the device number, then the MT-32's model ID.
const MAKER_ID: u8 = 0x41;
const MODEL_MT32: u8 = 0x16;
/// Command bytes: request one, data one.
const CMD_RQ1: u8 = 0x11;
const CMD_DT1: u8 = 0x12;

/// Data bytes per reply block. A dump larger than this comes back as several
/// DT1 messages, as it does from the hardware, so a receiver sees frames of
/// a size it is built for rather than one enormous one.
const BLOCK_BYTES: usize = 256;

/// Longest dump that will be answered, 64 KB: comfortably past every area
/// the module has, and short enough that a request asking for the whole
/// three-byte address space cannot size the reply.
const MAX_DUMP_BYTES: u32 = 0x10000;

/// The checksum: the seven-bit two's complement of the sum, so that the
/// address, the data and the checksum together come to a multiple of 128.
fn checksum(bytes: &[u8]) -> u8 {
    let sum: u32 = bytes.iter().map(|&b| u32::from(b) & 0x7F).sum();
    ((128 - (sum % 128)) % 128) as u8
}

/// An address as its three seven-bit bytes.
fn address_bytes(addr: u32) -> [u8; 3] {
    [
        ((addr >> 14) & 0x7F) as u8,
        ((addr >> 7) & 0x7F) as u8,
        (addr & 0x7F) as u8,
    ]
}

/// The engine's flat address for one written the way the manual prints it,
/// a byte per pair of hex digits (`0x10_0016` is the bytes `10 00 16`).
///
/// [`super::addr`] is written that way because that is how it reaches the
/// engine -- as the three address bytes of a message -- and this is the same
/// conversion the engine applies on the way in.
pub fn linear(packed: u32) -> u32 {
    ((packed & 0x7F_0000) >> 2) | ((packed & 0x7F00) >> 1) | (packed & 0x7F)
}

/// The address three seven-bit bytes stand for.
fn address_value(bytes: &[u8]) -> u32 {
    (u32::from(bytes[0] & 0x7F) << 14)
        | (u32::from(bytes[1] & 0x7F) << 7)
        | u32::from(bytes[2] & 0x7F)
}

/// The address as the manual prints it, a byte per pair of hex digits --
/// what [`super::addr`] holds, and what [`Mt32Synth::read_memory`] takes.
fn printed(bytes: [u8; 3]) -> u32 {
    (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2])
}

/// One DT1 message carrying `data` at `addr`.
fn data_block(device: u8, addr: [u8; 3], data: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(10 + data.len());
    msg.extend_from_slice(&[0xF0, MAKER_ID, device, MODEL_MT32, CMD_DT1]);
    msg.extend_from_slice(&addr);
    msg.extend_from_slice(data);
    let sum = checksum(&msg[5..]);
    msg.push(sum);
    msg.push(0xF7);
    msg
}

/// A dump request the module recognised: where to read and how much.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    device: u8,
    addr: u32,
    len: u32,
}

/// Read an RQ1 out of one complete exclusive message, `F0` to `F7`.
///
/// Anything that is not a dump request for this model -- a DT1 going the
/// other way, another maker's message, a truncated or mis-summed one -- is
/// not answered, which is what the hardware does with them too.
fn parse_request(msg: &[u8]) -> Option<Request> {
    // F0, maker, device, model, command, 3 address, 3 size, checksum, F7.
    if msg.len() != 13 || msg[0] != 0xF0 || *msg.last()? != 0xF7 {
        return None;
    }
    if msg[1] != MAKER_ID || msg[3] != MODEL_MT32 || msg[4] != CMD_RQ1 {
        return None;
    }
    let body = &msg[5..11];
    if checksum(body) != msg[11] {
        return None;
    }
    let len = address_value(&body[3..6]);
    if len == 0 || len > MAX_DUMP_BYTES {
        return None;
    }
    Some(Request {
        device: msg[2],
        addr: address_value(&body[0..3]),
        len,
    })
}

/// The reply to one request, as the bytes to put on the wire.
pub fn answer(synth: &Mt32Synth, request: Request) -> Vec<u8> {
    let mut out = Vec::new();
    let mut addr = request.addr;
    let mut left = request.len as usize;
    let mut buf = vec![0u8; BLOCK_BYTES];
    while left > 0 {
        let take = left.min(BLOCK_BYTES);
        let block = &mut buf[..take];
        // The wire counts addresses seven bits to the byte, so a block
        // advances by its own length; the module is asked in the form it
        // prints them.
        let bytes = address_bytes(addr);
        if !synth.read_memory(printed(bytes), block) {
            // No such region. The hardware answers a bad address with
            // silence rather than with a block of nothing.
            break;
        }
        out.extend_from_slice(&data_block(request.device, bytes, block));
        addr += take as u32;
        left -= take;
    }
    out
}

/// Watches what goes to the MT-32 and produces what comes back.
///
/// Exclusive messages arrive a byte at a time from Paula and may be split
/// across writes, so the message is gathered here before it is read. Only
/// exclusive traffic is collected: everything else the guest sends is a
/// channel message the module never answers.
#[derive(Debug, Default)]
pub struct Responder {
    /// The exclusive message being gathered, `F0` onwards.
    pending: Vec<u8>,
}

/// Longest exclusive message gathered before giving up on it. Comfortably
/// past the largest DT1 a guest sends, and short enough that a stream that
/// never delivers its `F7` cannot grow without bound.
const MAX_MESSAGE_BYTES: usize = 1024;

impl Responder {
    /// Take a byte on its way to the module, and report the request it
    /// completed, if it completed one. [`answer`] turns that into the bytes
    /// to hand back to the guest.
    pub fn write_byte(&mut self, b: u8) -> Option<Request> {
        match b {
            0xF0 => {
                self.pending.clear();
                self.pending.push(b);
            }
            0xF7 => {
                if self.pending.is_empty() {
                    return None;
                }
                self.pending.push(b);
                let msg = std::mem::take(&mut self.pending);
                return parse_request(&msg);
            }
            // A realtime byte may interrupt an exclusive message without
            // ending it, so it passes through and the message goes on.
            0xF8..=0xFF => {}
            // Any other status byte abandons the message: the guest has
            // moved on to something else and the `F7` is never coming.
            0x80..=0xEF => self.pending.clear(),
            _ => {
                if !self.pending.is_empty() {
                    if self.pending.len() >= MAX_MESSAGE_BYTES {
                        self.pending.clear();
                    } else {
                        self.pending.push(b);
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `F0 41 10 16 11 <addr> <size> <checksum> F7`.
    fn request_bytes(addr: u32, len: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&address_bytes(addr));
        body.extend_from_slice(&address_bytes(len));
        let mut msg = vec![0xF0, MAKER_ID, 0x10, MODEL_MT32, CMD_RQ1];
        msg.extend_from_slice(&body);
        msg.push(checksum(&body));
        msg.push(0xF7);
        msg
    }

    #[test]
    fn a_request_is_read_back_out_of_its_bytes() {
        let msg = request_bytes(linear(super::super::addr::PATCH_TEMP), 16);
        assert_eq!(
            parse_request(&msg),
            Some(Request {
                device: 0x10,
                addr: linear(super::super::addr::PATCH_TEMP),
                len: 16,
            })
        );
    }

    #[test]
    fn a_request_that_does_not_add_up_is_not_answered() {
        let mut msg = request_bytes(linear(super::super::addr::PATCH_TEMP), 16);
        let last = msg.len() - 2;
        msg[last] ^= 0x01;
        assert_eq!(parse_request(&msg), None);
    }

    #[test]
    fn other_makers_and_the_other_direction_are_left_alone() {
        // A different maker's ID.
        let mut other = request_bytes(linear(super::super::addr::PATCH_TEMP), 16);
        other[1] = 0x43;
        assert_eq!(parse_request(&other), None);

        // A DT1 going to the module, not a request coming out of it.
        let mut write = request_bytes(linear(super::super::addr::PATCH_TEMP), 16);
        write[4] = CMD_DT1;
        assert_eq!(parse_request(&write), None);
    }

    #[test]
    fn an_absurd_length_is_refused() {
        assert_eq!(parse_request(&request_bytes(0, MAX_DUMP_BYTES + 1)), None);
        assert_eq!(parse_request(&request_bytes(0, 0)), None);
    }

    #[test]
    fn a_block_carries_its_address_and_adds_up() {
        let block = data_block(0x10, address_bytes(linear(0x03_0000)), &[1, 2, 3]);
        assert_eq!(block[..5], [0xF0, MAKER_ID, 0x10, MODEL_MT32, CMD_DT1]);
        assert_eq!(block[5..8], [0x03, 0x00, 0x00]);
        assert_eq!(block[8..11], [1, 2, 3]);
        assert_eq!(*block.last().unwrap(), 0xF7);
        // Address, data and checksum together are a multiple of 128.
        let sum: u32 = block[5..block.len() - 1]
            .iter()
            .map(|&b| u32::from(b))
            .sum();
        assert_eq!(sum % 128, 0);
    }

    #[test]
    fn a_seven_bit_address_survives_the_round_trip() {
        for addr in [0, 0xC000, 0x40016, 0x1F_FFFF] {
            assert_eq!(address_value(&address_bytes(addr)), addr);
        }
    }

    #[test]
    fn the_manual_s_addresses_convert_to_the_engine_s() {
        // System area, master volume: bytes 10 00 16.
        assert_eq!(linear(0x10_0016), (0x10 << 14) | 0x16);
        // Patch temporary area: bytes 03 00 00.
        assert_eq!(linear(0x03_0000), 0x03 << 14);
        assert_eq!(address_bytes(linear(0x10_0016)), [0x10, 0x00, 0x16]);
        // And back again, which is the form the engine is asked in.
        assert_eq!(printed(address_bytes(linear(0x10_0016))), 0x10_0016);
    }

    /// Feed a whole message in and report what it asked for.
    fn feed(responder: &mut Responder, bytes: &[u8]) -> Option<Request> {
        let mut request = None;
        for &b in bytes {
            if let Some(r) = responder.write_byte(b) {
                request = Some(r);
            }
        }
        request
    }

    #[test]
    fn only_exclusive_traffic_is_gathered() {
        let mut responder = Responder::default();
        // Note on, note off: nothing to answer, nothing kept.
        assert!(feed(&mut responder, &[0x91, 0x3C, 0x40, 0x81, 0x3C, 0x00]).is_none());
        assert!(responder.pending.is_empty());
    }

    #[test]
    fn a_request_split_across_writes_still_arrives() {
        let msg = request_bytes(linear(super::super::addr::MASTER_VOLUME), 1);
        let mut responder = Responder::default();
        // A byte at a time is how it comes off Paula's serial line.
        assert!(feed(&mut responder, &msg[..4]).is_none());
        assert!(feed(&mut responder, &msg[4..]).is_some());
    }

    #[test]
    fn a_clock_byte_through_the_middle_does_not_break_it() {
        let msg = request_bytes(linear(super::super::addr::MASTER_VOLUME), 1);
        let mut responder = Responder::default();
        let mut with_clock = msg[..6].to_vec();
        with_clock.push(0xF8);
        with_clock.extend_from_slice(&msg[6..]);
        assert_eq!(feed(&mut responder, &with_clock), parse_request(&msg));
    }

    #[test]
    fn a_status_byte_through_the_middle_abandons_the_message() {
        let msg = request_bytes(linear(super::super::addr::MASTER_VOLUME), 1);
        let mut responder = Responder::default();
        let mut interrupted = msg[..6].to_vec();
        // A note on: the guest has moved on, and the F7 is never coming.
        interrupted.extend_from_slice(&[0x91, 0x3C, 0x40]);
        interrupted.extend_from_slice(&msg[6..]);
        assert_eq!(feed(&mut responder, &interrupted), None);
        assert!(responder.pending.is_empty());

        // And the next whole message is still read normally.
        assert!(feed(&mut responder, &msg).is_some());
    }

    #[test]
    fn a_message_that_never_ends_does_not_grow_without_bound() {
        let mut responder = Responder::default();
        responder.write_byte(0xF0);
        for _ in 0..MAX_MESSAGE_BYTES * 3 {
            responder.write_byte(0x00);
            assert!(responder.pending.len() <= MAX_MESSAGE_BYTES);
        }
    }
}
