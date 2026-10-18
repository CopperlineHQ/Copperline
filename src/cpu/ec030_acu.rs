//! The MC68EC030's access control unit (ACU).
//!
//! The EC030 is a 68030 without the paged MMU. In its place sits an access
//! control unit that answers the 68030's PMMU encodings without trapping
//! (M68000 Family Programmer's Reference Manual, section 6 "PMOVE/PTEST
//! (MC68EC030)"; MC68EC030 technical summary instruction table):
//!
//! - `PMOVE` moves the two access control registers AC0/AC1 (long, at the
//!   68030's TT0/TT1 encodings and in the same format) and the ACU status
//!   register ACUSR (word, at the MMUSR encoding).
//! - `PTEST` checks an address against AC0/AC1 and sets ACUSR bit 6 when
//!   either matches. The other ACUSR bits are undefined; they read as zero
//!   here, and the 68030 form's level/address-register operands are ignored
//!   (the EC030 does not return a descriptor address).
//! - `PFLUSH` and `PLOAD` have no effect (there is no ATC).
//! - `PMOVE` to or from TC, SRP or CRP is not an EC030 instruction; the
//!   result is undefined on silicon. Here it raises no exception, writes are
//!   dropped and reads store zero, so software that probes for a paged MMU
//!   by reading TC back finds none.
//!
//! None of this translates addresses: the core runs the EC030 with
//! `has_pmmu` clear. The ACU registers share the core's TT0/TT1/MMUSR
//! fields, so they save, restore and show in the debugger like the full
//! 68030's.
//!
//! The core raises Line-F for every PMMU encoding on its `M68EC030` type,
//! surfacing the trap before it is taken (`StepResult::FlineTrap` from a
//! precise step, `BatchExit::FlineTrap` from a batch); both paths hand it
//! to [`M68kMachine::service_fline_trap`]. A bus error on one of the
//! instruction's accesses ends it the way the core ends its own faulted
//! instructions: the fault is already stacked, nothing is committed, and
//! no trace exception follows.
//! TODO: move this into the m68k core's EC030 PMMU decode.

use m68k::core::ea::{AddressingMode, EaResult};
use m68k::core::execute::{RUN_MODE_BERR_AERR_RESET, RUN_MODE_NORMAL};
use m68k::{CpuType, Size};

use super::M68kMachine;
use crate::bus::CpuBusAccessKind;

/// ACUSR bit 6: the PTEST address matched AC0 and/or AC1.
const ACUSR_AC: u32 = 1 << 6;

/// Whether `opword` is a cpID-0 general instruction (`PMOVE`, `PTEST`,
/// `PFLUSH`, `PLOAD`): `1111 000 000 <ea>` followed by a command word.
pub(super) fn is_pmmu_general(opword: u16) -> bool {
    opword & 0xFFC0 == 0xF000
}

/// Whether access control register `ac` (68030 TT format) matches an access
/// to `addr` in function-code space `fc`.
fn ac_matches(ac: u32, addr: u32, fc: u32, read: bool) -> bool {
    if ac & 0x8000 == 0 {
        return false;
    }
    let base = ac >> 24;
    let mask = (ac >> 16) & 0xFF;
    if ((addr >> 24) ^ base) & !mask & 0xFF != 0 {
        return false;
    }
    let fc_base = (ac >> 4) & 7;
    let fc_mask = ac & 7;
    if (fc ^ fc_base) & !fc_mask & 7 != 0 {
        return false;
    }
    // RWM clear: the R/W field (1 = reads only) must agree with the access.
    ac & 0x0100 != 0 || (ac & 0x0200 != 0) == read
}

