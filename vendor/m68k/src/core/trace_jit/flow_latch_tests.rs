//! A completed trace-disabled batch must not make a later instruction look
//! flow-changing to the precise timing or T0-tracing machinery.
use super::*;
use crate::core::memory::{AddressBus, LinearMemoryBus};
use crate::{BatchExit, CycleBatchControl, CycleBatchExit, NoOpHleHandler, StepResult};

const HEAD: u32 = 0x100;
const NEXT: u32 = 0x200;
const HANDLER: u32 = 0x300;
const STACK: u32 = 0x3800;

struct RestoreJit(Option<TraceJit>);

impl Drop for RestoreJit {
    fn drop(&mut self) {
        TRACE_JIT.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

fn fixture() -> (CpuCore, LinearMemoryBus) {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);
    cpu.set_sr(0x2700);
    cpu.set_a(7, STACK);
    cpu.pc = HEAD;
    let mut bus = LinearMemoryBus::new(0x4000);
    for (i, opcode) in [0x7001, 0x7202, 0x60fa].into_iter().enumerate() {
        bus.write_word(HEAD + 2 * i as u32, opcode);
    }
    bus.write_word(NEXT, 0x7e12); // MOVEQ #18,D7: neither flow nor synchronization.
    bus.write_long(9 * 4, HANDLER);
    bus.write_word(HANDLER, 0x7c34);
    (cpu, bus)
}

fn batch_that_leaves_flow(native: bool, watched: bool) -> (CpuCore, LinearMemoryBus, RestoreJit) {
    let (mut cpu, mut bus) = fixture();
    #[allow(unused_mut)] // Only the native-feature setup installs a compiled head.
    let mut jit = TraceJit::new();
    if native {
        #[cfg(all(feature = "jit", not(target_family = "wasm")))]
        {
            cpu.fm_ptr = bus.as_mut_slice().as_mut_ptr() as usize;
            cpu.fm_len = bus.as_slice().len() as u32;
            let ops = [HEAD, HEAD + 2, HEAD + 4]
                .into_iter()
                .map(|pc| decode_trace_op(&cpu, &mut bus, pc, CpuType::M68040).unwrap())
                .collect();
            let mut compiled = jit
                .compile_decoded_ops(&cpu, HEAD, CpuType::M68040, ops, Some(HEAD))
                .unwrap();
            assert!(compiled.native_loop);
            compiled.adaptive_branch = false;
            jit.slots[trace_cache_index(HEAD)] = TraceSlot::Compiled(compiled);
            cpu.fm_ptr = 0;
            cpu.fm_len = 0;
        }
        #[cfg(not(all(feature = "jit", not(target_family = "wasm"))))]
        panic!("native fixture requires jit");
    } else {
        cpu.pc = HEAD + 4; // One decoded BRA suffices; no hand-set latch.
    }
    let restore = RestoreJit(TRACE_JIT.with(|slot| slot.replace(Some(jit))));
    TRACE_JIT_HAS_CANDIDATES.store(true, Ordering::Relaxed);
    let budget = if native { 6 } else { 1 };
    let watches = if watched { &[HEAD][..] } else { &[] };
    let result = cpu.run_batch(&mut bus, budget, watches);
    assert_eq!(
        result.instructions,
        if native && watched { 3 } else { budget }
    );
    assert_eq!(
        result.exit,
        if watched {
            BatchExit::WatchedPc { pc: HEAD }
        } else {
            BatchExit::BudgetExhausted
        }
    );
    if native {
        assert!(
            cpu.cycles_remaining < i32::MAX / 2,
            "the installed native trace must execute: decoded batching does not charge cycles"
        );
    }
    assert_eq!(cpu.pc, HEAD);
    assert!(
        cpu.change_of_flow,
        "reproduce the natural original-tier latch"
    );
    (cpu, bus, restore)
}

#[derive(Clone, Copy, Debug)]
enum Resume {
    Step,
    HleStep,
    Execute,
    Cycles,
    BoundaryHook,
    Batch,
}

fn resume(cpu: &mut CpuCore, bus: &mut LinearMemoryBus, entry: Resume) -> Option<i32> {
    match entry {
        Resume::Step | Resume::HleStep => {
            let result = match entry {
                Resume::Step => cpu.step(bus),
                _ => cpu.step_with_hle_handler(bus, &mut NoOpHleHandler),
            };
            let StepResult::Ok { cycles } = result else {
                panic!("unexpected single-step result {result:?}");
            };
            Some(cycles)
        }
        Resume::Execute => Some(cpu.execute(bus, 1)),
        Resume::Cycles => {
            let result = cpu.run_for_cycles(bus, 1);
            assert_eq!(result.instructions, 1);
            assert_eq!(result.exit, CycleBatchExit::BudgetExhausted);
            Some(result.cycles)
        }
        Resume::BoundaryHook => {
            let mut hooks = 0;
            let result = cpu.run_for_cycles_with_boundary_hook(bus, 1, |_, _, _| {
                hooks += 1;
                CycleBatchControl::Return
            });
            assert_eq!(hooks, 1);
            assert_eq!(result.instructions, 1);
            assert_eq!(result.exit, CycleBatchExit::BoundaryRequested);
            Some(result.cycles)
        }
        Resume::Batch => {
            let result = cpu.run_batch(bus, 1, &[]);
            assert_eq!(result.instructions, 1);
            assert_eq!(result.exit, BatchExit::BudgetExhausted);
            None // This API deliberately does not report cycles.
        }
    }
}

fn check_resume_after_batch(native: bool) {
    for watched in [false, true] {
        for t0 in [false, true] {
            for entry in [
                Resume::Step,
                Resume::HleStep,
                Resume::Execute,
                Resume::Cycles,
                Resume::BoundaryHook,
                Resume::Batch,
            ] {
                let (mut cpu, mut bus, _restore) = batch_that_leaves_flow(native, watched);
                let unchanged_ram = bus.as_slice().to_vec();
                let mut registers = cpu.dar;
                registers[7] = 18;
                cpu.pc = NEXT;
                cpu.set_sr(if t0 { 0x6700 } else { 0x2700 });
                let cycles = resume(&mut cpu, &mut bus, entry);
                assert_eq!(
                    cpu.last_exception_vector, None,
                    "native={native} watched={watched} t0={t0} entry={entry:?}: old flow must not request vector 9"
                );
                assert_eq!((cpu.pc, cpu.ppc, cpu.ir), (NEXT + 2, NEXT, 0x7e12));
                assert_eq!(cpu.dar, registers);
                assert_eq!(cpu.get_sr(), if t0 { 0x6700 } else { 0x2700 });
                assert_eq!(bus.as_slice(), unchanged_ram, "no extra trace frame");
                if let Some(cycles) = cycles {
                    assert_eq!(cycles, 1, "{entry:?}: MOVEQ is not a pipeline refill");
                }
            }
        }
    }
}

#[test]
fn flow_latch_decoded_batch_does_not_contaminate_a_new_instruction() {
    check_resume_after_batch(false);
}

#[cfg(all(feature = "jit", not(target_family = "wasm")))]
#[test]
fn flow_latch_native_batch_does_not_contaminate_a_new_instruction() {
    check_resume_after_batch(true);
}

#[test]
fn flow_latch_current_sr_writes_still_trace_against_the_old_t0() {
    for words in [
        [0x46fc, 0x2700], // MOVE #$2700,SR
        [0x027c, 0xbfff], // ANDI #$bfff,SR
        [0x0a7c, 0x4000], // EORI #$4000,SR
        [0x007c, 0x0000], // ORI #0,SR: synchronizes even without changing T0.
    ] {
        for entry in [
            Resume::Step,
            Resume::HleStep,
            Resume::Execute,
            Resume::Cycles,
            Resume::Batch,
        ] {
            let (mut cpu, mut bus) = fixture();
            cpu.pc = NEXT;
            cpu.set_sr(0x6700);
            bus.write_word(NEXT, words[0]);
            bus.write_word(NEXT + 2, words[1]);
            let _ = resume(&mut cpu, &mut bus, entry);
            assert_eq!(cpu.last_exception_vector, Some(9), "{words:x?} {entry:?}");
            assert_eq!(cpu.pc, HANDLER);
            assert_eq!(cpu.a(7), STACK - 12);
            assert_eq!(bus.read_long(STACK - 10), NEXT + 4);
            assert_eq!(bus.read_long(STACK - 4), NEXT);
        }
    }
}

#[test]
fn flow_latch_rte_restores_trace_for_following_instructions_only() {
    for restored_sr in [0x6700, 0xa700] {
        let (mut cpu, mut bus) = fixture();
        cpu.pc = NEXT;
        bus.write_word(NEXT, 0x4e73); // RTE, with a format-0 supervisor frame.
        bus.write_word(STACK, restored_sr);
        bus.write_long(STACK + 2, NEXT + 0x10);
        bus.write_word(STACK + 6, 0);
        bus.write_word(NEXT + 0x10, 0x7e12);
        bus.write_word(NEXT + 0x12, 0x6002);
        assert!(matches!(cpu.step(&mut bus), StepResult::Ok { .. }));
        assert_eq!(cpu.last_exception_vector, None, "RTE must not trace itself");
        assert_eq!(cpu.get_sr(), restored_sr);
        assert_eq!(cpu.pc, NEXT + 0x10);
        assert!(matches!(cpu.step(&mut bus), StepResult::Ok { .. }));
        if restored_sr & 0x8000 != 0 {
            assert_eq!(
                cpu.last_exception_vector,
                Some(9),
                "restored T1 traces MOVEQ"
            );
        } else {
            assert_eq!(
                cpu.last_exception_vector, None,
                "restored T0 does not trace MOVEQ"
            );
            assert!(matches!(cpu.step(&mut bus), StepResult::Ok { .. }));
            assert_eq!(cpu.last_exception_vector, Some(9), "restored T0 traces BRA");
        }
    }
}

#[test]
fn flow_latch_zero_work_and_stopped_calls_do_not_start_an_instruction() {
    let (mut cpu, mut bus, _restore) = batch_that_leaves_flow(false, false);
    cpu.set_sr(0x6700);
    assert_eq!(cpu.run_batch(&mut bus, 0, &[]).instructions, 0);
    assert_eq!(cpu.run_for_cycles(&mut bus, 0).instructions, 0);
    assert_eq!(cpu.execute(&mut bus, 0), 0);
    assert!(cpu.change_of_flow);
    assert_eq!(cpu.pc, HEAD);
    cpu.stopped = 1;
    assert!(matches!(cpu.step(&mut bus), StepResult::Stopped));
    assert!(matches!(
        cpu.step_with_hle_handler(&mut bus, &mut NoOpHleHandler),
        StepResult::Stopped
    ));
    assert_eq!(cpu.run_batch(&mut bus, 1, &[]).instructions, 0);
    assert!(cpu.change_of_flow);
    assert_eq!(cpu.pc, HEAD);
    assert_eq!(cpu.last_exception_vector, None);
}

struct FaultOnReturnAddressPush {
    ram: LinearMemoryBus,
    fault_pending: bool,
}

impl AddressBus for FaultOnReturnAddressPush {
    fn read_byte(&mut self, address: u32) -> u8 {
        self.ram.read_byte(address)
    }
    fn read_word(&mut self, address: u32) -> u16 {
        self.ram.read_word(address)
    }
    fn read_long(&mut self, address: u32) -> u32 {
        self.ram.read_long(address)
    }
    fn write_byte(&mut self, address: u32, value: u8) {
        self.ram.write_byte(address, value);
    }
    fn write_word(&mut self, address: u32, value: u16) {
        self.ram.write_word(address, value);
    }
    fn write_long(&mut self, address: u32, value: u32) {
        self.ram.write_long(address, value);
    }
    fn try_write_long(
        &mut self,
        address: u32,
        value: u32,
    ) -> Result<(), crate::core::memory::BusFault> {
        if self.fault_pending && address == STACK - 4 {
            self.fault_pending = false;
            return Err(crate::core::memory::BusFault {
                kind: crate::core::memory::BusFaultKind::BusError,
                address,
            });
        }
        self.write_long(address, value);
        Ok(())
    }
}

#[test]
fn flow_latch_fault_wins_over_trace_and_cannot_trace_a_later_handler_instruction() {
    let (mut cpu, mut ram) = fixture();
    cpu.pc = NEXT;
    cpu.set_sr(0x6700);
    ram.write_word(NEXT, 0x6102); // BSR marks flow, then its return-address push faults.
    ram.write_long(2 * 4, HANDLER);
    ram.write_word(HANDLER, 0x7e12);
    let mut bus = FaultOnReturnAddressPush {
        ram,
        fault_pending: true,
    };
    assert!(matches!(cpu.step(&mut bus), StepResult::Ok { .. }));
    assert!(!bus.fault_pending, "the store must actually fault");
    assert_eq!(
        cpu.last_exception_vector,
        Some(2),
        "fault suppresses T0 trace"
    );
    assert_eq!(cpu.pc, HANDLER);
    assert_eq!(cpu.a(7), STACK - 60, "one 68040 access-fault frame");
    let fault_frame = bus.ram.as_slice().to_vec();
    let sp = cpu.a(7);
    cpu.set_sr(0x6700); // Host asks to trace future flow, not the faulted BSR.
    assert_eq!(cpu.step(&mut bus), StepResult::Ok { cycles: 1 });
    assert_eq!(cpu.last_exception_vector, Some(2), "no later vector 9");
    assert_eq!(cpu.pc, HANDLER + 2);
    assert_eq!(cpu.a(7), sp);
    assert_eq!(
        bus.ram.as_slice(),
        fault_frame,
        "preserve the original fault frame"
    );
}
