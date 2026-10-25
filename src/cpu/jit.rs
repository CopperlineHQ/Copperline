// SPDX-License-Identifier: GPL-3.0-or-later

//! Instruction-budgeted CPU execution, synchronized at hardware deadlines.

use super::*;

/// State exists only while the core owns a batch. Persistent fractional clocks
/// move back to M68kMachine before any caller can save or roll back the machine.
#[derive(Clone, Copy)]
pub(super) struct JitBatchTiming {
    retired: u32,
    observed_retired: u32,
    observed_sr: u16,
    clocks: u32,
    cck: u32,
    clocks_per_cck: u32,
    clock_carry: u32,
    divisor_q16: u32,
    carry_q16: u32,
    bus_checkpoint: u32,
    deadline: u64,
    sync: bool,
}

impl CpuBus {
    fn jit_plain_access(&self, address: u32, size: usize) -> bool {
        matches!(
            self.classify_plain_memory(self.mask(address), size),
            Some(
                PlainMemRegion::Rom(_)
                    | PlainMemRegion::OverlayRom(_)
                    | PlainMemRegion::ExtendedRom(_)
                    | PlainMemRegion::MbRam(_)
                    | PlainMemRegion::AccelRam(_)
                    | PlainMemRegion::ZorroRam(..)
            )
        )
    }

    pub(super) fn note_jit_bus_access(&mut self, address: u32, size: usize) {
        if self.jit_batch.is_some() && !self.jit_plain_access(address, size) {
            self.flush_jit_batch_prefix();
            self.jit_bus_boundary = true;
        }
    }

    /// Record progress cheaply between decoded instructions. The initial
    /// budget already stops at the next hardware deadline; local memory cannot
    /// observe device time before then. Shared-bus accesses flush this prefix
    /// before touching hardware, and the host flushes the final batch count.
    pub(super) fn note_jit_batch_boundary(&mut self, retired: u32, pc: u32, sr: u16) -> bool {
        let Some(timing) = self.jit_batch.as_mut() else {
            return false;
        };
        timing.observed_retired = retired;
        timing.observed_sr = sr;
        self.diag_current_pc = pc;
        self.bus.cpu_pc = pc;
        self.jit_batch_should_stop(retired, sr)
    }

    pub(super) fn flush_jit_batch_prefix(&mut self) {
        if let Some(timing) = self
            .jit_batch
            .as_ref()
            .filter(|timing| timing.observed_retired > timing.retired)
        {
            self.sync_jit_batch(
                timing.observed_retired,
                self.diag_current_pc,
                timing.observed_sr,
            );
        }
    }

    fn jit_batch_should_stop(&self, retired: u32, sr: u16) -> bool {
        let Some(timing) = self.jit_batch.as_ref() else {
            return false;
        };
        let irq = if self.bus.paula.intena & INT_MASTER != 0 {
            pending_ipl(self.bus.paula.intena & self.bus.cpu_visible_intreq())
                > ((sr >> 8) & 7) as u8
        } else {
            false
        };
        // Always allow progress on entry, including a bring-up interrupt whose
        // vector has not been installed yet. The host applies that guard.
        retired != 0
            && (self.jit_bus_boundary
                || irq
                || self.bus.cartridge_nmi_pending()
                || self.bus.slice_preempted
                || self.bus.emulated_cck() >= timing.deadline)
    }

    pub(super) fn sync_jit_batch(&mut self, retired: u32, pc: u32, sr: u16) -> bool {
        let Some(mut timing) = self.jit_batch.take() else {
            return false;
        };
        let instructions = retired.saturating_sub(timing.retired);
        if instructions != 0 {
            let total = u64::from(instructions) * u64::from(timing.divisor_q16)
                + u64::from(timing.carry_q16);
            let clocks = (total >> 16) as u32;
            timing.carry_q16 = (total & 0xffff) as u32;
            timing.clocks += clocks;
            let overlap = self.bus.take_cpu_bus_overlap_clocks();
            let clocks = clocks.saturating_sub(overlap) + timing.clock_carry;
            let cck = clocks / timing.clocks_per_cck;
            timing.clock_carry = clocks % timing.clocks_per_cck;
            timing.cck += cck;
            if timing.sync {
                let on_bus = self
                    .bus
                    .slice_bus_advanced_cck()
                    .saturating_sub(timing.bus_checkpoint);
                self.bus
                    .advance_cpu_internal_cycles(cck.saturating_sub(on_bus));
            }
            timing.bus_checkpoint = self.bus.slice_bus_advanced_cck();
            timing.retired = retired;
            self.flush_timed_devices_dcache_coherent();
        }
        self.diag_current_pc = pc;
        self.bus.cpu_pc = pc;
        self.jit_batch = Some(timing);
        self.jit_batch_should_stop(retired, sr)
    }
}

