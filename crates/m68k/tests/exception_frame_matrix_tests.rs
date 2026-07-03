//! Exception stack-frame matrix: for every CPU model, every exception
//! class must push the frame format the model's manual specifies, with the
//! right stacked PC and vector offset.
//!
//! The rules under test (M68000PRM, M68020UM table 6-5, M68040UM 8.4,
//! M68060UM 8.2):
//!
//! - TRAP #n, illegal instruction, A-line, privilege violation, and
//!   interrupts push the four-word format $0 frame on the 68010+ (the
//!   six-word 68000 frame before that). TRAP #n and interrupts stack the
//!   *next* PC; the instruction exceptions stack the *faulting* PC.
//! - The group-2 instruction exceptions -- CHK, TRAPcc/TRAPV, zero divide,
//!   trace -- stack the next PC on every model, and on the 68020+ push the
//!   six-word format $2 frame whose extra long holds the address of the
//!   instruction that caused the exception.
//!
//! OS trap dispatchers and debuggers parse these frames by format nibble
//! (AmigaOS exec's trap handler, MuForce's trace step-over), so a wrong
//! format corrupts their frame walking even when RTE happens to cope.

use m68k::core::cpu::CpuCore;
use m68k::core::memory::AddressBus;
use m68k::core::types::CpuType;
use m68k::{NoOpHleHandler, StepResult};

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

const SSP: u32 = 0x1F00;
const USP: u32 = 0x1800;
const CODE: u32 = 0x0100;
const HANDLER: u32 = 0x0300;

const ALL_CPUS: [CpuType; 6] = [
    CpuType::M68000,
    CpuType::M68010,
    CpuType::M68020,
    CpuType::M68030,
    CpuType::M68040,
    CpuType::M68060,
];

/// CPUs with the 68020+ six-word format $2 group-2 frame.
fn has_format_2(cpu_type: CpuType) -> bool {
    !matches!(
        cpu_type,
        CpuType::M68000 | CpuType::M68010 | CpuType::SCC68070
    )
}

/// A user-mode CPU of the given model with every exception vector pointing
/// at HANDLER (a NOP; the tests only inspect the entry frame).
fn user_mode_cpu(bus: &mut TestBus, cpu_type: CpuType, sr: u16) -> CpuCore {
    for vec in 2..64 {
        bus.write_long(vec * 4, HANDLER);
    }
    bus.write_word(HANDLER, 0x4E71); // NOP

    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(cpu_type);
    cpu.set_sr(0x2700);
    cpu.set_a(7, SSP);
    cpu.set_sr(sr & !0x2000); // drop to user mode with the caller's flags
    cpu.set_a(7, USP);
    cpu.pc = CODE;
    cpu
}

fn step_once(cpu: &mut CpuCore, bus: &mut TestBus) {
    let mut hle = NoOpHleHandler;
    let r = cpu.step_with_hle_handler(bus, &mut hle);
    assert!(
        matches!(r, StepResult::Ok { .. }),
        "unexpected step result {r:?}"
    );
}

/// What the exception entry must have stacked.
struct ExpectedFrame {
    vector: u32,
    /// The PC value in the frame (next instruction or faulting instruction,
    /// per exception class).
    stacked_pc: u32,
    /// Some(instruction address) when the model uses the six-word format $2
    /// frame for this exception; None for format $0 / the 68000 frame.
    format_2_instr: Option<u32>,
}

fn check_frame(cpu: &CpuCore, bus: &mut TestBus, cpu_type: CpuType, exp: &ExpectedFrame) {
    let tag = format!("{cpu_type:?} vector {}", exp.vector);
    assert!(cpu.is_supervisor(), "{tag}: handler entered in supervisor");
    assert_eq!(
        cpu.pc & 0xFFFF,
        HANDLER,
        "{tag}: vectored through the table"
    );

    let sp = cpu.a(7);
    if cpu_type == CpuType::M68000 {
        assert_eq!(sp, SSP - 6, "{tag}: 68000 three-word frame");
        assert_eq!(bus.read_long(sp + 2), exp.stacked_pc, "{tag}: stacked PC");
        return;
    }

    let fmt_word = if exp.format_2_instr.is_some() && has_format_2(cpu_type) {
        assert_eq!(sp, SSP - 12, "{tag}: six-word format $2 frame");
        assert_eq!(
            bus.read_long(sp + 8),
            exp.format_2_instr.unwrap(),
            "{tag}: format $2 instruction address"
        );
        0x2000
    } else {
        assert_eq!(sp, SSP - 8, "{tag}: four-word format $0 frame");
        0x0000
    };
    assert_eq!(
        bus.read_word(sp + 6),
        fmt_word | (exp.vector as u16) << 2,
        "{tag}: format/vector word"
    );
    assert_eq!(bus.read_long(sp + 2), exp.stacked_pc, "{tag}: stacked PC");
    assert_eq!(
        bus.read_word(sp) & 0x2000,
        0,
        "{tag}: stacked SR is the user-mode SR"
    );
}

