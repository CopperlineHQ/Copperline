use m68k::core::memory::AddressBus;
use m68k::{CpuCore, CpuType, NoOpHleHandler};

struct TestBus {
    memory: [u8; 0x10000],
}

impl TestBus {
    fn new() -> Self {
        Self { memory: [0; 0x10000] }
    }

    fn write_word_at(&mut self, addr: u32, value: u16) {
        let bytes = value.to_be_bytes();
        let idx = addr as usize;
        self.memory[idx] = bytes[0];
        self.memory[idx + 1] = bytes[1];
    }

    fn write_long_at(&mut self, addr: u32, value: u32) {
        let bytes = value.to_be_bytes();
        let idx = addr as usize;
        self.memory[idx] = bytes[0];
        self.memory[idx + 1] = bytes[1];
        self.memory[idx + 2] = bytes[2];
        self.memory[idx + 3] = bytes[3];
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
        self.memory[addr] = bytes[0];
        self.memory[addr + 1] = bytes[1];
        self.memory[addr + 2] = bytes[2];
        self.memory[addr + 3] = bytes[3];
    }
}

/// Run a single MOVEC (`opcode` = $4E7A read / $4E7B write, `ext` names the
/// control register) on `cpu_type` and return the PC afterwards. The illegal
/// instruction vector points at 0x0300, so a trapped MOVEC lands there.
fn pc_after_movec(cpu_type: CpuType, opcode: u16, ext: u16) -> u32 {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(cpu_type);
    let mut bus = TestBus::new();

    // Vectors: SSP=0x1000, PC=0x0200
    bus.write_long_at(0x00, 0x1000);
    bus.write_long_at(0x04, 0x0200);
    // Illegal instruction vector (vector 4) -> 0x0300
    bus.write_long_at(0x10, 0x0300);

    bus.write_word_at(0x0200, opcode);
    bus.write_word_at(0x0202, ext);

    cpu.reset(&mut bus);
    cpu.pc = 0x0200;
    cpu.set_sr(0x2700);

    let mut hle = NoOpHleHandler;
    let result = cpu.step_with_hle_handler(&mut bus, &mut hle);
    assert!(matches!(result, m68k::StepResult::Ok { .. }));
    cpu.pc
}

#[test]
fn test_movec_pcr_is_illegal_on_68040() {
    // The 680x0.library CPU detection probes the 68060-only PCR ($808)
    // and relies on the 040 raising an illegal instruction exception.
    assert_eq!(
        pc_after_movec(CpuType::M68040, 0x4E7A, 0x0808),
        0x0300,
        "MOVEC PCR,D0 on 68040 should trap to illegal instruction vector"
    );
    assert_eq!(
        pc_after_movec(CpuType::M68040, 0x4E7B, 0x0808),
        0x0300,
        "MOVEC D0,PCR on 68040 should trap to illegal instruction vector"
    );
}

#[test]
fn test_movec_caar_is_illegal_on_68040() {
    // CAAR ($802) exists on the 020/030 only.
    assert_eq!(
        pc_after_movec(CpuType::M68040, 0x4E7A, 0x0802),
        0x0300,
        "MOVEC CAAR,D0 on 68040 should trap to illegal instruction vector"
    );
}

#[test]
fn test_movec_undefined_code_is_illegal_on_68040() {
    assert_eq!(
        pc_after_movec(CpuType::M68040, 0x4E7A, 0x0FFF),
        0x0300,
        "MOVEC of an undefined control register should trap"
    );
}

#[test]
fn test_movec_implemented_registers_execute_on_68040() {
    // TC ($003), VBR ($801), and SRP ($807) are all real 040 registers:
    // the instruction retires and the PC moves past the extension word.
    for ext in [0x0003u16, 0x0801, 0x0807] {
        assert_eq!(
            pc_after_movec(CpuType::M68040, 0x4E7A, ext),
            0x0204,
            "MOVEC of implemented register ${ext:03X} should execute on 68040"
        );
    }
}

#[test]
fn test_movec_mmu_registers_are_illegal_on_68ec040() {
    // The EC040 has no MMU: TC ($003) and SRP ($807) must trap.
    for ext in [0x0003u16, 0x0807] {
        assert_eq!(
            pc_after_movec(CpuType::M68EC040, 0x4E7A, ext),
            0x0300,
            "MOVEC of MMU register ${ext:03X} should trap on 68EC040"
        );
    }
}
