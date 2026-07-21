//! Integration tests for MMU fault handling.
//!
//! Tests transparent translation, exception processing bypass, and double-fault detection.

use m68k::StepResult;
use m68k::core::cpu::CpuCore;
use m68k::core::memory::AddressBus;
use m68k::core::types::CpuType;

/// Simple test bus that stores memory in a vector.
struct TestBus {
    mem: Vec<u8>,
}

impl TestBus {
    fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    fn write_long(&mut self, addr: u32, val: u32) {
        let addr = addr as usize;
        if addr + 3 < self.mem.len() {
            self.mem[addr] = (val >> 24) as u8;
            self.mem[addr + 1] = (val >> 16) as u8;
            self.mem[addr + 2] = (val >> 8) as u8;
            self.mem[addr + 3] = val as u8;
        }
    }
}

impl AddressBus for TestBus {
    fn read_byte(&mut self, addr: u32) -> u8 {
        self.mem.get(addr as usize).copied().unwrap_or(0)
    }

    fn write_byte(&mut self, addr: u32, val: u8) {
        if let Some(m) = self.mem.get_mut(addr as usize) {
            *m = val;
        }
    }

    fn read_word(&mut self, addr: u32) -> u16 {
        let hi = self.read_byte(addr) as u16;
        let lo = self.read_byte(addr + 1) as u16;
        (hi << 8) | lo
    }

    fn write_word(&mut self, addr: u32, val: u16) {
        self.write_byte(addr, (val >> 8) as u8);
        self.write_byte(addr + 1, val as u8);
    }

    fn read_long(&mut self, addr: u32) -> u32 {
        let hi = self.read_word(addr) as u32;
        let lo = self.read_word(addr + 2) as u32;
        (hi << 16) | lo
    }

    fn write_long(&mut self, addr: u32, val: u32) {
        self.write_word(addr, (val >> 16) as u16);
        self.write_word(addr + 2, val as u16);
    }
}

/// Test that TTR bypass works: addresses matching TTR return identity-mapped.
#[test]
fn test_ttr_bypass() {
    use m68k::mmu::ttr::ttr_matches;

    // TTR matching addresses 0x40xxxxxx, FC=5 (supervisor data), E=1
    let ttr = 0x4000_8500; // Base=0x40, Mask=0x00, E=1, FC=5

    // Should match
    assert!(ttr_matches(ttr, 0x4000_0000, 5, false));
    assert!(ttr_matches(ttr, 0x40FF_FFFF, 5, false));

    // Should not match - wrong address range
    assert!(!ttr_matches(ttr, 0x3F00_0000, 5, false));
    assert!(!ttr_matches(ttr, 0x4100_0000, 5, false));

    // Should not match - wrong function code
    assert!(!ttr_matches(ttr, 0x4000_0000, 1, false)); // User data
}

/// Test that disabled TTR doesn't match.
#[test]
fn test_ttr_disabled() {
    use m68k::mmu::ttr::ttr_matches;

    // TTR with E=0 (disabled)
    let ttr = 0x4000_0000; // E bit not set
    assert!(!ttr_matches(ttr, 0x4000_0000, 5, false));
}

/// Test TTR address mask functionality.
#[test]
fn test_ttr_address_mask() {
    use m68k::mmu::ttr::ttr_matches;

    // TTR matching 0x40-0x4F by using mask 0x0F, E=1, FC=any
    let ttr = 0x400F_801C; // Base=0x40, Mask=0x0F, E=1, FCBase=0, FCMask=7

    assert!(ttr_matches(ttr, 0x4000_0000, 5, false));
    assert!(ttr_matches(ttr, 0x4F00_0000, 5, false));
    assert!(!ttr_matches(ttr, 0x5000_0000, 5, false));
}

/// Exception dispatch runs through the MMU like real silicon: frame pushes
/// and vector fetches translate, and a translation failure during dispatch
/// is a double fault that halts the CPU -- the recursion stop is the halt,
/// not a translation bypass (an OS with split root pointers relies on the
/// dispatch translating in supervisor space).
#[test]
fn test_exception_processing_translates_and_double_faults() {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68030);

    // Enable PMMU with an invalid configuration (walks raise config errors).
    cpu.pmmu_enabled = true;
    cpu.mmu_crp_limit = 0; // Invalid mode

    cpu.exception_processing = true;

    let mut bus = TestBus::new(0x10000);
    bus.write_long(0x1000, 0x12345678);

    // The read must translate (and fault); faulting mid-dispatch halts.
    let val = cpu.read_32(&mut bus, 0x1000);
    assert_ne!(
        val, 0x12345678,
        "a read during exception processing must translate, not bypass"
    );
    assert_eq!(
        cpu.stopped, 1,
        "MMU fault during dispatch is a double fault"
    );
}

