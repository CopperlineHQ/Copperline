//! Core type definitions for the M68000 family.

use super::cpu::CpuCore;
use super::memory::AddressBus;

/// Supported CPU types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u32)]
pub enum CpuType {
    /// Sentinel used before a concrete processor model is selected.
    Invalid = 0,
    /// Original 16/32-bit M68000.
    #[default]
    M68000 = 1,
    /// M68010 with VBR, restartable faults, and loop mode.
    M68010 = 2,
    /// Embedded 68020 variant with a 24-bit external address bus.
    M68EC020 = 3,
    /// Full M68020.
    M68020 = 4,
    /// M68030 without an on-chip PMMU.
    M68EC030 = 5,
    /// Full M68030 with PMMU.
    M68030 = 6,
    /// M68040 without an MMU or FPU.
    M68EC040 = 7,
    /// M68040 with an MMU but without an FPU.
    M68LC040 = 8,
    /// Full M68040 with MMU and FPU.
    M68040 = 9,
    /// Philips SCC68070 system-controller CPU.
    SCC68070 = 10,
    /// Full superscalar M68060.
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

/// HLE handler that declines every interception.
///
/// Passing this handler to [`CpuCore::step_with_hle_handler`] causes each trap
/// to fall back to its architectural hardware exception.
#[derive(Default, Clone, Copy)]
pub struct NoOpHleHandler;

impl HleHandler for NoOpHleHandler {}

/// Operand size for instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// Eight-bit operand.
    Byte,
    /// Sixteen-bit operand.
    Word,
    /// Thirty-two-bit operand.
    Long,
}

impl Size {
    #[inline]
    /// Return the operand width in bytes.
    pub const fn bytes(self) -> u32 {
        match self {
            Size::Byte => 1,
            Size::Word => 2,
            Size::Long => 4,
        }
    }

    #[inline]
    /// Return the operand width in bits.
    pub const fn bits(self) -> u8 {
        match self {
            Size::Byte => 8,
            Size::Word => 16,
            Size::Long => 32,
        }
    }

    #[inline]
    /// Return a low-bit mask covering the operand width.
    pub const fn mask(self) -> u32 {
        match self {
            Size::Byte => 0xFF,
            Size::Word => 0xFFFF,
            Size::Long => 0xFFFF_FFFF,
        }
    }

    #[inline]
    /// Return the most-significant-bit mask for the operand width.
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
/// It includes trap variants that [`CpuCore::step`] exposes to its caller and
/// that [`CpuCore::step_with_hle_handler`] routes through callbacks.
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
/// [`CpuCore::step`] returns trap variants without taking their hardware
/// exceptions. [`CpuCore::step_with_hle_handler`] instead offers those traps to
/// an [`HleHandler`] and returns [`StepResult::Ok`] after either interception or
/// architectural exception delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepResult {
    /// Instruction executed normally.
    Ok {
        /// Number of CPU cycles consumed.
        cycles: i32,
    },
    /// A-line trap (0xAxxx opcode).
    AlineTrap {
        /// Trapping opcode word.
        opcode: u16,
    },
    /// F-line trap (0xFxxx opcode).
    FlineTrap {
        /// Trapping opcode word.
        opcode: u16,
    },
    /// TRAP #n instruction.
    TrapInstruction {
        /// Trap number from 0 through 15.
        trap_num: u8,
    },
    /// BKPT #n instruction.
    Breakpoint {
        /// Breakpoint number encoded by the instruction.
        bp_num: u8,
    },
    /// Illegal instruction.
    IllegalInstruction {
        /// Illegal opcode word.
        opcode: u16,
    },
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

/// Reason a [`CpuCore::run_batch`](crate::CpuCore::run_batch) call returned.
///
/// Trap variants surface the same state a corresponding
/// [`StepResult`] would after `step()`: the program counter has advanced
/// past the trapping opcode word and `CpuCore::ppc` holds the address of
/// the trapping instruction itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchExit {
    /// The instruction budget was fully consumed with no other event.
    BudgetExhausted,
    /// The bus requested synchronization before the next instruction.
    BoundaryRequested,
    /// The CPU is stopped (STOP instruction executed, or it was already
    /// stopped on entry — the latter returns with `instructions == 0`).
    Stopped,
    /// Execution reached a PC in the caller's watch list. The instruction
    /// at the watched PC has **not** been executed yet.
    WatchedPc {
        /// Watched program-counter value.
        pc: u32,
    },
    /// A-line trap (0xAxxx opcode).
    AlineTrap {
        /// Trapping opcode word.
        opcode: u16,
    },
    /// F-line trap (0xFxxx opcode).
    FlineTrap {
        /// Trapping opcode word.
        opcode: u16,
    },
    /// TRAP #n instruction.
    TrapInstruction {
        /// Trap number from 0 through 15.
        trap_num: u8,
    },
    /// BKPT #n instruction.
    Breakpoint {
        /// Breakpoint number encoded by the instruction.
        bp_num: u8,
    },
    /// Illegal instruction.
    IllegalInstruction {
        /// Illegal opcode word.
        opcode: u16,
    },
}

