// SPDX-License-Identifier: GPL-3.0-or-later

//! The precise core's wait-loop skip: a guest polling a flag in fast RAM or
//! the caches until an interrupt changes it retires the loop's turns in one
//! step instead of executing them, with the machine left exactly where
//! executing them would have left it.
//!
//! It is the running-CPU counterpart of the `STOP` fast-forward. A loop is
//! proven at run time, never matched by opcode: after a short backward
//! branch, two consecutive turns are executed with the CPU bus counting
//! every access a proof cannot rely on (`src/cpu/wait_loop.rs`). When both
//! turns were pure and the CPU-side state at their ends is the same, every
//! further turn from that state repeats the last one -- the same
//! instructions, cycle charges and reads -- until something outside the CPU
//! acts: an interrupt, or a bus master writing what the loop reads. Up to
//! the CPU's event horizon nothing can, so the turns that fit before it are
//! retired at once: the chipset and the devices advance through their
//! colour clocks in one span, and the instruction count, the reverse-debug
//! position and the pacing budget advance by what the turns would have
//! added. The skip stops a full turn short of the horizon, and short of the
//! frame wrap and the display-start line, so whatever happens there meets
//! an instruction executed the ordinary way.
//!
//! Every decision is a function of the machine state and of the run loop's
//! real-time quantum, whose end the skip never passes (the quantum's
//! boundaries are where the `STOP` fast-forward partitions its naps, so an
//! unskipped run must see them at the same instructions). Debugger-driven
//! stepping, reverse replay, the JIT, any debug or trace hook and any
//! device without an exact deadline turn it off. `COPPERLINE_WAIT_SKIP=0`
//! turns it off altogether.

use super::*;
use crate::cpu::{LoopFingerprint, LoopKey};

/// Longest loop body, in bytes from the backward branch to its target.
const MAX_LOOP_BYTES: u32 = 64;
/// Longest loop body, in instructions.
const MAX_LOOP_INSTRUCTIONS: usize = 16;
/// Turns recorded at one head before it is given up as not periodic. A
/// turn's CPU clocks need not be a whole number of colour clocks, so the
/// sub-colour-clock phase can take several turns to come back round: up to
/// one turn per CPU clock in a colour clock.
const MAX_PROOF_TURNS: usize = 40;

/// Whether the skip is on (`COPPERLINE_WAIT_SKIP`, default on).
fn wait_skip_enabled() -> bool {
    match crate::envcfg::var("COPPERLINE_WAIT_SKIP") {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => true,
    }
}

/// One instruction of a recorded turn, with what the run loop charged it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StepCost {
    pc: u32,
    bus_cck: u32,
    cpu_cycles: u32,
    cpu_cck: u32,
    debit: usize,
}

/// Where a recorded turn ended: the counts of steps and reads recorded up
/// to its end, and the key at the head it came back to.
struct TurnEnd {
    steps: usize,
    reads: usize,
    key: LoopKey,
}

/// The turns recorded from a candidate head, in buffers reused from one
/// candidate to the next.
struct Recorder {
    head: u32,
    /// The key at the head before the first turn.
    start: LoopKey,
    /// Every step of every turn so far.
    steps: Vec<StepCost>,
    /// Where the turn being recorded started in `steps`.
    turn_start: usize,
    ends: Vec<TurnEnd>,
    period: Option<Period>,
}

impl Recorder {
    /// The step and read counts at the head before turn `turn` (0-based).
    fn boundary(&self, turn: usize) -> (usize, usize) {
        turn.checked_sub(1).map_or((0, 0), |last| {
            (self.ends[last].steps, self.ends[last].reads)
        })
    }
}

/// A proven loop: from `start`, with the bytes in `reads`, a period of its
/// turns executes and comes back to `start`.
struct ProvenLoop {
    head: u32,
    lo: u32,
    hi: u32,
    start: LoopFingerprint,
    reads: Vec<(u32, u8, u32)>,
    instructions: u64,
    cck: u32,
    cpu_cycles: u32,
    cpu_cck: u32,
    debit: usize,
}

