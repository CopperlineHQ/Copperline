//! MC68020 integer instruction timing.
//!
//! The MC68020 User's Manual section 8.2 publishes three timing columns:
//! best case (instruction overlap), cache case (cached instruction stream
//! without overlap), and worst case (uncached stream without overlap).
//! Copperline does not yet model the 020 execution-stage overlap, so an
//! instruction selects Cache Case only when all of its opcode, extension,
//! and immediate words hit the instruction cache; any miss selects Worst
//! Case.
//!
//! The published totals include zero-wait, aligned transfers on a 32-bit
//! bus. Copperline bills the real target bus while the instruction executes;
//! the host advances only the part of this total not already consumed by
//! those accesses. Consequently chip-RAM arbitration and narrower buses can
//! extend a table time, but a table entry never hides a real wait.
//!
//! Full-format indexed extension words cannot be distinguished from the
//! brief form after the instruction has retired. Indexed entries therefore
//! use the manual's brief-form row; the host still bills every extension and
//! indirect operand access that actually occurred.

use super::cpu::CpuCore;
use super::types::Size;

#[derive(Clone, Copy)]
struct Case {
    cache: i32,
    worst: i32,
}

impl Case {
    const fn new(cache: i32, worst: i32) -> Self {
        Self { cache, worst }
    }

    #[inline]
    const fn pick(self, cached: bool) -> i32 {
        if cached { self.cache } else { self.worst }
    }
}

#[inline]
const fn size_00(bits: u16) -> Size {
    match bits {
        0 => Size::Byte,
        1 => Size::Word,
        _ => Size::Long,
    }
}

/// Table 8-1, "Fetch Effective Address Timing".
const fn fetch_ea(mode: u16, reg: u16, size: Size) -> Case {
    match (mode, reg) {
        (0 | 1, _) => Case::new(0, 0),
        (2 | 3, _) => Case::new(4, 4),
        (4, _) => Case::new(5, 5),
        (5, _) | (7, 2) => Case::new(5, 6),
        (6, _) | (7, 3) => Case::new(7, 8),
        (7, 0) => Case::new(4, 6),
        (7, 1) => Case::new(4, 7),
        (7, 4) if matches!(size, Size::Long) => Case::new(4, 5),
        (7, 4) => Case::new(2, 3),
        _ => Case::new(0, 0),
    }
}

/// Table 8-1, "Fetch Immediate Effective Address Timing".
const fn fetch_immediate_ea(mode: u16, reg: u16, size: Size) -> Case {
    let long = matches!(size, Size::Long);
    match (mode, reg, long) {
        (0 | 1, _, false) => Case::new(2, 3),
        (0 | 1, _, true) => Case::new(4, 5),
        (2, _, false) => Case::new(4, 4),
        (2, _, true) => Case::new(4, 7),
        (3, _, false) => Case::new(6, 7),
        (3, _, true) => Case::new(8, 9),
        (4, _, false) => Case::new(5, 6),
        (4, _, true) => Case::new(7, 8),
        (5, _, false) | (7, 0, false) => Case::new(5, 7),
        (5, _, true) | (7, 0, true) => Case::new(7, 10),
        (6, _, false) | (7, 3, false) => Case::new(9, 11),
        (6, _, true) | (7, 3, true) => Case::new(11, 13),
        (7, 1, false) => Case::new(6, 10),
        (7, 1, true) => Case::new(8, 12),
        _ => Case::new(0, 0),
    }
}

/// Table 8-1, "Calculate Effective Address Timing".
const fn calculate_ea(mode: u16, reg: u16) -> Case {
    match (mode, reg) {
        (0 | 1, _) => Case::new(0, 0),
        (2..=4, _) => Case::new(2, 2),
        (5, _) | (7, 0 | 2) => Case::new(2, 3),
        (6, _) | (7, 3) => Case::new(4, 5),
        (7, 1) => Case::new(4, 5),
        _ => Case::new(0, 0),
    }
}