impl M68kMachine {
    /// Whether the next instruction is a PMMU encoding the EC030's ACU
    /// answers, so the precise loop must step it through
    /// [`Self::service_fline_trap`].
    pub(super) fn ec030_acu_instruction_next(&self) -> bool {
        if self.cpu.cpu_type != CpuType::M68EC030 {
            return false;
        }
        // The opword the core will fetch: the instruction cache's copy when
        // it holds one (it does not snoop writes, so it can be stale),
        // otherwise memory.
        let pc = self.bus.mask(self.cpu.pc);
        let opword = match self.bus.cache_read(pc, 2, CpuBusAccessKind::Fetch) {
            Some(word) => word as u16,
            None => self.bus.peek_word(pc),
        };
        is_pmmu_general(opword)
    }

    /// Finish an F-line instruction the core surfaced instead of executing:
    /// the PC is at its first extension word and `ppc` at the opcode. An
    /// EC030 ACU instruction executes (with its trace check); anything else
    /// takes the Line-F exception. Returns the CPU clocks spent.
    pub(super) fn service_fline_trap(&mut self, opcode: u16) -> i32 {
        if self.cpu.cpu_type != CpuType::M68EC030 || !is_pmmu_general(opcode) {
            return self.cpu.take_fline_exception(&mut self.bus);
        }
        // Privileged, like the 68030's PMMU instructions.
        if !self.cpu.is_supervisor() {
            return self.cpu.exception_privilege(&mut self.bus);
        }
        let outcome = self.ec030_acu_execute(opcode);
        if self.acu_faulted() {
            self.end_faulted_acu_instruction();
            return outcome.unwrap_or(0);
        }
        match outcome {
            Some(mut cycles) => {
                if self.cpu.check_trace() {
                    cycles += self.cpu.exception_trace(&mut self.bus);
                }
                cycles
            }
            None => self.cpu.take_fline_exception(&mut self.bus),
        }
    }

    /// Whether one of the current instruction's accesses took a bus or
    /// address error. The core has then already stacked the fault and
    /// entered its handler, and turns further accesses into no-ops.
    fn acu_faulted(&self) -> bool {
        self.cpu.run_mode == RUN_MODE_BERR_AERR_RESET
    }

    /// Close out a faulted ACU instruction, as the core's
    /// `end_faulted_instruction` does for its own: resume at the handler
    /// entry and leave the fault run mode. An extension fetch after the
    /// fault still advanced the PC from there, so the entry is re-read from
    /// the vector just taken. A double fault has halted the CPU, which
    /// keeps the fault run mode until reset.
    fn end_faulted_acu_instruction(&mut self) {
        if self.cpu.stopped != 0 {
            return;
        }
        if let Some(vector) = self.cpu.last_exception_vector {
            let entry = self.cpu.vbr.wrapping_add(vector.wrapping_mul(4));
            self.cpu.pc = self.bus.peek_long(entry);
        }
        self.cpu.run_mode = RUN_MODE_NORMAL;
    }