/// A candidate period: the full fingerprint at the head after `at` turns,
/// to be compared with the one `len` turns later.
struct Period {
    at: usize,
    len: usize,
    start: LoopFingerprint,
}

enum State {
    Idle,
    /// Turns from a head are being recorded (`WaitSkip::recorder`).
    Recording,
    Proven(Box<ProvenLoop>),
    /// The loop at `lo..=hi` is not a wait loop; not tried again until the
    /// CPU leaves it.
    Rejected {
        lo: u32,
        hi: u32,
    },
}

pub(super) struct WaitSkip {
    enabled: bool,
    /// Whether this real-time call may skip: the skip is enabled and
    /// nothing that watches every instruction is armed. Settled once per
    /// call (`begin_wait_quantum`), since only the host arms those.
    active: bool,
    state: State,
    recorder: Box<Recorder>,
    /// Instructions retired by skips since power-on, for diagnostics and
    /// tests.
    skipped_instructions: u64,
}

impl WaitSkip {
    pub(super) fn new() -> Self {
        Self {
            enabled: wait_skip_enabled(),
            active: false,
            state: State::Idle,
            recorder: Box::new(Recorder {
                head: 0,
                start: LoopKey::default(),
                steps: Vec::new(),
                turn_start: 0,
                ends: Vec::new(),
                period: None,
            }),
            skipped_instructions: 0,
        }
    }

    /// Forget any loop being proven or proven. Anything outside the run
    /// loop (a debugger, a control client, a state load) may have changed
    /// the machine, so each real-time call starts over.
    pub(super) fn reset(&mut self) {
        self.state = State::Idle;
    }
}

fn span(steps: &[StepCost]) -> (u32, u32) {
    steps.iter().fold((u32::MAX, 0), |(lo, hi), step| {
        (lo.min(step.pc), hi.max(step.pc))
    })
}

impl Emulator {
    /// Turn the wait-loop skip on or off for this emulator (tests compare
    /// runs with it on and off; `COPPERLINE_WAIT_SKIP` sets the default).
    pub fn set_wait_skip(&mut self, on: bool) {
        self.wait_skip.enabled = on;
        self.wait_skip.reset();
    }

    /// Instructions the wait-loop skip has retired without executing them
    /// since power-on.
    pub fn wait_skip_instructions(&self) -> u64 {
        self.wait_skip.skipped_instructions
    }

    /// One iteration of the real-time loop, as `run_one_step`, skipping a
    /// proven wait loop's turns where it can (see the module comment).
    /// `budget` is what is left of the real-time quantum, which a skip
    /// never passes.
    pub(super) fn run_one_step_skipping(
        &mut self,
        cpu_idle: &mut bool,
        budget: usize,
    ) -> Result<RealSliceAccounting> {
        if !self.wait_skip.active || *cpu_idle {
            self.end_wait_loop();
            return self.run_one_step(cpu_idle, budget);
        }
        let pc = self.machine.pc();
        if matches!(&self.wait_skip.state, State::Proven(proven) if proven.head == pc) {
            if let Some(accounting) = self.try_wait_skip(pc, budget)? {
                return Ok(accounting);
            }
        }
        let impure = self.machine.wait_impure_count();
        let (accounting, run) = self.run_one_step_with_slice(cpu_idle, budget, false)?;
        let pure = run.actual_instructions == 1
            && !run.cpu_stopped
            && self.machine.wait_impure_count() == impure;
        let cost = StepCost {
            pc,
            bus_cck: run.bus_advanced_cck,
            cpu_cycles: run.actual_cpu_cycles,
            cpu_cck: run.actual_cpu_cck,
            debit: accounting.budget_debit,
        };
        self.observe_wait_step(cost, pure)?;
        Ok(accounting)
    }

    /// Start a real-time call: forget any loop (whatever ran the machine
    /// since the last call may have changed it) and settle whether this
    /// call may skip at all.
    pub(super) fn begin_wait_quantum(&mut self) {
        self.end_wait_loop();
        self.wait_skip.active =
            self.wait_skip.enabled && !self.cpu_jit && self.machine.wait_skip_supported();
    }

