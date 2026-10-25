//! Cycle timing tables.
//!
//! Ported from Musashi m68kcpu.c - m68ki_exception_cycle_table

/// Exception cycle counts for each CPU type.
/// Index: 0=68000, 1=68010, 2=68020, 3=68030, 4=68040
/// Each sub-array contains cycles for vectors 0-255.
pub const EXCEPTION_CYCLES: [[u8; 256]; 5] = [
    // 68000
    [
        40, //  0: Reset - Initial Stack Pointer
        4,  //  1: Reset - Initial Program Counter
        50, //  2: Bus Error
        50, //  3: Address Error
        34, //  4: Illegal Instruction
        38, //  5: Divide by Zero
        40, //  6: CHK
        34, //  7: TRAPV
        34, //  8: Privilege Violation
        34, //  9: Trace
        34, // 10: 1010 (Line-A)
        34, // 11: 1111 (Line-F)
        4,  // 12: RESERVED
        4,  // 13: Coprocessor Protocol Violation
        4,  // 14: Format Error
        44, // 15: Uninitialized Interrupt
        4,  // 16: RESERVED
        4,  // 17: RESERVED
        4,  // 18: RESERVED
        4,  // 19: RESERVED
        4,  // 20: RESERVED
        4,  // 21: RESERVED
        4,  // 22: RESERVED
        4,  // 23: RESERVED
        44, // 24: Spurious Interrupt
        44, // 25: Level 1 Interrupt Autovector
        44, // 26: Level 2 Interrupt Autovector
        44, // 27: Level 3 Interrupt Autovector
        44, // 28: Level 4 Interrupt Autovector
        44, // 29: Level 5 Interrupt Autovector
        44, // 30: Level 6 Interrupt Autovector
        44, // 31: Level 7 Interrupt Autovector
        34, // 32: TRAP #0
        34, // 33: TRAP #1
        34, // 34: TRAP #2
        34, // 35: TRAP #3
        34, // 36: TRAP #4
        34, // 37: TRAP #5
        34, // 38: TRAP #6
        34, // 39: TRAP #7
        34, // 40: TRAP #8
        34, // 41: TRAP #9
        34, // 42: TRAP #10
        34, // 43: TRAP #11
        34, // 44: TRAP #12
        34, // 45: TRAP #13
        34, // 46: TRAP #14
        34, // 47: TRAP #15
        4,  // 48: FP Branch or Set on Unknown Condition
        4,  // 49: FP Inexact Result
        4,  // 50: FP Divide by Zero
        4,  // 51: FP Underflow
        4,  // 52: FP Operand Error
        4,  // 53: FP Overflow
        4,  // 54: FP Signaling NAN
        4,  // 55: FP Unimplemented Data Type
        4,  // 56: MMU Configuration Error
        4,  // 57: MMU Illegal Operation Error
        4,  // 58: MMU Access Level Violation Error
        4,  // 59: RESERVED
        4,  // 60: RESERVED
        4,  // 61: RESERVED
        4,  // 62: RESERVED
        4,  // 63: RESERVED
        // 64-255: User Defined (all 4 cycles)
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    ],
    // 68010
    [
        40,  //  0: Reset - Initial Stack Pointer
        4,   //  1: Reset - Initial Program Counter
        126, //  2: Bus Error
        126, //  3: Address Error
        38,  //  4: Illegal Instruction
        44,  //  5: Divide by Zero
        44,  //  6: CHK
        34,  //  7: TRAPV
        38,  //  8: Privilege Violation
        38,  //  9: Trace
        4,   // 10: 1010 (Line-A)
        4,   // 11: 1111 (Line-F)
        4,   // 12: RESERVED
        4,   // 13: Coprocessor Protocol Violation
        4,   // 14: Format Error
        44,  // 15: Uninitialized Interrupt
        4,   // 16: RESERVED
        4,   // 17: RESERVED
        4,   // 18: RESERVED
        4,   // 19: RESERVED
        4,   // 20: RESERVED
        4,   // 21: RESERVED
        4,   // 22: RESERVED
        4,   // 23: RESERVED
        46,  // 24: Spurious Interrupt
        46,  // 25: Level 1 Interrupt Autovector
        46,  // 26: Level 2 Interrupt Autovector
        46,  // 27: Level 3 Interrupt Autovector
        46,  // 28: Level 4 Interrupt Autovector
        46,  // 29: Level 5 Interrupt Autovector
        46,  // 30: Level 6 Interrupt Autovector
        46,  // 31: Level 7 Interrupt Autovector
        38,  // 32: TRAP #0
        38,  // 33: TRAP #1
        38,  // 34: TRAP #2
        38,  // 35: TRAP #3
        38,  // 36: TRAP #4
        38,  // 37: TRAP #5
        38,  // 38: TRAP #6
        38,  // 39: TRAP #7
        38,  // 40: TRAP #8
        38,  // 41: TRAP #9
        38,  // 42: TRAP #10
        38,  // 43: TRAP #11
        38,  // 44: TRAP #12
        38,  // 45: TRAP #13
        38,  // 46: TRAP #14
        38,  // 47: TRAP #15
        4,   // 48-63: FP/MMU (unemulated)
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, // 64-255: User Defined
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    ],
    // 68020
    [
        4,  //  0: Reset - Initial Stack Pointer
        4,  //  1: Reset - Initial Program Counter
        50, //  2: Bus Error
        50, //  3: Address Error
        20, //  4: Illegal Instruction
        38, //  5: Divide by Zero
        40, //  6: CHK
        20, //  7: TRAPV
        34, //  8: Privilege Violation
        25, //  9: Trace
        20, // 10: 1010 (Line-A)
        20, // 11: 1111 (Line-F)
        4,  // 12: RESERVED
        4,  // 13: Coprocessor Protocol Violation
        4,  // 14: Format Error
        30, // 15: Uninitialized Interrupt
        4,  // 16: RESERVED
        4,  // 17: RESERVED
        4,  // 18: RESERVED
        4,  // 19: RESERVED
        4,  // 20: RESERVED
        4,  // 21: RESERVED
        4,  // 22: RESERVED
        4,  // 23: RESERVED
        30, // 24: Spurious Interrupt
        30, // 25: Level 1 Interrupt Autovector
        30, // 26: Level 2 Interrupt Autovector
        30, // 27: Level 3 Interrupt Autovector
        30, // 28: Level 4 Interrupt Autovector
        30, // 29: Level 5 Interrupt Autovector
        30, // 30: Level 6 Interrupt Autovector
        30, // 31: Level 7 Interrupt Autovector
        20, // 32: TRAP #0
        20, // 33: TRAP #1
        20, // 34: TRAP #2
        20, // 35: TRAP #3
        20, // 36: TRAP #4
        20, // 37: TRAP #5
        20, // 38: TRAP #6
        20, // 39: TRAP #7
        20, // 40: TRAP #8
        20, // 41: TRAP #9
        20, // 42: TRAP #10
        20, // 43: TRAP #11
        20, // 44: TRAP #12
        20, // 45: TRAP #13
        20, // 46: TRAP #14
        20, // 47: TRAP #15
        4,  // 48-63: FP/MMU
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, // 64-255: User Defined
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    ],
    // 68030 (same as 68020)
    [
        4,  //  0: Reset - Initial Stack Pointer
        4,  //  1: Reset - Initial Program Counter
        50, //  2: Bus Error
        50, //  3: Address Error
        20, //  4: Illegal Instruction
        38, //  5: Divide by Zero
        40, //  6: CHK
        20, //  7: TRAPV
        34, //  8: Privilege Violation
        25, //  9: Trace
        20, // 10: 1010 (Line-A)
        20, // 11: 1111 (Line-F)
        4,  // 12: RESERVED
        4,  // 13: Coprocessor Protocol Violation
        4,  // 14: Format Error
        30, // 15: Uninitialized Interrupt
        4,  // 16: RESERVED
        4,  // 17: RESERVED
        4,  // 18: RESERVED
        4,  // 19: RESERVED
        4,  // 20: RESERVED
        4,  // 21: RESERVED
        4,  // 22: RESERVED
        4,  // 23: RESERVED
        30, // 24: Spurious Interrupt
        30, // 25: Level 1 Interrupt Autovector
        30, // 26: Level 2 Interrupt Autovector
        30, // 27: Level 3 Interrupt Autovector
        30, // 28: Level 4 Interrupt Autovector
        30, // 29: Level 5 Interrupt Autovector
        30, // 30: Level 6 Interrupt Autovector
        30, // 31: Level 7 Interrupt Autovector
        20, // 32: TRAP #0
        20, // 33: TRAP #1
        20, // 34: TRAP #2
        20, // 35: TRAP #3
        20, // 36: TRAP #4
        20, // 37: TRAP #5
        20, // 38: TRAP #6
        20, // 39: TRAP #7
        20, // 40: TRAP #8
        20, // 41: TRAP #9
        20, // 42: TRAP #10
        20, // 43: TRAP #11
        20, // 44: TRAP #12
        20, // 45: TRAP #13
        20, // 46: TRAP #14
        20, // 47: TRAP #15
        4,  // 48-63: FP/MMU
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, // 64-255: User Defined
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    ],
    // 68040 (TODO: these values are approximate)
    [
        4,  //  0: Reset - Initial Stack Pointer
        4,  //  1: Reset - Initial Program Counter
        50, //  2: Bus Error
        50, //  3: Address Error
        20, //  4: Illegal Instruction
        38, //  5: Divide by Zero
        40, //  6: CHK
        20, //  7: TRAPV
        34, //  8: Privilege Violation
        25, //  9: Trace
        20, // 10: 1010 (Line-A)
        20, // 11: 1111 (Line-F)
        4,  // 12: RESERVED
        4,  // 13: Coprocessor Protocol Violation
        4,  // 14: Format Error
        30, // 15: Uninitialized Interrupt
        4,  // 16: RESERVED
        4,  // 17: RESERVED
        4,  // 18: RESERVED
        4,  // 19: RESERVED
        4,  // 20: RESERVED
        4,  // 21: RESERVED
        4,  // 22: RESERVED
        4,  // 23: RESERVED
        30, // 24: Spurious Interrupt
        30, // 25: Level 1 Interrupt Autovector
        30, // 26: Level 2 Interrupt Autovector
        30, // 27: Level 3 Interrupt Autovector
        30, // 28: Level 4 Interrupt Autovector
        30, // 29: Level 5 Interrupt Autovector
        30, // 30: Level 6 Interrupt Autovector
        30, // 31: Level 7 Interrupt Autovector
        20, // 32: TRAP #0
        20, // 33: TRAP #1
        20, // 34: TRAP #2
        20, // 35: TRAP #3
        20, // 36: TRAP #4
        20, // 37: TRAP #5
        20, // 38: TRAP #6
        20, // 39: TRAP #7
        20, // 40: TRAP #8
        20, // 41: TRAP #9
        20, // 42: TRAP #10
        20, // 43: TRAP #11
        20, // 44: TRAP #12
        20, // 45: TRAP #13
        20, // 46: TRAP #14
        20, // 47: TRAP #15
        4,  // 48-63: FP/MMU
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, // 64-255: User Defined
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    ],
];