/// Table 8-1, "Jump Effective Address Timing".
const fn jump_ea(mode: u16, reg: u16) -> Case {
    match (mode, reg) {
        (2, _) => Case::new(2, 2),
        (5, _) | (7, 2) => Case::new(4, 4),
        (6, _) | (7, 3) => Case::new(6, 6),
        (7, 0 | 1) => Case::new(2, 2),
        _ => Case::new(0, 0),
    }
}

/// Table 8-1, "Calculate Immediate Effective Address Timing".
const fn calculate_immediate_ea(mode: u16, reg: u16, size: Size) -> Case {
    let long = matches!(size, Size::Long);
    match (mode, reg, long) {
        (0 | 1, _, false) => Case::new(2, 3),
        (0 | 1, _, true) => Case::new(4, 5),
        (2, _, false) => Case::new(2, 3),
        (2, _, true) => Case::new(4, 5),
        (3 | 4, _, false) => Case::new(4, 5),
        (3 | 4, _, true) => Case::new(6, 7),
        (5, _, false) | (7, 0, false) => Case::new(4, 5),
        (5, _, true) | (7, 0, true) => Case::new(6, 8),
        (6, _, false) | (7, 3, false) => Case::new(6, 8),
        (6, _, true) | (7, 3, true) => Case::new(8, 10),
        (7, 1, false) => Case::new(4, 6),
        (7, 1, true) => Case::new(8, 10),
        _ => Case::new(0, 0),
    }
}

#[inline]
fn add(base: Case, ea: Case) -> Case {
    Case::new(base.cache + ea.cache, base.worst + ea.worst)
}

#[inline]
const fn move_source_row(mode: u16, reg: u16, size: Size) -> usize {
    match (mode, reg) {
        (0 | 1, _) => 0,
        (2, _) => 3,
        (3, _) => 4,
        (4, _) => 5,
        (5, _) | (7, 2) => 6,
        (7, 0) => 7,
        (7, 1) => 8,
        (6, _) | (7, 3) => 9,
        (7, 4) if matches!(size, Size::Long) => 2,
        (7, 4) => 1,
        _ => 0,
    }
}

#[inline]
const fn move_destination_col(mode: u16, reg: u16) -> usize {
    match (mode, reg) {
        (0 | 1, _) => 0,
        (2, _) => 1,
        (3, _) => 2,
        (4, _) => 3,
        (5, _) => 4,
        (7, 0) => 5,
        (7, 1) => 6,
        (6, _) => 7,
        _ => 0,
    }
}

/// Tables 8-4 through 8-9, restricted to standard opcode-visible EAs.
const MOVE_CACHE: [[i32; 8]; 10] = [
    [2, 4, 4, 5, 5, 4, 6, 7],
    [4, 6, 6, 7, 7, 6, 8, 7],
    [6, 8, 8, 9, 9, 8, 10, 9],
    [6, 7, 7, 7, 7, 7, 9, 9],
    [6, 7, 7, 7, 7, 7, 9, 9],
    [7, 8, 8, 8, 8, 8, 10, 10],
    [7, 8, 8, 8, 8, 8, 10, 10],
    [6, 7, 7, 7, 7, 7, 9, 9],
    [6, 7, 7, 7, 7, 7, 9, 9],
    [9, 10, 10, 10, 10, 10, 12, 12],
];

const MOVE_WORST: [[i32; 8]; 10] = [
    [3, 5, 5, 6, 7, 7, 9, 9],
    [3, 5, 8, 6, 7, 7, 9, 9],
    [5, 7, 7, 8, 9, 9, 11, 11],
    [7, 9, 9, 9, 11, 11, 13, 11],
    [7, 9, 9, 9, 11, 11, 13, 11],
    [8, 10, 10, 10, 12, 12, 14, 12],
    [9, 11, 11, 11, 13, 13, 15, 13],
    [8, 10, 10, 10, 12, 12, 14, 12],
    [10, 12, 12, 12, 14, 14, 16, 14],
    [11, 13, 13, 13, 15, 15, 17, 15],
];