    pub(super) fn end_wait_loop(&mut self) {
        if !matches!(self.wait_skip.state, State::Idle) {
            self.wait_skip.reset();
            self.machine.record_wait_reads(false);
        }
    }

    /// Advance the proof by one executed instruction.
    #[inline]
    fn observe_wait_step(&mut self, cost: StepCost, pure: bool) -> Result<()> {
        let pc = self.machine.pc();
        match &self.wait_skip.state {
            // The common case: no candidate, and this is no backward branch.
            State::Idle if !(pure && pc < cost.pc) => return Ok(()),
            State::Idle => {}
            State::Rejected { lo, hi } => {
                if (*lo..=*hi).contains(&pc) {
                    return Ok(());
                }
                self.wait_skip.state = State::Idle;
            }
            State::Proven(proven) => {
                if pure && (proven.lo..=proven.hi).contains(&pc) {
                    return Ok(());
                }
                self.end_wait_loop();
            }
            State::Recording => {
                if pure {
                    return self.record_wait_step(cost, pc);
                }
                self.end_wait_loop();
            }
        }
        // A short backward branch taken by a pure instruction lands on a
        // candidate head: record the turns from it.
        if pure && pc < cost.pc && cost.pc - pc <= MAX_LOOP_BYTES {
            self.machine.record_wait_reads(true);
            let recorder = &mut *self.wait_skip.recorder;
            recorder.head = pc;
            recorder.start = self.machine.wait_key();
            recorder.steps.clear();
            recorder.turn_start = 0;
            recorder.ends.clear();
            recorder.period = None;
            self.wait_skip.state = State::Recording;
        }
        Ok(())
    }

    /// Record one pure step of a candidate loop's turn, and at the head
    /// look for, then confirm, a period.
    fn record_wait_step(&mut self, cost: StepCost, pc: u32) -> Result<()> {
        let recorder = &mut *self.wait_skip.recorder;
        recorder.steps.push(cost);
        if pc != recorder.head {
            if recorder.steps.len() - recorder.turn_start >= MAX_LOOP_INSTRUCTIONS {
                self.end_wait_loop();
            }
            return Ok(());
        }
        let key = self.machine.wait_key();
        let (lo, hi) = span(&recorder.steps[recorder.turn_start..]);
        // The first turn may still settle the condition codes the code
        // before the loop left; from then on a wait loop's turns leave the
        // registers alone.
        if recorder
            .ends
            .last()
            .is_some_and(|last| !key.same_registers(&last.key))
        {
            self.reject_wait_loop(lo, hi);
            return Ok(());
        }
        recorder.turn_start = recorder.steps.len();
        let done = recorder.ends.len() + 1;
        match recorder.period.as_ref() {
            None => {
                // The first earlier head with the same key: its distance is
                // the period to check in full.
                let earlier = std::iter::once(&recorder.start)
                    .chain(recorder.ends.iter().map(|end| &end.key))
                    .position(|k| *k == key);
                if let Some(at) = earlier {
                    recorder.period = Some(Period {
                        at: done,
                        len: done - at,
                        start: self.machine.wait_fingerprint()?,
                    });
                }
            }
            Some(period) if done == period.at + period.len => {
                let at = period.at;
                let periodic = self.machine.wait_fingerprint()?.same_as(&period.start);
                let recorder = &mut *self.wait_skip.recorder;
                recorder.ends.push(TurnEnd {
                    steps: recorder.steps.len(),
                    reads: self.machine.wait_reads_len(),
                    key,
                });
                if periodic {
                    self.prove_wait_loop(at);
                } else {
                    self.reject_wait_loop(lo, hi);
                }
                return Ok(());
            }
            Some(_) => {}
        }
        let recorder = &mut *self.wait_skip.recorder;
        recorder.ends.push(TurnEnd {
            steps: recorder.steps.len(),
            reads: self.machine.wait_reads_len(),
            key,
        });
        if done >= MAX_PROOF_TURNS {
            self.reject_wait_loop(lo, hi);
        }
        Ok(())
    }

    fn reject_wait_loop(&mut self, lo: u32, hi: u32) {
        self.machine.record_wait_reads(false);
        self.wait_skip.state = State::Rejected { lo, hi };
    }

