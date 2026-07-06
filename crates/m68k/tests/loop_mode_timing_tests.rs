//! 68010 loop-mode engagement and per-iteration timing.
//!
//! A DBcc that branches -4 back to a loopable one-word instruction enters
//! loop mode: the body/DBcc pair is held in the prefetch queue and re-executed
//! without instruction fetches until the condition turns true, the counter
//! expires, or an exception is taken.

mod common;

use common::TestBus;
use m68k::core::types::CpuType;
use m68k::{CpuCore, StepResult};

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

fn step(cpu: &mut CpuCore, bus: &mut TestBus) -> i32 {
    let mut hle = m68k::NoOpHleHandler;
    match cpu.step_with_hle_handler(bus, &mut hle) {
        StepResult::Ok { cycles } => cycles,
        other => panic!("unexpected step result: {:?}", other),
    }
}

// move.w (a4),(a5); dbra d4,-4; nop
const MOVE_LOOP: [u16; 4] = [0x3A94, 0x51CC, 0xFFFC, 0x4E71];

#[test]
fn m68010_dbra_move_enters_loop_mode() {
    let (mut cpu, mut bus) = setup(&MOVE_LOOP, CpuType::M68010);
    cpu.dar[4] = 3; // D4: 4 iterations
    cpu.dar[8 + 4] = 0x300000; // A4
    cpu.dar[8 + 5] = 0x300100; // A5
    bus.extra_ram.write_word(0, 0xBEEF);

    let mut log = Vec::new();
    for _ in 0..16 {
        let pc = cpu.pc;
        let cycles = step(&mut cpu, &mut bus);
        log.push((pc, cycles, cpu.loop_mode));
        if pc == 0x10006 {
            break;
        }
    }
    assert!(
        log.iter().any(|&(_, _, lm)| lm),
        "loop mode never engaged: {:x?}",
        log
    );
    // The copy ran to completion.
    assert_eq!(cpu.dar[4] as u16, 0xFFFF);
    assert_eq!(bus.extra_ram.read_word(0x100), 0xBEEF);
    // Looping DBcc iterations execute with no bus activity in 6 clocks.
    let looped: Vec<i32> = log
        .iter()
        .filter(|&&(pc, _, lm)| pc == 0x10002 && lm)
        .map(|&(_, c, _)| c)
        .collect();
    assert!(
        looped.iter().any(|&c| c == 6),
        "no 6-cycle looping DBcc seen: {:x?}",
        log
    );
}

#[test]
fn m68000_dbra_does_not_enter_loop_mode() {
    let (mut cpu, mut bus) = setup(&MOVE_LOOP, CpuType::M68000);
    cpu.dar[4] = 3;
    cpu.dar[8 + 4] = 0x300000;
    cpu.dar[8 + 5] = 0x300100;
    for _ in 0..16 {
        step(&mut cpu, &mut bus);
        assert!(!cpu.loop_mode);
        if cpu.pc == 0x10006 {
            break;
        }
    }
    assert_eq!(cpu.dar[4] as u16, 0xFFFF);
}

#[test]
fn m68010_loop_mode_not_entered_for_two_word_body() {
    // move.w 2(a4),(a5) has an extension word: not loopable.
    let prog: [u16; 5] = [0x3AAC, 0x0002, 0x51CC, 0xFFFA, 0x4E71];
    let (mut cpu, mut bus) = setup(&prog, CpuType::M68010);
    cpu.dar[4] = 3;
    cpu.dar[8 + 4] = 0x300000;
    cpu.dar[8 + 5] = 0x300100;
    for _ in 0..24 {
        step(&mut cpu, &mut bus);
        assert!(!cpu.loop_mode);
        if cpu.pc == 0x10008 {
            break;
        }
    }
    assert_eq!(cpu.dar[4] as u16, 0xFFFF);
}
