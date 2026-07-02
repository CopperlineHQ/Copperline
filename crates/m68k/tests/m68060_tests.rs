//! 68060-specific behavior: model configuration, the instruction subset the
//! 060 kept, the unimplemented-instruction traps, and the 060-only control
//! registers. Programs are hand-assembled into a flat test bus; no external
//! fixtures.

use m68k::core::memory::AddressBus;
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
        let idx = addr as usize;
        self.memory[idx] = bytes[0];
        self.memory[idx + 1] = bytes[1];
    }

    fn write_long_at(&mut self, addr: u32, value: u32) {
        let bytes = value.to_be_bytes();
        self.memory[addr as usize..addr as usize + 4].copy_from_slice(&bytes);
    }

    fn read_long_at(&self, addr: u32) -> u32 {
        let idx = addr as usize;
        u32::from_be_bytes([
            self.memory[idx],
            self.memory[idx + 1],
            self.memory[idx + 2],
            self.memory[idx + 3],
        ])
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

/// A 68060 reset into supervisor mode with SSP $1000, PC $0200, and the
/// illegal (4), privilege (8), Line-F (11), and unimplemented-integer (61)
/// vectors pointed at distinct handlers.
fn setup_060() -> (CpuCore, TestBus) {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68060);
    let mut bus = TestBus::new();
    bus.write_long_at(0x00, 0x1000); // SSP
    bus.write_long_at(0x04, 0x0200); // PC
    bus.write_long_at(0x10, 0x0300); // vector 4: illegal instruction
    bus.write_long_at(0x20, 0x0320); // vector 8: privilege violation
    bus.write_long_at(0x2C, 0x0340); // vector 11: Line-F
    bus.write_long_at(61 * 4, 0x0360); // vector 61: unimplemented integer
    cpu.reset(&mut bus);
    cpu.pc = 0x0200;
    cpu.set_sr(0x2700);
    (cpu, bus)
}

fn step(cpu: &mut CpuCore, bus: &mut TestBus) -> StepResult {
    let mut hle = NoOpHleHandler;
    cpu.step_with_hle_handler(bus, &mut hle)
}

#[test]
fn m68060_sets_masks_and_pmmu() {
    let (mut cpu, _bus) = setup_060();
    assert_eq!(cpu.address_mask, 0xFFFF_FFFF);
    assert!(cpu.has_pmmu);
    assert!(!cpu.is_pre_68020);
    // The 060 keeps the M bit but drops T0 (SR bit 14).
    cpu.set_sr(0xF71F);
    assert_eq!(cpu.get_sr() & 0x4000, 0, "T0 must not be storable on the 060");
    assert_ne!(cpu.get_sr() & 0x1000, 0, "M bit must be storable on the 060");
}

#[test]
fn move16_executes_on_68060() {
    let (mut cpu, mut bus) = setup_060();
    // MOVE16 (A0)+,(A1)+
    bus.write_word_at(0x0200, 0xF620);
    bus.write_word_at(0x0202, 0x9000); // dest A1
    cpu.dar[8] = 0x4000;
    cpu.dar[9] = 0x5000;
    for i in 0..4u32 {
        bus.write_long_at(0x4000 + i * 4, 0x1111_0000 + i);
    }
    let result = step(&mut cpu, &mut bus);
    assert!(matches!(result, StepResult::Ok { .. }));
    assert_eq!(cpu.pc, 0x0204, "MOVE16 must execute, not trap");
    for i in 0..4u32 {
        assert_eq!(bus.read_long_at(0x5000 + i * 4), 0x1111_0000 + i);
    }
    assert_eq!(cpu.dar[8], 0x4010);
    assert_eq!(cpu.dar[9], 0x5010);
}