    /// The turns after the first `at` ran from the recorded period start
    /// and came back to an equal state: every such period from there
    /// repeats them.
    fn prove_wait_loop(&mut self, at: usize) {
        let recorder = &mut *self.wait_skip.recorder;
        let Some(period) = recorder.period.take() else {
            return;
        };
        let (first_step, first_read) = recorder.boundary(at);
        let (last_step, last_read) = recorder.boundary(at + period.len);
        let steps = &recorder.steps[first_step..last_step];
        let reads = self.machine.wait_reads(first_read..last_read);
        self.machine.record_wait_reads(false);
        let (lo, hi) = span(steps);
        let sum = |f: fn(&StepCost) -> u32| steps.iter().map(f).sum::<u32>();
        let proven = ProvenLoop {
            head: recorder.head,
            lo,
            hi,
            start: period.start,
            reads,
            instructions: steps.len() as u64,
            cck: sum(|s| s.bus_cck),
            cpu_cycles: sum(|s| s.cpu_cycles),
            cpu_cck: sum(|s| s.cpu_cck),
            debit: steps.iter().map(|s| s.debit).sum(),
        };
        // A period that takes no time cannot be bounded by a horizon.
        self.wait_skip.state = if proven.cck == 0 || proven.debit == 0 {
            State::Rejected { lo, hi }
        } else {
            State::Proven(Box::new(proven))
        };
    }

    /// At a proven loop's head, retire the turns that fit before the
    /// horizon and inside the quantum, if any.
    fn try_wait_skip(&mut self, pc: u32, budget: usize) -> Result<Option<RealSliceAccounting>> {
        let State::Proven(proven) = &self.wait_skip.state else {
            return Ok(None);
        };
        if pc != proven.head {
            return Ok(None);
        }
        let bus = self.bus();
        let horizon = bus.cpu_event_horizon();
        let batch = bus.device_batch_limit_cck();
        if !horizon.exact || batch == Some(0) || self.machine.wait_interrupt_asserted() {
            return Ok(None);
        }
        let limit = [
            horizon.cck,
            batch,
            Some(bus.next_frame_event_cck()),
            bus.next_display_start_event_cck(),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(1);
        // One full turn short of the horizon: the instructions that meet
        // the event run the ordinary way.
        let by_time = u64::from((limit / proven.cck).saturating_sub(1));
        let by_budget = (budget / proven.debit) as u64;
        let turns = by_time.min(by_budget);
        if turns == 0 {
            return Ok(None);
        }
        // Still the proven loop: the same CPU-side state and bytes.
        let current = self.machine.wait_fingerprint()?;
        if !current.same_as(&proven.start) || !self.machine.wait_reads_unchanged(&proven.reads) {
            self.end_wait_loop();
            return Ok(None);
        }
        let cck = (turns * u64::from(proven.cck)) as u32;
        let instructions = turns * proven.instructions;
        let run = ExecutedSlice {
            actual_instructions: instructions as usize,
            actual_cpu_cycles: (turns * u64::from(proven.cpu_cycles)).min(u64::from(u32::MAX))
                as u32,
            actual_cpu_cck: (turns * u64::from(proven.cpu_cck)).min(u64::from(u32::MAX)) as u32,
            bus_advanced_cck: 0,
            cpu_stopped: false,
        };
        let budget_debit = turns as usize * proven.debit;
        let advanced = self.machine.skip_wait_turns(cck);
        debug_assert_eq!(advanced, cck, "a skipped span advances exactly its turns");
        let run = ExecutedSlice {
            bus_advanced_cck: advanced,
            ..run
        };
        // The bookkeeping `execute_cpu_slice` does for executed turns.
        self.stats.slices += 1;
        self.stats.instructions = self.stats.instructions.saturating_add(instructions);
        self.retired_instructions = self.retired_instructions.saturating_add(instructions);
        self.wait_skip.skipped_instructions += instructions;
        let accounting = RealSliceAccounting {
            budget_debit,
            device_cck: advanced,
            chip_bus_wait_cck: 0,
            slice_cck: advanced,
        };
        self.machine.refresh_irq_line();
        self.real_pacing_profile.record_slice(&run, accounting);
        Ok(Some(accounting))
    }
}

#[cfg(test)]
mod tests {
    use crate::config::CpuModel;

