// SPDX-License-Identifier: LGPL-2.1-or-later

//! SysEx: the framing the module strips, and the commands inside it.
//!
//! Everything addressed arrives as `F0 41 <device> 16 <command> <body> F7`:
//! Roland's manufacturer byte, a device number, the model, and one of the
//! handshake or one-way commands. The one-way pair is what everything real
//! uses -- DT1 writes memory, RQ1 asks for it back -- and the handshake
//! forms DAT and RQD are taken the same way, as the engine takes them.
//!
//! The checksum runs over the body and is checked before anything else, as
//! on the real units; a message that fails it does nothing but light the
//! error on the display.

use crate::memory::{flat, Memory, Touched};

const MANUFACTURER_ROLAND: u8 = 0x41;
const MODEL_MT32: u8 = 0x16;
const MODEL_D50: u8 = 0x14;

const CMD_RQ1: u8 = 0x11;
const CMD_DT1: u8 = 0x12;
const CMD_WSD: u8 = 0x40;
const CMD_RQD: u8 = 0x41;
const CMD_DAT: u8 = 0x42;
const CMD_EOD: u8 = 0x45;

/// What a SysEx message asked of the module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Not for this module, malformed, or a command it ignores.
    Ignored,
    /// The checksum failed; the display shows its error.
    ChecksumError,
    /// A write landed; what it touched, in order.
    Written(Vec<Touched>),
    /// The whole module was reset by the 0x7F address.
    Reset,
    /// A short write to the display area: a control message, not text.
    DisplayControl(Vec<u8>),
    /// A read request: the printed address and how many bytes to answer
    /// with. Answering is the caller's business -- the engine's own MIDI
    /// OUT is whatever the host wires up.
    ReadRequest { printed_addr: u32, len: u32 },
}

/// The checksum the body must end with: the low seven bits of the sum's
/// negation.
pub fn checksum(body: &[u8]) -> u8 {
    let sum: u32 = body.iter().map(|&b| u32::from(b)).sum();
    (sum.wrapping_neg() & 0x7F) as u8
}

/// One complete message, frame and all, played into `memory`.
///
/// Only device-global messages (device 0x10) reach memory here; the
/// channel-addressed forms below 0x10 need the channel-to-part map, which
/// lives with the parts and joins in a later phase.
pub fn play(memory: &mut Memory, message: &[u8]) -> Outcome {
    // Junk after the terminator is commonplace, so the end is found, not
    // trusted from the length.
    if message.first() != Some(&0xF0) {
        return Outcome::Ignored;
    }
    let Some(end) = message[1..].iter().position(|&b| b == 0xF7) else {
        return Outcome::Ignored;
    };
    let inner = &message[1..1 + end];
    if inner.len() < 4
        || inner[0] != MANUFACTURER_ROLAND
        || inner[2] == MODEL_D50
        || inner[2] != MODEL_MT32
    {
        return Outcome::Ignored;
    }
    let device = inner[1];
    let command = inner[3];
    let body = &inner[4..];
    if device > 0x10 {
        return Outcome::Ignored;
    }
    // The checksum comes before everything, even command dispatch.
    if body.len() < 2 {
        return Outcome::Ignored;
    }
    let (payload, check) = body.split_at(body.len() - 1);
    if checksum(payload) != check[0] {
        return Outcome::ChecksumError;
    }
    match command {
        CMD_DT1 | CMD_DAT => write(memory, device, payload),
        CMD_RQ1 | CMD_RQD => read_request(payload),
        CMD_WSD | CMD_EOD => Outcome::Ignored,
        _ => Outcome::Ignored,
    }
}

/// A DT1 body: three address bytes and the data. The reset address answers
/// before anything is length-checked, as the real units do.
fn write(memory: &mut Memory, device: u8, payload: &[u8]) -> Outcome {
    match payload {
        [0x7F, ..] => return Outcome::Reset,
        [0x20, ..] if payload.len() < 3 => {
            return Outcome::DisplayControl(payload.to_vec());
        }
        _ if payload.len() < 3 => return Outcome::Ignored,
        _ => {}
    }
    let addr =
        flat(u32::from(payload[0]) << 16 | u32::from(payload[1]) << 8 | u32::from(payload[2]));
    let data = &payload[3..];
    if device < 0x10 {
        // Channel-addressed: remapped through the channel-to-part table,
        // which arrives with the parts.
        return Outcome::Ignored;
    }
    Outcome::Written(memory.write(addr, data))
}

/// An RQ1 body: the printed address and a three-byte length.
fn read_request(payload: &[u8]) -> Outcome {
    if payload.len() != 6 {
        return Outcome::Ignored;
    }
    Outcome::ReadRequest {
        printed_addr: u32::from(payload[0]) << 16
            | u32::from(payload[1]) << 8
            | u32::from(payload[2]),
        len: u32::from(payload[3]) << 16 | u32::from(payload[4]) << 8 | u32::from(payload[5]),
    }
}

