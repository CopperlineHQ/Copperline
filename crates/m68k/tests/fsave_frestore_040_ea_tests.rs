//! 68040 FSAVE/FRESTORE effective-address coverage.
//!
//! FSAVE takes any control-alterable EA plus -(An); FRESTORE takes any
//! control EA plus (An)+, PC-relative included (the frame is a source
//! operand). Linux/m68k's Amiga bootstrap parks and resets the FPU with
//! FSAVE/FRESTORE d16(sp) on its way into the kernel, so a core that only
//! decodes the (An)+/-(An) forms Line-Fs into whatever the bootstrap left
//! at vector 11 and never reaches the kernel entry point.

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
        self.write_word_at(address & 0xFFFF, value);
    }

    fn write_long(&mut self, address: u32, value: u32) {
        self.write_long_at(address & 0xFFFF, value);
    }
}

fn setup_040() -> (CpuCore, TestBus) {
    let bus = TestBus::new();
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);
    cpu.set_sr(0x2700);
    cpu.dar[15] = 0x5000;
    cpu.pc = 0x0200;
    (cpu, bus)
}

fn step(cpu: &mut CpuCore, bus: &mut TestBus) -> StepResult {
    let mut hle = NoOpHleHandler;
    cpu.step_with_hle_handler(bus, &mut hle)
}

/// FRESTORE d16(A7) of a one-long NULL frame resets the FPU and continues
/// (the Linux bootstrap's exact form, opcode $F36F).
#[test]
fn frestore_d16_sp_null_frame_resets_the_fpu() {
    let (mut cpu, mut bus) = setup_040();
    cpu.fpcr = 0x1234;
    bus.write_long_at(0x5004, 0); // NULL frame at 4(sp)
    bus.write_word_at(0x0200, 0xF36F); // FRESTORE ($4,A7)
    bus.write_word_at(0x0202, 0x0004);

    let r = step(&mut cpu, &mut bus);
    assert!(matches!(r, StepResult::Ok { .. }), "no Line-F: {r:?}");
    assert_eq!(cpu.pc, 0x0204, "extension word consumed");
    assert_eq!(cpu.fpcr, 0, "NULL frame resets the FPU");
    assert_eq!(cpu.dar[15], 0x5000, "control mode leaves A7 alone");
}

/// FSAVE d16(A7) after reset writes the one-long NULL frame at the
/// resolved address without touching A7.
#[test]
fn fsave_d16_sp_writes_null_frame_in_place() {
    let (mut cpu, mut bus) = setup_040();
    bus.write_long_at(0x5008, 0xDEAD_BEEF);
    bus.write_word_at(0x0200, 0xF32F); // FSAVE ($8,A7)
    bus.write_word_at(0x0202, 0x0008);

    let r = step(&mut cpu, &mut bus);
    assert!(matches!(r, StepResult::Ok { .. }), "no Line-F: {r:?}");
    assert_eq!(cpu.pc, 0x0204, "extension word consumed");
    assert_eq!(bus.read_long_at(0x5008), 0, "NULL frame written in place");
    assert_eq!(cpu.dar[15], 0x5000, "control mode leaves A7 alone");
}

/// FRESTORE (xxx).L and FSAVE (An) round-trip: the remaining control
/// modes decode instead of Line-F-ing.
#[test]
fn fsave_indirect_and_frestore_absolute_decode() {
    let (mut cpu, mut bus) = setup_040();
    cpu.dar[8] = 0x6000; // A0
    bus.write_word_at(0x0200, 0xF310); // FSAVE (A0)
    bus.write_word_at(0x0202, 0xF379); // FRESTORE ($00006000).L
    bus.write_long_at(0x0204, 0x0000_6000);

    let r = step(&mut cpu, &mut bus);
    assert!(matches!(r, StepResult::Ok { .. }), "FSAVE (A0): {r:?}");
    assert_eq!(bus.read_long_at(0x6000), 0, "NULL frame at (A0)");

    let r = step(&mut cpu, &mut bus);
    assert!(matches!(r, StepResult::Ok { .. }), "FRESTORE abs.L: {r:?}");
    assert_eq!(cpu.pc, 0x0208);
    assert_eq!(cpu.dar[8], 0x6000, "A0 untouched");
}