    /// `tests/data/wait_skip/guest.asm`: a main loop polling a fast-RAM
    /// flag that VBL, blitter and CIA-A timer interrupts set. Position
    /// independent; these are offsets into it.
    const GUEST: &[u8] = include_bytes!("../../tests/data/wait_skip/guest.bin");
    const GUEST_DATA: u32 = 0xDC;
    const GUEST_STACK: u32 = 0x4EC;
    const GUEST_WAIT: u32 = 0x62;
    /// Where the guest runs: accelerator RAM on a 32-bit CPU, a Zorro II
    /// fast-RAM board on the 68000.
    const ACCEL_BASE: u32 = 0x0800_0000;
    const ZORRO_BASE: u32 = 0x0020_0000;

    fn guest_base(model: CpuModel) -> u32 {
        if matches!(model, CpuModel::M68000 | CpuModel::M68010) {
            ZORRO_BASE
        } else {
            ACCEL_BASE
        }
    }

    fn guest(model: CpuModel, clocks_per_cck: u32, cacr: u32, skip: bool) -> super::Emulator {
        let base = guest_base(model);
        let mut zorro = crate::zorro::ZorroChain::default();
        let mut accel_ram = Vec::new();
        if base == ZORRO_BASE {
            zorro
                .add_board_configured_at(crate::zorro::BoardSpec::fast_ram(1 << 20), base)
                .unwrap();
        } else {
            accel_ram = vec![0u8; 64 * 1024];
        }
        let bus = crate::bus::Bus::new(
            crate::memory::Memory {
                chip_ram: vec![0u8; 512 * 1024],
                slow_ram: Vec::new(),
                mb_ram: Vec::new(),
                accel_ram,
                rom: crate::memory::test_rom_with_interrupt_vectors(),
                overlay: false,
                zorro,
                extended_rom: Vec::new(),
                extended_rom_base: 0,
                wcs: Vec::new(),
                wcs_write_protected: false,
            },
            crate::chipset::paula::Paula::new(
                Box::new(crate::serial::NullSerialSink),
                Box::new(crate::audio::NullSink),
            ),
            crate::floppy::FloppyController::default(),
        );
        let mut emu = super::Emulator::new(
            bus,
            model,
            false,
            Default::default(),
            crate::config::PacingBudget::Cycles,
            clocks_per_cck,
            false,
        )
        .unwrap();
        if cacr != 0 {
            emu.set_cache_emulation(true, true);
        }
        assert_eq!(emu.machine.debug_write_memory(base, GUEST), GUEST.len());
        emu.machine.debug_set_register(7, cacr);
        emu.machine.debug_set_register(15, base + GUEST_STACK);
        emu.machine.debug_set_register(16, 0x2700);
        emu.machine.debug_set_register(17, base);
        emu.set_wait_skip(skip);
        emu
    }

    fn counters(emu: &super::Emulator, model: CpuModel) -> [u32; 4] {
        let bytes = emu
            .machine
            .debug_read_memory(guest_base(model) + GUEST_DATA, 16);
        let long = |i: usize| u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap());
        [
            u32::from(u16::from_be_bytes([bytes[2], bytes[3]])),
            long(4),
            long(8),
            long(12),
        ]
    }

