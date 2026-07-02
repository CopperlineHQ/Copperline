//! Main execution loop.
//!
//! Implements the fetch-decode-execute cycle.

use super::cpu::{CpuCore, SFLAG_SET};
use super::decode::dispatch_instruction;
use super::memory::AddressBus;
use super::types::StepResult;

/// Stop level constants.
pub const STOP_LEVEL_STOP: u32 = 1;
pub const STOP_LEVEL_HALT: u32 = 2;

/// Run mode constants.
pub const RUN_MODE_NORMAL: u32 = 0;
pub const RUN_MODE_BERR_AERR_RESET: u32 = 1;

impl CpuCore {
    /// Execute instructions for the given number of cycles.
    ///
    /// Returns the number of cycles actually consumed.
    ///
    /// **Note**: This function is intended for batch execution without HLE support.
    /// A-line and F-line traps are silently ignored (treated as 0 cycles).
    /// For HLE support, use `step()` and handle `StepResult::AlineTrap`/`FlineTrap`.
    pub fn execute<B: AddressBus>(&mut self, bus: &mut B, num_cycles: i32) -> i32 {
        // Handle reset cycles
        if self.reset_cycles > 0 {
            let rc = self.reset_cycles as i32;
            self.reset_cycles = 0;
            let remaining = num_cycles - rc;
            if remaining <= 0 {
                return rc;
            }
            self.cycles_remaining = remaining;
        } else {
            self.cycles_remaining = num_cycles;
        }
        self.initial_cycles = num_cycles;

        // Check for pending interrupts
        self.check_and_service_interrupts(bus);

        // If stopped, consume no cycles
        if self.stopped != 0 {
            self.cycles_remaining = 0;
            return self.initial_cycles;
        }

        // Main execution loop
        while self.cycles_remaining > 0 {
            // Save previous PC
            self.ppc = self.pc;

            // Save D/A registers for bus error recovery
            self.dar_save = self.dar;
            // Save SR for bus/address error recovery
            self.sr_save = self.get_sr();

            // Fetch opcode
            self.ir = self.fetch_opcode(bus) as u32;

            // If a bus/address error occurred during fetch, the exception is already taken.
            if self.run_mode == RUN_MODE_BERR_AERR_RESET {
                self.run_mode = RUN_MODE_NORMAL;
                continue;
            }

            // Dispatch instruction
            let result = dispatch_instruction(self, bus, self.ir as u16);

            // Auto-take all trap exceptions, extract cycles
            use crate::core::types::InternalStepResult;
            let cycles = match result {
                InternalStepResult::Ok { cycles } => self.scale_cycles_for_cpu_type(cycles),
                InternalStepResult::AlineTrap { .. } => self.take_aline_exception(bus),
                InternalStepResult::FlineTrap { .. } => self.take_fline_exception(bus),
                InternalStepResult::TrapInstruction { trap_num } => {
                    self.take_trap_exception(bus, trap_num)
                }
                InternalStepResult::Breakpoint { .. } => self.take_bkpt_exception(bus),
                InternalStepResult::IllegalInstruction { .. } => self.take_illegal_exception(bus),
            };
            self.cycles_remaining -= cycles;

            // If a bus/address error occurred mid-instruction, we already built the exception frame
            // and jumped to the handler. Skip trace/interrupt checks for the faulting instruction.
            if self.run_mode == RUN_MODE_BERR_AERR_RESET {
                self.run_mode = RUN_MODE_NORMAL;
                continue;
            }

            // End-of-instruction prefetch: top the queue back up to two words
            // (a no-op after flow changes, whose refill already filled it).
            self.top_up_prefetch(bus);

            // Check for trace exception (T1 flag set before instruction)
            if self.check_trace() {
                let trace_cycles = self.exception_trace(bus);
                self.cycles_remaining -= trace_cycles;
            }

            // Check for interrupts after each instruction
            if self.int_level > 0 {
                self.check_and_service_interrupts(bus);
            }

            // Check if stopped/halted
            if self.stopped != 0 {
                break;
            }
        }

        // Return cycles consumed
        self.initial_cycles - self.cycles_remaining
    }

