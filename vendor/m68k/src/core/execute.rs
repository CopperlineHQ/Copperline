//! Main execution loop.
//!
//! Implements the fetch-decode-execute cycle.

use super::cpu::{CpuCore, SFLAG_SET};
use super::decode::{dispatch_instruction, needs_rollback_snapshot};
use super::memory::AddressBus;
use super::op_cache::{BatchInnerExit, DecodedSimpleOp, UnaryOp};
use super::trace_jit;
use super::types::{
    BatchExit, BatchResult, CpuType, CycleBatchControl, CycleBatchExit, CycleBatchResult,
    CycleBoundaryEvent, Size, StepResult,
};

/// Write callback used only during a native trace with a TrackedMem window.
/// The window contract excludes faults, reentry and unwinding here.
#[cfg(all(feature = "jit", not(target_family = "wasm")))]
unsafe extern "C" fn tracked_write<B: AddressBus>(
    bus: usize,
    operation: u32,
    address: u32,
    value: u32,
    source: u32,
) {
    // SAFETY: run_batch retains exclusive access to this bus and clears the
    // callback and opaque pointer before returning to its caller.
    let bus = unsafe { &mut *(bus as *mut B) };
    let bytes = operation & 7;
    let copied = operation & 8 != 0 && bus.begin_memory_copy(source, bytes);
    match bytes {
        1 => bus.write_byte(address, value as u8),
        2 => bus.write_word(address, value as u16),
        4 => bus.write_long(address, value),
        _ => unreachable!("invalid generated store width"),
    }
    if copied {
        bus.end_memory_copy(Some(address));
    }
}

/// Stop level constants.
pub const STOP_LEVEL_STOP: u32 = 1;
/// Halted after a double bus/address fault; only reset can resume execution.
pub const STOP_LEVEL_HALT: u32 = 2;

/// Run mode constants.
pub const RUN_MODE_NORMAL: u32 = 0;
/// Bus/address-error recovery or reset processing is active.
pub const RUN_MODE_BERR_AERR_RESET: u32 = 1;

/// The 020 timing tables select their Cache Case only when every
/// instruction-stream fetch for the retiring instruction hit the cache.
#[inline]
fn timing_uses_full_fetch_stream(cpu_type: CpuType) -> bool {
    matches!(
        cpu_type,
        CpuType::M68EC020 | CpuType::M68020 | CpuType::M68EC030 | CpuType::M68030
    )
}

/// Decode the deliberately narrow register-only subset whose precise
/// instruction completion is preserved by the boundary-hook dispatcher.
#[inline]
fn decode_boundary_hook_op(cpu_type: CpuType, opcode: u16) -> Option<DecodedSimpleOp> {
    let decoded = DecodedSimpleOp::decode(cpu_type, opcode);
    match (cpu_type, decoded) {
        // M68040 NOP is excluded because the normal path performs T0 pipeline
        // synchronization.
        (
            CpuType::M68000 | CpuType::M68010 | CpuType::M68020 | CpuType::M68030,
            Some(op @ DecodedSimpleOp::Nop),
        ) => Some(op),
        (
            CpuType::M68000 | CpuType::M68010 | CpuType::M68020 | CpuType::M68030 | CpuType::M68040,
            Some(op @ DecodedSimpleOp::Moveq { .. }),
        ) if opcode & 0x0100 == 0 => Some(op),
        (
            CpuType::M68000 | CpuType::M68010 | CpuType::M68020 | CpuType::M68030 | CpuType::M68040,
            Some(
                op @ DecodedSimpleOp::UnaryDataReg {
                    op: UnaryOp::Clr,
                    size: Size::Word | Size::Long,
                    ..
                },
            ),
        ) => Some(op),
        _ => None,
    }
}

impl CpuCore {
    #[inline]
    fn prepare_rollback_snapshot(&mut self, opcode: u16) {
        if needs_rollback_snapshot(opcode) {
            self.dar_save = self.dar;
            self.sr_save = self.get_sr();
        } else if (self.t1_flag | self.t0_flag) != 0 {
            // Trace checks still need the pre-instruction SR. Simple no-fault instructions do
            // not need the D/A rollback snapshot.
            self.sr_save = self.get_sr();
        } else {
            self.sr_save = 0;
        }
    }

    /// Unconditional snapshot for paths where the opcode is already known
    /// not to be a simple op (a decoded-op-cache miss). Skips
    /// `needs_rollback_snapshot`, which would re-run the full simple-op
    /// decode just to conclude the same thing.
    #[inline]
    fn prepare_rollback_snapshot_full(&mut self) {
        self.dar_save = self.dar;
        self.sr_save = self.get_sr();
    }