    /// Run the guest with the skip on and off, frame by frame, and require
    /// the two machines to be the same at every frame: the same snapshot
    /// bytes and the same instruction position.
    fn skipped_and_executed_runs_agree(model: CpuModel, clocks_per_cck: u32, cacr: u32) -> u64 {
        let mut skipped = guest(model, clocks_per_cck, cacr, true);
        let mut executed = guest(model, clocks_per_cck, cacr, false);
        for frame in 0..30 {
            skipped.step_frame().unwrap();
            executed.step_frame().unwrap();
            assert_eq!(
                skipped.retired_instructions(),
                executed.retired_instructions(),
                "instruction position after frame {frame}"
            );
            assert!(
                skipped.snapshot_blob().unwrap() == executed.snapshot_blob().unwrap(),
                "machine state after frame {frame} (counters {:?} vs {:?})",
                counters(&skipped, model),
                counters(&executed, model)
            );
        }
        assert_eq!(executed.wait_skip_instructions(), 0);
        let [seen, wakes, frames, ticks] = counters(&skipped, model);
        assert_eq!(seen, 7, "VBL, blitter and timer each woke the poll");
        assert!(
            frames > 5 && ticks > 100 && wakes > ticks,
            "{:?}",
            counters(&skipped, model)
        );
        let base = guest_base(model);
        assert!(
            (base..base + GUEST_STACK).contains(&skipped.machine.pc()),
            "still in the guest at {:#010X}",
            skipped.machine.pc()
        );
        skipped.wait_skip_instructions()
    }

    #[test]
    fn a_skipped_fast_ram_poll_ends_where_the_executed_one_does_on_a_68020() {
        let skipped = skipped_and_executed_runs_agree(CpuModel::M68020, 4, 0);
        assert!(
            skipped > 10_000,
            "the poll was skipped: {skipped} instructions"
        );
    }

    #[test]
    fn a_skipped_zorro_ram_poll_ends_where_the_executed_one_does_on_a_68000() {
        let skipped = skipped_and_executed_runs_agree(CpuModel::M68000, 2, 0);
        assert!(
            skipped > 10_000,
            "the poll was skipped: {skipped} instructions"
        );
    }

    #[test]
    fn a_skipped_cached_poll_ends_where_the_executed_one_does_on_a_68030() {
        // EI | ED: the poll's code and flag come from the cache models.
        let skipped = skipped_and_executed_runs_agree(CpuModel::M68030, 7, 0x0000_0101);
        assert!(
            skipped > 10_000,
            "the poll was skipped: {skipped} instructions"
        );
    }

    #[test]
    fn a_skipped_cached_poll_ends_where_the_executed_one_does_on_a_68040() {
        // IE | DE.
        let skipped = skipped_and_executed_runs_agree(CpuModel::M68040, 7, 0x8000_8000);
        assert!(
            skipped > 10_000,
            "the poll was skipped: {skipped} instructions"
        );
    }

    #[test]
    fn the_poll_is_the_loop_the_skip_proves() {
        let mut emu = guest(CpuModel::M68020, 4, 0, true);
        emu.step_frame().unwrap();
        emu.step_frame().unwrap();
        assert!(emu.wait_skip_instructions() > 0);
        // Between interrupts the CPU sits in the two-instruction poll.
        let pc = emu.machine.pc() - ACCEL_BASE;
        assert!(pc >= GUEST_WAIT, "{pc:#X}");
    }

    /// A floppy motor coasting to a stop while the guest polls: the
    /// controller's tick stops counting the drives' clocks when it goes
    /// idle, so no skip may run across that moment.
    #[test]
    fn a_poll_across_the_floppy_coasting_to_idle_ends_where_execution_does() {
        // Without drive sounds the idle transition bounds the skips; with
        // them the coasting platter's sound holds them off.
        for sounds in [false, true] {
            poll_across_the_floppy_coasting_to_idle(sounds);
        }
    }

    fn poll_across_the_floppy_coasting_to_idle(sounds: bool) {
        let mut skipped = guest(CpuModel::M68020, 4, 0, true);
        let mut executed = guest(CpuModel::M68020, 4, 0, false);
        for emu in [&mut skipped, &mut executed] {
            emu.bus_mut().paula.drive_sounds_mut().set_enabled(sounds);
            // CIA-B PRB: MTR (bit 7) is latched by DF0's select edge
            // (bit 3), so set it while deselected, then select.
            let floppy = &mut emu.bus_mut().floppy;
            for prb in [0x7F, 0x77, 0x7F] {
                floppy.write_prb(prb); // motor on
            }
            emu.step_frame().unwrap();
            emu.step_frame().unwrap();
            // Off part-way through a quantum, so the platter does not come
            // to rest on a quantum's end, where no skip runs across.
            emu.debug_step_instructions(5_000).unwrap();
            let floppy = &mut emu.bus_mut().floppy;
            for prb in [0xFF, 0xF7, 0xFF] {
                floppy.write_prb(prb); // motor off: the platter coasts
            }
            assert!(emu.bus().floppy.cck_until_idle().is_some());
        }
        for frame in 0..30 {
            skipped.step_frame().unwrap();
            executed.step_frame().unwrap();
            assert!(
                skipped.snapshot_blob().unwrap() == executed.snapshot_blob().unwrap(),
                "machine state after frame {frame}, drive sounds {sounds}"
            );
        }
        assert!(
            skipped.bus().floppy.cck_until_idle().is_none(),
            "coasted to idle"
        );
        assert!(skipped.wait_skip_instructions() > 0);
    }