    /// Execute a single instruction.
    ///
    /// Returns a `StepResult` indicating:
    /// - `Ok { cycles }` - Normal instruction execution
    /// - `Stopped` - CPU is stopped
    ///
    /// Traps are surfaced as `StepResult` variants; exceptions are not taken
    /// automatically in this mode. For HLE interception with automatic fallback
    /// to exceptions, use `step_with_hle_handler()`.
    pub fn step<B: AddressBus>(&mut self, bus: &mut B) -> StepResult {
        use crate::core::types::{InternalStepResult, StepResult};

        if self.stopped != 0 {
            return StepResult::Stopped;
        }

        self.ppc = self.pc;
        self.dar_save = self.dar;
        self.sr_save = self.get_sr();
        self.ir = self.fetch_opcode(bus) as u32;

        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            self.run_mode = RUN_MODE_NORMAL;
            return StepResult::Ok { cycles: 0 };
        }

        let result = dispatch_instruction(self, bus, self.ir as u16);

        let res = match result {
            InternalStepResult::Ok { cycles } => StepResult::Ok {
                cycles: self.scale_cycles_for_cpu_type(cycles),
            },
            InternalStepResult::AlineTrap { opcode } => StepResult::AlineTrap { opcode },
            InternalStepResult::FlineTrap { opcode } => StepResult::FlineTrap { opcode },
            InternalStepResult::TrapInstruction { trap_num } => {
                StepResult::TrapInstruction { trap_num }
            }
            InternalStepResult::Breakpoint { bp_num } => StepResult::Breakpoint { bp_num },
            InternalStepResult::IllegalInstruction { opcode } => {
                StepResult::IllegalInstruction { opcode }
            }
        };

        if matches!(res, StepResult::Ok { .. }) {
            if self.run_mode == RUN_MODE_BERR_AERR_RESET {
                self.run_mode = RUN_MODE_NORMAL;
                return res;
            }

            // End-of-instruction prefetch: top the queue back up to two words
            // (a no-op after flow changes, whose refill already filled it).
            self.top_up_prefetch(bus);

            // Check for trace exception
            if !self.sst_m68000_compat && self.check_trace() {
                let trace_cycles = self.exception_trace(bus);
                if let StepResult::Ok { cycles } = res {
                    return StepResult::Ok {
                        cycles: cycles + trace_cycles,
                    };
                }
            }

            // Check for interrupts after instruction
            if self.int_level > 0 {
                self.check_and_service_interrupts(bus);
            }
        }

