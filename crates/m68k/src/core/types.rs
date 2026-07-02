//! Core type definitions for the M68000 family.

use super::cpu::CpuCore;
use super::memory::AddressBus;

/// Supported CPU types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[repr(u32)]
pub enum CpuType {
    Invalid = 0,
    #[default]
    M68000 = 1,
    M68010 = 2,
    M68EC020 = 3,
    M68020 = 4,
    M68EC030 = 5,
    M68030 = 6,
    M68EC040 = 7,
    M68LC040 = 8,
    M68040 = 9,
    SCC68070 = 10,
    M68060 = 11,
}

/// Trap handler with CPU and bus access for HLE.
///
/// This is the recommended trait for high-level emulation: handlers get
/// direct access to CPU state and the memory bus while a trap is being
/// serviced. Return `true` to mark the trap as handled, or `false` to
/// fall back to the real hardware exception.
pub trait HleHandler {
    /// Handle an A-line trap (0xAxxx opcode).
    #[inline]
    fn handle_aline(
        &mut self,
        _cpu: &mut CpuCore,
        _bus: &mut dyn AddressBus,
        _opcode: u16,
    ) -> bool {
        false
    }

    /// Handle an F-line trap (0xFxxx opcode).
    #[inline]
    fn handle_fline(
        &mut self,
        _cpu: &mut CpuCore,
        _bus: &mut dyn AddressBus,
        _opcode: u16,
    ) -> bool {
        false
    }

    /// Handle a TRAP #n instruction.
    #[inline]
    fn handle_trap(
        &mut self,
        _cpu: &mut CpuCore,
        _bus: &mut dyn AddressBus,
        _trap_num: u8,
    ) -> bool {
        false
    }

    /// Handle a BKPT #n instruction.
    #[inline]
    fn handle_breakpoint(
        &mut self,
        _cpu: &mut CpuCore,
        _bus: &mut dyn AddressBus,
        _bp_num: u8,
    ) -> bool {
        false
    }

    /// Handle an illegal instruction.
    #[inline]
    fn handle_illegal(
        &mut self,
        _cpu: &mut CpuCore,
        _bus: &mut dyn AddressBus,
        _opcode: u16,
    ) -> bool {
        false
    }
}

/// A no-op HLE handler that takes all exceptions (default behavior).
#[derive(Default, Clone, Copy)]
pub struct NoOpHleHandler;

impl HleHandler for NoOpHleHandler {}

/// Operand size for instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Byte,
    Word,
    Long,
}

impl Size {
    #[inline]
    pub const fn bytes(self) -> u32 {
        match self {
            Size::Byte => 1,
            Size::Word => 2,
            Size::Long => 4,
        }
    }

    #[inline]
    pub const fn bits(self) -> u8 {
        match self {
            Size::Byte => 8,
            Size::Word => 16,
            Size::Long => 32,
        }
    }

    #[inline]
    pub const fn mask(self) -> u32 {
        match self {
            Size::Byte => 0xFF,
            Size::Word => 0xFFFF,
            Size::Long => 0xFFFF_FFFF,
        }
    }

    #[inline]
    pub const fn msb_mask(self) -> u32 {
        match self {
            Size::Byte => 0x80,
            Size::Word => 0x8000,
            Size::Long => 0x8000_0000,
        }
    }
}

/// Internal result from instruction dispatch.
///
/// This is used internally by `dispatch_instruction` and `step_with_hle_handler`.
/// It includes trap variants for internal handling - the public `StepResult`
/// doesn't expose these since traps are handled via callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InternalStepResult {
    /// Instruction executed normally.
    Ok { cycles: i32 },
    /// A-line trap intercepted.
    AlineTrap { opcode: u16 },
    /// F-line trap intercepted.
    FlineTrap { opcode: u16 },
    /// TRAP #n instruction.
    TrapInstruction { trap_num: u8 },
    /// BKPT #n instruction.
    Breakpoint { bp_num: u8 },
    /// Illegal instruction.
    IllegalInstruction { opcode: u16 },
}

/// Result from executing a single CPU instruction.
///
/// This enum is simplified - traps are handled internally via `step_with_hle_handler()`.
/// For HLE interception, implement `HleHandler` and use `step_with_hle_handler()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepResult {
    /// Instruction executed normally.
    Ok {
        /// Number of CPU cycles consumed.
        cycles: i32,
    },
    /// A-line trap (0xAxxx opcode).
    AlineTrap { opcode: u16 },
    /// F-line trap (0xFxxx opcode).
    FlineTrap { opcode: u16 },
    /// TRAP #n instruction.
    TrapInstruction { trap_num: u8 },
    /// BKPT #n instruction.
    Breakpoint { bp_num: u8 },
    /// Illegal instruction.
    IllegalInstruction { opcode: u16 },
    /// CPU is stopped (STOP instruction executed).
    Stopped,
}

impl StepResult {
    /// Returns the cycle count if instruction executed normally.
    #[inline]
    pub fn cycles(&self) -> Option<i32> {
        match self {
            StepResult::Ok { cycles } => Some(*cycles),
            _ => None,
        }
    }

    /// Returns `true` if the CPU is stopped.
    #[inline]
    pub fn is_stopped(&self) -> bool {
        matches!(self, StepResult::Stopped)
    }
}