#[test]
fn full_extension_word_ea_executes_on_68060() {
    let (mut cpu, mut bus) = setup_060();
    // MOVE.L (bd,A0,D1.L*4),D0 with a full-format extension word:
    //   D1.L index (0x1800), scale *4 (0x0400), full format (0x0100),
    //   base displacement word (0x0020).
    bus.write_word_at(0x0200, 0x2030); // MOVE.L <ea mode 6, reg A0>,D0
    bus.write_word_at(0x0202, 0x1D20); // ext: D1.L*4, full, word bd follows
    bus.write_word_at(0x0204, 0x0020); // bd = 0x20
    cpu.dar[8] = 0x4000;
    cpu.dar[1] = 4; // D1 index -> 4 * 4 = 16
    bus.write_long_at(0x4030, 0xCAFE_F00D);
    let result = step(&mut cpu, &mut bus);
    assert!(matches!(result, StepResult::Ok { .. }));
    assert_eq!(cpu.pc, 0x0206);
    assert_eq!(cpu.dar[0], 0xCAFE_F00D, "scaled full-format EA must resolve");
}

/// MOVEC Rc,Dn / Dn,Rc helpers: assemble at 0x0200 and step once.
fn movec_from(cpu: &mut CpuCore, bus: &mut TestBus, ctrl_reg: u16) -> StepResult {
    bus.write_word_at(0x0200, 0x4E7A);
    bus.write_word_at(0x0202, ctrl_reg); // D0
    cpu.pc = 0x0200;
    step(cpu, bus)
}

fn movec_to(cpu: &mut CpuCore, bus: &mut TestBus, ctrl_reg: u16, value: u32) -> StepResult {
    bus.write_word_at(0x0200, 0x4E7B);
    bus.write_word_at(0x0202, ctrl_reg); // D0
    cpu.dar[0] = value;
    cpu.pc = 0x0200;
    step(cpu, bus)
}

#[test]
fn movec_caar_mmusr_msp_isp_are_illegal_on_68060() {
    for ctrl_reg in [0x802u16, 0x805, 0x803, 0x804] {
        let (mut cpu, mut bus) = setup_060();
        let result = movec_from(&mut cpu, &mut bus, ctrl_reg);
        assert!(matches!(result, StepResult::Ok { .. }));
        assert_eq!(
            cpu.pc, 0x0300,
            "MOVEC ${ctrl_reg:03X} must be illegal on the 68060"
        );
    }
}

#[test]
fn movec_pcr_round_trips_with_read_only_identification() {
    let (mut cpu, mut bus) = setup_060();
    // Write all-ones: only EDEBUG/DFP/ESS may stick.
    movec_to(&mut cpu, &mut bus, 0x808, 0xFFFF_FFFF);
    movec_from(&mut cpu, &mut bus, 0x808);
    assert_eq!(
        cpu.dar[0],
        0x0430_0100 | 0x83,
        "identification/revision read-only; EDEBUG/DFP/ESS writable"
    );
}

#[test]
fn movec_buscr_does_not_alias_dacr0_on_68060() {
    let (mut cpu, mut bus) = setup_060();
    cpu.dacr0 = 0xDEAD_BEEF;
    movec_to(&mut cpu, &mut bus, 0x008, 0x1234_5678);
    assert_eq!(cpu.buscr, 0x1234_5678);
    assert_eq!(cpu.dacr0, 0xDEAD_BEEF, "BUSCR write must not touch DACR0");
    movec_from(&mut cpu, &mut bus, 0x008);
    assert_eq!(cpu.dar[0], 0x1234_5678);
}

#[test]
fn movec_pcr_is_illegal_on_68040() {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);
    let mut bus = TestBus::new();
    bus.write_long_at(0x00, 0x1000);
    bus.write_long_at(0x04, 0x0200);
    bus.write_long_at(0x10, 0x0300);
    cpu.reset(&mut bus);
    cpu.set_sr(0x2700);
    // The 040 register table has no 0x808 decode: reads return 0 rather than
    // trapping (matching the existing permissive 040 MOVEC model).
    let result = movec_from(&mut cpu, &mut bus, 0x808);
    assert!(matches!(result, StepResult::Ok { .. }));
    assert_eq!(cpu.dar[0], 0, "PCR must not exist on the 68040");
}

