//! 68060 cycle-cost model: classification spot checks and scalar costs.
//! Pairing and branch-cache behavior get their own tests as those land.

use m68k::core::memory::AddressBus;
use m68k::core::timing_060::{info_060, OepClass};
use m68k::{CpuCore, CpuType, NoOpHleHandler, StepResult};

struct TestBus {
    memory: Vec<u8>,
}

impl TestBus {
    fn new() -> Self {
        Self {
            memory: vec![0; 0x10000],
        }
    }

    fn write_word_at(&mut self, addr: u32, value: u16) {
        let bytes = value.to_be_bytes();
        self.memory[addr as usize] = bytes[0];
        self.memory[addr as usize + 1] = bytes[1];
    }

    fn write_long_at(&mut self, addr: u32, value: u32) {
        let bytes = value.to_be_bytes();
        self.memory[addr as usize..addr as usize + 4].copy_from_slice(&bytes);
    }
}

impl AddressBus for TestBus {
    fn read_byte(&mut self, address: u32) -> u8 {
        self.memory[(address as usize) & 0xFFFF]
    }

    fn read_word(&mut self, address: u32) -> u16 {
        let addr = (address as usize) & 0xFFFF;
        u16::from_be_bytes([self.memory[addr], self.memory[addr + 1]])
    }

    fn read_long(&mut self, address: u32) -> u32 {
        let addr = (address as usize) & 0xFFFF;
        u32::from_be_bytes([
            self.memory[addr],
            self.memory[addr + 1],
            self.memory[addr + 2],
            self.memory[addr + 3],
        ])
    }

    fn write_byte(&mut self, address: u32, value: u8) {
        self.memory[(address as usize) & 0xFFFF] = value;
    }

    fn write_word(&mut self, address: u32, value: u16) {
        let addr = (address as usize) & 0xFFFF;
        let bytes = value.to_be_bytes();
        self.memory[addr] = bytes[0];
        self.memory[addr + 1] = bytes[1];
    }

    fn write_long(&mut self, address: u32, value: u32) {
        let addr = (address as usize) & 0xFFFF;
        let bytes = value.to_be_bytes();
        self.memory[addr..addr + 4].copy_from_slice(&bytes);
    }
}

fn setup(cpu_type: CpuType) -> (CpuCore, TestBus) {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(cpu_type);
    let mut bus = TestBus::new();
    bus.write_long_at(0x00, 0x1000);
    bus.write_long_at(0x04, 0x0200);
    cpu.reset(&mut bus);
    cpu.pc = 0x0200;
    cpu.set_sr(0x2700);
    (cpu, bus)
}

fn step_cycles(cpu: &mut CpuCore, bus: &mut TestBus) -> i32 {
    let mut hle = NoOpHleHandler;
    match cpu.step_with_hle_handler(bus, &mut hle) {
        StepResult::Ok { cycles } => cycles,
        other => panic!("unexpected step result: {other:?}"),
    }
}

#[test]
fn classification_spot_checks() {
    // MOVEQ #0,D0: the canonical 1-cycle pOEP|sOEP instruction.
    assert_eq!(info_060(0x7000).class(), OepClass::PoepSoep);
    assert_eq!(info_060(0x7000).cycles(), 1);
    // MULU.W D0,D0: pOEP-only.
    assert_eq!(info_060(0xC0C0).class(), OepClass::PoepOnly);
    // MOVEM.L D0-A6,-(A7): pOEP-until-last.
    assert_eq!(info_060(0x48E7).class(), OepClass::PoepUntilLast);
    // ADD.L D1,D0 (0xD081): pOEP|sOEP.
    assert_eq!(info_060(0xD081).class(), OepClass::PoepSoep);
    // LSL.L #1,D0 (0xE388): register shifts pair.
    assert_eq!(info_060(0xE388).class(), OepClass::PoepSoep);
    // ROXL.L #1,D0 (0xE390): consumes X, pOEP-only.
    assert_eq!(info_060(0xE390).class(), OepClass::PoepOnly);
}