/// Test that double fault halts the CPU.
#[test]
fn test_double_fault_halt() {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68030);

    let mut bus = TestBus::new(0x10000);

    // Set up minimal vector table
    bus.write_long(0x00, 0x1000); // SSP
    bus.write_long(0x04, 0x0100); // PC
    bus.write_long(56 * 4, 0x0200); // MMU config error vector

    cpu.set_a(7, 0x1000);
    cpu.pc = 0x0100;
    cpu.set_sr(0x2700);

    // Simulate being in the middle of exception processing
    cpu.exception_processing = true;

    // Taking another exception should trigger double-fault and halt
    let _ = cpu.take_exception(&mut bus, 56); // MMU config error

    assert_eq!(cpu.stopped, 1, "CPU should halt on double fault");
}

/// Test that PMOVE TC with E=0 doesn't enable MMU translation.
#[test]
fn test_pmove_tc_disabled() {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68030);

    let mut bus = TestBus::new(0x10000);

    // Set up vector table and stack
    bus.write_long(0x00, 0x1000);
    bus.write_long(0x04, 0x0100);

    // TC value at address 0x2000 with E=0 (disabled)
    bus.write_long(0x2000, 0x0000_0002); // E=0, mode=2

    cpu.set_a(7, 0x1000);
    cpu.set_a(0, 0x2000);
    cpu.pc = 0x0100;
    cpu.set_sr(0x2700);

    // PMOVE (A0), TC = load TC from memory
    // Opcode: 0xF010, Extension: 0x4000
    bus.write_word(0x0100, 0xF010);
    bus.write_word(0x0102, 0x4000);
    bus.write_word(0x0104, 0x4E71); // NOP to verify continued execution

    let result = cpu.step(&mut bus);
    assert!(
        matches!(result, StepResult::Ok { .. }),
        "PMOVE should execute"
    );

    // TC should be loaded but PMMU should NOT be enabled (E=0)
    assert_eq!(cpu.mmu_tc, 0x0000_0002);
    assert!(!cpu.pmmu_enabled, "PMMU should not be enabled when TC.E=0");
}

/// Test that normal instruction execution preserves exception_processing = false.
#[test]
fn test_normal_execution_no_exception_flag() {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68000);

    let mut bus = TestBus::new(0x10000);

    // Set up vector table
    bus.write_long(0x00, 0x1000);
    bus.write_long(0x04, 0x0100);

    // NOP instruction
    bus.write_word(0x0100, 0x4E71);

    cpu.set_a(7, 0x1000);
    cpu.pc = 0x0100;
    cpu.set_sr(0x2700);

    let _ = cpu.step(&mut bus);

    // exception_processing should remain false during normal execution
    assert!(
        !cpu.exception_processing,
        "exception_processing should be false after normal instruction"
    );
}

/// Test that supervisor mode is enabled after taking a TRAP exception.
#[test]
fn test_supervisor_mode_on_trap() {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);

    let mut bus = TestBus::new(0x10000);

    // Set up minimal vector table
    bus.write_long(0x00, 0x1000); // SSP
    bus.write_long(0x80, 0x0200); // TRAP #0 vector

    cpu.set_a(7, 0x1000);
    cpu.pc = 0x0100;
    cpu.set_sr(0x0000); // Start in USER mode (S=0)

    assert!(!cpu.is_supervisor(), "Should start in user mode");

    cpu.take_trap_exception(&mut bus, 0);

    assert!(
        cpu.is_supervisor(),
        "Should be in supervisor mode after TRAP (s_flag={}, SR=0x{:04X})",
        cpu.s_flag,
        cpu.get_sr()
    );
}