    /// Compare after every single skip, not just every quantum: the
    /// executed machine is stepped to the skipped one's instruction
    /// position and must be the same machine there.
    fn every_skip_lands_where_execution_does(model: CpuModel, clocks_per_cck: u32, cacr: u32) {
        let mut skipped = guest(model, clocks_per_cck, cacr, true);
        let mut executed = guest(model, clocks_per_cck, cacr, false);
        let (mut skipped_idle, mut executed_idle) = (false, false);
        skipped.begin_wait_quantum();
        executed.begin_wait_quantum();
        let mut skips = 0;
        let mut steps = 0u64;
        while skips < 200 {
            steps += 1;
            assert!(steps < 5_000_000, "only {skips} skips");
            let before = skipped.wait_skip_instructions();
            skipped
                .run_one_step_skipping(&mut skipped_idle, usize::MAX)
                .unwrap();
            while executed.retired_instructions() < skipped.retired_instructions() {
                executed
                    .run_one_step_skipping(&mut executed_idle, usize::MAX)
                    .unwrap();
            }
            if skipped.wait_skip_instructions() == before {
                continue;
            }
            skips += 1;
            assert_eq!(
                skipped.retired_instructions(),
                executed.retired_instructions()
            );
            assert_eq!(
                skipped.bus().emulated_cck(),
                executed.bus().emulated_cck(),
                "skip {skips} ends at the same colour clock"
            );
            assert!(
                skipped.snapshot_blob().unwrap() == executed.snapshot_blob().unwrap(),
                "skip {skips} ends in the same machine state"
            );
        }
    }

    #[test]
    fn every_skip_on_a_68020_lands_where_execution_does() {
        every_skip_lands_where_execution_does(CpuModel::M68020, 4, 0);
    }

    #[test]
    fn every_skip_on_a_cached_68040_lands_where_execution_does() {
        every_skip_lands_where_execution_does(CpuModel::M68040, 7, 0x8000_8000);
    }

    /// Host time for the poll guest with the skip on and off. Ignored: a
    /// timing, not a check. `cargo test --profile ci --lib -- --ignored
    /// --nocapture wait_skip_host_cost`.
    #[test]
    #[ignore]
    fn wait_skip_host_cost() {
        for (name, model, clocks, cacr) in [
            ("68020 fast RAM", CpuModel::M68020, 4, 0),
            ("68040 cached", CpuModel::M68040, 7, 0x8000_8000),
        ] {
            let mut times = [0.0f64; 2];
            for (i, skip) in [false, true].into_iter().enumerate() {
                let mut emu = guest(model, clocks, cacr, skip);
                let started = std::time::Instant::now();
                while emu.bus().emulated_seconds() < 5.0 {
                    emu.step_frame().unwrap();
                }
                times[i] = started.elapsed().as_secs_f64();
            }
            eprintln!(
                "{name}: 5 s emulated in {:.3} s executed, {:.3} s skipped ({:.1}x)",
                times[0],
                times[1],
                times[0] / times[1]
            );
        }
    }

    #[test]
    fn the_kill_switch_executes_every_turn() {
        let mut emu = guest(CpuModel::M68020, 4, 0, false);
        for _ in 0..5 {
            emu.step_frame().unwrap();
        }
        assert_eq!(emu.wait_skip_instructions(), 0);
    }
}
