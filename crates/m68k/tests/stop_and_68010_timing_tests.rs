//! STOP state semantics and 68010 instruction-cycle calibration.
//!
//! Cycle counts are the vAmigaTS CPU/Timing + CPU/Timing2 measured values
//! (Moira's cycle-exact 68010 path, which matches A500+68010 photos):
//! MOVES per-EA-mode totals, MOVE from CCR, format-0 RTE, and the
//! interrupt dispatch (44 clocks on the 68000, 46 on the 68010).
//! STOP: an incoming SR with S clear stops only momentarily -- the stopped
//! state's supervisor check raises a privilege violation that stacks the
//! STOP instruction itself; a pending trace has priority and recovers from
//! the stop; a trace bit LOADED by STOP does not fire while stopped.

mod common;

use common::TestBus;
use m68k::core::types::CpuType;
use m68k::{AddressBus, CpuCore, StepResult};

fn setup(prog: &[u16], cpu_type: CpuType) -> (CpuCore, TestBus) {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(cpu_type);
    let mut bus = TestBus::new();
    let mut bytes = Vec::new();
    for w in prog {
        bytes.extend_from_slice(&w.to_be_bytes());
    }
    bus.load_rom(&bytes);
    bus.setup_boot();
    cpu.reset(&mut bus);
    cpu.pc = 0x10000;
    cpu.set_sr(0x2700);
    (cpu, bus)
}

fn step(cpu: &mut CpuCore, bus: &mut TestBus) -> StepResult {
    let mut hle = m68k::NoOpHleHandler;
    cpu.step_with_hle_handler(bus, &mut hle)
}

fn step_cycles(cpu: &mut CpuCore, bus: &mut TestBus) -> i32 {
    match step(cpu, bus) {
        StepResult::Ok { cycles } => cycles,
        other => panic!("unexpected step result: {:?}", other),
    }
}

// ========== 68010 MOVES cycle totals ==========

/// One MOVES case: opcode words, register/EA setup, expected 68010 cycles.
fn run_moves(prog: &[u16], expected: i32, what: &str) {
    let (mut cpu, mut bus) = setup(prog, CpuType::M68010);
    cpu.dar[8 + 4] = 0x300010; // A4 (also predecrement-safe)
    cpu.dar[5] = 0; // D5 index
    let cycles = step_cycles(&mut cpu, &mut bus);
    assert_eq!(cycles, expected, "{what}");
}

#[test]
fn m68010_moves_memory_to_register_cycle_totals() {
    // moves.b <ea>,d0 (extension word 0x0000)
    run_moves(&[0x0E14, 0x0000], 18, "moves.b (a4),d0");
    run_moves(&[0x0E1C, 0x0000], 20, "moves.b (a4)+,d0");
    run_moves(&[0x0E24, 0x0000], 20, "moves.b -(a4),d0");
    run_moves(&[0x0E2C, 0x0000, 0x0006], 20, "moves.b 6(a4),d0");
    run_moves(&[0x0E34, 0x0000, 0x5006], 24, "moves.b 6(a4,d5),d0");
    run_moves(&[0x0E38, 0x0000, 0x8000], 20, "moves.b (xxx).w,d0");
    run_moves(&[0x0E39, 0x0000, 0x0030, 0x0000], 24, "moves.b (xxx).l,d0");
    // Long data cycles add 4.
    run_moves(&[0x0E94, 0x0000], 22, "moves.l (a4),d0");
    run_moves(&[0x0EB4, 0x0000, 0x5006], 28, "moves.l 6(a4,d5),d0");
}

#[test]
fn m68010_moves_register_to_memory_cycle_totals() {
    // moves.b d0,<ea> (extension word 0x0800): same totals as the read form.
    run_moves(&[0x0E14, 0x0800], 18, "moves.b d0,(a4)");
    run_moves(&[0x0E1C, 0x0800], 20, "moves.b d0,(a4)+");
    run_moves(&[0x0E24, 0x0800], 20, "moves.b d0,-(a4)");
    run_moves(&[0x0E2C, 0x0800, 0x0006], 20, "moves.b d0,6(a4)");
    run_moves(&[0x0E34, 0x0800, 0x5006], 24, "moves.b d0,6(a4,d5)");
    run_moves(&[0x0E94, 0x0800], 22, "moves.l d0,(a4)");
}

// ========== 68010 MOVE from CCR ==========

#[test]
fn m68010_move_from_ccr_cycle_totals() {
    // move.w ccr,d0: only the final prefetch (4 clocks).
    let (mut cpu, mut bus) = setup(&[0x42C0], CpuType::M68010);
    assert_eq!(step_cycles(&mut cpu, &mut bus), 4, "move.w ccr,d0");

    // move.w ccr,(a4): 2 internal + prefetch + write.
    let (mut cpu, mut bus) = setup(&[0x42D4], CpuType::M68010);
    cpu.dar[8 + 4] = 0x300010;
    assert_eq!(step_cycles(&mut cpu, &mut bus), 10, "move.w ccr,(a4)");
}

// ========== 68010 RTE (format 0) ==========

#[test]
fn m68010_rte_format0_takes_24_cycles() {
    let (mut cpu, mut bus) = setup(&[0x4E73], CpuType::M68010);
    // Build a format-0 frame: SR, PC, format/vector word.
    let sp = 0x0000_8000;
    cpu.dar[15] = sp;
    bus.write_word(sp, 0x2700);
    bus.write_long(sp + 2, 0x0001_0000);
    bus.write_word(sp + 6, 0x0064); // format 0, vector offset 0x64
    let cycles = step_cycles(&mut cpu, &mut bus);
    assert_eq!(cycles, 24, "68010 RTE (format 0)");
    assert_eq!(cpu.pc, 0x0001_0000);
    assert_eq!(cpu.dar[15], sp + 8);
}