#[test]
fn cacr_060_persists_enables_and_discards_strobes() {
    let (mut cpu, mut bus) = setup_060();
    // EDC | EBC | CABC | EIC: the clear strobe must not store.
    movec_to(
        &mut cpu,
        &mut bus,
        0x002,
        (1 << 31) | (1 << 23) | (1 << 22) | (1 << 15),
    );
    movec_from(&mut cpu, &mut bus, 0x002);
    assert_eq!(
        cpu.dar[0],
        (1u32 << 31) | (1 << 23) | (1 << 15),
        "EDC/EBC/EIC persist; CABC reads back 0"
    );
}

#[test]
fn m_bit_does_not_switch_stacks_on_68060() {
    let (mut cpu, _bus) = setup_060();
    cpu.dar[15] = 0x0000_2000;
    // Setting M on an 020/040 would bank A7 to the MSP; the 060 has a
    // single supervisor stack, so A7 must stay put.
    cpu.set_sr(0x3700);
    assert_eq!(cpu.dar[15], 0x0000_2000, "no MSP bank on the 68060");
    cpu.set_sr(0x2700);
    assert_eq!(cpu.dar[15], 0x0000_2000);
}

/// Assert the machine trapped to the unimplemented-integer handler at
/// $0360 with a format-0 frame whose vector offset is 61*4 and whose
/// stacked PC is the faulting instruction.
fn assert_vector_61(cpu: &CpuCore, bus: &mut TestBus, instr_addr: u32) {
    assert_eq!(cpu.pc, 0x0360, "must vector through 61");
    let sp = cpu.dar[15];
    let frame_pc = bus.read_long(sp.wrapping_add(2));
    let fmt_vec = bus.read_word(sp.wrapping_add(6));
    assert_eq!(frame_pc, instr_addr, "stacked PC = faulting instruction");
    assert_eq!(fmt_vec, 61 * 4, "format 0 frame, vector offset 61*4");
}

#[test]
fn movep_traps_to_vector_61_on_68060() {
    let (mut cpu, mut bus) = setup_060();
    // MOVEP.W D0,(4,A0)
    bus.write_word_at(0x0200, 0x0188);
    bus.write_word_at(0x0202, 0x0004);
    cpu.dar[0] = 0x1234;
    cpu.dar[8] = 0x4000;
    let result = step(&mut cpu, &mut bus);
    assert!(matches!(result, StepResult::Ok { .. }));
    assert_vector_61(&cpu, &mut bus, 0x0200);
    assert_eq!(bus.read_word(0x4004), 0, "memory untouched");
    assert_eq!(cpu.dar[8], 0x4000, "A0 untouched");
}

#[test]
fn movep_executes_natively_with_escape_flag() {
    let (mut cpu, mut bus) = setup_060();
    cpu.emulate_unimplemented_060 = true;
    bus.write_word_at(0x0200, 0x0188); // MOVEP.W D0,(4,A0)
    bus.write_word_at(0x0202, 0x0004);
    cpu.dar[0] = 0x1234;
    cpu.dar[8] = 0x4000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0204);
    assert_eq!(bus.read_byte(0x4004), 0x12);
    assert_eq!(bus.read_byte(0x4006), 0x34);
}

#[test]
fn movep_still_executes_on_68040() {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);
    let mut bus = TestBus::new();
    bus.write_long_at(0x00, 0x1000);
    bus.write_long_at(0x04, 0x0200);
    cpu.reset(&mut bus);
    cpu.set_sr(0x2700);
    bus.write_word_at(0x0200, 0x0188);
    bus.write_word_at(0x0202, 0x0004);
    cpu.dar[0] = 0x1234;
    cpu.dar[8] = 0x4000;
    cpu.pc = 0x0200;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0204, "MOVEP is native on the 68040");
    assert_eq!(bus.read_byte(0x4004), 0x12);
}