    /// Execute supervisor-mode PMMU opcode `opcode` on the EC030's ACU.
    /// `None` means the encoding is not an ACU instruction and the caller
    /// raises Line-F (unless an access faulted, see
    /// [`Self::acu_faulted`]). No ACU register changes once an access has
    /// faulted.
    fn ec030_acu_execute(&mut self, opcode: u16) -> Option<i32> {
        let ext = self.cpu.read_imm_16(&mut self.bus);
        if self.acu_faulted() {
            return None;
        }
        let mode = ((opcode >> 3) & 7) as u8;
        let reg = (opcode & 7) as u8;
        let to_memory = ext & 0x0200 != 0;
        let preg = (ext >> 10) & 7;
        match ext >> 13 {
            // PMOVE AC0/AC1 <-> <ea> (long). Bit 8 is the 68030's FD flag.
            0b000 if ext & 0x00FF == 0 && matches!(preg, 2 | 3) => {
                let addr = self.ec030_operand_address(mode, reg)?;
                if to_memory {
                    let value = if preg == 2 {
                        self.cpu.mmu_tt0
                    } else {
                        self.cpu.mmu_tt1
                    };
                    self.cpu.write_32(&mut self.bus, addr, value);
                } else {
                    let value = self.cpu.read_32(&mut self.bus, addr);
                    if self.acu_faulted() {
                        return Some(4);
                    }
                    if preg == 2 {
                        self.cpu.mmu_tt0 = value;
                    } else {
                        self.cpu.mmu_tt1 = value;
                    }
                }
                Some(4)
            }
            // PMOVE TC (long) / SRP / CRP (quad): no such registers.
            0b010 if ext & 0x00FF == 0 && matches!(preg, 0 | 2 | 3) => {
                let addr = self.ec030_operand_address(mode, reg)?;
                let quad = preg != 0;
                if to_memory {
                    self.cpu.write_32(&mut self.bus, addr, 0);
                    if quad {
                        self.cpu.write_32(&mut self.bus, addr.wrapping_add(4), 0);
                    }
                }
                Some(if quad { 8 } else { 4 })
            }
            // PMOVE ACUSR <-> <ea> (word).
            0b011 if ext & 0x1DFF == 0 => {
                let addr = self.ec030_operand_address(mode, reg)?;
                if to_memory {
                    let value = self.cpu.mmu_sr as u16;
                    self.cpu.write_16(&mut self.bus, addr, value);
                } else {
                    let value = self.cpu.read_16(&mut self.bus, addr);
                    if self.acu_faulted() {
                        return Some(4);
                    }
                    self.cpu.mmu_sr = u32::from(value);
                }
                Some(4)
            }
            // PLOAD fc,<ea> / PFLUSHA / PFLUSH fc,#mask[,<ea>]: no effect,
            // beyond consuming the operand's extension words. PVALID and
            // PFLUSHS (68851 only) fall through to Line-F.
            0b001 => match preg {
                0b001 | 0b100 => Some(8),
                0b000 | 0b110 => {
                    self.ec030_operand_address(mode, reg)?;
                    Some(8)
                }
                _ => None,
            },
            // PTEST{R,W} fc,<ea>: ACUSR reports whether AC0 or AC1 match.
            0b100 => {
                let fc = self.ec030_function_code(ext & 0x1F);
                let addr = self.ec030_operand_address(mode, reg)?;
                let ac0 = self.cpu.mmu_tt0;
                let ac1 = self.cpu.mmu_tt1;
                let hit =
                    ac_matches(ac0, addr, fc, to_memory) || ac_matches(ac1, addr, fc, to_memory);
                self.cpu.mmu_sr = if hit { ACUSR_AC } else { 0 };
                Some(8)
            }
            _ => None,
        }
    }

    /// Resolve a PMMU instruction's operand address. Only the control
    /// alterable modes are valid; anything else is `None` (Line-F) and is
    /// rejected before the core touches a register, so a postincrement or
    /// predecrement never steps its address register.
    fn ec030_operand_address(&mut self, mode: u8, reg: u8) -> Option<u32> {
        if !(matches!(mode, 2 | 5 | 6) || (mode == 7 && reg <= 1)) {
            return None;
        }
        let am = AddressingMode::decode(mode, reg)?;
        match self.cpu.resolve_ea(&mut self.bus, am, Size::Long) {
            EaResult::Memory(addr) if !self.acu_faulted() => Some(addr),
            _ => None,
        }
    }

    /// The function code named by a PTEST/PLOAD/PFLUSH FC field: `10XXX`
    /// immediate, `01DDD` from a data register, `00000` SFC, `00001` DFC.
    fn ec030_function_code(&self, field: u16) -> u32 {
        if field & 0x10 != 0 {
            u32::from(field & 7)
        } else if field & 0x08 != 0 {
            self.cpu.d(usize::from(field & 7)) & 7
        } else if field == 1 {
            self.cpu.dfc & 7
        } else {
            self.cpu.sfc & 7
        }
    }
}