/// Trace test for trap privilege to find where S-flag goes wrong.
#[test]
fn test_trap_privilege_trace() {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);

    let mut bus = TestBus::new(0x20000);

    // Set up SSP and PC vectors
    bus.write_long(0x00, 0x1000); // SSP
    bus.write_long(0x04, 0x10000); // PC

    // Set up TRAP #0 vector to handler
    bus.write_long(0x80, 0x10020); // TRAP #0 vector points to 0x10020

    // Code at 0x10000 (entry point):
    // ANDI.W #0xDFFF, SR  (switch to user mode)
    bus.write_word(0x10000, 0x027C);
    bus.write_word(0x10002, 0xDFFF);
    // TRAP #0
    bus.write_word(0x10004, 0x4E40);
    // After RTE: MOVE.W SR, D1
    bus.write_word(0x10006, 0x40C1);
    // NOP (end)
    bus.write_word(0x10008, 0x4E71);

    // Handler at 0x10020:
    // MOVE.W SR, D2 (to check S flag)
    bus.write_word(0x10020, 0x40C2);
    // RTE
    bus.write_word(0x10022, 0x4E73);

    cpu.set_a(7, 0x1000);
    cpu.pc = 0x10000;
    cpu.set_sr(0x2700); // Start in supervisor mode

    println!("=== Initial state ===");
    println!(
        "  PC=0x{:08X} SR=0x{:04X} is_supervisor={}",
        cpu.pc,
        cpu.get_sr(),
        cpu.is_supervisor()
    );

    let mut hle = m68k::NoOpHleHandler;

    // Step 1: ANDI.W #0xDFFF, SR
    let result = cpu.step_with_hle_handler(&mut bus, &mut hle);
    println!("=== After ANDI to SR ===");
    println!(
        "  PC=0x{:08X} SR=0x{:04X} is_supervisor={} result={:?}",
        cpu.pc,
        cpu.get_sr(),
        cpu.is_supervisor(),
        result
    );
    assert!(
        !cpu.is_supervisor(),
        "Should be in user mode after ANDI to SR"
    );

    // Step 2: TRAP #0 - auto-take the exception via NoOpHleHandler
    let result = cpu.step_with_hle_handler(&mut bus, &mut hle);
    println!("=== After TRAP #0 (step_with_hle_handler auto-took exception) ===");
    println!(
        "  PC=0x{:08X} SR=0x{:04X} is_supervisor={} result={:?}",
        cpu.pc,
        cpu.get_sr(),
        cpu.is_supervisor(),
        result
    );
    // With auto-exception, CPU should now be in supervisor mode and at handler
    assert!(
        cpu.is_supervisor(),
        "Should be in supervisor mode after TRAP (auto-exception)"
    );

    // Step 3: MOVE.W SR, D2 (in handler)
    let _result = cpu.step_with_hle_handler(&mut bus, &mut hle);
    println!("=== After MOVE.W SR, D2 ===");
    println!(
        "  PC=0x{:08X} SR=0x{:04X} D2=0x{:08X}",
        cpu.pc,
        cpu.get_sr(),
        cpu.d(2)
    );
    assert_eq!(
        cpu.d(2) & 0x2000,
        0x2000,
        "D2 should have S-bit set from SR (D2=0x{:08X})",
        cpu.d(2)
    );

    // Step 4: RTE (returns to user mode)
    let result = cpu.step_with_hle_handler(&mut bus, &mut hle);
    println!("=== After RTE ===");
    println!(
        "  PC=0x{:08X} SR=0x{:04X} is_supervisor={} result={:?}",
        cpu.pc,
        cpu.get_sr(),
        cpu.is_supervisor(),
        result
    );
}

/// Load and trace actual trap_privilege.bin to find the failure point.
#[test]
fn test_trap_privilege_binary() {
    let binary = include_bytes!("fixtures/extra/privilege/bin/trap_privilege.bin");

    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);

    let mut bus = TestBus::new(0x50000);

    // Load binary at 0x10000 (ROM location)
    for (i, &b) in binary.iter().enumerate() {
        bus.mem[0x10000 + i] = b;
    }

    // Setup boot vectors (matching common/mod.rs)
    bus.write_long(0x00, 0x3F0); // SSP
    bus.write_long(0x04, 0x10000); // PC

    cpu.set_a(7, 0x3F0);
    cpu.pc = 0x10000;
    cpu.set_sr(0x2700);

    println!("=== Starting trap_privilege.bin ===");
    println!("Binary size: {} bytes", binary.len());

    let mut step_count = 0;
    for _ in 0..200 {
        step_count += 1;
        let pc_before = cpu.pc;
        let sr_before = cpu.get_sr();

        let result = cpu.step(&mut bus);

        // Print state for key instructions
        if step_count <= 20 || !matches!(result, StepResult::Ok { .. }) {
            println!(
                "Step {}: PC=0x{:08X}→0x{:08X} SR=0x{:04X}→0x{:04X} result={:?}",
                step_count,
                pc_before,
                cpu.pc,
                sr_before,
                cpu.get_sr(),
                result
            );
        }

        match result {
            StepResult::Stopped => {
                println!("Stopped at step {}", step_count);
                break;
            }
            StepResult::Ok { .. } => {}
            _ => {}
        }

        if cpu.stopped != 0 {
            println!("CPU stopped at step {}", step_count);
            break;
        }
    }

    // Check test device registers to see pass/fail
    let test_fail = bus.mem.get(0x100000).copied().unwrap_or(0xFF);
    let test_pass = bus.mem.get(0x100004).copied().unwrap_or(0xFF);
    println!("TEST_FAIL_REG accessed: {}", test_fail != 0xFF);
    println!("TEST_PASS_REG accessed: {}", test_pass != 0xFF);
}