    /// Execute instructions for the given number of cycles.
    ///
    /// Returns the number of cycles actually consumed.
    ///
    /// Trap instructions are taken as hardware exceptions. Use
    /// [`step`](Self::step) when the host needs to intercept trap opcodes.
    pub fn execute<B: AddressBus>(&mut self, bus: &mut B, num_cycles: i32) -> i32 {
        self.set_precise_bus(true);
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
            // Flow/timing belongs to the instruction about to execute.
            self.change_of_flow = false;
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
            let fetch_cached = if timing_uses_full_fetch_stream(self.cpu_type) {
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

    /// Execute complete instructions until `cycle_budget` is met or crossed,
    /// or until an event requiring host attention occurs.
    ///
    /// This is the cycle-scheduled counterpart to [`run_batch`](Self::run_batch):
    /// it keeps the transaction-exact [`AddressBus`] execution contract and
    /// reports the actual cycle count, while surfacing traps in the same CPU
    /// and PC state as [`step`](Self::step).
    ///
    /// # Boundary semantics
    ///
    /// - Instructions are never split. The final instruction may overshoot
    ///   the budget.
    /// - A surfaced A-line/F-line/TRAP/BKPT/illegal instruction is not counted
    ///   in `instructions`, and no exception-entry cycles are charged.
    /// - RESET executes synchronously, calls [`AddressBus::reset_devices`],
    ///   and counts as an instruction.
    /// - Exceptions raised internally by an instruction (for example,
    ///   divide-by-zero, privilege, bus, and address faults) complete their
    ///   exception entry and the faulting instruction is counted.
    /// - A serviceable interrupt is taken on entry or after an instruction.
    ///   Its entry cycles are included, but it does not increment the
    ///   instruction count. An interrupt can therefore satisfy the budget
    ///   before the first handler instruction executes.
    /// - A non-positive budget returns immediately with
    ///   [`CycleBatchExit::BudgetExhausted`].
    /// - A bus boundary request is polled after each normally completed
    ///   instruction and after interrupt entry on batch entry. Completed work
    ///   is included in the totals, and the request takes precedence over
    ///   simultaneous budget exhaustion. STOP and surfaced traps retain their
    ///   existing exit reasons.
    pub fn run_for_cycles<B: AddressBus>(
        &mut self,
        bus: &mut B,
        cycle_budget: i32,
    ) -> CycleBatchResult {
        self.run_for_cycles_inner::<B, _, false, false>(bus, cycle_budget, &mut |_, _, _| {
            CycleBatchControl::Continue
        })
    }

    /// Execute complete instructions with host synchronization after each
    /// normally completed instruction.
    ///
    /// The hook receives the CPU, bus, and cycles consumed by the just-completed
    /// instruction. [`CpuCore::ppc`](Self::ppc) identifies that instruction;
    /// [`CpuCore::pc`](Self::pc) identifies the next instruction or an exception
    /// handler selected while the instruction completed. CPU and bus changes
    /// made by the hook are visible before another instruction is fetched.
    ///
    /// Returning [`CycleBatchControl::Return`] exits with
    /// [`CycleBatchExit::BoundaryRequested`]. The completed instruction and its
    /// cycles remain included in the result, and no further interrupt entry or
    /// instruction executes. Returning [`CycleBatchControl::Continue`] allows a
    /// newly serviceable interrupt to be taken before the next instruction.
    /// Interrupt-entry cycles are included in the batch total but are not passed
    /// to the hook as instruction cycles.
    ///
    /// The hook is not called for surfaced traps, STOP, interrupt entry, or an
    /// already-stopped CPU. Bus boundary requests and STOP retain their existing
    /// precedence over budget exhaustion. A bus request raised by an instruction
    /// or the hook is honored before a newly raised interrupt is serviced.
    ///
    /// The hook must not call CPU execution methods or modify the runner's cycle
    /// bookkeeping fields; doing so would invalidate the returned totals.
    ///
    /// Use [`run_for_cycles`](Self::run_for_cycles) when synchronization is not
    /// required; its hook branch is disabled at compile time.
    pub fn run_for_cycles_with_hook<B, F>(
        &mut self,
        bus: &mut B,
        cycle_budget: i32,
        mut hook: F,
    ) -> CycleBatchResult
    where
        B: AddressBus,
        F: FnMut(&mut CpuCore, &mut B, i32) -> CycleBatchControl,
    {
        self.run_for_cycles_inner::<B, _, true, false>(bus, cycle_budget, &mut |cpu, bus, event| {
            match event {
                CycleBoundaryEvent::Instruction { cycles } => hook(cpu, bus, cycles),
                CycleBoundaryEvent::InterruptEntry { .. } => {
                    unreachable!("the instruction hook does not receive interrupt entry")
                }
            }
        })
    }

    /// Execute complete instructions with host synchronization after every
    /// instruction and interrupt-entry boundary.
    ///
    /// The hook receives [`CycleBoundaryEvent::Instruction`] after an
    /// instruction retires and [`CycleBoundaryEvent::InterruptEntry`] after
    /// interrupt entry completes. An interrupt-entry event occurs before the
    /// first handler instruction is fetched or executed, contributes cycles
    /// but no retired instruction, and can request a return. CPU and bus
    /// changes made for either event are visible before execution continues.
    ///
    /// A bus boundary request raised during interrupt entry remains pending
    /// while the hook runs and forces [`CycleBatchExit::BoundaryRequested`]
    /// afterward. Use [`run_for_cycles_with_hook`](Self::run_for_cycles_with_hook)
    /// when only retired instructions need synchronization, or
    /// [`run_for_cycles`](Self::run_for_cycles) when no synchronization hook is
    /// required.
    pub fn run_for_cycles_with_boundary_hook<B, F>(
        &mut self,
        bus: &mut B,
        cycle_budget: i32,
        mut hook: F,
    ) -> CycleBatchResult
    where
        B: AddressBus,
        F: FnMut(&mut CpuCore, &mut B, CycleBoundaryEvent) -> CycleBatchControl,
    {
        self.run_for_cycles_inner::<B, F, true, true>(bus, cycle_budget, &mut hook)
    }

    #[inline]
    fn service_interrupt_boundary<B, F, const CALL_INTERRUPT_HOOK: bool>(
        &mut self,
        bus: &mut B,
        hook: &mut F,
    ) -> Option<CycleBatchControl>
    where
        B: AddressBus,
        F: FnMut(&mut CpuCore, &mut B, CycleBoundaryEvent) -> CycleBatchControl,
    {
        let before_interrupt = self.cycles_remaining;
        if !self.check_and_service_interrupts(bus) {
            return None;
        }
        let entry_cycles = before_interrupt - self.cycles_remaining;
        Some(if CALL_INTERRUPT_HOOK {
            hook(
                self,
                bus,
                CycleBoundaryEvent::InterruptEntry {
                    cycles: entry_cycles,
                },
            )
        } else {
            CycleBatchControl::Continue
        })
    }

    #[inline]
    fn run_for_cycles_inner<
        B,
        F,
        const CALL_INSTRUCTION_HOOK: bool,
        const CALL_INTERRUPT_HOOK: bool,
    >(
        &mut self,
        bus: &mut B,
        cycle_budget: i32,
        hook: &mut F,
    ) -> CycleBatchResult
    where
        B: AddressBus,
        F: FnMut(&mut CpuCore, &mut B, CycleBoundaryEvent) -> CycleBatchControl,
    {
        self.set_precise_bus(true);
        self.initial_cycles = cycle_budget;

        if cycle_budget <= 0 {
            self.cycles_remaining = cycle_budget;
            return CycleBatchResult {
                cycles: 0,
                instructions: 0,
                exit: CycleBatchExit::BudgetExhausted,
            };
        }

        if self.reset_cycles > 0 {
            let reset_cycles = self.reset_cycles as i32;
            self.reset_cycles = 0;
            self.cycles_remaining = cycle_budget - reset_cycles;
        } else {
            self.cycles_remaining = cycle_budget;
        }

        // Interrupts are instruction-boundary events. Service one before the
        // first fetch, including when it wakes a stopped CPU.
        if let Some(hook_control) =
            self.service_interrupt_boundary::<B, F, CALL_INTERRUPT_HOOK>(bus, hook)
            && (bus.take_boundary_request() || hook_control == CycleBatchControl::Return)
        {
            return CycleBatchResult {
                cycles: self.initial_cycles - self.cycles_remaining,
                instructions: 0,
                exit: CycleBatchExit::BoundaryRequested,
            };
        }

        let mut instructions = 0;
        loop {
            if self.cycles_remaining <= 0 {
                return CycleBatchResult {
                    cycles: self.initial_cycles - self.cycles_remaining,
                    instructions,
                    exit: CycleBatchExit::BudgetExhausted,
                };
            }
            if self.stopped != 0 {
                return CycleBatchResult {
                    cycles: self.initial_cycles - self.cycles_remaining,
                    instructions,
                    exit: CycleBatchExit::Stopped,
                };
            }

            // Hook-enabled runners must expose the instruction boundary before
            // servicing an interrupt made pending by that instruction or its
            // hook. `step()` normally services the current level on return, so
            // defer it locally and restore it before invoking the hook.
            let deferred_irq = if CALL_INSTRUCTION_HOOK {
                std::mem::take(&mut self.int_level)
            } else {
                0
            };
            let step_result = if CALL_INSTRUCTION_HOOK && CALL_INTERRUPT_HOOK {
                self.step_with_dispatch(bus, CpuCore::dispatch_boundary_hook_decoded)
            } else {
                self.step(bus)
            };
            if CALL_INSTRUCTION_HOOK {
                self.int_level = deferred_irq;
            }

            match step_result {
                StepResult::Ok { cycles } => {
                    self.cycles_remaining -= cycles;
                    instructions += 1;
                    // STOP is an observable exit even when its own cycles
                    // simultaneously exhaust the budget. If STOP itself
                    // unmasked a pending interrupt, service that entry first
                    // without turning STOP into an ordinary instruction-hook
                    // event.
                    if self.stopped != 0 {
                        if CALL_INSTRUCTION_HOOK
                            && let Some(interrupt_control) = self
                                .service_interrupt_boundary::<B, F, CALL_INTERRUPT_HOOK>(bus, hook)
                        {
                            if bus.take_boundary_request()
                                || interrupt_control == CycleBatchControl::Return
                            {
                                return CycleBatchResult {
                                    cycles: self.initial_cycles - self.cycles_remaining,
                                    instructions,
                                    exit: CycleBatchExit::BoundaryRequested,
                                };
                            }
                            continue;
                        }
                        return CycleBatchResult {
                            cycles: self.initial_cycles - self.cycles_remaining,
                            instructions,
                            exit: CycleBatchExit::Stopped,
                        };
                    }

                    let hook_control = if CALL_INSTRUCTION_HOOK {
                        hook(self, bus, CycleBoundaryEvent::Instruction { cycles })
                    } else {
                        CycleBatchControl::Continue
                    };

                    // Preserve the bus boundary before performing additional
                    // CPU work requested by a hook update. This also consumes a
                    // request raised from inside the hook.
                    if bus.take_boundary_request() || hook_control == CycleBatchControl::Return {
                        return CycleBatchResult {
                            cycles: self.initial_cycles - self.cycles_remaining,
                            instructions,
                            exit: CycleBatchExit::BoundaryRequested,
                        };
                    }

                    // Service the level restored above, including an interrupt
                    // newly raised by the instruction hook, before the next
                    // fetch.
                    if CALL_INSTRUCTION_HOOK
                        && let Some(interrupt_control) =
                            self.service_interrupt_boundary::<B, F, CALL_INTERRUPT_HOOK>(bus, hook)
                        && (bus.take_boundary_request()
                            || interrupt_control == CycleBatchControl::Return)
                    {
                        return CycleBatchResult {
                            cycles: self.initial_cycles - self.cycles_remaining,
                            instructions,
                            exit: CycleBatchExit::BoundaryRequested,
                        };
                    }
                }
                StepResult::Stopped => {
                    return CycleBatchResult {
                        cycles: self.initial_cycles - self.cycles_remaining,
                        instructions,
                        exit: CycleBatchExit::Stopped,
                    };
                }
                StepResult::AlineTrap { opcode } => {
                    return CycleBatchResult {
                        cycles: self.initial_cycles - self.cycles_remaining,
                        instructions,
                        exit: CycleBatchExit::AlineTrap { opcode },
                    };
                }
                StepResult::FlineTrap { opcode } => {
                    return CycleBatchResult {
                        cycles: self.initial_cycles - self.cycles_remaining,
                        instructions,
                        exit: CycleBatchExit::FlineTrap { opcode },
                    };
                }
                StepResult::TrapInstruction { trap_num } => {
                    return CycleBatchResult {
                        cycles: self.initial_cycles - self.cycles_remaining,
                        instructions,
                        exit: CycleBatchExit::TrapInstruction { trap_num },
                    };
                }
                StepResult::Breakpoint { bp_num } => {
                    return CycleBatchResult {
                        cycles: self.initial_cycles - self.cycles_remaining,
                        instructions,
                        exit: CycleBatchExit::Breakpoint { bp_num },
                    };
                }
                StepResult::IllegalInstruction { opcode } => {
                    return CycleBatchResult {
                        cycles: self.initial_cycles - self.cycles_remaining,
                        instructions,
                        exit: CycleBatchExit::IllegalInstruction { opcode },
                    };
                }
            }
        }
    }

    /// Execute up to `max_instructions` instructions, returning on the first
    /// interesting event.
    ///
    /// This is the fast path for High-Level Emulation embedders that would
    /// otherwise call [`step`](Self::step) in a loop: the whole batch runs
    /// inside the decoded-operation and trace-execution loops, and control
    /// returns to the caller only when it has something to do:
    ///
    /// - a trap the embedder wants to intercept (A-line/F-line/TRAP/BKPT/
    ///   illegal — surfaced exactly like [`step`](Self::step), never taken
    ///   as a hardware exception),
    /// - the CPU stopping (STOP instruction),
    /// - execution reaching a PC listed in `watch_pcs`, or
    /// - the instruction budget running out.
    ///
    /// Watch semantics: `watch_pcs` is checked after every retired
    /// instruction, *before* the instruction at the new PC executes. The
    /// entry PC is intentionally **not** checked, so a caller that resumes
    /// from a watched PC does not loop forever; keep the list short (it is
    /// scanned linearly).
    ///
    /// Unlike [`execute`](Self::execute), this entry point is
    /// instruction-budgeted and does not maintain cycle accounting
    /// (`cycles_remaining` is clobbered). Trace exceptions are taken
    /// internally and pending interrupts are serviced between instructions,
    /// matching [`step`](Self::step) semantics. A host that models the
    /// caches learns of a CACR write or cache invalidation inside the batch
    /// through [`AddressBus::cache_control`], before the next fetch.
    pub fn run_batch<B: AddressBus>(
        &mut self,
        bus: &mut B,
        max_instructions: u32,
        watch_pcs: &[u32],
    ) -> BatchResult {
        let prior_precision = self.precise_bus;
        self.set_precise_bus(false);
        // Capture the bus's fastmem window for the duration of this batch.
        // Its addresses are physical: with the PMMU enabled every access
        // through it is translated first (`crate::core::window`).
        if let Some(fm) = bus.fast_mem()
            && fm.len >= 4
            && !fm.ptr.is_null()
        {
            self.fm_ptr = fm.ptr as usize;
            self.fm_base = fm.base;
            self.fm_len = fm.len;
            // Memory traces are skipped (and probe-filtered) while no
            // window is active; with the window up they can run, so
            // re-arm the trace filters.
            self.trace_record_skip = [super::trace_jit::TRACE_PC_NONE; 4];
            self.trace_probe_skip = [super::trace_jit::TRACE_PC_NONE; 4];
        }
        #[cfg(all(feature = "jit", not(target_family = "wasm")))]
        // The pre-020 interpreter splits some long stores into ordered word
        // writes. Keep that bus sequencing until tracked codegen models it.
        if self.fm_len == 0
            && !super::op_cache::is_pre_68020(self.cpu_type)
            && let Some(window) = bus.tracked_mem()
            && window.len >= 4
            && !window.ptr.is_null()
        {
            self.fm_ptr = window.ptr as usize;
            self.fm_base = window.base;
            self.fm_len = window.len;
            self.fm_bus = bus as *mut B as usize;
            self.fm_write_hook = tracked_write::<B> as *const () as usize;
            // Only a zero-based window can index the filter by guest page.
            self.fm_store_filter = if window.base == 0 {
                bus.tracked_store_filter() as usize
            } else {
                0
            };
            self.trace_record_skip = [super::trace_jit::TRACE_PC_NONE; 4];
            self.trace_probe_skip = [super::trace_jit::TRACE_PC_NONE; 4];
        }
        #[cfg(all(feature = "jit", not(target_family = "wasm")))]
        if self.fm_len != 0 && self.window_overlaps_cpu() {
            // Generated traces keep guest registers and flags in native
            // values between entry and exit, so guest memory must never alias
            // this CpuCore. A host that maps it there gets the interpreter.
            self.fm_ptr = 0;
            self.fm_base = 0;
            self.fm_len = 0;
            self.fm_bus = 0;
            self.fm_write_hook = 0;
            self.fm_store_filter = 0;
        }
        if self.has_pmmu {
            self.refresh_window_mmu();
        }
        let result = self.run_batch_inner(bus, max_instructions, watch_pcs);
        self.fm_ptr = 0;
        self.fm_base = 0;
        self.fm_len = 0;
        self.fm_bus = 0;
        self.fm_write_hook = 0;
        self.fm_store_filter = 0;
        self.fm_mmu = crate::mmu::ResidentMmu::default();
        self.set_precise_bus(prior_precision);
        result
    }

    /// Whether the installed guest window overlaps this `CpuCore`.
    #[cfg(all(feature = "jit", not(target_family = "wasm")))]
    pub(crate) fn window_overlaps_cpu(&self) -> bool {
        let cpu = self as *const Self as usize;
        let cpu_end = cpu + std::mem::size_of::<Self>();
        let window_end = self.fm_ptr.saturating_add(self.fm_len as usize);
        self.fm_ptr < cpu_end && cpu < window_end
    }

    /// Close out a fault taken inside `run_batch` (`execute` and `step` do
    /// the same): the faulting instruction's own register writes after the
    /// exception entry -- a JSR or RTS assigning PC, a MOVE setting the
    /// condition codes -- give way to the handler entry state. Returns true
    /// when the fault was a double fault that halted the CPU.
    #[inline]
    fn end_batch_fault(&mut self) -> bool {
        self.end_faulted_instruction();
        self.stopped != 0
    }

    /// Hand a cache-control change made inside `run_batch` to the bus
    /// before the next fetch (see [`AddressBus::cache_control`]).
    #[cold]
    #[inline(never)]
    fn report_cache_control<B: AddressBus>(&mut self, bus: &mut B) {
        if bus.cache_control(self.cacr, self.caar, self.cacr_pending_ops) {
            self.cacr_pending_ops = 0;
        }
    }

    fn run_batch_inner<B: AddressBus>(
        &mut self,
        bus: &mut B,
        max_instructions: u32,
        watch_pcs: &[u32],
    ) -> BatchResult {
        use crate::core::types::InternalStepResult;

        if self.stopped != 0 {
            return BatchResult {
                instructions: 0,
                exit: BatchExit::Stopped,
            };
        }

        let mut retired: u32 = 0;
        let mut probe_on_entry = true;

        // A recording that closed at an A-line on the previous batch named
        // its expected continuation; if the host resumed exactly there, the
        // continuation becomes a trace-head candidate (counting, then
        // recording, like any backward-branch target). A host that resumed
        // anywhere else silently drops the hint.
        if let Some(expected) = self.pending_trap_resume.take()
            && expected == self.pc
        {
            trace_jit::record_trace_target(self.pc, self.cpu_type);
            probe_on_entry = true;
        }

        loop {
            if bus.batch_boundary(retired, self.pc, self.get_sr()) {
                trace_jit::stop_recording(self, trace_jit::RecordingStop::HostBoundary);
                return BatchResult {
                    instructions: retired,
                    exit: BatchExit::BoundaryRequested,
                };
            }
            // The trace JIT's headroom guard compares against
            // `cycles_remaining`; keep it topped up so it can never gate a
            // trace in this instruction-budgeted mode (traces decrement it
            // as they run).
            self.cycles_remaining = i32::MAX / 2;

            if retired >= max_instructions {
                // A full-dispatch instruction may have just extended a path
                // recording before exhausting this outer batch's budget.
                // The caller may mutate guest state before the next batch.
                trace_jit::stop_recording(self, trace_jit::RecordingStop::HostBoundary);
                return BatchResult {
                    instructions: retired,
                    exit: BatchExit::BudgetExhausted,
                };
            }

            let mut known_complex = false;
            let opcode = if self.can_run_decoded_simple_ops() {
                match self.run_decoded_simple_batch(
                    bus,
                    max_instructions - retired,
                    watch_pcs,
                    &mut retired,
                    probe_on_entry,
                ) {
                    BatchInnerExit::Boundary => {
                        return BatchResult {
                            instructions: retired,
                            exit: BatchExit::BoundaryRequested,
                        };
                    }
                    BatchInnerExit::Budget => {
                        return BatchResult {
                            instructions: retired,
                            exit: BatchExit::BudgetExhausted,
                        };
                    }
                    BatchInnerExit::Watched(pc) => {
                        return BatchResult {
                            instructions: retired,
                            exit: BatchExit::WatchedPc { pc },
                        };
                    }
                    BatchInnerExit::Fault => {
                        if self.end_batch_fault() {
                            return BatchResult {
                                instructions: retired,
                                exit: BatchExit::Stopped,
                            };
                        }
                        probe_on_entry = true;
                        continue;
                    }
                    BatchInnerExit::Miss(opcode) => {
                        known_complex = true;
                        opcode
                    }
                }
            } else {
                self.ppc = self.pc;
                let opcode = self.read_opcode_16(bus);
                if self.run_mode == RUN_MODE_BERR_AERR_RESET {
                    if self.end_batch_fault() {
                        return BatchResult {
                            instructions: retired,
                            exit: BatchExit::Stopped,
                        };
                    }
                    probe_on_entry = true;
                    continue;
                }
                self.ir = opcode as u32;
                opcode
            };

            if bus.batch_boundary(retired, self.ppc, self.get_sr()) {
                self.pc = self.ppc;
                trace_jit::stop_recording(self, trace_jit::RecordingStop::HostBoundary);
                return BatchResult {
                    instructions: retired,
                    exit: BatchExit::BoundaryRequested,
                };
            }
            if self.ir != opcode as u32 {
                self.ir = opcode as u32;
            }

            // The decoded/native loop runs with tracing disabled and can leave
            // its last flow latch set. This full-dispatch instruction starts a
            // new trace/timing decision; its own SR writes and branches may set
            // the latch again before the existing old-SR check below.
            self.change_of_flow = false;
            if known_complex {
                self.prepare_rollback_snapshot_full();
            } else {
                self.prepare_rollback_snapshot(opcode);
            }

            // MOVEC to CACR, CINV and CPUSH never run on the decoded paths,
            // so a cache-control change can only come from full dispatch.
            let cache_control = (self.cacr, self.cacr_pending_ops);
            let result = dispatch_instruction(self, bus, opcode);
            if (self.cacr, self.cacr_pending_ops) != cache_control {
                self.report_cache_control(bus);
            }

            // A dispatched instruction may have written TC or a TT register
            // (PMOVE/MOVEC): the window translates with the new state from
            // the next access on.
            if self.fm_len != 0 && self.has_pmmu {
                self.refresh_window_mmu();
            }

            let exit = match result {
                InternalStepResult::Ok { .. } => None,
                InternalStepResult::AlineTrap { opcode } => Some(BatchExit::AlineTrap { opcode }),
                InternalStepResult::FlineTrap { opcode } => Some(BatchExit::FlineTrap { opcode }),
                InternalStepResult::TrapInstruction { trap_num } => {
                    Some(BatchExit::TrapInstruction { trap_num })
                }
                InternalStepResult::Breakpoint { bp_num } => Some(BatchExit::Breakpoint { bp_num }),
                InternalStepResult::IllegalInstruction { opcode } => {
                    Some(BatchExit::IllegalInstruction { opcode })
                }
            };
            if let Some(exit) = exit {
                // An A-line reached by falling through a recorded region is
                // a trap-boundary terminal: the region compiles ending at
                // the trap and the expected resume point (the word after
                // the A-line) gains head candidacy on the next batch entry,
                // so segments chain themselves down a trap-punctuated run
                // (docs/trap-crossing-traces-design.md). Everything else --
                // F-line, TRAP #n, exceptions, non-sequential arrival --
                // keeps the previous behavior: the caller may resume at an
                // unrelated guest PC, so the recording is discarded rather
                // than allowed to cross a host-controlled boundary.
                if matches!(exit, BatchExit::AlineTrap { .. }) {
                    match trace_jit::finish_recording_at_trap(self) {
                        // Chain quality gate: only a segment the compiler
                        // accepted right up to this boundary earns a seeded
                        // continuation; a rejected closure must not extend
                        // the head chain past code that will never pay.
                        trace_jit::TrapFinish::Compiled => {
                            self.pending_trap_resume = Some(self.ppc.wrapping_add(2));
                        }
                        trace_jit::TrapFinish::Closed => {}
                        trace_jit::TrapFinish::None => {
                            trace_jit::stop_recording(
                                self,
                                trace_jit::RecordingStop::TrapOrException,
                            );
                        }
                    }
                } else {
                    trace_jit::stop_recording(self, trace_jit::RecordingStop::TrapOrException);
                }
                return BatchResult {
                    instructions: retired,
                    exit,
                };
            }
            retired += 1;

            // A bus/address error mid-instruction already built the exception
            // frame and jumped to the handler; skip trace/interrupt checks
            // for the faulting instruction (mirrors `execute`).
            if self.run_mode == RUN_MODE_BERR_AERR_RESET {
                // The fault handler is not the sequential continuation of the
                // instruction being recorded. Discard the partial path before
                // execution resumes at the exception vector.
                trace_jit::stop_recording(self, trace_jit::RecordingStop::TrapOrException);
                // Re-assert the handler entry the fault established, as
                // `execute` and `step` do; a double fault stays halted and
                // returns at the stopped check below.
                self.end_faulted_instruction();
                probe_on_entry = true;
            } else {
                // A complex opcode leaves the decoded fast path and executes
                // through the full dispatcher above. Offer that successfully
                // executed instruction to an in-progress trace just as the
                // decoded paths do. The trace decoder remains the authority
                // on whether the exact opcode/extension form is safe to
                // replay; an unsupported operation simply ends recording.
                trace_jit::record_executed(self, bus, self.ppc, self.pc);

                // Mirrors `execute`: only backward branches can reach a
                // trace head, so straight-line dispatches re-enter the
                // fast loop without a trace-cache probe.
                probe_on_entry =
                    self.pc <= self.ppc && trace_jit::note_backward_branch(self, self.cpu_type);

                if !self.sst_m68000_compat && self.check_trace() {
                    let _ = self.exception_trace(bus);
                }

                if self.int_level > 0 {
                    self.check_and_service_interrupts(bus);
                }
            }

            if self.stopped != 0 {
                trace_jit::stop_recording(self, trace_jit::RecordingStop::HostBoundary);
                return BatchResult {
                    instructions: retired,
                    exit: BatchExit::Stopped,
                };
            }

            if !watch_pcs.is_empty() && watch_pcs.contains(&self.pc) {
                // Match the decoded fast path: a watched-PC return is a host
                // boundary, so a partial recording cannot survive it.
                trace_jit::stop_recording(self, trace_jit::RecordingStop::HostBoundary);
                return BatchResult {
                    instructions: retired,
                    exit: BatchExit::WatchedPc { pc: self.pc },
                };
            }
        }
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
        self.step_with_dispatch(bus, dispatch_instruction)
    }

    /// Execute one precise instruction using the supplied post-fetch dispatcher.
    ///
    /// The helper owns the normal `step()` prologue and epilogue so internal
    /// callers can select a dispatcher after the precise opcode fetch without
    /// fetching the opcode twice.
    fn step_with_dispatch<B, F>(&mut self, bus: &mut B, dispatch: F) -> StepResult
    where
        B: AddressBus,
        F: FnOnce(&mut CpuCore, &mut B, u16) -> super::types::InternalStepResult,
    {
        use crate::core::types::InternalStepResult;

        self.set_precise_bus(true);
        if self.stopped != 0 {
            if let Some(cycles) = self.stopped_supervisor_check(bus) {
                return StepResult::Ok { cycles };
            }
            return StepResult::Stopped;
        }

        // A trace-disabled batch or fault may leave the previous flow latch set.
        // Reset it only when a new instruction starts, before its own effects.
        self.change_of_flow = false;
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
        let result = dispatch(self, bus, self.ir as u16);
        let fetch_cached = if timing_uses_full_fetch_stream(self.cpu_type) {
            bus.instruction_fetches_were_cached()
        } else {
            opcode_fetch_cached
        };

        if !matches!(result, InternalStepResult::Ok { .. }) {
            self.clear_execution_pipeline_state();
        }

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

    /// Execute the narrow decoded subset allowed by the boundary-hook runner.
    ///
    /// Called only after the precise opcode fetch. Unsupported operations pass the
    /// same fetched opcode to the existing dispatcher without repeating fetch or
    /// prefetch work.
    fn dispatch_boundary_hook_decoded<B: AddressBus>(
        &mut self,
        bus: &mut B,
        opcode: u16,
    ) -> super::types::InternalStepResult {
        use super::types::InternalStepResult;

        // Trace states remain on the normal dispatcher so it can preserve trace
        // exceptions and model-specific behavior.
        if self.run_mode != RUN_MODE_NORMAL || (self.t1_flag | self.t0_flag) != 0 {
            return dispatch_instruction(self, bus, opcode);
        }

        let Some(fast_op) = decode_boundary_hook_op(self.cpu_type, opcode) else {
            return dispatch_instruction(self, bus, opcode);
        };

        let cycles = match (self.cpu_type, fast_op) {
            // The generic decoded executor mutates Dn immediately. M68000
            // CLR.W/L Dn instead performs its final prefetch and IPL poll
            // first, plus the long form's two-clock sync, so reuse the precise
            // implementation for that model after the opcode-only decode.
            (
                CpuType::M68000,
                DecodedSimpleOp::UnaryDataReg {
                    op: UnaryOp::Clr,
                    reg,
                    size,
                },
            ) => self.exec_clr(bus, size, super::ea::AddressingMode::DataDirect(reg)),
            _ => fast_op.execute(self, bus),
        };

        InternalStepResult::Ok { cycles }
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

        self.set_precise_bus(true);
        if self.stopped != 0 {
            if let Some(cycles) = self.stopped_supervisor_check(bus) {
                return StepResult::Ok { cycles };
            }
            return StepResult::Stopped;
        }

        // A trace-disabled batch or fault may leave the previous flow latch set.
        // Reset it only when a new instruction starts, before its own effects.
        self.change_of_flow = false;
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
        let fetch_cached = if timing_uses_full_fetch_stream(self.cpu_type) {
            bus.instruction_fetches_were_cached()
        } else {
            opcode_fetch_cached
        };

        // A surfaced or HLE-handled trap still represents an architectural
        // execution boundary. Clear timing state before invoking callbacks,
        // since a handler may itself resume execution.
        if !matches!(result, InternalStepResult::Ok { .. }) {
            self.clear_execution_pipeline_state();
        }

        // Handle trap results via callbacks, fallback to exception if not handled
        let cycles = match result {
            InternalStepResult::Ok { cycles } => self.finalize_cycles(cycles, fetch_cached),
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
        // Any exception entry breaks open execution-pipeline state.
        self.clear_execution_pipeline_state();
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

    /// Check and service pending interrupts, returning whether one was taken.
    fn check_and_service_interrupts<B: AddressBus>(&mut self, bus: &mut B) -> bool {
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
            true
        } else {
            false
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
        self.clear_execution_pipeline_state();
        self.stopped |= STOP_LEVEL_HALT;
    }

    /// Stop the CPU (STOP instruction).
    pub fn stop(&mut self, new_sr: u16) {
        self.clear_execution_pipeline_state();
        self.set_sr(new_sr);
        self.stopped |= STOP_LEVEL_STOP;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_fetch_stream_timing_covers_020_and_030_families() {
        for cpu_type in [
            CpuType::M68EC020,
            CpuType::M68020,
            CpuType::M68EC030,
            CpuType::M68030,
        ] {
            assert!(timing_uses_full_fetch_stream(cpu_type));
        }
        for cpu_type in [
            CpuType::M68000,
            CpuType::M68010,
            CpuType::M68040,
            CpuType::M68060,
        ] {
            assert!(!timing_uses_full_fetch_stream(cpu_type));
        }
    }

    #[test]
    fn boundary_hook_clr_admission_is_exact() {
        for cpu_type in [
            CpuType::M68000,
            CpuType::M68010,
            CpuType::M68020,
            CpuType::M68030,
            CpuType::M68040,
        ] {
            for opcode in 0x4240..=0x4247 {
                assert!(matches!(
                    decode_boundary_hook_op(cpu_type, opcode),
                    Some(DecodedSimpleOp::UnaryDataReg {
                        op: UnaryOp::Clr,
                        size: Size::Word,
                        ..
                    })
                ));
            }
            for opcode in 0x4280..=0x4287 {
                assert!(matches!(
                    decode_boundary_hook_op(cpu_type, opcode),
                    Some(DecodedSimpleOp::UnaryDataReg {
                        op: UnaryOp::Clr,
                        size: Size::Long,
                        ..
                    })
                ));
            }
            for opcode in [0x4200, 0x4248, 0x4250, 0x42C0] {
                assert!(decode_boundary_hook_op(cpu_type, opcode).is_none());
            }
        }
    }
}

#[cfg(test)]
mod batch_host_boundary_tests {
    use crate::{AddressBus, BatchExit, CpuCore, CpuType, FastMem};

    struct Bus {
        ram: Vec<u8>,
        synced: u32,
        stop_at: u32,
        writes: Vec<(u32, u16)>,
    }
    impl Bus {
        fn new() -> Self {
            let mut ram = vec![0; 4096];
            // D0 = 0; D1 = 63; increment D0 64 times; write COLOR00; repeat.
            let words = [
                0x7000u16, 0x723f, 0x5280, 0x51c9, 0xfffc, 0x33c0, 0x00df, 0xf180, 0x60ee,
            ];
            for (i, word) in words.iter().enumerate() {
                ram[0x100 + i * 2..0x102 + i * 2].copy_from_slice(&word.to_be_bytes());
            }
            Self {
                ram,
                synced: 0,
                stop_at: u32::MAX,
                writes: Vec::new(),
            }
        }
    }
    impl AddressBus for Bus {
        fn read_byte(&mut self, a: u32) -> u8 {
            self.ram[a as usize]
        }
        fn read_word(&mut self, a: u32) -> u16 {
            u16::from_be_bytes(self.ram[a as usize..a as usize + 2].try_into().unwrap())
        }
        fn read_long(&mut self, a: u32) -> u32 {
            u32::from_be_bytes(self.ram[a as usize..a as usize + 4].try_into().unwrap())
        }
        fn write_byte(&mut self, a: u32, v: u8) {
            self.ram[a as usize] = v;
        }
        fn write_word(&mut self, a: u32, v: u16) {
            if a == 0xdff180 {
                self.writes.push((self.synced, v));
            } else {
                self.ram[a as usize..a as usize + 2].copy_from_slice(&v.to_be_bytes());
            }
        }
        fn write_long(&mut self, a: u32, v: u32) {
            self.ram[a as usize..a as usize + 4].copy_from_slice(&v.to_be_bytes());
        }
        fn fast_mem(&mut self) -> Option<FastMem> {
            Some(FastMem {
                ptr: self.ram.as_mut_ptr(),
                base: 0,
                len: self.ram.len() as u32,
            })
        }
        fn batch_boundary(&mut self, retired: u32, _pc: u32, _sr: u16) -> bool {
            self.synced = retired;
            retired >= self.stop_at
        }
    }
    fn cpu() -> CpuCore {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.pc = 0x100;
        cpu
    }

    #[test]
    fn host_sees_native_prefix_before_device_write() {
        let mut bus = Bus::new();
        let result = cpu().run_batch(&mut bus, 132 * 100, &[]);
        assert_eq!(result.instructions, 132 * 100);
        assert_eq!(bus.writes.len(), 100);
        for (retired, value) in bus.writes {
            assert_eq!(
                retired % 132,
                130,
                "complete native prefix before bus write"
            );
            assert_eq!(value, 64);
        }
    }

    #[test]
    fn requested_host_boundary_resumes_without_replaying_an_instruction() {
        let mut bus = Bus::new();
        let mut split = cpu();
        bus.stop_at = 132;
        let first = split.run_batch(&mut bus, 10_000, &[]);
        assert_eq!(first.exit, BatchExit::BoundaryRequested);
        assert_eq!(first.instructions, 132);
        bus.stop_at = u32::MAX;
        let second = split.run_batch(&mut bus, 132, &[]);
        assert_eq!(second.instructions, 132);
        let mut whole = cpu();
        whole.run_batch(&mut Bus::new(), 264, &[]);
        assert_eq!(split.pc, whole.pc);
        assert_eq!(split.d(0), whole.d(0));
        assert_eq!(split.d(1), whole.d(1));
        assert_eq!(split.get_sr(), whole.get_sr());
    }
}