fn move_cycles(op: u16, size: Size, cached: bool) -> i32 {
    let src_mode = (op >> 3) & 7;
    let src_reg = op & 7;
    let dst_mode = (op >> 6) & 7;
    let dst_reg = (op >> 9) & 7;
    let row = move_source_row(src_mode, src_reg, size);
    let col = move_destination_col(dst_mode, dst_reg);
    if cached {
        MOVE_CACHE[row][col]
    } else {
        MOVE_WORST[row][col]
    }
}

#[inline]
fn legacy_fallback(raw: i32) -> i32 {
    ((raw * 5 + 7) / 8).max(2)
}

fn exception_cycles(cpu: &CpuCore, raw: i32, cached: bool) -> Option<i32> {
    let vector = cpu.instruction_exception_vector?;
    let op = cpu.ir as u16;
    let cycles = match vector {
        // Table 8-17: these share the same format-$0 entry cost.
        4 | 8 | 10 | 11 => Case::new(20, 27).pick(cached),
        // Group-2 TRAPV/TRAPcc uses a six-word format-$2 frame.
        7 if op == 0x4E76 => Case::new(25, 32).pick(cached),
        7 if op >> 12 == 5 => {
            let submode = op & 7;
            match submode {
                2 | 3 => Case::new(25, 33).pick(cached),
                _ => Case::new(25, 32).pick(cached),
            }
        }
        // CHK and divide-by-zero timing is not specified as a complete trap
        // path in section 8.2; retain the prior conservative calibration.
        _ => legacy_fallback(raw),
    };
    Some(cycles)
}

fn group_0(cpu: &CpuCore, op: u16, cached: bool) -> Option<i32> {
    let mode = (op >> 3) & 7;
    let reg = op & 7;

    // CAS2.W/L.
    if op == 0x0CFC || op == 0x0EFC {
        let base = if cpu.flag_z() {
            Case::new(25, 28)
        } else {
            Case::new(22, 25)
        };
        return Some(base.pick(cached));
    }

    // CAS.B/W/L.
    if matches!(op & 0x0FC0, 0x0AC0 | 0x0CC0 | 0x0EC0) {
        let base = if cpu.flag_z() {
            Case::new(15, 16)
        } else {
            Case::new(12, 13)
        };
        return Some(add(base, calculate_immediate_ea(mode, reg, Size::Word)).pick(cached));
    }

    // MOVES: the direction bit is in the consumed extension word. Use the
    // slower EA->Rn base; both directions still use the exact EA component.
    if op & 0xFF00 == 0x0E00 {
        return Some(
            add(
                Case::new(7, 8),
                calculate_immediate_ea(mode, reg, Size::Word),
            )
            .pick(cached),
        );
    }

    // CALLM/RTM are 68020-only and encode information not retained after
    // retirement. Use the slower documented type-1 paths.
    if op & 0xFFF0 == 0x06C0 {
        return Some(Case::new(32, 35).pick(cached));
    }
    if op & 0xFFC0 == 0x06C0 {
        return Some(
            add(Case::new(57, 64), fetch_immediate_ea(mode, reg, Size::Word)).pick(cached),
        );
    }

    // CMP2/CHK2. Both have the same section-8.2 base time.
    if op & 0x0800 == 0 && op & 0x0100 == 0 && op & 0x00C0 == 0x00C0 && (op >> 9) & 3 != 3 {
        return Some(
            add(Case::new(18, 18), fetch_immediate_ea(mode, reg, Size::Word)).pick(cached),
        );
    }

    // MOVEP.
    if op & 0xF138 == 0x0108 {
        let long = op & 0x0040 != 0;
        let reg_to_mem = op & 0x0080 != 0;
        let case = match (long, reg_to_mem) {
            (false, true) => Case::new(11, 11),
            (true, true) => Case::new(17, 17),
            (false, false) => Case::new(12, 12),
            (true, false) => Case::new(18, 18),
        };
        return Some(case.pick(cached));
    }

    let dynamic_bit = op & 0x0100 != 0;
    let static_bit = op & 0x0F00 == 0x0800;
    if dynamic_bit || static_bit {
        if mode == 0 {
            return Some(Case::new(4, 5).pick(cached));
        }
        let ea = if static_bit {
            fetch_immediate_ea(mode, reg, Size::Word)
        } else {
            fetch_ea(mode, reg, Size::Byte)
        };
        return Some(add(Case::new(4, 5), ea).pick(cached));
    }

    // ORI/ANDI/EORI to CCR/SR.
    if matches!(op, 0x003C | 0x007C | 0x023C | 0x027C | 0x0A3C | 0x0A7C) {
        return Some(Case::new(12, 15).pick(cached));
    }

    // Immediate arithmetic/logical instructions.
    let subop = (op >> 8) & 0xF;
    if matches!(subop, 0x0 | 0x2 | 0x4 | 0x6 | 0xA | 0xC) {
        let size = size_00((op >> 6) & 3);
        let base = if mode == 0 || subop == 0xC {
            Case::new(2, 3)
        } else {
            Case::new(4, 6)
        };
        return Some(add(base, fetch_immediate_ea(mode, reg, size)).pick(cached));
    }

    None
}

