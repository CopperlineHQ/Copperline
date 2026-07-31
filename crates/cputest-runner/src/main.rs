//! Drives WinUAE cputest-generated instruction test sets against the
//! published m68k core.
//!
//! The vendored (MIT) runner from emoon's m68k_cpu_tester_api parses the
//! `.dat` sets that Toni Wilen's cputest generator produces -- every opcode
//! of a CPU model, hundreds of state combinations each, validated against
//! real hardware over years -- and calls back here once per test with the
//! initial register file. This side loads the registers into `CpuCore`,
//! executes until the test's terminating exception fires (the generator
//! places an illegal-instruction sentinel after each tested instruction,
//! so a clean run ends in vector 4 at the expected PC), and reports the
//! final state; the runner performs all comparison, including exception
//! stack frames and memory stores.
//!
//! Usage:
//!   cputest-runner <data-dir> <opcode|all> <cpu: 68000|68010|68020|68030|68040|68060>
//!
//! Generate the data with tools/cputest-gen.sh.

use std::ffi::{c_char, c_int, c_void, CStr, CString};

use m68k::core::cpu::{CpuCore, MFLAG_SET, SFLAG_SET};
use m68k::core::memory::AddressBus;
use m68k::core::types::CpuType;
use m68k::NoOpHleHandler;

#[repr(C)]
#[derive(Clone, Copy)]
struct FpuReg {
    exp: u16,
    dummy: u16,
    m: [u32; 2],
}

/// Mirror of the runner's internal `struct registers` (the callback pointer
/// is a cast of it; the public header's layout is stale, so this follows
/// m68k_cpu_tester.c).
#[repr(C)]
struct Registers {
    regs: [u32; 16],
    ssp: u32,
    msp: u32,
    pc: u32,
    sr: u32,
    exc: u32,
    exc010: u32,
    excframe: u32,
    fpuregs: [FpuReg; 8],
    fpiar: u32,
    fpcr: u32,
    fpsr: u32,
    srcaddr: u32,
    dstaddr: u32,
    /// Appended by the vendored runner's local patch: the newer
    /// generator's instruction-end PC, branch target, and cycle records.
    endpc: u32,
    branchtarget: u32,
    cycles: u32,
    branchtarget_mode: u8,
}

#[repr(C)]
struct MemoryRange {
    buffer: *mut u8,
    start: u32,
    end: u32,
    size: u32,
}

#[repr(C)]
struct Context {
    opcode: *const c_char,
    stop_on_error: u32,
    low_memory: MemoryRange,
    high_memory: MemoryRange,
    test_memory: MemoryRange,
    name: [c_char; 17],
    cpu_path: [c_char; 2048],
}

#[repr(C)]
struct RunSettings {
    opcode: *const c_char,
    cpu_level: u8,
    check_undefined_sr: u8,
    continue_on_error: u8,
}

#[repr(C)]
struct InitResult {
    context: *mut Context,
    error: *const c_char,
}

unsafe extern "C" {
    fn M68KTester_init(path: *const c_char, settings: *const RunSettings) -> InitResult;
    fn M68KTester_run_tests(
        context: *mut Context,
        user_data: *mut c_void,
        callback: extern "C" fn(*mut c_void, *const Context, *mut Registers),
    ) -> c_int;
}

/// The three test memory regions, shared with the runner (it applies each
/// test's memory setup into these buffers and inspects them afterwards, so
/// the CPU must read and write them in place). Every access is masked to
/// the model's address-bus width first: word/long accesses at the top of
/// the 24-bit space wrap around to low memory on a 68000/010, and the
/// test data exercises exactly that.
struct TesterBus {
    low: (*mut u8, u32, u32),
    high: (*mut u8, u32, u32),
    test: (*mut u8, u32, u32),
    mask: u32,
}

impl TesterBus {
    fn from_context(ctx: &Context, mask: u32) -> Self {
        let r = |m: &MemoryRange| (m.buffer, m.start, m.end);
        Self {
            low: r(&ctx.low_memory),
            high: r(&ctx.high_memory),
            test: r(&ctx.test_memory),
            mask,
        }
    }

    fn slot(&self, addr: u32) -> Option<*mut u8> {
        let addr = addr & self.mask;
        for &(buf, start, end) in [&self.low, &self.high, &self.test] {
            if !buf.is_null() && addr >= start && addr < end {
                return Some(unsafe { buf.add((addr - start) as usize) });
            }
        }
        None
    }
}

impl AddressBus for TesterBus {
    fn read_byte(&mut self, addr: u32) -> u8 {
        self.slot(addr).map(|p| unsafe { *p }).unwrap_or(0)
    }