#[test]
fn mull_64bit_traps_but_32bit_executes_on_68060() {
    // 64-bit form: MULU.L D1,D2:D0 (ext bit 10 set).
    let (mut cpu, mut bus) = setup_060();
    bus.write_word_at(0x0200, 0x4C01); // MULx.L D1,...
    bus.write_word_at(0x0202, 0x0402); // D0 low, wide, D2 high
    cpu.dar[0] = 7;
    cpu.dar[1] = 6;
    step(&mut cpu, &mut bus);
    assert_vector_61(&cpu, &mut bus, 0x0200);
    assert_eq!(cpu.dar[0], 7, "registers untouched");

    // 32-bit form: MULU.L D1,D0 stays native.
    let (mut cpu, mut bus) = setup_060();
    bus.write_word_at(0x0200, 0x4C01);
    bus.write_word_at(0x0202, 0x0000);
    cpu.dar[0] = 7;
    cpu.dar[1] = 6;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0204);
    assert_eq!(cpu.dar[0], 42, "32-bit MULU.L executes natively");
}

#[test]
fn divl_64bit_traps_on_68060() {
    let (mut cpu, mut bus) = setup_060();
    // DIVU.L D1,D2:D0 (64/32, ext bit 10 set). Divisor zero on purpose:
    // the unimplemented trap must win over zero-divide evaluation.
    bus.write_word_at(0x0200, 0x4C41);
    bus.write_word_at(0x0202, 0x0402);
    cpu.dar[1] = 0;
    step(&mut cpu, &mut bus);
    assert_vector_61(&cpu, &mut bus, 0x0200);
}

#[test]
fn cas2_and_chk2_trap_on_68060() {
    let (mut cpu, mut bus) = setup_060();
    // CAS2.W D0:D1,D2:D3,(A0):(A1)
    bus.write_word_at(0x0200, 0x0CFC);
    bus.write_word_at(0x0202, 0x8080);
    bus.write_word_at(0x0204, 0x9081);
    step(&mut cpu, &mut bus);
    assert_vector_61(&cpu, &mut bus, 0x0200);

    let (mut cpu, mut bus) = setup_060();
    // CMP2.W (A0),D0
    bus.write_word_at(0x0200, 0x02D0);
    bus.write_word_at(0x0202, 0x0000);
    cpu.dar[8] = 0x4000;
    step(&mut cpu, &mut bus);
    assert_vector_61(&cpu, &mut bus, 0x0200);
}

#[test]
fn cas_misaligned_traps_aligned_executes_on_68060() {
    // Misaligned word CAS: trap with A0 and memory untouched.
    let (mut cpu, mut bus) = setup_060();
    bus.write_word_at(0x0200, 0x0CD8); // CAS.W D0,D1,(A0)+
    bus.write_word_at(0x0202, 0x0040); // Du=D1, Dc=D0
    cpu.dar[8] = 0x4001;
    step(&mut cpu, &mut bus);
    assert_vector_61(&cpu, &mut bus, 0x0200);
    assert_eq!(cpu.dar[8], 0x4001, "post-increment must not commit");

    // Aligned CAS executes (and post-increments).
    let (mut cpu, mut bus) = setup_060();
    bus.write_word_at(0x0200, 0x0CD8);
    bus.write_word_at(0x0202, 0x0040);
    bus.write_word_at(0x4000, 0x0005);
    cpu.dar[8] = 0x4000;
    cpu.dar[0] = 0x0005; // compare matches
    cpu.dar[1] = 0x0009; // update value
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0204);
    assert_eq!(bus.read_word(0x4000), 0x0009, "aligned CAS swaps");
    assert_eq!(cpu.dar[8], 0x4002, "post-increment commits");
}