/// TRAP #n: format $0 on every 68010+ model (it is NOT a format $2
/// exception, unlike the group-2 instruction traps), next PC stacked.
#[test]
fn trap_pushes_format_0_with_next_pc() {
    for cpu_type in ALL_CPUS {
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_cpu(&mut bus, cpu_type, 0);
        bus.write_word(CODE, 0x4E43); // TRAP #3

        step_once(&mut cpu, &mut bus);
        check_frame(
            &cpu,
            &mut bus,
            cpu_type,
            &ExpectedFrame {
                vector: 35,
                stacked_pc: CODE + 2,
                format_2_instr: None,
            },
        );
    }
}

/// TRAPV: group-2, so format $2 on the 68020+ with the TRAPV instruction's
/// address in the extra long.
#[test]
fn trapv_pushes_format_2_on_020_plus() {
    for cpu_type in ALL_CPUS {
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_cpu(&mut bus, cpu_type, 0x0002); // V set
        bus.write_word(CODE, 0x4E76); // TRAPV

        step_once(&mut cpu, &mut bus);
        check_frame(
            &cpu,
            &mut bus,
            cpu_type,
            &ExpectedFrame {
                vector: 7,
                stacked_pc: CODE + 2,
                format_2_instr: Some(CODE),
            },
        );
    }
}

/// TRAPcc (68020+ only): same group-2 frame as TRAPV.
#[test]
fn trapcc_pushes_format_2() {
    for cpu_type in ALL_CPUS {
        if !has_format_2(cpu_type) {
            continue; // TRAPcc does not exist before the 68020
        }
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_cpu(&mut bus, cpu_type, 0);
        bus.write_word(CODE, 0x50FC); // TRAPT

        step_once(&mut cpu, &mut bus);
        check_frame(
            &cpu,
            &mut bus,
            cpu_type,
            &ExpectedFrame {
                vector: 7,
                stacked_pc: CODE + 2,
                format_2_instr: Some(CODE),
            },
        );
    }
}

/// CHK out-of-bounds: group-2 frame, next PC stacked on every model.
#[test]
fn chk_pushes_format_2_on_020_plus() {
    for cpu_type in ALL_CPUS {
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_cpu(&mut bus, cpu_type, 0);
        cpu.set_d(0, 0xFFFF_FFFF); // negative -> CHK traps
        cpu.set_d(1, 0x0010);
        bus.write_word(CODE, 0x4181); // CHK.W D1,D0

        step_once(&mut cpu, &mut bus);
        check_frame(
            &cpu,
            &mut bus,
            cpu_type,
            &ExpectedFrame {
                vector: 6,
                stacked_pc: CODE + 2,
                format_2_instr: Some(CODE),
            },
        );
    }
}

/// Integer divide by zero: group-2 frame.
#[test]
fn zero_divide_pushes_format_2_on_020_plus() {
    for cpu_type in ALL_CPUS {
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_cpu(&mut bus, cpu_type, 0);
        cpu.set_d(0, 1234);
        cpu.set_d(1, 0);
        bus.write_word(CODE, 0x80C1); // DIVU.W D1,D0

        step_once(&mut cpu, &mut bus);
        check_frame(
            &cpu,
            &mut bus,
            cpu_type,
            &ExpectedFrame {
                vector: 5,
                stacked_pc: CODE + 2,
                format_2_instr: Some(CODE),
            },
        );
    }
}

/// Trace: group-2 frame with the traced instruction's address in the
/// format $2 extra long (how a debugger attributes the step).
#[test]
fn trace_pushes_format_2_on_020_plus() {
    for cpu_type in ALL_CPUS {
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_cpu(&mut bus, cpu_type, 0x8000); // T1 set
        bus.write_word(CODE, 0x4E71); // NOP (traced)

        step_once(&mut cpu, &mut bus);
        check_frame(
            &cpu,
            &mut bus,
            cpu_type,
            &ExpectedFrame {
                vector: 9,
                stacked_pc: CODE + 2,
                format_2_instr: Some(CODE),
            },
        );
    }
}

/// Illegal instruction: format $0 with the FAULTING instruction's PC, so
/// the handler can decode or patch the opcode (AmigaOS SetFunction-style
/// trap emulation depends on it).
#[test]
fn illegal_pushes_format_0_with_faulting_pc() {
    for cpu_type in ALL_CPUS {
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_cpu(&mut bus, cpu_type, 0);
        bus.write_word(CODE, 0x4AFC); // ILLEGAL

        step_once(&mut cpu, &mut bus);
        check_frame(
            &cpu,
            &mut bus,
            cpu_type,
            &ExpectedFrame {
                vector: 4,
                stacked_pc: CODE,
                format_2_instr: None,
            },
        );
    }
}

/// A-line: format $0, faulting PC (the classic Mac/patch-table shape).
#[test]
fn aline_pushes_format_0_with_faulting_pc() {
    for cpu_type in ALL_CPUS {
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_cpu(&mut bus, cpu_type, 0);
        bus.write_word(CODE, 0xA123);

        step_once(&mut cpu, &mut bus);
        check_frame(
            &cpu,
            &mut bus,
            cpu_type,
            &ExpectedFrame {
                vector: 10,
                stacked_pc: CODE,
                format_2_instr: None,
            },
        );
    }
}

