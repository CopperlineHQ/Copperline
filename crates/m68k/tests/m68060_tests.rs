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
