// SPDX-License-Identifier: GPL-3.0-or-later

//! Call-stack reconstruction for the 68000: call-frame information
//! where the program's DWARF covers the PC, and otherwise the same
//! return-address scan the debugger console's `STACK` command uses (a
//! stack slot whose preceding instruction is a `JSR`/`BSR`).

use super::dwarf::RegRule;
use super::DebugInfo;

/// The CPU registers a frame is described by, DWARF-numbered (0-7 =
/// D0-D7, 8-15 = A0-A7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Registers {
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub pc: u32,
}

impl Registers {
    pub fn get(&self, reg: u16) -> Option<u32> {
        match reg {
            0..=7 => Some(self.d[reg as usize]),
            8..=15 => Some(self.a[reg as usize - 8]),
            _ => None,
        }
    }

    pub fn set(&mut self, reg: u16, value: u32) {
        match reg {
            0..=7 => self.d[reg as usize] = value,
            8..=15 => self.a[reg as usize - 8] = value,
            _ => {}
        }
    }

    pub fn sp(&self) -> u32 {
        self.a[7]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameVia {
    /// The innermost frame: the live registers.
    #[default]
    Entry,
    /// Recovered through call-frame information.
    Cfi,
    /// Guessed from a return address found on the stack.
    Scan,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub pc: u32,
    /// The stack pointer in this frame (the CFA of the callee).
    pub sp: u32,
    /// The registers as far as they are known in this frame.
    pub regs: Registers,
    pub via: FrameVia,
}

/// How many stack slots the return-address scan looks through.
const SCAN_SLOTS: u32 = 64;

/// Walk the stack from `regs`, at most `max_frames` deep. `read32` /
/// `read16` read guest memory (big-endian), returning `None` for an
/// unreadable address.
pub fn unwind(
    info: &DebugInfo,
    regs: &Registers,
    read32: &mut dyn FnMut(u32) -> Option<u32>,
    read16: &mut dyn FnMut(u32) -> Option<u16>,
    max_frames: usize,
) -> Vec<Frame> {
    let mut frames = vec![Frame {
        pc: regs.pc,
        sp: regs.sp(),
        regs: *regs,
        via: FrameVia::Entry,
    }];
    while frames.len() < max_frames {
        let cur = frames[frames.len() - 1];
        let next = cfi_step(info, &cur.regs, read32)
            .or_else(|| scan_step(info, &cur.regs, read32, read16));
        let Some(next) = next else { break };
        if next.pc == 0 || frames.iter().any(|f| f.pc == next.pc && f.sp == next.sp) {
            break;
        }
        frames.push(next);
    }
    frames
}

fn cfi_step(
    info: &DebugInfo,
    regs: &Registers,
    read32: &mut dyn FnMut(u32) -> Option<u32>,
) -> Option<Frame> {
    let at = info.locate(regs.pc)?;
    let link = info.link.to_link(at)?;
    let row = info.cfi.as_ref()?.row_for(link)?;
    let cfa = i64::from(regs.get(row.cfa_reg)?).checked_add(row.cfa_offset)?;
    let cfa = u32::try_from(cfa).ok()?;
    let mut next = *regs;
    let mut ra: Option<u32> = None;
    for (reg, rule) in &row.rules {
        let value = match rule {
            RegRule::Offset(n) => read32(cfa.wrapping_add(*n as u32))?,
            RegRule::ValOffset(n) => cfa.wrapping_add(*n as u32),
            RegRule::Register(r) => regs.get(*r)?,
            RegRule::SameValue | RegRule::Undefined | RegRule::Unsupported => continue,
        };
        if *reg == row.ra_reg {
            ra = Some(value);
        } else {
            next.set(*reg, value);
        }
    }
    let ra = match ra {
        Some(ra) => ra,
        // No rule for the return column: it is in a register (a leaf
        // frame described by a CIE that keeps RA in a register).
        None => regs.get(row.ra_reg)?,
    };
    next.a[7] = cfa;
    next.pc = ra;
    Some(Frame {
        pc: ra,
        sp: cfa,
        regs: next,
        via: FrameVia::Cfi,
    })
}

fn scan_step(
    info: &DebugInfo,
    regs: &Registers,
    read32: &mut dyn FnMut(u32) -> Option<u32>,
    read16: &mut dyn FnMut(u32) -> Option<u16>,
) -> Option<Frame> {
    let sp = regs.sp();
    for slot in 0..SCAN_SLOTS {
        let addr = sp.wrapping_add(slot * 4);
        let Some(candidate) = read32(addr) else {
            break;
        };
        if candidate & 1 != 0 || candidate == 0 {
            continue;
        }
        // With relocation known, a return address must land in the
        // program (calls into ROM return into the program too).
        if info.relocated() && info.locate(candidate).is_none() {
            continue;
        }
        if !follows_a_call(candidate, read16) {
            continue;
        }
        let mut next = *regs;
        next.a[7] = addr.wrapping_add(4);
        next.pc = candidate;
        return Some(Frame {
            pc: candidate,
            sp: addr.wrapping_add(4),
            regs: next,
            via: FrameVia::Scan,
        });
    }
    None
}

/// Whether the instruction ending at `ra` is a subroutine call.
pub fn follows_a_call(ra: u32, read16: &mut dyn FnMut(u32) -> Option<u16>) -> bool {
    // Two-byte forms: bsr.s (displacement 1..=254), jsr (An).
    if let Some(w) = read16(ra.wrapping_sub(2)) {
        if (w & 0xFF00) == 0x6100 && (w & 0xFF) != 0 && (w & 0xFF) != 0xFF {
            return true;
        }
        if (w & 0xFFF8) == 0x4E90 {
            return true;
        }
    }
    // Four-byte forms: bsr.w, jsr d16(An), jsr d8(An,Xn), jsr abs.w,
    // jsr d16(PC), jsr d8(PC,Xn).
    if let Some(w) = read16(ra.wrapping_sub(4)) {
        if w == 0x6100
            || (w & 0xFFF8) == 0x4EA8
            || (w & 0xFFF8) == 0x4EB0
            || w == 0x4EB8
            || w == 0x4EBA
            || w == 0x4EBB
        {
            return true;
        }
    }
    // Six-byte forms: jsr abs.l, bsr.l (68020+).
    if let Some(w) = read16(ra.wrapping_sub(6)) {
        if w == 0x4EB9 || w == 0x61FF {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debuginfo::hunk::tests::Builder;
    use crate::debuginfo::hunk::{HunkFile, HUNK_CODE};
    use std::collections::HashMap;

    /// A little big-endian memory for the tests.
    struct Mem(HashMap<u32, u8>);

    impl Mem {
        fn new() -> Self {
            Mem(HashMap::new())
        }
        fn put16(&mut self, addr: u32, v: u16) {
            for (i, b) in v.to_be_bytes().iter().enumerate() {
                self.0.insert(addr + i as u32, *b);
            }
        }
        fn put32(&mut self, addr: u32, v: u32) {
            for (i, b) in v.to_be_bytes().iter().enumerate() {
                self.0.insert(addr + i as u32, *b);
            }
        }
        fn r16(&self, addr: u32) -> Option<u16> {
            Some(u16::from_be_bytes([
                *self.0.get(&addr)?,
                *self.0.get(&(addr + 1))?,
            ]))
        }
        fn r32(&self, addr: u32) -> Option<u32> {
            Some((u32::from(self.r16(addr)?) << 16) | u32::from(self.r16(addr + 2)?))
        }
    }

    #[test]
    fn call_detection_covers_the_bsr_and_jsr_forms() {
        let mut mem = Mem::new();
        mem.put16(0x100, 0x6104); // bsr.s -> returns to 0x102
        mem.put16(0x200, 0x4EB9); // jsr abs.l -> returns to 0x206
        mem.put16(0x300, 0x4EAE); // jsr d16(a6) -> returns to 0x304
        mem.put16(0x400, 0x4E90); // jsr (a0) -> returns to 0x402
        mem.put16(0x500, 0x4E75); // rts: not a call
        let mut r16 = |a| mem.r16(a);
        assert!(follows_a_call(0x102, &mut r16));
        assert!(follows_a_call(0x206, &mut r16));
        assert!(follows_a_call(0x304, &mut r16));
        assert!(follows_a_call(0x402, &mut r16));
        assert!(!follows_a_call(0x502, &mut r16));
    }

    #[test]
    fn scan_finds_the_return_address_past_local_junk() {
        let mut b = Builder::default();
        b.hunk(HUNK_CODE, &[0; 0x100], None).end();
        let mut info =
            crate::debuginfo::DebugInfo::from_hunk_file(&HunkFile::parse(&b.build()).unwrap());
        info.relocate(vec![0x1000]);
        let mut mem = Mem::new();
        mem.put16(0x1010, 0x6120); // bsr.s at 0x1010, returns to 0x1012
        mem.put32(0x8000, 0x0000_0042); // a local
        mem.put32(0x8004, 0x0000_1050); // even, in the hunk, but no call before it
        mem.put32(0x8008, 0x0000_1012); // the return address
        let regs = Registers {
            a: [0, 0, 0, 0, 0, 0, 0, 0x8000],
            pc: 0x1040,
            ..Default::default()
        };
        let frames = unwind(&info, &regs, &mut |a| mem.r32(a), &mut |a| mem.r16(a), 8);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].pc, 0x1012);
        assert_eq!(frames[1].sp, 0x800C);
        assert_eq!(frames[1].via, FrameVia::Scan);
    }
}