    fn write_byte(&mut self, addr: u32, val: u8) {
        if let Some(p) = self.slot(addr) {
            unsafe { *p = val };
        }
    }

    fn read_word(&mut self, addr: u32) -> u16 {
        ((self.read_byte(addr) as u16) << 8) | self.read_byte(addr.wrapping_add(1)) as u16
    }

    fn write_word(&mut self, addr: u32, val: u16) {
        self.write_byte(addr, (val >> 8) as u8);
        self.write_byte(addr.wrapping_add(1), val as u8);
    }

    fn read_long(&mut self, addr: u32) -> u32 {
        ((self.read_word(addr) as u32) << 16) | self.read_word(addr.wrapping_add(2)) as u32
    }

    fn write_long(&mut self, addr: u32, val: u32) {
        self.write_word(addr, (val >> 16) as u16);
        self.write_word(addr.wrapping_add(2), val as u16);
    }
}

static mut CPU_TYPE: CpuType = CpuType::M68000;

extern "C" {
    // Local patches in vendor/m68k_cpu_tester.c: the addressing mask and
    // FPU model the current data set was generated with (24-bit sets exist
    // for every CPU level; fpu_model 0 = no FPU attached).
    fn m68k_tester_addressing_mask() -> u32;
    fn m68k_tester_fpu_model() -> u32;
}

/// Ceiling on instructions per test: a test normally ends at its sentinel
/// within a couple of steps (the generator also plants sentinels at branch
/// targets); a runaway means the core lost the plot.
const MAX_STEPS: usize = 64;