/// A framed DT1, built the way the module expects to receive one: for the
/// tests here, and for any caller with something to say to a module.
pub fn dt1(printed_addr: u32, data: &[u8]) -> Vec<u8> {
    let mut body = vec![
        ((printed_addr >> 16) & 0x7F) as u8,
        ((printed_addr >> 8) & 0x7F) as u8,
        (printed_addr & 0x7F) as u8,
    ];
    body.extend_from_slice(data);
    let mut message = vec![0xF0, MANUFACTURER_ROLAND, 0x10, MODEL_MT32, CMD_DT1];
    message.extend_from_slice(&body);
    message.push(checksum(&body));
    message.push(0xF7);
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LAYOUTS;
    use crate::memory::Region;

    fn a_memory() -> Memory {
        // A blank 64 KiB image is not a real ROM, but power_on only reads
        // tables from it, and zeroed limit tables just write-protect
        // everything -- which the system max table would too. Give the
        // tests real limits instead: a ROM of 0x7F everywhere.
        let image = vec![0x7F; 64 * 1024];
        Memory::power_on(&image, &LAYOUTS[0]).expect("opens")
    }

    /// A framed DT1 writes, and reads back what it wrote.
    #[test]
    fn a_write_lands_and_reads_back() {
        let mut memory = a_memory();
        let message = dt1(0x10_0001, &[2]);
        let outcome = play(&mut memory, &message);
        assert_eq!(
            outcome,
            Outcome::Written(vec![Touched::Ram {
                region: Region::System,
                offset: 1,
                len: 1,
            }])
        );
        let mut back = [0u8; 2];
        memory.read(flat(0x10_0000), &mut back);
        assert_eq!(back[1], 2, "reverb mode took the write");
    }

    /// The checksum gate: one wrong byte and nothing happens but the
    /// error, exactly as the units behave.
    #[test]
    fn a_bad_checksum_writes_nothing() {
        let mut memory = a_memory();
        let mut message = dt1(0x10_0001, &[2]);
        let at = message.len() - 2;
        message[at] ^= 1;
        assert_eq!(play(&mut memory, &message), Outcome::ChecksumError);
        let mut back = [0u8; 2];
        memory.read(flat(0x10_0000), &mut back);
        assert_eq!(back[1], 0, "reverb mode kept its power-on value");
    }

    /// The special addresses: 0x7F resets, a short 0x20 write is display
    /// control, and junk after the terminator does not confuse the frame.
    #[test]
    fn the_special_addresses_answer() {
        let mut memory = a_memory();
        let body = [0x7F, 0x00, 0x00];
        let mut message = vec![0xF0, 0x41, 0x10, 0x16, CMD_DT1];
        message.extend_from_slice(&body);
        message.push(checksum(&body));
        message.push(0xF7);
        message.extend_from_slice(&[0x55, 0xAA]);
        assert_eq!(play(&mut memory, &message), Outcome::Reset);

        let body = [0x20, 0x01];
        let mut message = vec![0xF0, 0x41, 0x10, 0x16, CMD_DT1];
        message.extend_from_slice(&body);
        message.push(checksum(&body));
        message.push(0xF7);
        assert_eq!(
            play(&mut memory, &message),
            Outcome::DisplayControl(vec![0x20, 0x01])
        );
    }

    /// RQ1 comes back as a request to answer, with the manual's spelling
    /// of the address.
    #[test]
    fn a_read_request_is_handed_up() {
        let mut memory = a_memory();
        let body = [0x10, 0x00, 0x00, 0x00, 0x00, 0x17];
        let mut message = vec![0xF0, 0x41, 0x10, 0x16, CMD_RQ1];
        message.extend_from_slice(&body);
        message.push(checksum(&body));
        message.push(0xF7);
        assert_eq!(
            play(&mut memory, &message),
            Outcome::ReadRequest {
                printed_addr: 0x10_0000,
                len: 0x17
            }
        );
    }

    /// Messages for someone else pass straight through.
    #[test]
    fn what_is_not_for_this_module_is_ignored() {
        let mut memory = a_memory();
        for message in [
            &[0xF0u8, 0x43, 0x10, 0x16, 0x12, 0x00, 0xF7][..], // another maker
            &[0xF0, 0x41, 0x10, 0x14, 0x12, 0x00, 0xF7],       // a D-50
            &[0xF0, 0x41, 0x11, 0x16, 0x12, 0x00, 0xF7],       // another device
            &[0xF0, 0x41, 0x10, 0x16, 0x12, 0x00],             // never terminated
        ] {
            assert_eq!(
                play(&mut memory, message),
                Outcome::Ignored,
                "{message:02X?}"
            );
        }
    }
}