impl M68kMachine {
    /// Convert the next observable hardware deadline to an instruction budget,
    /// retaining both fractional-clock carries. One instruction is the minimum
    /// when a slow configured CPU cannot stop exactly on a colour-clock edge.
    fn jit_batch_budget(&self, remaining: usize, horizon: u32) -> usize {
        if !self.bus.jit_plain_access(self.cpu.pc, 2) {
            // Code on the shared bus must synchronize its fetches as well as
            // operands. Native bursts are reserved for local RAM and ROM.
            return 1;
        }
        if self.jit_clock_divisor_q16 == 0 {
            return remaining.min(JIT_BATCH_INSTRUCTIONS);
        }
        let clocks = u64::from(horizon) * u64::from(self.cpu_clocks_per_cck)
            - u64::from(self.cpu_clock_carry);
        let q16 = (clocks << 16).saturating_sub(u64::from(self.jit_clock_carry_q16));
        let instructions = q16.div_ceil(u64::from(self.jit_clock_divisor_q16)).max(1);
        remaining
            .min(JIT_BATCH_INSTRUCTIONS)
            .min(instructions as usize)
    }

    /// One bounded spare-host-time burst. Local CPU work consumes no chipset
    /// clocks; shared-bus accesses retain arbitration and end the burst.
    /// Restore fractional timing before returning so snapshots never contain
    /// the transient zero divisor. The caller owns the host deadline.
    pub(crate) fn step_jit_spare_slice(&mut self, count: usize) -> Result<CpuStepSlice> {
        if !self.jit_batch_path_active() {
            return Ok(CpuStepSlice {
                instructions: 0,
                cpu_cycles: 0,
                cpu_cck: 0,
                bus_advanced_cck: 0,
                stopped: self.stopped(),
            });
        }
        let saved = (
            self.jit_clock_divisor_q16,
            self.jit_clock_carry_q16,
            self.cpu_clock_carry,
        );
        self.jit_clock_divisor_q16 = 0;
        self.jit_clock_carry_q16 = 0;
        self.cpu_clock_carry = 0;
        let result = self.step_slice_jit(count);
        (
            self.jit_clock_divisor_q16,
            self.jit_clock_carry_q16,
            self.cpu_clock_carry,
        ) = saved;
        result
    }

