// SPDX-License-Identifier: LGPL-2.1-or-later

//! What the module sends back down its MIDI OUT.
//!
//! The module has no keyboard, so everything on its OUT jack is an
//! answer: a librarian sends an RQ1 asking for a stretch of memory, and
//! the module returns it as one or more DT1 blocks. The reference engine
//! never implemented that answer -- its `readSysex` is empty -- so this
//! half is held to the hardware's documented behaviour rather than to a
//! differential: blocks of at most 256 data bytes, each framed and
//! checksummed like any DT1, an unreadable address answered with silence,
//! and an absurd request not answered at all.
//!
//! Carried over from the player Copperline grew beside the reference
//! engine, reshaped onto this crate's memory model.

use crate::memory::{flat, Memory};
use crate::sysex;

/// Data bytes per reply block. A larger dump comes back as several DT1
/// messages, as it does from the hardware, so a receiver sees frames of
/// a size it is built for rather than one enormous one.
const BLOCK_BYTES: usize = 256;

/// The longest dump answered, 64 KiB: comfortably past every area the
/// module has, and short enough that a request asking for the whole
/// three-byte address space cannot size the reply.
const MAX_DUMP_BYTES: u32 = 0x10000;

/// The printed form of a flat seven-bit address: a byte per pair of hex
/// digits, the way the manual writes them and [sysex::dt1] takes them.
fn printed(flat_addr: u32) -> u32 {
    ((flat_addr & 0x1F_C000) << 2) | ((flat_addr & 0x3F80) << 1) | (flat_addr & 0x7F)
}

/// The reply to one read request, as the bytes to put on the wire:
/// the printed address and length exactly as the RQ1 carried them.
pub fn answer(memory: &mut Memory, printed_addr: u32, printed_len: u32) -> Vec<u8> {
    let len = flat(printed_len);
    if len == 0 || len > MAX_DUMP_BYTES {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut addr = flat(printed_addr);
    let mut left = len as usize;
    let mut buf = [0u8; BLOCK_BYTES];
    while left > 0 {
        let take = left.min(BLOCK_BYTES);
        let block = &mut buf[..take];
        if memory.read(addr, block) != take {
            // The hardware answers a bad address with silence rather
            // than with a block of nothing.
            break;
        }
        out.extend_from_slice(&sysex::dt1(printed(addr), block));
        addr += take as u32;
        left -= take;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LAYOUTS;

    fn a_memory() -> Memory {
        // A blank image with permissive limits, as the SysEx tests use.
        let image = vec![0x7F; 64 * 1024];
        Memory::power_on(&image, &LAYOUTS[0]).expect("opens")
    }

    /// A small request comes back as one DT1 carrying exactly the bytes
    /// the memory holds, addressed and checksummed; played back into a
    /// second module, it writes what it read.
    #[test]
    fn a_dump_reads_back_what_memory_holds() {
        let mut memory = a_memory();
        let reply = answer(&mut memory, 0x10_0000, 0x17);
        assert_eq!(reply[..5], [0xF0, 0x41, 0x10, 0x16, 0x12]);
        assert_eq!(reply[5..8], [0x10, 0x00, 0x00], "the address it answers");
        assert_eq!(*reply.last().unwrap(), 0xF7);
        let mut system = vec![0u8; 0x17];
        memory.read(flat(0x10_0000), &mut system);
        assert_eq!(reply[8..8 + 0x17], system, "the bytes are the memory's");

        let mut other = a_memory();
        assert!(matches!(
            sysex::play(&mut other, &reply),
            sysex::Outcome::Written(_)
        ));
    }

    /// A dump larger than a block comes back as several messages, each
    /// carrying its own address, together covering the whole stretch.
    #[test]
    fn a_long_dump_arrives_in_blocks() {
        let mut memory = a_memory();
        // Six patches past a whole block: 0x2A8 bytes of the patch area.
        let reply = answer(&mut memory, 0x05_0000, 0x0528);
        let frames = reply.iter().filter(|&&b| b == 0xF0).count();
        assert_eq!(frames, 3, "two full blocks and the remainder");
        assert_eq!(reply.iter().filter(|&&b| b == 0xF7).count(), 3);
    }

    /// An unreadable address, a zero length and an absurd length are all
    /// answered the hardware's way: with silence.
    #[test]
    fn what_cannot_be_answered_is_not() {
        let mut memory = a_memory();
        assert!(answer(&mut memory, 0x7F_0000, 0x10).is_empty());
        assert!(answer(&mut memory, 0x10_0000, 0).is_empty());
        assert!(answer(&mut memory, 0x03_0000, 0x7F_7F7F).is_empty());
    }

    /// The printed form survives the round trip through the flat one.
    #[test]
    fn addresses_survive_the_round_trip() {
        for addr in [0u32, 0x03_0110, 0x10_0016, 0x7F_7F7F] {
            assert_eq!(printed(flat(addr)), addr);
        }
    }
}