/// Result of a [`CpuCore::run_batch`](crate::CpuCore::run_batch) call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchResult {
    /// Number of instructions that fully retired during the batch.
    ///
    /// Trapping instructions (A-line/F-line/TRAP/BKPT/illegal) are **not**
    /// included — the embedder decides how to account for them after
    /// handling the trap. Instructions that faulted mid-execution (bus or
    /// address error, exception taken internally) count as one.
    pub instructions: u32,
    /// Why the batch returned.
    pub exit: BatchExit,
}

/// Boundary reported by
/// [`CpuCore::run_for_cycles_with_boundary_hook`](crate::CpuCore::run_for_cycles_with_boundary_hook).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleBoundaryEvent {
    /// A normally completed instruction retired.
    Instruction {
        /// Cycles consumed by the instruction, including any internally
        /// taken synchronous exception.
        cycles: i32,
    },
    /// Interrupt entry completed without retiring an instruction.
    InterruptEntry {
        /// Cycles consumed by interrupt entry.
        cycles: i32,
    },
}

/// Control returned by a cycle-boundary hook passed to
/// [`CpuCore::run_for_cycles_with_hook`](crate::CpuCore::run_for_cycles_with_hook)
/// or
/// [`CpuCore::run_for_cycles_with_boundary_hook`](crate::CpuCore::run_for_cycles_with_boundary_hook).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleBatchControl {
    /// Continue execution after applying the hook's CPU and bus updates.
    Continue,
    /// Return from the runner before another instruction is fetched.
    Return,
}

/// Reason a [`CpuCore::run_for_cycles`](crate::CpuCore::run_for_cycles),
/// [`CpuCore::run_for_cycles_with_hook`](crate::CpuCore::run_for_cycles_with_hook),
/// or
/// [`CpuCore::run_for_cycles_with_boundary_hook`](crate::CpuCore::run_for_cycles_with_boundary_hook)
/// call returned.
///
/// Trap variants have exactly the same CPU/PC state as [`StepResult`]: the
/// trapping opcode has been fetched and `pc` points past it, but no hardware
/// exception entry has been performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleBatchExit {
    /// The requested cycle budget was met or crossed at an instruction or
    /// interrupt boundary.
    BudgetExhausted,
    /// The address bus or a cycle-boundary hook requested a return after a
    /// completed instruction or interrupt entry.
    ///
    /// Completed work is included in the result. Interrupt entry contributes
    /// cycles but no retired instruction. This exit takes precedence when the
    /// completed work also meets or crosses the cycle budget.
    BoundaryRequested,
    /// The CPU executed STOP, or was already stopped with no serviceable
    /// interrupt on entry.
    Stopped,
    /// A-line trap (0xAxxx opcode).
    AlineTrap {
        /// Trapping opcode word.
        opcode: u16,
    },
    /// F-line trap (0xFxxx opcode).
    FlineTrap {
        /// Trapping opcode word.
        opcode: u16,
    },
    /// TRAP #n instruction.
    TrapInstruction {
        /// Trap number from 0 through 15.
        trap_num: u8,
    },
    /// BKPT #n instruction.
    Breakpoint {
        /// Breakpoint number encoded by the instruction.
        bp_num: u8,
    },
    /// Illegal instruction.
    IllegalInstruction {
        /// Illegal opcode word.
        opcode: u16,
    },
}

/// Result of [`CpuCore::run_for_cycles`](crate::CpuCore::run_for_cycles) or
/// one of its hook-enabled variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleBatchResult {
    /// Actual CPU cycles consumed. This may exceed the requested budget
    /// because instructions and interrupt entry are never split.
    pub cycles: i32,
    /// Number of instructions that fully retired.
    ///
    /// A surfaced trapping instruction is excluded. RESET and internally
    /// taken exceptions count as instructions; interrupt entry does not.
    pub instructions: u32,
    /// Why execution returned.
    pub exit: CycleBatchExit,
}