fn group_4(op: u16, raw: i32, cached: bool) -> Option<i32> {
    let mode = (op >> 3) & 7;
    let reg = op & 7;

    if op & 0xFFC0 == 0x4C00 {
        return Some(
            add(Case::new(43, 44), fetch_immediate_ea(mode, reg, Size::Word)).pick(cached),
        );
    }
    if op & 0xFFC0 == 0x4C40 {
        // Signedness lives in the consumed extension word; DIVS.L is the
        // conservative choice and differs from DIVU.L by twelve clocks.
        return Some(
            add(Case::new(90, 91), fetch_immediate_ea(mode, reg, Size::Word)).pick(cached),
        );
    }

    // MOVE from SR/CCR.
    if matches!(op & 0xFFC0, 0x40C0 | 0x42C0) {
        let case = if mode == 0 {
            Case::new(4, 5)
        } else {
            add(Case::new(5, 7), calculate_ea(mode, reg))
        };
        return Some(case.pick(cached));
    }
    // MOVE to CCR/SR.
    if matches!(op & 0xFFC0, 0x44C0 | 0x46C0) {
        let base = if op & 0x0200 == 0 {
            Case::new(4, 5)
        } else {
            Case::new(8, 11)
        };
        return Some(add(base, fetch_ea(mode, reg, Size::Word)).pick(cached));
    }

    // MOVEM (long multiply/divide was decoded above).
    if op & 0xFB80 == 0x4880 && mode != 0 {
        let long = op & 0x0040 != 0;
        let to_register = op & 0x0400 != 0;
        let transfer_cost = if long { 8 } else { 4 };
        let count = if to_register {
            raw.saturating_sub(12) / transfer_cost
        } else {
            raw.saturating_sub(8) / transfer_cost
        };
        let base = if to_register {
            Case::new(8 + 4 * count, 9 + 4 * count)
        } else {
            Case::new(4 + 3 * count, 5 + 3 * count)
        };
        return Some(add(base, calculate_immediate_ea(mode, reg, Size::Word)).pick(cached));
    }

    // LEA / CHK.
    if op & 0xF1C0 == 0x41C0 {
        return Some(add(Case::new(2, 3), calculate_ea(mode, reg)).pick(cached));
    }
    let chk_opmode = (op >> 6) & 7;
    if matches!(chk_opmode, 0b100 | 0b110) {
        let size = if chk_opmode == 0b100 {
            Size::Long
        } else {
            Size::Word
        };
        return Some(add(Case::new(8, 8), fetch_ea(mode, reg, size)).pick(cached));
    }

    // JSR / JMP.
    if op & 0xFFC0 == 0x4E80 {
        return Some(add(Case::new(5, 11), jump_ea(mode, reg)).pick(cached));
    }
    if op & 0xFFC0 == 0x4EC0 {
        return Some(add(Case::new(4, 7), jump_ea(mode, reg)).pick(cached));
    }

    // LINK.L / LINK.W / UNLK.
    if op & 0xFFF8 == 0x4808 {
        return Some(Case::new(6, 10).pick(cached));
    }
    if op & 0xFFF8 == 0x4E50 {
        return Some(Case::new(5, 7).pick(cached));
    }
    if op & 0xFFF8 == 0x4E58 {
        return Some(Case::new(6, 7).pick(cached));
    }

    // MOVE USP and MOVEC.
    if op & 0xFFF0 == 0x4E60 {
        return Some(Case::new(2, 3).pick(cached));
    }
    if op == 0x4E7A {
        return Some(Case::new(6, 7).pick(cached));
    }
    if op == 0x4E7B {
        return Some(Case::new(12, 13).pick(cached));
    }

    match op {
        0x4E70 => return Some(Case::new(518, 519).pick(cached)),
        0x4E71 => return Some(Case::new(2, 3).pick(cached)),
        0x4E72 => return Some(Case::new(8, 8).pick(cached)),
        0x4E73 => {
            let case = match raw {
                15 => Case::new(16, 39),
                31 => Case::new(32, 33),
                42 => Case::new(43, 45),
                91 => Case::new(92, 94),
                _ => Case::new(21, 24),
            };
            return Some(case.pick(cached));
        }
        0x4E74 => return Some(Case::new(10, 12).pick(cached)),
        0x4E75 => return Some(Case::new(10, 12).pick(cached)),
        0x4E76 => return Some(Case::new(4, 5).pick(cached)),
        0x4E77 => return Some(Case::new(14, 15).pick(cached)),
        _ => {}
    }

    // PEA (after SWAP's register-direct overlap).
    if op & 0xFFC0 == 0x4840 && mode != 0 {
        return Some(add(Case::new(5, 6), calculate_ea(mode, reg)).pick(cached));
    }
    if op & 0xFFF8 == 0x4840 {
        return Some(Case::new(4, 4).pick(cached)); // SWAP
    }

    // EXT.W/EXT.L/EXTB.L.
    if matches!(op & 0xFFF8, 0x4880 | 0x48C0 | 0x49C0) {
        return Some(Case::new(4, 4).pick(cached));
    }

    // TAS.
    if op & 0xFFC0 == 0x4AC0 {
        let base = if mode == 0 {
            Case::new(4, 4)
        } else {
            add(Case::new(12, 13), calculate_ea(mode, reg))
        };
        return Some(base.pick(cached));
    }

    // TST.
    if op & 0xFF00 == 0x4A00 {
        let size = size_00((op >> 6) & 3);
        return Some(add(Case::new(2, 3), fetch_ea(mode, reg, size)).pick(cached));
    }

    // CLR, NEGX, NEG, NOT.
    if matches!(op & 0xFF00, 0x4000 | 0x4200 | 0x4400 | 0x4600) {
        let size = size_00((op >> 6) & 3);
        if mode == 0 {
            return Some(Case::new(2, 3).pick(cached));
        }
        let ea = if op & 0xFF00 == 0x4200 {
            calculate_ea(mode, reg)
        } else {
            fetch_ea(mode, reg, size)
        };
        return Some(add(Case::new(4, 6), ea).pick(cached));
    }

    // NBCD is fully specified only for Dn in the manual.
    if op & 0xFFC0 == 0x4800 && mode == 0 {
        return Some(Case::new(6, 6).pick(cached));
    }

    None
}