#[test]
fn scalar_costs_one_cycle_alu_on_68060() {
    let (mut cpu, mut bus) = setup(CpuType::M68060);
    bus.write_word_at(0x0200, 0x7000); // MOVEQ #0,D0
    bus.write_word_at(0x0202, 0xD081); // ADD.L D1,D0
    bus.write_word_at(0x0204, 0x4E71); // NOP
    assert_eq!(step_cycles(&mut cpu, &mut bus), 1, "MOVEQ is 1 cycle");
    assert_eq!(step_cycles(&mut cpu, &mut bus), 1, "ADD.L Dn,Dn is 1 cycle");
    assert_eq!(step_cycles(&mut cpu, &mut bus), 1, "NOP is 1 cycle");
}

#[test]
fn branch_costs_taken_vs_not_taken_on_68060() {
    let (mut cpu, mut bus) = setup(CpuType::M68060);
    // BEQ.S +2 with Z clear: not taken.
    bus.write_word_at(0x0200, 0x6702);
    // BRA.S back to 0x0200: taken.
    bus.write_word_at(0x0202, 0x60FC);
    cpu.set_sr(0x2700); // Z clear
    assert_eq!(step_cycles(&mut cpu, &mut bus), 1, "not-taken Bcc is 1 cycle");
    assert_eq!(
        step_cycles(&mut cpu, &mut bus),
        7,
        "taken branch without branch cache pays the refill"
    );
    assert_eq!(cpu.pc, 0x0200);
}

#[test]
fn dbcc_loop_cost_on_68060() {
    let (mut cpu, mut bus) = setup(CpuType::M68060);
    // DBF D0,-2 (loop on itself while D0 >= 0).
    bus.write_word_at(0x0200, 0x51C8);
    bus.write_word_at(0x0202, 0xFFFE);
    cpu.dar[0] = 1;
    assert_eq!(step_cycles(&mut cpu, &mut bus), 2, "looping DBcc");
    assert_eq!(cpu.pc, 0x0200);
    assert_eq!(step_cycles(&mut cpu, &mut bus), 3, "expired DBcc");
    assert_eq!(cpu.pc, 0x0204);
}

#[test]
fn flow_change_pays_refill_floor_on_68060() {
    let (mut cpu, mut bus) = setup(CpuType::M68060);
    bus.write_word_at(0x0200, 0x4ED0); // JMP (A0)
    cpu.dar[8] = 0x0300;
    let cycles = step_cycles(&mut cpu, &mut bus);
    assert!(cycles >= 5, "JMP must pay the refill floor, got {cycles}");
    assert_eq!(cpu.pc, 0x0300);
}

#[test]
fn other_models_keep_their_cycle_counts() {
    // Regression guard: the 060 cost model must not disturb 000-040 paths.
    for (cpu_type, moveq_expected) in [
        (CpuType::M68000, 4),
        (CpuType::M68030, 3), // ((4*5+7)/8).max(2)
    ] {
        let (mut cpu, mut bus) = setup(cpu_type);
        bus.write_word_at(0x0200, 0x7000);
        assert_eq!(
            step_cycles(&mut cpu, &mut bus),
            moveq_expected,
            "{cpu_type:?} MOVEQ cycles changed"
        );
    }
}

/// Enable the branch cache (CACR.EBC) via MOVEC.
fn enable_ebc(cpu: &mut CpuCore, bus: &mut TestBus, extra: u32) {
    let pc = cpu.pc;
    bus.write_word_at(0x0100, 0x4E7B); // MOVEC D0,CACR
    bus.write_word_at(0x0102, 0x0002);
    cpu.dar[0] = (1 << 23) | extra;
    cpu.pc = 0x0100;
    step_cycles(cpu, bus);
    cpu.pc = pc;
}

#[test]
fn branch_cache_folds_the_second_taken_branch() {
    let (mut cpu, mut bus) = setup(CpuType::M68060);
    enable_ebc(&mut cpu, &mut bus, 0);
    // BRA.S looping on itself.
    bus.write_word_at(0x0200, 0x60FE);
    assert_eq!(
        step_cycles(&mut cpu, &mut bus),
        7,
        "first taken branch misses and pays the refill"
    );
    assert_eq!(
        step_cycles(&mut cpu, &mut bus),
        0,
        "second execution is predicted and folded"
    );
    assert_eq!(step_cycles(&mut cpu, &mut bus), 0);
}