    /// Fast execution remains a one-clock-per-instruction accelerator model.
    /// Hardware accesses and interrupts synchronize at instruction boundaries;
    /// direct-memory traces can run until the next device deadline.
    pub(super) fn step_slice_jit(&mut self, count: usize) -> Result<CpuStepSlice> {
        self.bus.bus.begin_cpu_slice();
        self.bus.bus.slice_preempted = false;
        self.bus.bus.set_cpu_bus_arbitration_enabled(true);
        self.bus.bus.set_cpu_access_phase_sync(false);
        let (mut instructions, mut cpu_cycles, mut cpu_cck) = (0usize, 0u32, 0u32);
        let mut stopped = false;

        while instructions < count {
            let bus_before = self.bus.bus.slice_bus_advanced_cck();
            if let Some(irq) = self.service_pending_irq_cycles() {
                let clocks = self.jit_scaled_clocks(positive_cpu_cycles(irq.cycles));
                cpu_cycles = cpu_cycles.saturating_add(clocks);
                let cck = self.charge_cpu_clocks_less_bus_overlap(clocks as i32);
                cpu_cck = cpu_cck.saturating_add(cck);
                if self.sync_cck_on {
                    let on_bus = self
                        .bus
                        .bus
                        .slice_bus_advanced_cck()
                        .saturating_sub(bus_before);
                    self.bus
                        .bus
                        .advance_cpu_internal_cycles(cck.saturating_sub(on_bus));
                }
                self.bus.flush_timed_devices_dcache_coherent();
            }
            // Recompute INTREQ on every host boundary, including after RTE.
            // Never leave a stale request latched in the core's batch runner.
            self.cpu.set_irq(0);
            // Shared-bus code already has a one-instruction budget. Predicting
            // blitter, disk and CIA deadlines cannot shorten it, and doing so
            // for every fetch is expensive in polling loops.
            let horizon = if count - instructions > 1 && self.bus.jit_plain_access(self.cpu.pc, 2) {
                self.bus.bus.jit_event_horizon_cck()
            } else {
                1
            };
            let budget = self.jit_batch_budget(count - instructions, horizon) as u32;
            self.bus.diag_current_pc = self.cpu.pc;
            self.bus.bus.cpu_pc = self.cpu.pc;
            self.bus.window_offered = false;
            self.bus.jit_bus_boundary = false;
            self.bus.jit_batch = Some(JitBatchTiming {
                retired: 0,
                observed_retired: 0,
                observed_sr: self.cpu.get_sr(),
                clocks: 0,
                cck: 0,
                clocks_per_cck: self.cpu_clocks_per_cck,
                clock_carry: self.cpu_clock_carry,
                divisor_q16: self.jit_clock_divisor_q16,
                carry_q16: self.jit_clock_carry_q16,
                bus_checkpoint: self.bus.bus.slice_bus_advanced_cck(),
                deadline: self.bus.bus.emulated_cck() + u64::from(horizon),
                sync: self.sync_cck_on,
            });
            let cpu_was_stopped = self.cpu.is_stopped();
            let result = self.cpu.run_batch(&mut self.bus, budget, &[]);
            self.bus
                .sync_jit_batch(result.instructions, self.cpu.pc, self.cpu.get_sr());
            let timing = self.bus.jit_batch.take().expect("active JIT batch");
            self.cpu_clock_carry = timing.clock_carry;
            self.jit_clock_carry_q16 = timing.carry_q16;
            cpu_cycles = cpu_cycles.saturating_add(timing.clocks);
            cpu_cck = cpu_cck.saturating_add(timing.cck);
            instructions = instructions.saturating_add(result.instructions as usize);
            if !self.bus.window_offered {
                self.bus.cache_free_window = None;
                self.bus.cache_free_regions.fill(None);
            }
            if self.bus.icache.is_some() || self.bus.dcache.is_some() {
                self.apply_cacr_updates();
            }

            let bus_before = self.bus.bus.slice_bus_advanced_cck();
            let exception_cycles = match result.exit {
                BatchExit::BudgetExhausted
                | BatchExit::BoundaryRequested
                | BatchExit::WatchedPc { .. } => None,
                BatchExit::Stopped => {
                    if !cpu_was_stopped {
                        self.bus.bus.note_cpu_stop(false);
                    }
                    stopped = true;
                    None
                }
                BatchExit::AlineTrap { .. } => Some(self.cpu.take_aline_exception(&mut self.bus)),
                BatchExit::FlineTrap { opcode } => Some(self.service_fline_trap(opcode)),
                BatchExit::TrapInstruction { trap_num } => {
                    Some(self.cpu.take_trap_exception(&mut self.bus, trap_num))
                }
                BatchExit::Breakpoint { .. } => Some(self.cpu.take_bkpt_exception(&mut self.bus)),
                BatchExit::IllegalInstruction { .. } => {
                    Some(self.cpu.take_illegal_exception(&mut self.bus))
                }
            };
            if let Some(cycles) = exception_cycles {
                instructions += 1;
                let clocks = self.jit_scaled_clocks(
                    positive_cpu_cycles(cycles).saturating_add(JIT_CPU_CLOCKS_PER_INSTRUCTION),
                );
                cpu_cycles = cpu_cycles.saturating_add(clocks);
                let cck = self.charge_cpu_clocks_less_bus_overlap(clocks as i32);
                cpu_cck = cpu_cck.saturating_add(cck);
                if self.sync_cck_on {
                    let on_bus = self
                        .bus
                        .bus
                        .slice_bus_advanced_cck()
                        .saturating_sub(bus_before);
                    self.bus
                        .bus
                        .advance_cpu_internal_cycles(cck.saturating_sub(on_bus));
                }
            }
            if stopped || self.bus.bus.slice_preempted || self.jit_clock_divisor_q16 == 0 {
                break;
            }
        }
        self.bus.flush_timed_devices_dcache_coherent();
        self.bus.bus.set_cpu_bus_arbitration_enabled(false);
        let (bus_advanced_cck, _) = self.bus.bus.take_slice_bus_advance();
        Ok(CpuStepSlice {
            instructions,
            cpu_cycles,
            cpu_cck,
            bus_advanced_cck,
            stopped,
        })
    }
}