use super::ea::AddressingMode;
use super::types::Size;

/// M68000UM Table 8-2: effective-address calculation time, including the
/// operand fetch, for the 68000/68010. Added to a standard instruction's
/// base time when the operand is in memory (register-direct forms cost 0).
#[inline]
pub(crate) fn ea_cycles_68000(mode: AddressingMode, size: Size) -> i32 {
    let long = size == Size::Long;
    match mode {
        AddressingMode::DataDirect(_) | AddressingMode::AddressDirect(_) => 0,
        AddressingMode::AddressIndirect(_) | AddressingMode::PostIncrement(_) => {
            if long {
                8
            } else {
                4
            }
        }
        AddressingMode::PreDecrement(_) => {
            if long {
                10
            } else {
                6
            }
        }
        AddressingMode::Displacement(_)
        | AddressingMode::AbsoluteShort
        | AddressingMode::PcDisplacement => {
            if long {
                12
            } else {
                8
            }
        }
        AddressingMode::Index(_) | AddressingMode::PcIndex => {
            if long {
                14
            } else {
                10
            }
        }
        AddressingMode::AbsoluteLong => {
            if long {
                16
            } else {
                12
            }
        }
        AddressingMode::Immediate => {
            if long {
                8
            } else {
                4
            }
        }
    }
}

/// Control-address calculation time for LEA (M68000UM Table 8-9 column).
#[inline]
pub(crate) fn lea_ea_cycles_68000(mode: AddressingMode) -> i32 {
    match mode {
        AddressingMode::AddressIndirect(_) => 4,
        AddressingMode::Displacement(_)
        | AddressingMode::AbsoluteShort
        | AddressingMode::PcDisplacement => 8,
        AddressingMode::Index(_) | AddressingMode::PcIndex => 12,
        AddressingMode::AbsoluteLong => 12,
        _ => 0,
    }
}

impl super::cpu::CpuCore {
    /// Standard-operand EA time (M68000UM Table 8-2) on 68000/68010;
    /// 0 on later types, which keep the legacy flat timings.
    #[inline]
    pub(crate) fn ea_time(&self, mode: AddressingMode, size: Size) -> i32 {
        if self.is_pre_68020 {
            ea_cycles_68000(mode, size)
        } else {
            0
        }
    }
}