#[test]
fn branch_cache_cabc_strobe_clears_and_ebc_off_is_static() {
    let (mut cpu, mut bus) = setup(CpuType::M68060);
    enable_ebc(&mut cpu, &mut bus, 0);
    bus.write_word_at(0x0200, 0x60FE); // BRA.S self
    step_cycles(&mut cpu, &mut bus);
    assert_eq!(step_cycles(&mut cpu, &mut bus), 0, "folded before the clear");

    enable_ebc(&mut cpu, &mut bus, 1 << 22); // EBC | CABC strobe
    assert_eq!(
        step_cycles(&mut cpu, &mut bus),
        7,
        "CABC cleared the entry: miss again"
    );

    // EBC off: static cost every time, no learning.
    let (mut cpu, mut bus) = setup(CpuType::M68060);
    bus.write_word_at(0x0200, 0x60FE);
    assert_eq!(step_cycles(&mut cpu, &mut bus), 7);
    assert_eq!(step_cycles(&mut cpu, &mut bus), 7, "EBC off never folds");
}

#[test]
fn branch_cache_counter_follows_alternating_condition() {
    let (mut cpu, mut bus) = setup(CpuType::M68060);
    enable_ebc(&mut cpu, &mut bus, 0);
    // BEQ.S +2: toggle Z each execution by re-running the same branch.
    bus.write_word_at(0x0200, 0x6702);
    // taken (Z set), miss -> mispredict cost, allocates weakly-taken.
    cpu.set_sr(0x2704);
    cpu.pc = 0x0200;
    assert_eq!(step_cycles(&mut cpu, &mut bus), 7, "taken miss");
    // not taken (Z clear), predicted taken -> mispredict; counter 2->1.
    cpu.set_sr(0x2700);
    cpu.pc = 0x0200;
    assert_eq!(step_cycles(&mut cpu, &mut bus), 7, "hit-wrong");
    // not taken again, counter 1 predicts not-taken -> 1 cycle.
    cpu.pc = 0x0200;
    assert_eq!(step_cycles(&mut cpu, &mut bus), 1, "correct not-taken");
    // taken, predicted not-taken -> mispredict; counter 0->1... then grows.
    cpu.set_sr(0x2704);
    cpu.pc = 0x0200;
    assert_eq!(step_cycles(&mut cpu, &mut bus), 7);
}

#[test]
fn branch_cache_cubc_clears_only_user_entries() {
    let (mut cpu, mut bus) = setup(CpuType::M68060);
    enable_ebc(&mut cpu, &mut bus, 0);
    // Allocate in supervisor mode.
    bus.write_word_at(0x0200, 0x60FE);
    step_cycles(&mut cpu, &mut bus);
    assert_eq!(step_cycles(&mut cpu, &mut bus), 0, "supervisor entry folded");
    // CUBC must not clear a supervisor entry.
    enable_ebc(&mut cpu, &mut bus, 1 << 21);
    assert_eq!(
        step_cycles(&mut cpu, &mut bus),
        0,
        "supervisor entry survives CUBC"
    );
}

#[test]
fn dbcc_loop_folds_with_branch_cache() {
    let (mut cpu, mut bus) = setup(CpuType::M68060);
    enable_ebc(&mut cpu, &mut bus, 0);
    bus.write_word_at(0x0200, 0x51C8); // DBF D0,-2 (self-loop)
    bus.write_word_at(0x0202, 0xFFFE);
    cpu.dar[0] = 3;
    assert_eq!(step_cycles(&mut cpu, &mut bus), 7, "first loop iteration misses");
    assert_eq!(step_cycles(&mut cpu, &mut bus), 0, "steady-state loop folds");
    assert_eq!(step_cycles(&mut cpu, &mut bus), 0);
    // Expiry: predicted taken but falls through -> mispredict.
    assert_eq!(step_cycles(&mut cpu, &mut bus), 7, "loop exit mispredicts");
}
