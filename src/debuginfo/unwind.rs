// SPDX-License-Identifier: GPL-3.0-or-later

//! Call-stack reconstruction for the 68000: call-frame information
//! where the program's DWARF covers the PC, and otherwise the same
//! return-address scan the debugger console's `STACK` command uses (a
//! stack slot whose preceding instruction is a `JSR`/`BSR`).
//!
//! A machine stopped at an exception handler's first instruction has the
//! CPU's exception stack frame at A7. [`decode_exception_frame`] reads it
//! and [`unwind_exception`] continues the walk in the context the
//! exception interrupted, so the faulting instruction and its callers
//! appear below the handler.

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
    /// The context an exception interrupted, from the exception stack
    /// frame. Its PC is the instruction itself, not a return address.
    Exception,
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
    let frames = vec![Frame {
        pc: regs.pc,
        sp: regs.sp(),
        regs: *regs,
        via: FrameVia::Entry,
    }];
    walk(info, frames, read32, read16, max_frames)
}

/// How a CPU model lays out its exception stack frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameStyle {
    /// The 68000: SR and PC, below which a bus or address error adds a
    /// status word, the access address and the instruction register.
    /// There is no format word.
    M68000,
    /// The 68010 and later: SR, PC and a format/vector word whose format
    /// gives the frame's length and its extra fields.
    Formatted,
}

impl FrameStyle {
    /// The style of a CPU as the control protocol's `status` names it
    /// (`M68000`, `M68010`, `M68EC020`, ...).
    pub fn for_cpu(name: &str) -> Self {
        if name == "M68000" {
            FrameStyle::M68000
        } else {
            FrameStyle::Formatted
        }
    }
}

/// An exception stack frame, read where the CPU pushed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExceptionFrame {
    /// The address the frame starts at, on the stack the CPU pushed it on.
    pub at: u32,
    /// The status register of the interrupted context.
    pub sr: u16,
    /// The PC the frame holds: where RTE resumes. Depending on the
    /// vector and CPU model it is the faulting instruction, the next
    /// one, or (68000 bus and address errors) a prefetch address 2 to 10
    /// bytes past the faulting opcode.
    pub stacked_pc: u32,
    /// The frame format (68010 and later).
    pub format: Option<u8>,
    /// The frame's length in bytes: the interrupted context's supervisor
    /// stack pointer is this far above `at`.
    pub size: u32,
    /// For a bus or address error, the address of the faulted access,
    /// where the frame records one.
    pub access: Option<u32>,
}

/// Length in bytes of a 68010+ exception frame of `format` (M68000PRM
/// and the M68010/M68020/M68030/M68040/M68060 user's manuals), or `None`
/// for a format no CPU in the family pushes.
fn formatted_frame_size(format: u8) -> Option<u32> {
    Some(match format {
        // Four-word frame; throwaway four-word frame.
        0x0 | 0x1 => 8,
        // Six-word frames: instruction or fault address; FP post-instruction.
        0x2 | 0x3 => 12,
        // Eight-word frame: 68060 access error, 68040 FP unimplemented.
        0x4 => 16,
        // 68040 access error.
        0x7 => 60,
        // 68010 bus and address error.
        0x8 => 58,
        // 68020/68030 coprocessor mid-instruction.
        0x9 => 20,
        // 68020/68030 short and long bus cycle fault.
        0xA => 32,
        0xB => 92,
        _ => return None,
    })
}