fn dyadic(op: u16, cached: bool) -> Option<i32> {
    let group = op >> 12;
    let opmode = (op >> 6) & 7;
    let mode = (op >> 3) & 7;
    let reg = op & 7;

    // BCD, extend, and paired-memory forms.
    if group == 0x8 && opmode == 4 && mode <= 1 {
        return Some(
            if mode == 0 {
                Case::new(4, 5)
            } else {
                Case::new(16, 17)
            }
            .pick(cached),
        );
    }
    if group == 0xC && opmode == 4 && mode <= 1 {
        return Some(
            if mode == 0 {
                Case::new(4, 5)
            } else {
                Case::new(16, 17)
            }
            .pick(cached),
        );
    }
    if matches!(group, 0x9 | 0xD) && (4..=6).contains(&opmode) && mode <= 1 {
        return Some(
            if mode == 0 {
                Case::new(2, 3)
            } else {
                Case::new(12, 13)
            }
            .pick(cached),
        );
    }
    if group == 0xB && (4..=6).contains(&opmode) && mode == 1 {
        return Some(Case::new(9, 10).pick(cached));
    }
    if group == 0xB && (4..=6).contains(&opmode) && mode == 0 {
        return Some(Case::new(2, 3).pick(cached)); // EOR Dn,Dn
    }
    if group == 0x8 && opmode == 5 && mode <= 1 {
        return Some(
            if mode == 0 {
                Case::new(6, 7)
            } else {
                Case::new(13, 13)
            }
            .pick(cached),
        );
    }
    if group == 0x8 && opmode == 6 && mode <= 1 {
        return Some(
            if mode == 0 {
                Case::new(8, 9)
            } else {
                Case::new(13, 13)
            }
            .pick(cached),
        );
    }

    // EXG.
    if group == 0xC && (4..=6).contains(&opmode) && matches!((op >> 3) & 0x1F, 0x08 | 0x09 | 0x11) {
        return Some(Case::new(2, 3).pick(cached));
    }

    // Word multiply/divide.
    if group == 0xC && matches!(opmode, 3 | 7) {
        return Some(add(Case::new(27, 28), fetch_ea(mode, reg, Size::Word)).pick(cached));
    }
    if group == 0x8 && opmode == 3 {
        return Some(add(Case::new(44, 44), fetch_ea(mode, reg, Size::Word)).pick(cached));
    }
    if group == 0x8 && opmode == 7 {
        return Some(add(Case::new(56, 57), fetch_ea(mode, reg, Size::Word)).pick(cached));
    }

    match opmode {
        0..=2 => Some(add(Case::new(2, 3), fetch_ea(mode, reg, size_00(opmode))).pick(cached)),
        3 | 7 if matches!(group, 0x9 | 0xB | 0xD) => {
            let base = if group == 0xB {
                Case::new(4, 4) // CMPA
            } else {
                Case::new(2, 3) // ADDA/SUBA
            };
            let size = if opmode == 3 { Size::Word } else { Size::Long };
            Some(add(base, fetch_ea(mode, reg, size)).pick(cached))
        }
        4..=6 => Some(add(Case::new(4, 6), fetch_ea(mode, reg, size_00(opmode - 4))).pick(cached)),
        _ => None,
    }
}