        res
    }

    /// Execute a single instruction with HLE trap handling (CPU + bus access).
    ///
    /// This method is the preferred way to run the CPU with High-Level Emulation.
    /// When a trap instruction is encountered, the appropriate `HleHandler` method
    /// is called. If the handler returns `true`, the trap is considered handled
    /// and execution continues. If it returns `false` (or is not implemented),
    /// the real hardware exception is taken automatically.
    ///
    /// # Example
    /// ```
    /// use m68k::{AddressBus, CpuCore, HleHandler};
    ///
    /// struct MyHandler { handled: bool }
    /// impl HleHandler for MyHandler {
    ///     fn handle_aline(
    ///         &mut self,
    ///         _cpu: &mut CpuCore,
    ///         _bus: &mut dyn AddressBus,
    ///         _opcode: u16,
    ///     ) -> bool {
    ///         self.handled = true;
    ///         true // HLE handled it
    ///     }
    /// }
    /// ```
    pub fn step_with_hle_handler<B: AddressBus, T: super::types::HleHandler>(
        &mut self,
        bus: &mut B,
        handler: &mut T,
    ) -> StepResult {
        use crate::core::types::{InternalStepResult, StepResult};

        if self.stopped != 0 {
            return StepResult::Stopped;
        }

        self.ppc = self.pc;
        self.dar_save = self.dar;
        self.sr_save = self.get_sr();
        self.ir = self.fetch_opcode(bus) as u32;

        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            self.run_mode = RUN_MODE_NORMAL;
            return StepResult::Ok { cycles: 0 };
        }

        let result = dispatch_instruction(self, bus, self.ir as u16);

        // Handle trap results via callbacks, fallback to exception if not handled
        let cycles = match result {
            InternalStepResult::Ok { cycles } => self.scale_cycles_for_cpu_type(cycles),
            InternalStepResult::AlineTrap { opcode } => {
                if !handler.handle_aline(self, bus, opcode) {
                    self.take_aline_exception(bus)
                } else {
                    0 // HLE handled, 0 cycles for now
                }
            }
            InternalStepResult::FlineTrap { opcode } => {
                if !handler.handle_fline(self, bus, opcode) {
                    self.take_fline_exception(bus)
                } else {
                    0
                }
            }
            InternalStepResult::TrapInstruction { trap_num } => {
                if !handler.handle_trap(self, bus, trap_num) {
                    self.take_trap_exception(bus, trap_num)
                } else {
                    0
                }
            }
            InternalStepResult::Breakpoint { bp_num } => {
                if !handler.handle_breakpoint(self, bus, bp_num) {
                    self.take_bkpt_exception(bus)
                } else {
                    0
                }
            }
            InternalStepResult::IllegalInstruction { opcode } => {
                if !handler.handle_illegal(self, bus, opcode) {
                    self.take_illegal_exception(bus)
                } else {
                    0
                }
            }
        };

        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            self.run_mode = RUN_MODE_NORMAL;
            return StepResult::Ok { cycles };
        }

        // End-of-instruction prefetch: top the queue back up to two words
        // (a no-op after flow changes, whose refill already filled it).
        self.top_up_prefetch(bus);

        // Check for trace exception
        if !self.sst_m68000_compat && self.check_trace() {
            let trace_cycles = self.exception_trace(bus);
            return StepResult::Ok {
                cycles: cycles + trace_cycles,
            };
        }

        // Check for interrupts after instruction
        if self.int_level > 0 {
            self.check_and_service_interrupts(bus);
        }

        StepResult::Ok { cycles }
    }

    // step_with_trap_handler removed in favor of step_with_hle_handler.

    // ========== Stack Operations ==========

    /// Push a word onto the stack.
    #[inline]
    pub fn push_16<B: AddressBus>(&mut self, bus: &mut B, value: u16) {
        self.dar[15] = self.dar[15].wrapping_sub(2);
        self.write_16(bus, self.dar[15], value);
    }

    /// Push a long onto the stack.
    #[inline]
    pub fn push_32<B: AddressBus>(&mut self, bus: &mut B, value: u32) {
        self.dar[15] = self.dar[15].wrapping_sub(4);
        self.write_32(bus, self.dar[15], value);
    }

    /// Pull a word from the stack.
    #[inline]
    pub fn pull_16<B: AddressBus>(&mut self, bus: &mut B) -> u16 {
        let value = self.read_16(bus, self.dar[15]);
        self.dar[15] = self.dar[15].wrapping_add(2);
        value
    }

    /// Pull a long from the stack.
    #[inline]
    pub fn pull_32<B: AddressBus>(&mut self, bus: &mut B) -> u32 {
        let value = self.read_32(bus, self.dar[15]);
        self.dar[15] = self.dar[15].wrapping_add(4);
        value
    }

    // ========== Program Flow ==========

    /// Jump to a new PC.
    #[inline]
    pub fn jump(&mut self, new_pc: u32) {
        self.pc = self.address(new_pc);
    }

    /// Jump to an exception vector.
    pub fn jump_vector<B: AddressBus>(&mut self, bus: &mut B, vector: u32) {
        self.last_exception_vector = Some(vector);
        let addr = (vector << 2).wrapping_add(self.vbr);
        self.pc = self.read_32(bus, addr);
        // Exception entry refills the prefetch queue from the handler
        // address, with 2 internal clocks between the two refill reads.
        self.prefetch_first(bus);
        self.internal_cycles(2);
        self.prefetch_second(bus);
    }

    /// Branch with 8-bit displacement.
    #[inline]
    pub fn branch_8(&mut self, offset: u8) {
        self.pc = self.pc.wrapping_add(offset as i8 as i32 as u32);
    }

    /// Branch with 16-bit displacement.
    #[inline]
    pub fn branch_16(&mut self, offset: u16) {
        self.pc = self.pc.wrapping_add(offset as i16 as i32 as u32);
    }

    /// Branch with 32-bit displacement.
    #[inline]
    pub fn branch_32(&mut self, offset: u32) {
        self.pc = self.pc.wrapping_add(offset);
    }

    // ========== Interrupt Handling ==========

    /// Check and service pending interrupts.
    fn check_and_service_interrupts<B: AddressBus>(&mut self, bus: &mut B) {
        // NMI (level 7) always triggers, others compare to mask
        let mask_level = (self.int_mask >> 8) & 7;
        let int_level = self.int_level & 7;

        if int_level == 7 || int_level > mask_level {
            self.service_interrupt(bus, int_level as u8);
            // Clear pending interrupt level - bus.interrupt_acknowledge was called in
            // service_interrupt, so the device has had a chance to update its state.
            // We clear cpu.int_level here; the test harness will re-poll and set it
            // again in the next step if another interrupt is pending.
            self.int_level = 0;
        }
    }

    /// Service an interrupt.
    fn service_interrupt<B: AddressBus>(&mut self, bus: &mut B, level: u8) {
        // Get vector from interrupt acknowledge
        let vector = bus.interrupt_acknowledge(level);
        let vector = if vector == 0xFFFFFFFF {
            // Autovector
            24 + level as u32
        } else {
            vector & 0xFF
        };

        // Match Musashi `m68ki_exception_interrupt`:
        // - save old SR
        // - clear trace, enter supervisor (but do not modify M)
        // - set interrupt mask
        // - stack format-0 frame; if M=1 and 68020+ also stack a format-1 throwaway frame on ISP
        let old_sr = self.get_sr();
        self.t1_flag = 0;
        self.t0_flag = 0;
        self.set_s_flag(SFLAG_SET);
        self.int_mask = ((level as u32) & 7) << 8;

        let stacked_pc = self.pc;
        let vec_word = (vector as u16) << 2;

        if self.cpu_type == super::types::CpuType::M68000 {
            // 68000: 3-word frame, hardware bus order (PC low, SR, PC high).
            self.push_exception_frame_68000(bus, stacked_pc, old_sr);
        } else {
            // 68010+: format 0 frame: (vector<<2), PC, SR (vector word ends up at +6)
            self.push_16(bus, vec_word);
            self.push_32(bus, stacked_pc);
            self.push_16(bus, old_sr);
        }

        // If we were in supervisor master state, generate a throwaway frame on ISP.
        // (Musashi: clear M, force S in the stacked SR, then stack format-1 frame.)
        let is_ec020_plus = matches!(
            self.cpu_type,
            super::types::CpuType::M68EC020
                | super::types::CpuType::M68020
                | super::types::CpuType::M68EC030
                | super::types::CpuType::M68030
                | super::types::CpuType::M68EC040
                | super::types::CpuType::M68LC040
                | super::types::CpuType::M68040
                | super::types::CpuType::M68060
        );
        if is_ec020_plus && self.m_flag != 0 {
            self.set_sm_flag(SFLAG_SET); // clear M => ISP active
            let sr2 = old_sr | 0x2000;
            self.push_16(bus, 0x1000 | (vec_word & 0x0FFF));
            self.push_32(bus, stacked_pc);
            self.push_16(bus, sr2);
        }

        // Jump to vector
        self.jump_vector(bus, vector);

        // Clear stopped state
        self.stopped = 0;

        // Use exception cycles
        self.cycles_remaining -= 44; // Approximate interrupt cycles
    }

    /// Halt the CPU.
    pub fn halt(&mut self) {
        self.stopped |= STOP_LEVEL_HALT;
    }

    /// Stop the CPU (STOP instruction).
    pub fn stop(&mut self, new_sr: u16) {
        self.set_sr(new_sr);
        self.stopped |= STOP_LEVEL_STOP;
    }
}