/// Read the exception frame at `sp`, the supervisor stack pointer at the
/// first instruction of the handler for `vector`.
pub fn decode_exception_frame(
    style: FrameStyle,
    vector: u16,
    sp: u32,
    read16: &mut dyn FnMut(u32) -> Option<u16>,
    read32: &mut dyn FnMut(u32) -> Option<u32>,
) -> Option<ExceptionFrame> {
    let at = |offset: u32| sp.wrapping_add(offset);
    match style {
        // Group 0: status word, access address, IR, SR, PC (14 bytes).
        FrameStyle::M68000 if matches!(vector, 2 | 3) => Some(ExceptionFrame {
            at: sp,
            sr: read16(at(8))?,
            stacked_pc: read32(at(10))?,
            format: None,
            size: 14,
            access: Some(read32(at(2))?),
        }),
        FrameStyle::M68000 => Some(ExceptionFrame {
            at: sp,
            sr: read16(at(0))?,
            stacked_pc: read32(at(2))?,
            format: None,
            size: 6,
            access: None,
        }),
        FrameStyle::Formatted => {
            let format_word = read16(at(6))?;
            // The frame must be the one this vector pushed.
            if (format_word & 0x0FFF) >> 2 != vector & 0x3FF {
                return None;
            }
            let format = (format_word >> 12) as u8;
            let size = formatted_frame_size(format)?;
            let access = match format {
                // 68040/68060 address error: the fault address.
                0x2 if vector == 3 => Some(read32(at(0x08))?),
                // 68060 access error: the fault address before the FSLW.
                0x4 if vector == 2 => Some(read32(at(0x08))?),
                0x7 => Some(read32(at(0x14))?),
                0x8 => Some(read32(at(0x0A))?),
                // The SSW's DF bit marks a faulted data cycle, whose
                // address is the data cycle fault address; an instruction
                // stream fault records the stage B address (long frame).
                0xA | 0xB => {
                    if read16(at(0x0A))? & 0x0100 != 0 {
                        Some(read32(at(0x10))?)
                    } else if format == 0xB {
                        Some(read32(at(0x24))?)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            Some(ExceptionFrame {
                at: sp,
                sr: read16(at(0))?,
                stacked_pc: read32(at(2))?,
                format: Some(format),
                size,
                access,
            })
        }
    }
}

/// The frame an exception left for the context it interrupted, with the
/// handler's A7 at `sp`. An interrupt taken in master mode on a 68020,
/// 68030 or 68040 pushes its frame on the master stack and a format $1
/// throwaway copy on the interrupt stack the handler runs on; `msp` is
/// the banked master stack pointer, which then points at the real frame.
/// Without it, a throwaway frame cannot be followed and yields `None`.
pub fn locate_exception_frame(
    style: FrameStyle,
    vector: u16,
    sp: u32,
    msp: Option<u32>,
    read16: &mut dyn FnMut(u32) -> Option<u16>,
    read32: &mut dyn FnMut(u32) -> Option<u32>,
) -> Option<ExceptionFrame> {
    let frame = decode_exception_frame(style, vector, sp, read16, read32)?;
    if frame.format != Some(0x1) {
        return Some(frame);
    }
    decode_exception_frame(style, vector, msp?, read16, read32)
        .filter(|master| master.format != Some(0x1))
}

/// Walk the stack of a machine stopped at the first instruction of an
/// exception handler: the handler (`regs`), then the context the
/// exception interrupted, then that context's callers.
///
/// The interrupted context keeps the live registers, which exception
/// entry does not change, except A7: the stack above `frame` when the
/// stacked SR was in supervisor mode, else `usp`. Its PC is
/// `fault_pc`, the instruction that raised the exception, when the stop
/// knows it, else the stacked PC.
#[allow(clippy::too_many_arguments)]
pub fn unwind_exception(
    info: &DebugInfo,
    regs: &Registers,
    usp: u32,
    frame: &ExceptionFrame,
    fault_pc: Option<u32>,
    read32: &mut dyn FnMut(u32) -> Option<u32>,
    read16: &mut dyn FnMut(u32) -> Option<u16>,
    max_frames: usize,
) -> Vec<Frame> {
    let handler = Frame {
        pc: regs.pc,
        sp: regs.sp(),
        regs: *regs,
        via: FrameVia::Entry,
    };
    let mut interrupted = *regs;
    interrupted.a[7] = if frame.sr & 0x2000 != 0 {
        frame.at.wrapping_add(frame.size)
    } else {
        usp
    };
    interrupted.pc = fault_pc.unwrap_or(frame.stacked_pc);
    let mut frames = vec![handler];
    if max_frames > 1 {
        frames.push(Frame {
            pc: interrupted.pc,
            sp: interrupted.sp(),
            regs: interrupted,
            via: FrameVia::Exception,
        });
    }
    walk(info, frames, read32, read16, max_frames)
}

/// Extend `frames` outward from its last frame, at most `max_frames`
/// deep.
fn walk(
    info: &DebugInfo,
    mut frames: Vec<Frame>,
    read32: &mut dyn FnMut(u32) -> Option<u32>,
    read16: &mut dyn FnMut(u32) -> Option<u16>,
    max_frames: usize,
) -> Vec<Frame> {
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

    fn decode(mem: &Mem, style: FrameStyle, vector: u16, sp: u32) -> Option<ExceptionFrame> {
        decode_exception_frame(style, vector, sp, &mut |a| mem.r16(a), &mut |a| mem.r32(a))
    }

    #[test]
    fn decodes_the_68000_group0_frame_with_its_access_address() {
        let mut mem = Mem::new();
        mem.put16(0x7000, 0x3015); // status word: IR bits, read, FC 5
        mem.put32(0x7002, 0x0000_1001); // access address
        mem.put16(0x7006, 0x3010); // IR: move.w (a0),d0
        mem.put16(0x7008, 0x0004); // SR: user mode
        mem.put32(0x700A, 0x0000_1018); // prefetch PC
        let frame = decode(&mem, FrameStyle::M68000, 3, 0x7000).unwrap();
        assert_eq!(frame.sr, 0x0004);
        assert_eq!(frame.stacked_pc, 0x1018);
        assert_eq!(frame.format, None);
        assert_eq!(frame.size, 14);
        assert_eq!(frame.access, Some(0x1001));
    }

    #[test]
    fn decodes_the_68000_short_frame_for_other_vectors() {
        let mut mem = Mem::new();
        mem.put16(0x7000, 0x2700);
        mem.put32(0x7002, 0x0000_1020);
        let frame = decode(&mem, FrameStyle::M68000, 4, 0x7000).unwrap();
        assert_eq!(frame.sr, 0x2700);
        assert_eq!(frame.stacked_pc, 0x1020);
        assert_eq!(frame.size, 6);
        assert_eq!(frame.access, None);
    }

    #[test]
    fn formatted_frames_take_their_length_and_access_from_the_format() {
        let base = |mem: &mut Mem, format: u16, vector: u16| {
            mem.put16(0x7000, 0x0010);
            mem.put32(0x7002, 0x0000_1030);
            mem.put16(0x7006, (format << 12) | (vector << 2));
        };
        // Format $0 (TRAP #0): four words, no access.
        let mut mem = Mem::new();
        base(&mut mem, 0x0, 32);
        let frame = decode(&mem, FrameStyle::Formatted, 32, 0x7000).unwrap();
        assert_eq!((frame.format, frame.size, frame.access), (Some(0), 8, None));
        assert_eq!((frame.sr, frame.stacked_pc), (0x0010, 0x1030));

        // Format $2: the address field is the fault address only for an
        // address error; for CHK it is the instruction address.
        let mut mem = Mem::new();
        base(&mut mem, 0x2, 3);
        mem.put32(0x7008, 0x0000_2001);
        let frame = decode(&mem, FrameStyle::Formatted, 3, 0x7000).unwrap();
        assert_eq!((frame.size, frame.access), (12, Some(0x2001)));
        let mut mem = Mem::new();
        base(&mut mem, 0x2, 6);
        mem.put32(0x7008, 0x0000_102C);
        let frame = decode(&mem, FrameStyle::Formatted, 6, 0x7000).unwrap();
        assert_eq!((frame.size, frame.access), (12, None));

        // Format $4 (68060 access error): fault address at +$08.
        let mut mem = Mem::new();
        base(&mut mem, 0x4, 2);
        mem.put32(0x7008, 0x00E0_0000);
        let frame = decode(&mem, FrameStyle::Formatted, 2, 0x7000).unwrap();
        assert_eq!((frame.size, frame.access), (16, Some(0x00E0_0000)));

        // Format $7 (68040 access error): fault address at +$14.
        let mut mem = Mem::new();
        base(&mut mem, 0x7, 2);
        mem.put32(0x7014, 0x00E0_0004);
        let frame = decode(&mem, FrameStyle::Formatted, 2, 0x7000).unwrap();
        assert_eq!((frame.size, frame.access), (60, Some(0x00E0_0004)));

        // Format $8 (68010 bus/address error): fault address at +$0A.
        let mut mem = Mem::new();
        base(&mut mem, 0x8, 3);
        mem.put32(0x700A, 0x0000_1001);
        let frame = decode(&mem, FrameStyle::Formatted, 3, 0x7000).unwrap();
        assert_eq!((frame.size, frame.access), (58, Some(0x1001)));

        // Format $B: a data fault (SSW DF) reports the data cycle fault
        // address, an instruction fault the stage B address.
        let mut mem = Mem::new();
        base(&mut mem, 0xB, 2);
        mem.put16(0x700A, 0x0145); // DF | RW | FC 5
        mem.put32(0x7010, 0x00E0_0008);
        mem.put32(0x7024, 0xDEAD_BEEE);
        let frame = decode(&mem, FrameStyle::Formatted, 2, 0x7000).unwrap();
        assert_eq!((frame.size, frame.access), (92, Some(0x00E0_0008)));
        mem.put16(0x700A, 0x5006); // FB | RB | FC 6
        let frame = decode(&mem, FrameStyle::Formatted, 2, 0x7000).unwrap();
        assert_eq!(frame.access, Some(0xDEAD_BEEE));
    }

    #[test]
    fn a_formatted_frame_for_another_vector_or_an_unknown_format_is_refused() {
        let mut mem = Mem::new();
        mem.put16(0x7000, 0x0000);
        mem.put32(0x7002, 0x0000_1030);
        mem.put16(0x7006, 0x0080); // format $0, vector 32
        assert!(decode(&mem, FrameStyle::Formatted, 4, 0x7000).is_none());
        mem.put16(0x7006, 0x5080); // format $5: no such frame
        assert!(decode(&mem, FrameStyle::Formatted, 32, 0x7000).is_none());
        assert_eq!(FrameStyle::for_cpu("M68000"), FrameStyle::M68000);
        assert_eq!(FrameStyle::for_cpu("M68010"), FrameStyle::Formatted);
        assert_eq!(FrameStyle::for_cpu("M68EC020"), FrameStyle::Formatted);
    }

    #[test]
    fn a_throwaway_frame_is_followed_to_the_master_stack() {
        let mut mem = Mem::new();
        // Interrupt stack: the format $1 throwaway copy.
        mem.put16(0x6FFF8, 0x3000);
        mem.put32(0x6FFFA, 0x0000_1030);
        mem.put16(0x6FFFE, 0x1000 | (27 << 2));
        // Master stack: the real format $0 frame.
        mem.put16(0x73FF8, 0x3000);
        mem.put32(0x73FFA, 0x0000_1030);
        mem.put16(0x73FFE, 27 << 2);
        let mut r16 = |a| mem.r16(a);
        let mut r32 = |a| mem.r32(a);
        let frame = locate_exception_frame(
            FrameStyle::Formatted,
            27,
            0x6FFF8,
            Some(0x73FF8),
            &mut r16,
            &mut r32,
        )
        .unwrap();
        assert_eq!((frame.at, frame.format, frame.size), (0x73FF8, Some(0), 8));
        assert_eq!(frame.at.wrapping_add(frame.size), 0x74000);
        // Without the master stack pointer there is nothing to follow.
        assert!(locate_exception_frame(
            FrameStyle::Formatted,
            27,
            0x6FFF8,
            None,
            &mut r16,
            &mut r32
        )
        .is_none());
        // Any other frame is the one at A7.
        let frame =
            locate_exception_frame(FrameStyle::Formatted, 27, 0x73FF8, None, &mut r16, &mut r32)
                .unwrap();
        assert_eq!(frame.at, 0x73FF8);

        // The interrupted context's A7 is above the real frame, not above
        // the throwaway copy at the handler's A7.
        let (info, code) = program_with_a_call();
        let master = locate_exception_frame(
            FrameStyle::Formatted,
            27,
            0x6FFF8,
            Some(0x73FF8),
            &mut r16,
            &mut r32,
        )
        .unwrap();
        let regs = Registers {
            a: [0, 0, 0, 0, 0, 0, 0, 0x6FFF8],
            pc: 0x00F8_0100,
            ..Default::default()
        };
        let frames = unwind_exception(
            &info,
            &regs,
            0x8000,
            &master,
            None,
            &mut |a| code.r32(a),
            &mut |a| code.r16(a),
            2,
        );
        assert_eq!((frames[1].pc, frames[1].sp), (0x1030, 0x74000));
    }

    /// A one-hunk program at 0x1000 with a `bsr.s` at 0x1010 (returning
    /// to 0x1012).
    fn program_with_a_call() -> (crate::debuginfo::DebugInfo, Mem) {
        let mut b = Builder::default();
        b.hunk(HUNK_CODE, &[0; 0x100], None).end();
        let mut info =
            crate::debuginfo::DebugInfo::from_hunk_file(&HunkFile::parse(&b.build()).unwrap());
        info.relocate(vec![0x1000]);
        let mut mem = Mem::new();
        mem.put16(0x1010, 0x6120);
        (info, mem)
    }

    #[test]
    fn a_user_mode_fault_continues_on_the_user_stack_from_the_faulting_instruction() {
        let (info, mut mem) = program_with_a_call();
        mem.put32(0x8000, 0x0000_1012); // the faulting routine's return address
        let frame = ExceptionFrame {
            at: 0x3FF2,
            sr: 0x0000,
            stacked_pc: 0x1044,
            format: None,
            size: 14,
            access: Some(0x0001),
        };
        let regs = Registers {
            d: [7, 0, 0, 0, 0, 0, 0, 0],
            a: [0x0001, 0, 0, 0, 0, 0, 0, 0x3FF2],
            pc: 0x00F8_0100,
        };
        let frames = unwind_exception(
            &info,
            &regs,
            0x8000,
            &frame,
            Some(0x1040),
            &mut |a| mem.r32(a),
            &mut |a| mem.r16(a),
            8,
        );
        assert_eq!(frames.len(), 3, "{frames:?}");
        assert_eq!(
            (frames[0].pc, frames[0].via),
            (0x00F8_0100, FrameVia::Entry)
        );
        assert_eq!(frames[1].pc, 0x1040);
        assert_eq!(frames[1].sp, 0x8000);
        assert_eq!(frames[1].via, FrameVia::Exception);
        // Exception entry leaves the other registers as the fault left them.
        assert_eq!(frames[1].regs.d[0], 7);
        assert_eq!(frames[1].regs.a[0], 0x0001);
        assert_eq!((frames[2].pc, frames[2].via), (0x1012, FrameVia::Scan));
    }

    #[test]
    fn a_supervisor_fault_continues_above_the_frame_at_the_stacked_pc() {
        let (info, mut mem) = program_with_a_call();
        mem.put32(0x3FF8, 0x0000_1012); // just above the six-byte frame
        let frame = ExceptionFrame {
            at: 0x3FF2,
            sr: 0x2000,
            stacked_pc: 0x1050,
            format: None,
            size: 6,
            access: None,
        };
        let regs = Registers {
            a: [0, 0, 0, 0, 0, 0, 0, 0x3FF2],
            pc: 0x00F8_0100,
            ..Default::default()
        };
        let frames = unwind_exception(
            &info,
            &regs,
            0x8000,
            &frame,
            None,
            &mut |a| mem.r32(a),
            &mut |a| mem.r16(a),
            8,
        );
        assert_eq!(frames.len(), 3, "{frames:?}");
        assert_eq!((frames[1].pc, frames[1].sp), (0x1050, 0x3FF8));
        assert_eq!(frames[2].pc, 0x1012);
        // A depth of one keeps just the handler.
        let frames = unwind_exception(
            &info,
            &regs,
            0x8000,
            &frame,
            None,
            &mut |a| mem.r32(a),
            &mut |a| mem.r16(a),
            1,
        );
        assert_eq!(frames.len(), 1);
    }
}