// ========== Interrupt dispatch cycles ==========

fn dispatch_cycles(cpu_type: CpuType) -> i32 {
    let (mut cpu, mut bus) = setup(&[0x4E71, 0x4E71], cpu_type);
    bus.write_long(0x6C, 0x0001_0002); // level-3 autovector handler
    cpu.set_sr(0x2000); // supervisor, IPL mask 0
    cpu.dar[15] = 0x0000_8000;
    cpu.set_irq(3);
    cpu.execute(&mut bus, 0)
}

#[test]
fn interrupt_dispatch_takes_44_clocks_on_68000_and_46_on_68010() {
    assert_eq!(dispatch_cycles(CpuType::M68000), 44);
    assert_eq!(dispatch_cycles(CpuType::M68010), 46);
}

// ========== STOP semantics ==========

#[test]
fn stop_with_s_clear_loads_sr_verbatim_then_wakes_into_privilege() {
    for cpu_type in [CpuType::M68000, CpuType::M68010] {
        let (mut cpu, mut bus) = setup(&[0x4E72, 0x0014], cpu_type);
        bus.write_long(0x20, 0x0001_0010); // privilege-violation vector
        cpu.dar[15] = 0x0000_8000;
        // The STOP step itself loads the SR VERBATIM (S clear, flags as
        // written) and stops -- the SST m68000 single-step fixtures observe
        // exactly this state.
        let result = step(&mut cpu, &mut bus);
        assert!(matches!(result, StepResult::Ok { .. }), "{result:?}");
        assert!(cpu.is_stopped(), "{cpu_type:?}: stopped after the STOP step");
        assert_eq!(cpu.get_sr(), 0x0014, "{cpu_type:?}: SR verbatim");
        // The next instruction boundary runs the stopped state's supervisor
        // check: privilege violation, stacking the STOP itself so the
        // handler's RTE re-executes it.
        let result = step(&mut cpu, &mut bus);
        assert!(matches!(result, StepResult::Ok { .. }), "{result:?}");
        assert!(!cpu.is_stopped(), "{cpu_type:?}: must not stay stopped");
        assert_eq!(cpu.pc, 0x0001_0010, "{cpu_type:?}: privilege handler");
        assert!(cpu.is_supervisor());
        let stacked_pc = bus.read_long(cpu.dar[15] + 2);
        assert_eq!(stacked_pc, 0x0001_0000, "{cpu_type:?}: stacked PC");
    }
}

#[test]
fn stop_with_s_clear_takes_42_clocks_to_the_handler_on_68000() {
    let (mut cpu, mut bus) = setup(&[0x4E72, 0x0000], CpuType::M68000);
    bus.write_long(0x20, 0x0001_0010);
    cpu.dar[15] = 0x0000_8000;
    // STOP (4), then at the next boundary 4 internal clocks in the stopped
    // state + the privilege exception (34).
    let stop_cycles = step_cycles(&mut cpu, &mut bus);
    let wake_cycles = step_cycles(&mut cpu, &mut bus);
    assert_eq!(stop_cycles, 4);
    assert_eq!(wake_cycles, 38);
}

#[test]
fn stop_with_pending_trace_traces_instead_of_stopping() {
    let (mut cpu, mut bus) = setup(&[0x4E72, 0x2000], CpuType::M68000);
    bus.write_long(0x24, 0x0001_0020); // trace vector
    cpu.dar[15] = 0x0000_8000;
    cpu.set_sr(0xA700); // T1 set entering STOP
    let result = step(&mut cpu, &mut bus);
    assert!(matches!(result, StepResult::Ok { .. }), "{result:?}");
    assert!(!cpu.is_stopped(), "trace must recover from the stop state");
    assert_eq!(cpu.pc, 0x0001_0020, "trace handler");
    // The stacked PC is the instruction after STOP.
    let stacked_pc = bus.read_long(cpu.dar[15] + 2);
    assert_eq!(stacked_pc, 0x0001_0004);
}

#[test]
fn stop_with_pending_trace_and_s_clear_prefers_the_trace() {
    let (mut cpu, mut bus) = setup(&[0x4E72, 0x0000], CpuType::M68000);
    bus.write_long(0x20, 0x0001_0010); // privilege vector
    bus.write_long(0x24, 0x0001_0020); // trace vector
    cpu.dar[15] = 0x0000_8000;
    cpu.set_sr(0xA700);
    let _ = step(&mut cpu, &mut bus);
    assert!(!cpu.is_stopped());
    assert_eq!(cpu.pc, 0x0001_0020, "trace wins over the supervisor check");
}

#[test]
fn stop_loading_the_trace_bit_stays_stopped() {
    let (mut cpu, mut bus) = setup(&[0x4E72, 0xA000], CpuType::M68000);
    cpu.dar[15] = 0x0000_8000;
    let result = step(&mut cpu, &mut bus);
    assert!(matches!(result, StepResult::Ok { .. }), "{result:?}");
    assert!(cpu.is_stopped(), "T loaded BY the stop must not trace");
    assert!(matches!(step(&mut cpu, &mut bus), StepResult::Stopped));
}