/// Privilege violation: format $0, faulting PC (exec's MOVE-SR emulation
/// re-decodes the instruction at the stacked PC).
#[test]
fn privilege_violation_pushes_faulting_pc() {
    for cpu_type in ALL_CPUS {
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_cpu(&mut bus, cpu_type, 0);
        bus.write_word(CODE, 0x4E72); // STOP (privileged)
        bus.write_word(CODE + 2, 0x2700);

        step_once(&mut cpu, &mut bus);
        check_frame(
            &cpu,
            &mut bus,
            cpu_type,
            &ExpectedFrame {
                vector: 8,
                stacked_pc: CODE,
                format_2_instr: None,
            },
        );
    }
}

/// An RTE from each generated frame format resumes at the stacked PC in
/// the stacked mode: the frames round-trip through their own RTE pops.
#[test]
fn frames_round_trip_through_rte() {
    for cpu_type in ALL_CPUS {
        // TRAP #3 (format $0 / 68000 frame) ...
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_cpu(&mut bus, cpu_type, 0);
        bus.write_word(CODE, 0x4E43);
        bus.write_word(CODE + 2, 0x4E71); // NOP after the trap
        bus.write_word(HANDLER, 0x4E73); // RTE
        step_once(&mut cpu, &mut bus); // trap
        step_once(&mut cpu, &mut bus); // rte
        step_once(&mut cpu, &mut bus); // nop
        assert!(!cpu.is_supervisor(), "{cpu_type:?}: back in user mode");
        assert_eq!(cpu.pc, CODE + 4, "{cpu_type:?}: resumed after TRAP");

        // ... and TRAPV (format $2 on the 020+).
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_cpu(&mut bus, cpu_type, 0x0002);
        bus.write_word(CODE, 0x4E76);
        bus.write_word(CODE + 2, 0x4E71);
        bus.write_word(HANDLER, 0x4E73);
        step_once(&mut cpu, &mut bus);
        step_once(&mut cpu, &mut bus);
        step_once(&mut cpu, &mut bus);
        assert!(!cpu.is_supervisor(), "{cpu_type:?}: back in user mode");
        assert_eq!(cpu.pc, CODE + 4, "{cpu_type:?}: resumed after TRAPV");
    }
}

/// Autovectored interrupts: a pending level above the SR mask is serviced
/// before the next instruction with a format $0 frame (vector 24+level)
/// whose stacked PC is the instruction that had not yet executed; the new
/// SR mask is raised to the serviced level. A level at or below the mask
/// stays pending, and level 7 (NMI) is serviced even at mask 7.
#[test]
fn autovector_interrupt_frames_and_masking() {
    for cpu_type in ALL_CPUS {
        // Level 3 against mask 0: serviced, format $0, next-PC semantics.
        // This core recognizes pending interrupts at the instruction
        // boundary after execution (the host models recognition latency
        // separately), so the NOP completes and the frame stacks the PC
        // after it.
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_cpu(&mut bus, cpu_type, 0);
        bus.write_word(CODE, 0x4E71); // NOP (runs before recognition)
        cpu.set_irq(3);
        assert!(cpu.check_interrupts(), "{cpu_type:?}: level 3 above mask 0");
        step_once(&mut cpu, &mut bus);
        check_frame(
            &cpu,
            &mut bus,
            cpu_type,
            &ExpectedFrame {
                vector: 24 + 3,
                stacked_pc: CODE + 2,
                format_2_instr: None,
            },
        );
        assert_eq!(
            cpu.get_sr() & 0x0700,
            0x0300,
            "{cpu_type:?}: SR mask raised to the serviced level"
        );

        // Level 3 against mask 7: masked, the NOP executes instead.
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_cpu(&mut bus, cpu_type, 0);
        cpu.set_sr(0x2700);
        cpu.pc = CODE;
        bus.write_word(CODE, 0x4E71);
        cpu.set_irq(3);
        assert!(!cpu.check_interrupts(), "{cpu_type:?}: level 3 masked at 7");
        step_once(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, CODE + 2, "{cpu_type:?}: masked interrupt waits");

        // Level 7 against mask 7: the NMI is serviced anyway.
        let mut bus = TestBus::new(0x10000);
        let mut cpu = user_mode_cpu(&mut bus, cpu_type, 0);
        cpu.set_sr(0x2700);
        cpu.set_a(7, SSP);
        cpu.pc = CODE;
        bus.write_word(CODE, 0x4E71);
        cpu.set_irq(7);
        assert!(cpu.check_interrupts(), "{cpu_type:?}: NMI beats mask 7");
        step_once(&mut cpu, &mut bus);
        assert_eq!(
            cpu.pc & 0xFFFF,
            HANDLER,
            "{cpu_type:?}: NMI vectored through the table"
        );
    }
}