fn group_5(cpu: &CpuCore, op: u16, cached: bool) -> Option<i32> {
    let mode = (op >> 3) & 7;
    let reg = op & 7;
    let size_bits = (op >> 6) & 3;

    if size_bits == 3 && mode == 7 && matches!(reg, 2..=4) {
        // TRAPcc, no-trap path. The trap path is caught before classification.
        let case = match reg {
            2 => Case::new(6, 7),
            3 => Case::new(8, 10),
            _ => Case::new(4, 5),
        };
        return Some(case.pick(cached));
    }
    if size_bits == 3 && mode == 1 {
        let condition = ((op >> 8) & 0xF) as u8;
        let case = if cpu.test_condition(condition) {
            Case::new(6, 7)
        } else if cpu.d(reg as usize) as u16 == 0xFFFF {
            Case::new(10, 10)
        } else {
            Case::new(6, 9)
        };
        return Some(case.pick(cached));
    }
    if size_bits == 3 {
        if mode == 0 {
            return Some(Case::new(4, 4).pick(cached));
        }
        return Some(add(Case::new(6, 6), calculate_ea(mode, reg)).pick(cached));
    }

    let size = size_00(size_bits);
    if mode <= 1 {
        Some(Case::new(2, 3).pick(cached))
    } else {
        Some(add(Case::new(4, 6), fetch_ea(mode, reg, size)).pick(cached))
    }
}

fn group_6(cpu: &CpuCore, op: u16, cached: bool) -> i32 {
    let condition = (op >> 8) & 0xF;
    if condition == 1 {
        return Case::new(7, 13).pick(cached); // BSR
    }
    if condition == 0 || cpu.change_of_flow {
        return Case::new(6, 9).pick(cached);
    }
    match op as u8 {
        0 => Case::new(6, 7).pick(cached),
        0xFF => Case::new(6, 9).pick(cached),
        _ => Case::new(4, 5).pick(cached),
    }
}