extern "C" fn run_one_test(_user: *mut c_void, ctx: *const Context, regs: *mut Registers) {
    let ctx = unsafe { &*ctx };
    let regs = unsafe { &mut *regs };
    let mut bus = TesterBus::from_context(ctx, unsafe { m68k_tester_addressing_mask() });

    // The native runner copies the top of the (generator-prepared) user
    // stack image onto the supervisor stack before each round, so frames
    // popped by RTE/RTR in supervisor rounds see the same data (its host
    // build compiles this out behind #ifdef M68K).
    for i in 0..0x20 {
        let b = bus.read_byte(regs.regs[15].wrapping_add(i));
        bus.write_byte(regs.ssp.wrapping_add(i), b);
    }

    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(unsafe { CPU_TYPE });
    // Match the coprocessor configuration the data set was generated with
    // (fpu_model 0 = no FPU: cpID-1 ops take Line-F on the 020/030; the
    // 060 models FPU absence through PCR.DFP instead).
    cpu.fpu_present = unsafe { m68k_tester_fpu_model() } != 0;
    if !cpu.fpu_present && cpu.is_060() {
        cpu.pcr |= m68k::core::cpu::PCR_DFP;
    }

    // Register file: SR flags first (without banking), then the explicit
    // stack banks, then the active A7 by S/M -- the same order the
    // SingleStepTests loader uses.
    cpu.set_sr_noint_nosp(regs.sr as u16);
    cpu.dar[..15].copy_from_slice(&regs.regs[..15]);
    cpu.sp[0] = regs.regs[15];
    cpu.sp[SFLAG_SET as usize] = regs.ssp;
    cpu.sp[(SFLAG_SET | MFLAG_SET) as usize] = regs.msp;
    let active = if cpu.s_flag != 0 {
        if cpu.m_flag != 0 {
            regs.msp
        } else {
            regs.ssp
        }
    } else {
        regs.regs[15]
    };
    cpu.set_sp(active);
    cpu.pc = regs.pc;

    // Run like the native tester's execute_ins: until the instruction
    // stream reaches the recorded end-of-instruction PC (or the branch
    // target for taken branches), or a real exception fires first.
    let debug = std::env::var("CPUTEST_DEBUG").is_ok();
    let mut hle = NoOpHleHandler;
    cpu.last_exception_vector = None;
    regs.cycles = 0;
    let mut exc = 0u32;
    let mut excframe = 0u32;
    for step in 0..MAX_STEPS {
        if debug {
            let op = bus.read_word(cpu.pc);
            let ext = bus.read_word(cpu.pc.wrapping_add(2));
            let ext2 = bus.read_word(cpu.pc.wrapping_add(4));
            let _ = ext2;
            eprintln!(
                "  step {step}: pc={:#010X} op={op:#06X} ext={ext:#06X} ext2={ext2:#06X} sr={:#06X} a7={:#010X} end={:#010X} d={:08X?}",
                cpu.pc,
                cpu.get_sr(),
                cpu.a(7),
                regs.endpc,
                &cpu.dar[..8]
            );
        }
        let step_result = cpu.step_with_hle_handler(&mut bus, &mut hle);
        let step_cycles = match step_result {
            m68k::StepResult::Ok { cycles } => cycles as u32,
            _ => 0,
        };
        let _ = step;
        if let Some(v) = cpu.last_exception_vector.take() {
            // The generator's cycle counter stops when the terminating
            // sentinel exception is recognized, so the final faulting step
            // is not added to the measured total (CPUTEST_CYCLES). A trace
            // is the one exception the core bundles with a COMPLETED
            // instruction's step: count that instruction, not the 34-clock
            // trace stacking.
            if v == 9 {
                regs.cycles = regs
                    .cycles
                    .wrapping_add(step_cycles.saturating_sub(34));
            }
            exc = v;
            excframe = cpu.a(7);
            if debug {
                eprintln!("  -> exception {v} frame={excframe:#010X}");
            }
            break;
        }
        regs.cycles = regs.cycles.wrapping_add(step_cycles);
        // If the tested instruction left T1 set (e.g. RTE restoring a traced
        // SR), the trace fires after the NEXT instruction: keep stepping so
        // the sentinel at endpc executes and raises the expected trace (or
        // illegal-instruction) exception, exactly like real hardware.
        let trace_pending = cpu.get_sr() & 0x8000 != 0;
        if !trace_pending
            && (cpu.pc == regs.endpc
                || (regs.branchtarget != 0xFFFF_FFFF && cpu.pc == regs.branchtarget))
        {
            // A taken branch stops on ARRIVAL at the target sentinel; the
            // generator's cycle counter also includes the target's leading
            // NOP (the linear path executes its trailing NOP before the
            // stop, so only this side needs the correction).
            if cpu.pc != regs.endpc && bus.read_word(cpu.pc) == 0x4E71 {
                regs.cycles = regs.cycles.wrapping_add(4);
            }
            break;
        }
    }

    // Report the state the way the native runner's exception stub does:
    // registers as the faulting context saw them, the pre-exception PC/SR
    // from the pushed frame, and the frame address for the runner's
    // exception validation.
    regs.regs[..15].copy_from_slice(&cpu.dar[..15]);
    regs.exc = exc;
    regs.exc010 = exc;
    regs.excframe = excframe;
    // regs[15] always reports the USER stack pointer; the supervisor stack
    // travels in the ssp field (the native runner's register-capture stub
    // uses the same convention).
    regs.regs[15] = cpu.get_usp();
    if exc != 0 {
        regs.sr = bus.read_word(excframe) as u32;
        regs.pc = bus.read_long(excframe.wrapping_add(2));
        // The frame was just pushed on the supervisor stack.
        regs.ssp = cpu.a(7);
    } else {
        regs.sr = cpu.get_sr() as u32;
        regs.pc = cpu.pc;
        regs.ssp = if cpu.s_flag != 0 {
            cpu.a(7)
        } else {
            cpu.sp[SFLAG_SET as usize]
        };
    }
    regs.msp = cpu.sp[(SFLAG_SET | MFLAG_SET) as usize];
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: cputest-runner <data-dir> <opcode|all> <cpu model>");
        std::process::exit(2);
    }
    let (cpu_type, level) = match args[3].as_str() {
        "68000" => (CpuType::M68000, 0u8),
        "68010" => (CpuType::M68010, 1),
        "68020" => (CpuType::M68020, 2),
        "68030" => (CpuType::M68030, 3),
        "68040" => (CpuType::M68040, 4),
        "68060" => (CpuType::M68060, 5),
        other => {
            eprintln!("unknown cpu model {other}");
            std::process::exit(2);
        }
    };
    unsafe {
        CPU_TYPE = cpu_type;
    }

    let path = CString::new(args[1].clone()).unwrap();
    let opcode = CString::new(args[2].clone()).unwrap();
    let settings = RunSettings {
        opcode: opcode.as_ptr(),
        cpu_level: level,
        check_undefined_sr: 1,
        continue_on_error: 1,
    };

    let res = unsafe { M68KTester_init(path.as_ptr(), &settings) };
    // CPUTEST_STOP=1 stops at the first failure, so a CPUTEST_DEBUG trace
    // ends at the failing test instead of the last one in the file.
    if std::env::var("CPUTEST_STOP").is_ok() && !res.context.is_null() {
        unsafe { (*res.context).stop_on_error = 1 };
    }
    if res.context.is_null() {
        let msg = if res.error.is_null() {
            "unknown error".to_string()
        } else {
            unsafe { CStr::from_ptr(res.error) }
                .to_string_lossy()
                .into_owned()
        };
        eprintln!("cputest init failed: {msg}");
        std::process::exit(1);
    }

    let ok = unsafe { M68KTester_run_tests(res.context, std::ptr::null_mut(), run_one_test) };
    std::process::exit(if ok == 1 { 0 } else { 1 });
}
