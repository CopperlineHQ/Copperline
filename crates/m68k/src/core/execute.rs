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

        // If stopped, run the stopped-state supervisor check (a STOP that
        // loaded an S-clear SR wakes into a privilege violation here);
        // otherwise consume no cycles.
        if self.stopped != 0 {
            if let Some(cycles) = self.stopped_supervisor_check(bus) {
                self.cycles_remaining -= cycles;
            } else {
                self.cycles_remaining = 0;
                return self.initial_cycles;
            }
        }

        // Main execution loop
        while self.cycles_remaining > 0 {
            self.instruction_exception_vector = None;
            bus.begin_instruction_fetches();
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
                self.end_faulted_instruction();
                // A double fault halts the CPU: no further fetches, and the
                // fault run mode is left set for is_halted().
                if self.stopped != 0 {
                    break;
                }
                continue;
            }

            // Dispatch instruction (sampling whether the opcode fetch
            // hit the icache before dispatch consumes more of the stream).
            let opcode_fetch_cached = bus.last_fetch_was_cached();
            let result = dispatch_instruction(self, bus, self.ir as u16);
            let fetch_cached = if matches!(
                self.cpu_type,
                super::types::CpuType::M68EC020 | super::types::CpuType::M68020
            ) {
                bus.instruction_fetches_were_cached()
            } else {
                opcode_fetch_cached
            };

            // Auto-take all trap exceptions, extract cycles
            use crate::core::types::InternalStepResult;
            let cycles = match result {
                InternalStepResult::Ok { cycles } => self.finalize_cycles(cycles, fetch_cached),
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
                self.end_faulted_instruction();
                // A double fault halts the CPU: stop the batch dead instead
                // of letting another opcode through before the bottom-of-loop
                // stopped check.
                if self.stopped != 0 {
                    break;
                }
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
            if let Some(cycles) = self.stopped_supervisor_check(bus) {
                return StepResult::Ok { cycles };
            }
            return StepResult::Stopped;
        }

        self.instruction_exception_vector = None;
        bus.begin_instruction_fetches();
        self.ppc = self.pc;
        self.dar_save = self.dar;
        self.sr_save = self.get_sr();
        self.ir = self.fetch_opcode(bus) as u32;

        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            self.end_faulted_instruction();
            return StepResult::Ok { cycles: 0 };
        }

        let opcode_fetch_cached = bus.last_fetch_was_cached();
        let result = dispatch_instruction(self, bus, self.ir as u16);
        let fetch_cached = if matches!(
            self.cpu_type,
            super::types::CpuType::M68EC020 | super::types::CpuType::M68020
        ) {
            bus.instruction_fetches_were_cached()
        } else {
            opcode_fetch_cached
        };

        let res = match result {
            InternalStepResult::Ok { cycles } => StepResult::Ok {
                cycles: self.finalize_cycles(cycles, fetch_cached),
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
                self.end_faulted_instruction();
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
            if let Some(cycles) = self.stopped_supervisor_check(bus) {
                return StepResult::Ok { cycles };
            }
            return StepResult::Stopped;
        }

        self.instruction_exception_vector = None;
        bus.begin_instruction_fetches();
        self.ppc = self.pc;
        self.dar_save = self.dar;
        self.sr_save = self.get_sr();
        self.ir = self.fetch_opcode(bus) as u32;

        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            self.end_faulted_instruction();
            return StepResult::Ok { cycles: 0 };
        }

        let opcode_fetch_cached = bus.last_fetch_was_cached();
        let result = dispatch_instruction(self, bus, self.ir as u16);
        let fetch_cached = if matches!(
            self.cpu_type,
            super::types::CpuType::M68EC020 | super::types::CpuType::M68020
        ) {
            bus.instruction_fetches_were_cached()
        } else {
            opcode_fetch_cached
        };

        // Handle trap results via callbacks, fallback to exception if not handled
        let cycles = match result {
            InternalStepResult::Ok { cycles } => self.finalize_cycles(cycles, fetch_cached),
            InternalStepResult::AlineTrap { opcode } => {
                if !handler.handle_aline(self, bus, opcode) {
                    self.take_aline_exception(bus)
                } else {
                    // On real hardware this is an exception: no pairing
                    // window survives it.
                    self.break_060_pipeline();
                    0 // HLE handled, 0 cycles for now
                }
            }
            InternalStepResult::FlineTrap { opcode } => {
                if !handler.handle_fline(self, bus, opcode) {
                    self.take_fline_exception(bus)
                } else {
                    self.break_060_pipeline();
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
            self.end_faulted_instruction();
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
        // Any exception entry breaks an open 68060 pairing window.
        self.break_060_pipeline();
        // An earlier poll-point hold must not survive into the handler:
        // the refill below is a fresh IPL poll point (Moira jumpToVector
        // polls during the final refill read).
        bus.ipl_release_sample();
        // Any vectored dispatch ends 68010 loop mode; the refill below
        // restores normal instruction fetching.
        self.loop_mode = false;
        self.last_exception_vector = Some(vector);
        self.instruction_exception_vector = Some(vector);
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

    /// Map an interrupt-acknowledge response to a vector number.
    #[inline]
    fn iack_vector(response: u32, level: u8) -> u32 {
        if response == 0xFFFFFFFF {
            // Autovector
            24 + level as u32
        } else {
            response & 0xFF
        }
    }

    /// Service an interrupt.
    fn service_interrupt<B: AddressBus>(&mut self, bus: &mut B, level: u8) {
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
        let vector;

        if self.cpu_type == super::types::CpuType::M68000 {
            // 68000 interrupt microcode (per Moira execInterrupt, same
            // sequence as yacht): 6 idle clocks, PC-low write, the 4-clock
            // interrupt-acknowledge bus cycle that latches the vector
            // number, 4 more internal clocks, then the SR and PC-high
            // writes. The idle periods are billed in place so every frame
            // write and the IACK land at their hardware bus-time offsets
            // (previously the frame writes ran back to back and all idle
            // time was paid after the handler prefetch, letting the
            // handler's first instruction start ~14 clocks early).
            self.internal_cycles(6);
            let sp = self.dar[15].wrapping_sub(6);
            self.dar[15] = sp;
            self.write_16(bus, sp.wrapping_add(4), (stacked_pc & 0xFFFF) as u16);
            self.internal_cycles(4);
            self.flush_sync(bus);
            vector = Self::iack_vector(bus.interrupt_acknowledge(level), level);
            self.internal_cycles(4);
            self.write_16(bus, sp, old_sr);
            self.write_16(bus, sp.wrapping_add(2), (stacked_pc >> 16) as u16);
        } else if self.cpu_type == super::types::CpuType::M68010 {
            // 68010 interrupt microcode (Moira execInterrupt): 12 idle
            // clocks with the IACK at their end, then the format-0 frame
            // in hardware bus order PC low, SR, PC high, vector word.
            self.internal_cycles(12);
            self.flush_sync(bus);
            vector = Self::iack_vector(bus.interrupt_acknowledge(level), level);
            let sp = self.dar[15].wrapping_sub(8);
            self.dar[15] = sp;
            self.write_16(bus, sp.wrapping_add(4), (stacked_pc & 0xFFFF) as u16);
            self.write_16(bus, sp, old_sr);
            self.write_16(bus, sp.wrapping_add(2), (stacked_pc >> 16) as u16);
            self.write_16(bus, sp.wrapping_add(6), (vector as u16) << 2);
        } else {
            vector = Self::iack_vector(bus.interrupt_acknowledge(level), level);
            let vec_word = (vector as u16) << 2;
            // 68020+: format 0 frame: (vector<<2), PC, SR (vector word ends up at +6)
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
        );
        if is_ec020_plus && self.m_flag != 0 {
            self.set_sm_flag(SFLAG_SET); // clear M => ISP active
            let sr2 = old_sr | 0x2000;
            self.push_16(bus, 0x1000 | (((vector as u16) << 2) & 0x0FFF));
            self.push_32(bus, stacked_pc);
            self.push_16(bus, sr2);
        } else if self.cpu_type == super::types::CpuType::M68060 && self.m_flag != 0 {
            // The 68060 clears M on interrupt entry but has a single
            // supervisor stack: no bank switch and no throwaway frame.
            self.m_flag = 0;
        }

        // Jump to vector
        self.jump_vector(bus, vector);

        // Clear stopped state
        self.stopped = 0;

        // Use exception cycles: 44 clocks on the 68000, 46 on the 68010
        // (12 leading internal clocks + the four-word frame), matching the
        // internal + bus clocks billed above so the accounting equals the
        // bus time actually consumed.
        self.cycles_remaining -= if self.cpu_type == super::types::CpuType::M68010 {
            46
        } else {
            44
        };
    }

    /// Stopped-state supervisor check, run at every instruction boundary
    /// while stopped: STOP loads its SR operand verbatim (a single-stepped
    /// STOP observes S and T exactly as written), and a loaded S-clear SR
    /// wakes the CPU here with a privilege violation -- 4 internal clocks,
    /// then the exception, stacking the STOP instruction itself so the
    /// handler's RTE re-executes it. Returns the cycles consumed when the
    /// wake fired; None leaves the CPU stopped (including a HALT).
    fn stopped_supervisor_check<B: AddressBus>(&mut self, bus: &mut B) -> Option<i32> {
        if self.stopped != STOP_LEVEL_STOP || self.s_flag != 0 {
            return None;
        }
        self.stopped = 0;
        self.internal_cycles(4);
        // PC sits past the STOP opcode and its SR operand word.
        self.ppc = self.pc.wrapping_sub(4);
        Some(4 + self.exception_privilege(bus))
    }

    /// Halt the CPU.
    pub fn halt(&mut self) {
        self.stopped |= STOP_LEVEL_HALT;
    }

    /// Stop the CPU (STOP instruction).
    pub fn stop(&mut self, new_sr: u16) {
        self.break_060_pipeline();
        self.set_sr(new_sr);
        self.stopped |= STOP_LEVEL_STOP;
    }
}