fn group_e(op: u16, cached: bool) -> Option<i32> {
    let mode = (op >> 3) & 7;
    let reg = op & 7;

    if op & 0x00C0 == 0x00C0 && (op >> 8) & 0xF >= 8 {
        let selector = (op >> 8) & 0xF;
        let base = if mode == 0 {
            match selector {
                0x8 => Case::new(6, 7),
                0x9 | 0xB => Case::new(8, 8),
                0xA | 0xC | 0xE => Case::new(12, 12),
                0xD => Case::new(18, 18),
                0xF => Case::new(10, 10),
                _ => return None,
            }
        } else {
            // Use the five-byte row: the executor can span five bytes and
            // does not retain the extension-derived span after retirement.
            match selector {
                0x8 => Case::new(15, 16),
                0x9 | 0xB => Case::new(18, 18),
                0xA | 0xC | 0xE => Case::new(24, 24),
                0xD => Case::new(32, 32),
                0xF => Case::new(20, 21),
                _ => return None,
            }
        };
        return Some(
            if mode == 0 {
                base
            } else {
                add(base, calculate_immediate_ea(mode, reg, Size::Word))
            }
            .pick(cached),
        );
    }

    if (op >> 6) & 3 == 3 {
        let kind = (op >> 9) & 3;
        let base = match kind {
            0 if op & 0x0100 != 0 => Case::new(6, 7), // ASL
            0 => Case::new(5, 6),                     // ASR
            1 => Case::new(5, 6),                     // LSL/LSR
            2 => Case::new(5, 6),                     // ROXL/ROXR
            _ => Case::new(7, 7),                     // ROL/ROR
        };
        return Some(add(base, fetch_ea(mode, reg, Size::Word)).pick(cached));
    }

    let kind = (op >> 3) & 3;
    let case = match kind {
        0 if op & 0x0100 != 0 => Case::new(8, 8), // ASL
        0 => Case::new(6, 6),                     // ASR
        1 if op & 0x0020 != 0 => Case::new(6, 6), // dynamic LS
        1 => Case::new(4, 4),                     // static LS
        2 => Case::new(12, 12),                   // ROXL/ROXR
        _ => Case::new(8, 8),                     // ROL/ROR
    };
    Some(case.pick(cached))
}

impl CpuCore {
    /// Return the section-8.2 timing for the 68020/68EC020 instruction that
    /// just retired. Instructions not covered by the integer tables (notably
    /// coprocessor operations and full-format indexed detail) retain the old
    /// conservative scale until a dedicated model exists.
    pub(crate) fn cycles_020(&self, raw: i32, fetch_cached: bool) -> i32 {
        if let Some(cycles) = exception_cycles(self, raw, fetch_cached) {
            return cycles;
        }

        let op = self.ir as u16;
        let cycles = match op >> 12 {
            0x0 => group_0(self, op, fetch_cached),
            0x1 => Some(move_cycles(op, Size::Byte, fetch_cached)),
            0x2 => Some(move_cycles(op, Size::Long, fetch_cached)),
            0x3 => Some(move_cycles(op, Size::Word, fetch_cached)),
            0x4 => group_4(op, raw, fetch_cached),
            0x5 => group_5(self, op, fetch_cached),
            0x6 => Some(group_6(self, op, fetch_cached)),
            0x7 => Some(Case::new(2, 3).pick(fetch_cached)),
            0x8 | 0x9 | 0xB | 0xC | 0xD => dyadic(op, fetch_cached),
            0xE => group_e(op, fetch_cached),
            _ => None,
        };
        cycles.unwrap_or_else(|| legacy_fallback(raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timed(op: u16, raw: i32, cached: bool) -> i32 {
        let mut cpu = CpuCore::new();
        cpu.ir = u32::from(op);
        cpu.cycles_020(raw, cached)
    }

    #[test]
    fn register_and_move_timings_follow_cache_and_worst_tables() {
        assert_eq!(timed(0x7001, 4, true), 2); // MOVEQ #1,D0
        assert_eq!(timed(0x7001, 4, false), 3);
        assert_eq!(timed(0x2284, 8, true), 4); // MOVE.L D4,(A1)
        assert_eq!(timed(0x2284, 8, false), 5);
        assert_eq!(timed(0x2501, 12, true), 5); // MOVE.L D1,-(A2)
        assert_eq!(timed(0x2501, 12, false), 6);
    }

    #[test]
    fn arithmetic_and_expensive_integer_ops_use_manual_bases() {
        assert_eq!(timed(0xD280, 4, true), 2); // ADD.L D0,D1
        assert_eq!(timed(0xD280, 4, false), 3);
        assert_eq!(timed(0x0650, 12, true), 8); // ADDI.W #imm,(A0)
        assert_eq!(timed(0x0650, 12, false), 10);
        assert_eq!(timed(0x0680, 16, true), 6); // ADDI.L #imm,D0
        assert_eq!(timed(0x0680, 16, false), 8);
        assert_eq!(timed(0x0C50, 12, true), 6); // CMPI.W #imm,(A0)
        assert_eq!(timed(0x0C50, 12, false), 7);
        assert_eq!(timed(0xB100, 4, true), 2); // EOR.B D0,D0
        assert_eq!(timed(0xB100, 4, false), 3);
        assert_eq!(timed(0xC0C1, 42, true), 27); // MULU.W D1,D0
        assert_eq!(timed(0xC0C1, 42, false), 28);
        assert_eq!(timed(0x81C1, 90, true), 56); // DIVS.W D1,D0
        assert_eq!(timed(0x81C1, 90, false), 57);
    }

    #[test]
    fn barrel_shifter_cost_is_count_independent() {
        assert_eq!(timed(0xE108, 6, true), 4); // LSL.B #8,D0
        assert_eq!(timed(0xE368, 6, true), 6); // LSL.W D1,D0
        assert_eq!(timed(0xE570, 6, true), 12); // ROXL.W D2,D0
    }

    #[test]
    fn dbcc_distinguishes_loop_expiry_and_true_condition() {
        let mut cpu = CpuCore::new();
        cpu.ir = 0x51C8; // DBF D0,<disp>
        cpu.dar[0] = 7;
        assert_eq!(cpu.cycles_020(12, true), 6);
        assert_eq!(cpu.cycles_020(12, false), 9);

        cpu.dar[0] = 0xFFFF;
        assert_eq!(cpu.cycles_020(14, true), 10);
        assert_eq!(cpu.cycles_020(14, false), 10);

        cpu.ir = 0x50C8; // DBT: condition true
        assert_eq!(cpu.cycles_020(12, true), 6);
        assert_eq!(cpu.cycles_020(12, false), 7);
    }

    #[test]
    fn control_and_save_restore_use_complete_table_entries() {
        assert_eq!(timed(0x4E71, 4, true), 2); // NOP
        assert_eq!(timed(0x4E71, 4, false), 3);
        assert_eq!(timed(0x4E75, 16, true), 10); // RTS
        assert_eq!(timed(0x4E75, 16, false), 12);
        assert_eq!(timed(0x4E73, 20, true), 21); // RTE, normal frame
        assert_eq!(timed(0x4E73, 20, false), 24);
        assert_eq!(timed(0x4E70, 132, true), 518); // RESET
    }

    #[test]
    fn model_dispatch_uses_tables_only_for_the_020_family() {
        use crate::core::types::CpuType;

        let mut cpu = CpuCore::new();
        cpu.ir = 0x4E71; // NOP
        cpu.set_cpu_type(CpuType::M68EC020);
        assert_eq!(cpu.finalize_cycles(4, true), 2);
        cpu.set_cpu_type(CpuType::M68020);
        assert_eq!(cpu.finalize_cycles(4, false), 3);
        cpu.set_cpu_type(CpuType::M68030);
        assert_eq!(cpu.finalize_cycles(4, true), 3);
    }
}
