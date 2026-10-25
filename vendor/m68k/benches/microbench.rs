use m68k::core::memory::{AddressBus, LinearMemoryBus};
use m68k::{CpuCore, CpuType};
use std::time::Instant;

trait BenchBus: AddressBus {
    fn new() -> Self;
    fn write_word_at(&mut self, address: u32, value: u16);

    fn filled(opcode: u16) -> Self
    where
        Self: Sized,
    {
        let mut bus = Self::new();
        for addr in (0..0x10000).step_by(2) {
            bus.write_word_at(addr as u32, opcode);
        }
        bus
    }
}

struct PlainBenchBus {
    memory: [u8; 0x10000],
}

impl BenchBus for PlainBenchBus {
    fn new() -> Self {
        Self {
            memory: [0; 0x10000],
        }
    }

    fn write_word_at(&mut self, address: u32, value: u16) {
        let addr = (address as usize) & 0xFFFF;
        let bytes = value.to_be_bytes();
        self.memory[addr] = bytes[0];
        self.memory[(addr + 1) & 0xFFFF] = bytes[1];
    }
}

impl AddressBus for PlainBenchBus {
    fn read_byte(&mut self, address: u32) -> u8 {
        self.memory[(address as usize) & 0xFFFF]
    }

    fn read_word(&mut self, address: u32) -> u16 {
        let addr = (address as usize) & 0xFFFF;
        u16::from_be_bytes([self.memory[addr], self.memory[(addr + 1) & 0xFFFF]])
    }

    fn read_long(&mut self, address: u32) -> u32 {
        let addr = (address as usize) & 0xFFFF;
        u32::from_be_bytes([
            self.memory[addr],
            self.memory[(addr + 1) & 0xFFFF],
            self.memory[(addr + 2) & 0xFFFF],
            self.memory[(addr + 3) & 0xFFFF],
        ])
    }

    fn write_byte(&mut self, address: u32, value: u8) {
        self.memory[(address as usize) & 0xFFFF] = value;
    }

    fn write_word(&mut self, address: u32, value: u16) {
        self.write_word_at(address, value);
    }

    fn write_long(&mut self, address: u32, value: u32) {
        let addr = (address as usize) & 0xFFFF;
        let bytes = value.to_be_bytes();
        self.memory[addr] = bytes[0];
        self.memory[(addr + 1) & 0xFFFF] = bytes[1];
        self.memory[(addr + 2) & 0xFFFF] = bytes[2];
        self.memory[(addr + 3) & 0xFFFF] = bytes[3];
    }
}

impl BenchBus for LinearMemoryBus {
    fn new() -> Self {
        LinearMemoryBus::new(0x10000)
    }

    fn write_word_at(&mut self, address: u32, value: u16) {
        LinearMemoryBus::write_word_at(self, address, value);
    }
}

fn cpu_at_zero() -> CpuCore {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68000);
    cpu.set_sr(0x2700);
    cpu.pc = 0;
    cpu
}

fn bench_linear<B: BenchBus>(
    label: &str,
    name: &str,
    opcode: u16,
    cycles_per_instr: i32,
    instrs: u64,
) {
    let mut bus = B::filled(opcode);
    let mut cpu = cpu_at_zero();
    cpu.execute(&mut bus, 100_000 * cycles_per_instr);

    let mut cpu = cpu_at_zero();
    let cycles = (instrs as i32) * cycles_per_instr;
    let start = Instant::now();
    let used = cpu.execute(&mut bus, cycles);
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "{label:9} {name:18} {:8.1} M instr/s  cycles={used}",
        instrs as f64 / elapsed / 1_000_000.0
    );
}

fn bench_loop<B: BenchBus>(
    label: &str,
    name: &str,
    words: &[u16],
    cycles_per_iter: i32,
    instrs_per_iter: u64,
    iters: u64,
) {
    let mut bus = B::new();
    for (i, word) in words.iter().enumerate() {
        bus.write_word_at((i * 2) as u32, *word);
    }

    let mut cpu = cpu_at_zero();
    cpu.set_d(0, 3);
    cpu.set_d(1, 2);
    cpu.execute(&mut bus, 10_000 * cycles_per_iter);

    let mut cpu = cpu_at_zero();
    cpu.set_d(0, 3);
    cpu.set_d(1, 2);
    let cycles = (iters as i32) * cycles_per_iter;
    let instrs = iters * instrs_per_iter;
    let start = Instant::now();
    let used = cpu.execute(&mut bus, cycles);
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "{label:9} {name:18} {:8.1} M instr/s  cycles={used}",
        instrs as f64 / elapsed / 1_000_000.0
    );
}

fn bench_batch_loop(label: &str, name: &str, words: &[u16], instrs: u32) {
    bench_batch_loop_at(label, name, words, instrs, 0x100);
}

fn bench_batch_loop_at(label: &str, name: &str, words: &[u16], instrs: u32, code_base: usize) {
    let elapsed = measure_batch_loop_at(words, instrs, code_base);
    println!(
        "{label:9} {name:18} {:8.1} M instr/s",
        instrs as f64 / elapsed / 1_000_000.0
    );
}

fn measure_batch_loop_at(words: &[u16], instrs: u32, code_base: usize) -> f64 {
    let mut bus = LinearMemoryBus::new(0x10000);
    for (i, word) in words.iter().enumerate() {
        bus.write_word_at((code_base + i * 2) as u32, *word);
    }

    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = code_base as u32;
        cpu.set_a(0, 0x4000);
        cpu.set_a(5, 0x1000);
        cpu.set_a(6, 0x5000);
        cpu.set_a(7, 0x8000);
        cpu.set_d(2, 0x8000);
        cpu.set_d(7, 1);
        cpu
    };

    let mut warm_cpu = prepare_cpu();
    // Some callers always watch PC 0 as a clean-exit sentinel. An unrelated
    // watched PC must not force a nonzero self-loop to execute only one
    // iteration per native trace call.
    let warm = warm_cpu.run_batch(&mut bus, 5_000_000, &[0]);
    assert_eq!(warm.instructions, 5_000_000);

    let mut cpu = prepare_cpu();
    let start = Instant::now();
    let result = cpu.run_batch(&mut bus, instrs, &[0]);
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(result.instructions, instrs);
    elapsed
}

fn bench_one_shot_trace(head_ops: usize, instrs: u32) {
    assert!((2..=16).contains(&head_ops));
    let mut words = vec![0x5280; head_ops - 1]; // ADDQ.L #1,D0
    words.push(0x6002); // BRA.B over the padding word
    words.push(0x4E71); // padding, never executed
    words.push(0x5381); // SUBQ.L #1,D1 (interpreted return path)
    let bytes_after_back_branch = (words.len() + 1) * 2;
    let back_disp = -(bytes_after_back_branch as i16);
    assert!((-128..=-1).contains(&back_disp));
    words.push(0x6000 | (back_disp as u8 as u16));

    bench_batch_loop_at(
        "batch",
        &format!("one-shot {head_ops}"),
        &words,
        instrs,
        0x100 + head_ops * 0x40,
    );
}

fn bench_one_shot_displacement_trace(head_ops: usize, instrs: u32) {
    assert!((2..=9).contains(&head_ops));
    let displacement_ops: &[&[u16]] = &[
        &[0x4A2D, 0x0100],         // TST.B $0100(A5)
        &[0x526D, 0x0100],         // ADDQ.W #1,$0100(A5)
        &[0x322D, 0x0100],         // MOVE.W $0100(A5),D1
        &[0x422D, 0x0100],         // CLR.B $0100(A5)
        &[0x1B40, 0x0100],         // MOVE.B D0,$0100(A5)
        &[0x082D, 0x0003, 0x0100], // BTST #3,$0100(A5)
    ];
    let mut words = Vec::new();
    for i in 0..head_ops - 1 {
        words.extend_from_slice(displacement_ops[i % displacement_ops.len()]);
    }
    words.push(0x6002); // BRA.B over the padding word
    words.push(0x4E71); // padding, never executed
    words.push(0x5381); // SUBQ.L #1,D1 (interpreted return path)
    let bytes_after_back_branch = (words.len() + 1) * 2;
    let back_disp = -(bytes_after_back_branch as i16);
    assert!((-128..=-1).contains(&back_disp));
    words.push(0x6000 | (back_disp as u8 as u16));

    bench_batch_loop_at(
        "batch",
        &format!("d16(An) one-shot {head_ops}"),
        &words,
        instrs,
        0x800 + head_ops * 0x40,
    );
}

/// Exercise the complete application-style trace round trip: validate and
/// enter one non-self-looping native trace, return to the decoded Rust loop,
/// execute a short tail, and take a backward branch to probe the trace again.
fn bench_trace_roundtrip(head_ops: usize, instrs: u32) {
    assert!((3..=16).contains(&head_ops));
    let mut words = vec![0x5280; head_ops - 1]; // ADDQ.L #1,D0
    words.extend_from_slice(&[
        0x51CF, 0x0004, // DBF D7, reset (terminal non-self-loop trace op)
        0x4E71, // padding, skipped by the taken DBF
        0x7E01, // reset: MOVEQ #1,D7 (interpreted tail)
    ]);
    let bytes_after_back_branch = (words.len() + 1) * 2;
    let back_disp = -(bytes_after_back_branch as i16);
    assert!((-128..=-1).contains(&back_disp));
    words.push(0x6000 | (back_disp as u8 as u16));

    bench_batch_loop_at(
        "batch",
        &format!("roundtrip {head_ops}"),
        &words,
        instrs,
        0x1000 + head_ops * 0x40,
    );
}

fn blocked_roundtrip(prefix_ops: usize, blocker: &[u16]) -> Vec<u16> {
    let mut words = vec![0x5280; prefix_ops]; // traceable ADDQ.L #1,D0 prefix
    words.extend_from_slice(blocker);
    words.extend_from_slice(&[
        0x51CF, 0x0004, // DBF D7, reset (terminal non-self-loop trace op)
        0x4E71, // padding, skipped by the taken DBF
        0x7E01, // reset: MOVEQ #1,D7 (interpreted tail)
    ]);
    let bytes_after_back_branch = (words.len() + 1) * 2;
    let back_disp = -(bytes_after_back_branch as i16);
    assert!((-128..=-1).contains(&back_disp));
    words.push(0x6000 | (back_disp as u8 as u16));
    words
}

fn blocked_self_loop(prefix_ops: usize, blocker: &[u16]) -> Vec<u16> {
    let mut words = vec![0x5280; prefix_ops];
    words.extend_from_slice(blocker);
    let bytes_after_back_branch = (words.len() + 1) * 2;
    let back_disp = -(bytes_after_back_branch as i16);
    assert!((-128..=-1).contains(&back_disp));
    words.push(0x6000 | (back_disp as u8 as u16));
    words
}

/// Measure a rejected-trace shape with 24 traceable operations before
/// `ASR.W #1,D7`, then seven more before `LSL.L #3,D0`. The surrounding
/// ADDQs are synthetic so the benchmark isolates the cost of rejecting
/// versus compiling that topology.
fn bench_immediate_shift_trace() {
    let mut words = vec![0x5280; 24];
    words.push(0xE247);
    words.extend(std::iter::repeat_n(0x5280, 7));
    words.push(0xE788);
    let bytes_after_back_branch = (words.len() + 1) * 2;
    let back_disp = -(bytes_after_back_branch as i16);
    assert!((-128..=-1).contains(&back_disp));
    words.push(0x6000 | (back_disp as u8 as u16));
    bench_batch_loop_at(
        "batch",
        "shift blockers p24/32",
        &words,
        200_000_000,
        0x7000,
    );
}

/// Measure seven traceable operations followed by `ADD.W d16(A5),D7`.
/// The ADDQ prefix is synthetic so the benchmark isolates the cost of
/// admitting its memory-source ADD rather than rejecting the whole trace.
fn bench_memory_add_trace() {
    let words = blocked_self_loop(7, &[0xDE6D, 0x0100]);
    bench_batch_loop_at("batch", "ADD.W d16(A5),D7 p7", &words, 200_000_000, 0x7800);
}

/// Isolate the largest rejected trace after memory-source ADD was admitted:
/// ten traceable operations followed by `SUB.W d16(A5),D4`.
fn bench_memory_sub_trace() {
    let words = blocked_self_loop(10, &[0x986D, 0x0100]);
    bench_batch_loop_at("batch", "SUB.W d16(A5),D4 p10", &words, 200_000_000, 0x7A00);
}

/// Measure a compiler-shaped unsigned fixed-point state update. Dividing the
/// fixed-point values by 256 becomes `LSR.L #8`; updating a field in place
/// becomes `ADD.L D0,d16(A1)`.
///
/// ```c
/// struct State {
///     uint8_t *base;
///     uint32_t step;
///     uint32_t accumulator;
///     uint16_t *cursor;
/// };
///
/// state->accumulator += state->step >> 8;
/// state->cursor = (uint16_t *)(state->base
///                           + 2 * (state->accumulator >> 8));
/// ```
fn bench_fixed_point_state_update() {
    const CODE_BASE: u32 = 0x7C00;
    const STATE_BASE: u32 = 0x4000;
    const INSTRS: u32 = 100_000_000;
    let words = [
        0x2029, 0x0010, // MOVE.L 16(A1),D0
        0xE088, // LSR.L #8,D0
        0xD1A9, 0x0018, // ADD.L D0,24(A1)
        0x2029, 0x0018, // MOVE.L 24(A1),D0
        0xE088, // LSR.L #8,D0
        0xD080, // ADD.L D0,D0
        0x2069, 0x0008, // MOVEA.L 8(A1),A0
        0xD1C0, // ADDA.L D0,A0
        0x2348, 0x0020, // MOVE.L A0,32(A1)
        0x60E2, // BRA.S loop
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    bus.write_long(STATE_BASE + 0x08, 0x5000);
    bus.write_long(STATE_BASE + 0x10, 0x0200);
    bus.write_long(STATE_BASE + 0x18, 0x0100);

    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(1, STATE_BASE);
        cpu.set_a(7, 0xF000);
        cpu
    };

    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "batch     fixed-point update      {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

#[derive(Clone, Copy)]
enum IndirectJsrMix {
    Register,
    MemoryAlu,
    MemoryHeavy,
}

impl IndirectJsrMix {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "register" => Some(Self::Register),
            "memory-alu" => Some(Self::MemoryAlu),
            "memory-heavy" => Some(Self::MemoryHeavy),
            _ => None,
        }
    }

    fn index(self) -> u32 {
        match self {
            Self::Register => 0,
            Self::MemoryAlu => 1,
            Self::MemoryHeavy => 2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::MemoryAlu => "memory-ALU",
            Self::MemoryHeavy => "memory-heavy",
        }
    }

    fn append_prefix_op(self, words: &mut Vec<u16>, index: usize) {
        match self {
            Self::Register => words.push(0x5280), // ADDQ.L #1,D0
            Self::MemoryAlu if index == 0 => {
                words.extend_from_slice(&[0x986D, 0x0100]); // SUB.W $0100(A5),D4
            }
            Self::MemoryAlu => words.push(0x5280), // ADDQ.L #1,D0
            Self::MemoryHeavy => {
                let op: &[u16] = match index % 4 {
                    0 => &[0x986D, 0x0100], // SUB.W $0100(A5),D4
                    1 => &[0x322D, 0x0100], // MOVE.W $0100(A5),D1
                    2 => &[0x526D, 0x0100], // ADDQ.W #1,$0100(A5)
                    _ => &[0x4A2D, 0x0100], // TST.B $0100(A5)
                };
                words.extend_from_slice(op);
            }
        }
    }
}

/// Measure a non-self-loop region ending in `JSR (A0)`, followed by an RTS
/// and a backward branch that re-enters the region. The three mixes separate
/// fixed trace/call overhead from the savings available for register-only,
/// memory-ALU, and memory-heavy code.
fn measure_indirect_jsr_region(
    mix: IndirectJsrMix,
    head_ops: usize,
    instrs: u32,
    code_base: u32,
) -> f64 {
    assert!((3..=24).contains(&head_ops));
    let mut words = Vec::new();
    for index in 0..head_ops - 1 {
        mix.append_prefix_op(&mut words, index);
    }
    words.push(0x4E90); // JSR (A0)
    let branch_word = words.len();
    let bytes_after_branch = (branch_word + 1) * 2;
    let back_disp = -(bytes_after_branch as i16);
    assert!((-128..=-1).contains(&back_disp));
    words.push(0x6000 | back_disp as u8 as u16); // return path: BRA.S head
    let rts_word = words.len();
    words.push(0x4E75); // subroutine: RTS

    let mut bus = LinearMemoryBus::new(0x10000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(code_base + index as u32 * 2, *word);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = code_base;
        cpu.set_a(0, code_base + rts_word as u32 * 2);
        cpu.set_a(5, 0x1000);
        cpu.set_a(7, 0xF000);
        cpu
    };

    let mut warm_cpu = prepare_cpu();
    let warm = warm_cpu.run_batch(&mut bus, 5_000_000, &[0]);
    assert_eq!(warm.instructions, 5_000_000);

    let mut cpu = prepare_cpu();
    let start = Instant::now();
    let result = cpu.run_batch(&mut bus, instrs, &[0]);
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(result.instructions, instrs);
    elapsed
}

fn bench_indirect_jsr_regions() {
    const INSTRS: u32 = 50_000_000;
    let mixes = [
        IndirectJsrMix::Register,
        IndirectJsrMix::MemoryAlu,
        IndirectJsrMix::MemoryHeavy,
    ];
    for mix in mixes {
        for head_ops in 3usize..=12 {
            bench_indirect_jsr_case(mix, head_ops, INSTRS);
        }
    }
}

fn bench_indirect_jsr_case(mix: IndirectJsrMix, head_ops: usize, instrs: u32) {
    let code_base = 0x4000 + mix.index() * 0x1000 + head_ops as u32 * 0x80;
    let elapsed = measure_indirect_jsr_region(mix, head_ops, instrs, code_base);
    println!(
        "batch     JSR {:12} {head_ops:2} {:8.1} M instr/s",
        mix.label(),
        f64::from(instrs) / elapsed / 1_000_000.0
    );
}

/// Replay three profiled rejected-trace shapes. Instruction budgets preserve
/// their measured rejected-loop ratio (483,003 : 399,980 : 407,271). These
/// are the dynamic backedges minus the initial visit that installs each trace
/// candidate; consulting the actual trace slot avoids undercounting when the
/// PC falls out of the CPU's four-entry skip filter. Each case's instruction
/// budget also includes its full synthetic loop length, avoiding the prefix-
/// length double-weighting that a projected-dispatch ratio causes.
fn bench_profiled_opportunities() {
    const SCALE: u32 = 10;
    let cases = [
        (
            "CMP.B (A0),D1 p5",
            blocked_roundtrip(5, &[0xB210]),
            483_003u32,
            9u32,
        ),
        (
            "CMP.W d16(A6) p4",
            blocked_roundtrip(4, &[0xBC6E, 0x0100]),
            399_980u32,
            8u32,
        ),
        (
            "CMP.B (A0),D1 p2",
            blocked_roundtrip(2, &[0xB210]),
            407_271u32,
            6u32,
        ),
    ];

    let mut total_instrs = 0u64;
    let mut total_elapsed = 0.0;
    for (index, (name, words, rejected_hits, instrs_per_iteration)) in cases.iter().enumerate() {
        let instrs = rejected_hits * instrs_per_iteration * SCALE;
        let elapsed = measure_batch_loop_at(words, instrs, 0x2000 + index * 0x100);
        total_instrs += u64::from(instrs);
        total_elapsed += elapsed;
        println!(
            "batch     {name:18} {:8.1} M instr/s",
            f64::from(instrs) / elapsed / 1_000_000.0
        );
    }
    println!(
        "batch     profiled weighted  {:8.1} M instr/s",
        total_instrs as f64 / total_elapsed / 1_000_000.0
    );
}

/// Optimistic counterpart to `profiled-opportunities`: the same measured
/// blocker mix when the instruction following the captured prefix eventually
/// closes directly back to the trace head. This is the only topology for
/// which memory CMP traces are admitted after the round-trip regression test.
fn bench_profiled_self_loops() {
    const SCALE: u32 = 10;
    let cases = [
        (
            "CMP.B self p5",
            blocked_self_loop(5, &[0xB210]),
            483_003u32,
            7u32,
        ),
        (
            "CMP.W self p4",
            blocked_self_loop(4, &[0xBC6E, 0x0100]),
            399_980u32,
            6u32,
        ),
        (
            "CMP.B self p2",
            blocked_self_loop(2, &[0xB210]),
            407_271u32,
            4u32,
        ),
    ];
    let mut total_instrs = 0u64;
    let mut total_elapsed = 0.0;
    for (index, (name, words, rejected_hits, instrs_per_iteration)) in cases.iter().enumerate() {
        let instrs = rejected_hits * instrs_per_iteration * SCALE;
        let elapsed = measure_batch_loop_at(words, instrs, 0x3000 + index * 0x100);
        total_instrs += u64::from(instrs);
        total_elapsed += elapsed;
        println!(
            "batch     {name:18} {:8.1} M instr/s",
            f64::from(instrs) / elapsed / 1_000_000.0
        );
    }
    println!(
        "batch     profiled self-loop {:8.1} M instr/s",
        total_instrs as f64 / total_elapsed / 1_000_000.0
    );
}

/// The dominant decoded-memory sites remaining after memory-source CMP
/// traces are two-instruction copy/fill loops. Each synthetic outer loop
/// resets its pointers and counter so the measured inner DBRA loop can run
/// indefinitely without leaving the fastmem window.
fn bench_profiled_two_op_memory_loops() {
    const SCALE: u32 = 10;
    let cases = [
        (
            "MOVE.B D1,(A0)+",
            vec![
                0x2042, // MOVEA.L D2,A0
                0x707F, // MOVEQ #127,D0
                0x10C1, // inner: MOVE.B D1,(A0)+
                0x51C8, 0xFFFC, // DBRA D0,inner
                0x60F4, // BRA.S outer
            ],
            3_702_308u32,
        ),
        (
            "MOVE.B (A4)+,(A0)+",
            vec![
                0x2042, // MOVEA.L D2,A0
                0x2842, // MOVEA.L D2,A4
                0x707F, // MOVEQ #127,D0
                0x10DC, // inner: MOVE.B (A4)+,(A0)+
                0x51C8, 0xFFFC, // DBRA D0,inner
                0x60F2, // BRA.S outer
            ],
            4_532_090u32,
        ),
        (
            "MOVE.L (A1)+,(A0)+",
            vec![
                0x2042, // MOVEA.L D2,A0
                0x2242, // MOVEA.L D2,A1
                0x707F, // MOVEQ #127,D0
                0x20D9, // inner: MOVE.L (A1)+,(A0)+
                0x51C8, 0xFFFC, // DBRA D0,inner
                0x60F2, // BRA.S outer
            ],
            2_405_305u32,
        ),
    ];
    let mut total_instrs = 0u64;
    let mut total_elapsed = 0.0;
    for (index, (name, words, loop_iterations)) in cases.iter().enumerate() {
        let instrs = loop_iterations * 2 * SCALE;
        let elapsed = measure_batch_loop_at(words, instrs, 0x4000 + index * 0x100);
        total_instrs += u64::from(instrs);
        total_elapsed += elapsed;
        println!(
            "batch     {name:23} {:8.1} M instr/s",
            f64::from(instrs) / elapsed / 1_000_000.0
        );
    }
    println!(
        "batch     profiled two-op loops   {:8.1} M instr/s",
        total_instrs as f64 / total_elapsed / 1_000_000.0
    );
}

/// Exercise five indexed byte loads, two long and one word register-to-memory
/// ADDs, twelve register-only instructions, and a closing DBRA. Keeping the
/// same 21-instruction shape provides an end-to-end measure of whether tracing
/// the missing memory forms amortizes validation, guards, and native entry.
fn bench_indexed_memory_loop() {
    const INSTRS: u32 = 210_000_000;
    let words = [
        0x2042, // outer: MOVEA.L D2,A0
        0x2442, // MOVEA.L D2,A2
        0x707F, // MOVEQ #127,D0
        0x1832, 0x1000, // inner: MOVE.B 0(A2,D1.W),D4
        0x4E71, 0x4E71, 0x4E71, 0x1832, 0x1001, // MOVE.B 1(A2,D1.W),D4
        0x4E71, 0x4E71, 0xD998, // ADD.L D4,(A0)+
        0x1832, 0x1002, // MOVE.B 2(A2,D1.W),D4
        0x4E71, 0x4E71, 0x4E71, 0x1832, 0x1003, // MOVE.B 3(A2,D1.W),D4
        0x4E71, 0x4E71, 0xD998, // ADD.L D4,(A0)+
        0x1832, 0x1004, // MOVE.B 4(A2,D1.W),D4
        0x4E71, 0x4E71, 0xD958, // ADD.W D4,(A0)+
        0x51C8, 0xFFCC, // DBRA D0,inner
        0x60C2, // BRA.S outer
    ];
    bench_batch_loop_at("batch", "indexed memory loop", &words, INSTRS, 0x7000);
}

/// Exercise a MOVEM.W that loads seven signed lookup indexes, seven indexed
/// byte MOVEs that write looked-up values contiguously, and a closing DBRA.
/// Keeping MOVEM in the complete loop ensures the benchmark covers trace
/// admission as well as the indexed operations.
fn bench_movem_indexed_loop() {
    const INSTRS: u32 = 210_000_000;
    const CODE_BASE: u32 = 0x7000;
    let words = [
        0x204B, // outer: MOVEA.L A3,A0 (index-list source)
        0x224C, // MOVEA.L A4,A1 (byte destination)
        0x244D, // MOVEA.L A5,A2 (lookup table)
        0x707F, // MOVEQ #127,D0
        0x4C98, 0x00FE, // inner: MOVEM.W (A0)+,D1-D7
        0x12F2, 0x1000, // MOVE.B 0(A2,D1.W),(A1)+
        0x12F2, 0x2000, // MOVE.B 0(A2,D2.W),(A1)+
        0x12F2, 0x3000, // MOVE.B 0(A2,D3.W),(A1)+
        0x12F2, 0x4000, // MOVE.B 0(A2,D4.W),(A1)+
        0x12F2, 0x5000, // MOVE.B 0(A2,D5.W),(A1)+
        0x12F2, 0x6000, // MOVE.B 0(A2,D6.W),(A1)+
        0x12F2, 0x7000, // MOVE.B 0(A2,D7.W),(A1)+
        0x51C8, 0xFFDE, // DBRA D0,inner
        0x60D2, // BRA.S outer
    ];
    let mut bus = LinearMemoryBus::new(0x10000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(3, 0x4000);
        cpu.set_a(4, 0x5000);
        cpu.set_a(5, 0x6000);
        cpu.set_a(7, 0x8000);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "batch     MOVEM indexed loop      {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// Exercise the register-to-memory subtract loop the profile flagged: four
/// `SUB.L D0,(A0)` against one cell between a MOVEA reload and a DBRA, so
/// the admitted form dominates the trace rather than the loop plumbing.
fn bench_sub_reg_to_mem_loop() {
    // 326,594 outer cycles of 643 instructions end exactly at the outer
    // label, with 512 subtracts per cycle applied to the same cell.
    const CYCLES: u32 = 326_594;
    const INSTRS: u32 = CYCLES * 643;
    const CODE_BASE: u32 = 0x7000;
    let words = [
        0x204B, // outer: MOVEA.L A3,A0
        0x727F, // MOVEQ #127,D1
        0x9190, // inner: SUB.L D0,(A0)
        0x9190, // SUB.L D0,(A0)
        0x9190, // SUB.L D0,(A0)
        0x9190, // SUB.L D0,(A0)
        0x51C9, 0xFFF6, // DBRA D1,inner
        0x60EE, // BRA.S outer
    ];
    let mut bus = LinearMemoryBus::new(0x10000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_d(0, 1);
        cpu.set_a(3, 0x4000);
        cpu.set_a(7, 0x8000);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    bus.write_long(0x4000, 0);
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(cpu.pc, CODE_BASE, "the run ends exactly at the outer label");
    assert_eq!(
        bus.read_long(0x4000),
        0u32.wrapping_sub(512 * CYCLES),
        "every subtract landed on the cell"
    );
    println!(
        "batch     SUB reg-to-mem loop     {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// Exercise the brief-indexed store forms the profiles flagged: a MOVE.W
/// to a scaled-index destination and a CLR.B one byte beside it, swept by
/// the DBRA counter so every iteration stores through a different address.
fn bench_indexed_dest_store_loop() {
    // 542,635 outer cycles of 387 instructions end exactly at the outer
    // label. Each inner iteration writes D0 to (A0 + 2*D1) and clears the
    // low byte, so the final words hold D0 with a zeroed low byte.
    const CYCLES: u32 = 542_635;
    const INSTRS: u32 = CYCLES * 387;
    const CODE_BASE: u32 = 0x7000;
    let words = [
        0x204B, // outer: MOVEA.L A3,A0
        0x727F, // MOVEQ #127,D1
        0x3180, 0x1200, // inner: MOVE.W D0,(0,A0,D1.W*2)
        0x4230, 0x1201, // CLR.B (1,A0,D1.W*2)
        0x51C9, 0xFFF6, // DBRA D1,inner
        0x60EE, // BRA.S outer
    ];
    let mut bus = LinearMemoryBus::new(0x10000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_d(0, 0x0000_BEEF);
        cpu.set_a(3, 0x4000);
        cpu.set_a(7, 0x8000);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(cpu.pc, CODE_BASE, "the run ends exactly at the outer label");
    assert_eq!(
        bus.read_word(0x4000),
        0xBE00,
        "the store went through and the clear zeroed its low byte"
    );
    assert_eq!(
        bus.read_word(0x4000 + 254),
        0xBE00,
        "the sweep reached D1=127"
    );
    println!(
        "batch     indexed dest stores     {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// Exercise the predecrement CLR forms the gameplay profile flagged:
/// CLR.L -(SP) and CLR.W -(SP) with an ADDQ restoring the stack pointer
/// each iteration, so the loop runs indefinitely at a fixed SP.
fn bench_clr_predec_loop() {
    // 408,560 outer cycles of 514 instructions end exactly at the outer
    // label with SP restored; the six bytes below SP end cleared.
    const CYCLES: u32 = 408_560;
    const INSTRS: u32 = CYCLES * 514;
    const CODE_BASE: u32 = 0x7000;
    let words = [
        0x727F, // outer: MOVEQ #127,D1
        0x42A7, // inner: CLR.L -(SP)
        0x4267, // CLR.W -(SP)
        0x5C8F, // ADDQ.L #6,SP
        0x51C9, 0xFFF8, // DBRA D1,inner
        0x60F2, // BRA.S outer
    ];
    let mut bus = LinearMemoryBus::new(0x10000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    for address in 0x3FFA..0x4000 {
        bus.write_byte(address, 0xAA);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(7, 0x4000);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(cpu.pc, CODE_BASE, "the run ends exactly at the outer label");
    assert_eq!(cpu.a(7), 0x4000, "SP restored every iteration");
    for address in 0x3FFA..0x4000 {
        assert_eq!(bus.read_byte(address), 0, "stack slot cleared");
    }
    println!(
        "batch     predec CLR loop         {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// Exercise the immediate-store family the gameplay profile flagged: a
/// word push (MOVE.W #imm,-(SP)) with an ADDQ restoring SP, and an
/// indexed immediate store swept by the DBRA counter.
fn bench_move_imm_store_loop() {
    // Outer cycles of 514 instructions (128 four-op iterations plus the
    // reload and restart) end exactly at the head with SP restored.
    const CYCLES: u32 = 350_000;
    const INSTRS: u32 = CYCLES * 514;
    const CODE_BASE: u32 = 0x7000;
    let words = [
        0x3F3C, 0x1111, // head: MOVE.W #$1111,-(SP)
        0x31BC, 0x0042, 0x1200, // MOVE.W #$42,(0,A0,D1.W*2)
        0x548F, // ADDQ.L #2,SP
        0x51C9, 0xFFF2, // DBRA D1,head
        0x727F, // MOVEQ #127,D1 (reload: D1 is both counter and index)
        0x60EC, // BRA.S head
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(0, 0x4000);
        cpu.set_a(7, 0x8000);
        cpu.set_d(1, 0x7F);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(cpu.a(7), 0x8000, "SP restored every iteration");
    assert_eq!(bus.read_word(0x4000), 0x0042, "indexed immediate landed");
    assert_eq!(
        bus.read_word(0x7FFE),
        0x1111,
        "push landed below initial SP"
    );
    println!(
        "batch     imm store loop          {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// The ROM-prologue frame shape: LINK, a store through the frame pointer,
/// UNLK, loop. Base cannot admit LINK (the head blocks at op 0) and
/// interprets the whole loop; with admission the frame ops compile.
fn bench_link_unlk_frame_loop() {
    const INSTRS: u32 = 100_000_000;
    const CODE_BASE: u32 = 0x6000;
    let words = [
        0x4E56, 0xFFF8, // head: LINK A6,#-8
        0x5283, // ADDQ.L #1,D3
        0x3D43, 0xFFFC, // MOVE.W D3,-4(A6)
        0x4E5E, // UNLK A6
        0x51C8, 0xFFF2, // DBRA D0,head
        0x707F, // MOVEQ #127,D0
        0x60EC, // BRA.S head
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(7, 0x8000);
        cpu.set_d(0, 0x7F);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert!(cpu.d(3) > 5_000, "the loop actually iterated");
    // The budget may stop mid-iteration; the frame slot trails D3 by at
    // most one.
    assert!(
        (cpu.d(3) & 0xFFFF).abs_diff(u32::from(bus.read_word(0x7FF8))) <= 1,
        "the frame store lands every iteration"
    );
    println!(
        "batch     link/unlk frame loop    {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// Exercise the retry-gated call-through: a loop whose body calls a
/// two-op leaf through BSR.W. Base cannot record past the call and
/// interprets the whole loop; with call-through the push, leaf, checked
/// return, and tail compile as one native trace.
fn bench_call_through_leaf() {
    const INSTRS: u32 = 100_000_000;
    const CODE_BASE: u32 = 0x6000;
    let words = [
        0x6100, 0x000C, // head: BSR.W leaf
        0x5283, // ADDQ.L #1,D3
        0x51C8, 0xFFF8, // DBRA D0,head
        0x707F, // MOVEQ #127,D0
        0x60F2, // BRA.S head
        0x5282, // leaf: ADDQ.L #1,D2
        0x4E75, // RTS
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(7, 0x8000);
        cpu.set_d(0, 0x7F);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert!(
        cpu.d(2).abs_diff(cpu.d(3)) <= 1,
        "leaf and tail in lockstep"
    );
    println!(
        "batch     call-through leaf       {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// The call-through shape that needs two SMC intervals: the leaf sits at
/// BSR.W's maximum forward reach and the loop body stores into the gap
/// between the code regions. A unified caller..callee interval would
/// false-bail that store every iteration (and the old per-call span cap
/// rejected the far call outright), so base interprets the whole loop.
fn bench_far_call_through_leaf() {
    const INSTRS: u32 = 100_000_000;
    const CODE_BASE: u32 = 0x6000;
    const LEAF: u32 = CODE_BASE + 0x8000;
    const GAP_SLOT: u32 = 0xA000;
    let bsr_disp = (LEAF - (CODE_BASE + 2)) as u16;
    let dbra_disp = CODE_BASE.wrapping_sub(CODE_BASE + 0x0A) as u16;
    let bra_disp = CODE_BASE.wrapping_sub(CODE_BASE + 0x10) as u8;
    let caller = [
        0x6100,
        bsr_disp, // head: BSR.W leaf
        0x5283,   // ADDQ.L #1,D3
        0x3083,   // MOVE.W D3,(A0)  (store into the gap)
        0x51C8,
        dbra_disp,                    // DBRA D0,head
        0x707F,                       // MOVEQ #127,D0
        0x6000 | u16::from(bra_disp), // BRA.S head
    ];
    let leaf_words = [
        0x5282, // leaf: ADDQ.L #1,D2
        0x4E75, // RTS
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in caller.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    for (index, word) in leaf_words.iter().enumerate() {
        bus.write_word_at(LEAF + index as u32 * 2, *word);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(7, 0x8000);
        cpu.set_a(0, GAP_SLOT);
        cpu.set_d(0, 0x7F);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert!(
        cpu.d(2).abs_diff(cpu.d(3)) <= 1,
        "leaf and tail in lockstep"
    );
    // The budget may stop between the ADDQ and the store, so the stored
    // word trails D3 by at most one.
    assert!(
        (cpu.d(3) & 0xFFFF).abs_diff(u32::from(bus.read_word(GAP_SLOT))) <= 1,
        "the gap store lands every iteration"
    );
    println!(
        "batch     far call-through+store  {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// The field shape salvage targets: a store-heavy body with one interior
/// branch, then an instruction the decoder refuses (LEA (abs).L) -- the
/// ROM regions the gameplay profile shows dying 14-26 ops deep. Base
/// rejects the whole head and interprets; with salvage the twelve-op
/// prefix through the branch compiles and only the tail stays
/// interpreted. Two register tests sit between the branch and the
/// blocker so the recording's last op is not a terminal: without salvage
/// the whole head rejects. (A register-only nine-op variant measures
/// ~1.0x -- trace entry/exit cost cancels the win on cheap ops.)
fn bench_salvaged_prefix_loop() {
    const INSTRS: u32 = 100_000_000;
    const CODE_BASE: u32 = 0x6000;
    let words = [
        0x3083, // head: MOVE.W D3,(A0)
        0x5283, // ADDQ.L #1,D3
        0x3143, 0x0002, // MOVE.W D3,2(A0)
        0x5284, // ADDQ.L #1,D4
        0x3144, 0x0004, // MOVE.W D4,4(A0)
        0x5285, // ADDQ.L #1,D5
        0x3145, 0x0006, // MOVE.W D5,6(A0)
        0x5286, // ADDQ.L #1,D6
        0x3146, 0x0008, // MOVE.W D6,8(A0)
        0x5281, // ADDQ.L #1,D1
        0x4A41, // TST.W D1
        0x6602, // BNE.S +2 (taken until D1 wraps 16 bits)
        0x4E71, // NOP (skipped)
        0x4A42, // TST.W D2 -- past the branch: master has no terminal here
        0x4A42, // TST.W D2
        0x4E57, 0x0000, // LINK A7,#0 -- refused by design (A7 exclusion)
        0x4E5F, // UNLK A7
        0x51C8, 0xFFD2, // DBRA D0,head
        0x707F, // MOVEQ #127,D0
        0x60CC, // BRA.S head
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }

    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(0, 0x4000);
        cpu.set_a(7, 0x8000);
        cpu.set_d(0, 0x7F);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert!(cpu.d(3) > 5_000, "the loop actually iterated");
    // After iteration one the interpreted LEA holds A0 at $3000, so the
    // steady-state head store lands there.
    assert!(
        (cpu.d(3) & 0xFFFF).abs_diff(u32::from(bus.read_word(0x3000))) <= 1
            || (cpu.d(3) & 0xFFFF).abs_diff(u32::from(bus.read_word(0x3000))) >= 0xFFFF,
        "the head store lands every iteration"
    );
    println!(
        "batch     salvaged prefix loop    {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// A mixed-path loop whose conditional branch alternates every iteration
/// (EORI #1 flips Z), so whichever spine the head trace records, it
/// guard-exits on half of all calls forever -- below the adaptive
/// re-record ratio, above any useful coverage. The continuation between
/// the branch join and the DBRA is what exit-seeded candidacy compiles.
fn bench_mixed_path_loop() {
    const CODE_BASE: u32 = 0x6000;
    const INSTRS: u32 = 100_000_000;
    let words = [
        0x0A01, 0x0001, // head: EORI.B #1,D1
        0x6602, // BNE.S join
        0x5282, // ADDQ.L #1,D2
        // join: a field-shaped continuation -- long enough that a chained
        // native call amortizes its entry boundary, as the stranded
        // continuations in the gameplay profile are.
        0x1ADC, // join: MOVE.B (A4)+,(A5)+
        0x5283, // ADDQ.L #1,D3
        0x1ADC, 0x5283, // x2
        0x1ADC, 0x5283, // x3
        0x1ADC, 0x5283, // x4
        0x1ADC, 0x5283, // x5
        0x1ADC, 0x5283, // x6
        0x51C8, 0xFFDE, // DBRA D0,head
        0x2845, // outer: MOVEA.L D5,A4
        0x2A46, // MOVEA.L D6,A5
        0x707F, // MOVEQ #127,D0
        0x60D4, // BRA.S head
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(4, 0x4000);
        cpu.set_a(5, 0x5000);
        cpu.set_a(7, 0x9000);
        cpu.set_d(0, 127);
        cpu.set_d(5, 0x4000);
        cpu.set_d(6, 0x5000);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "batch     mixed-path loop         {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// Reproduce a path-biased trace that is first recorded through an uncommon
/// conditional edge before settling into a copy loop on the opposite edge.
/// A first-path-only
/// recorder keeps side-exiting after the CMP/branch pair and interprets the
/// MOVE/DBRA pair even though the four-op common path is a profitable loop.
fn bench_trace_branch_bias() {
    const CODE_BASE: u32 = 0x6000;
    const INSTRS: u32 = 100_000_000;
    let words = [
        0xB210, // CMP.B (A0),D1
        0x6606, // BNE.S outer
        0x10DC, // common: MOVE.B (A4)+,(A0)+
        0x51C8, 0xFFF8, // DBRA D0,head
        0x2042, // outer: MOVEA.L D2,A0
        0x2843, // MOVEA.L D3,A4
        0x707F, // MOVEQ #127,D0
        0x5884, // ADDQ.L #4,D4
        0x60EC, // BRA.S head
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }

    let prepare_cpu = |comparison: u32| {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(0, 0x4000);
        cpu.set_a(4, 0x5000);
        cpu.set_d(0, 127);
        cpu.set_d(1, comparison);
        cpu.set_d(2, 0x4000);
        cpu.set_d(3, 0x5000);
        cpu
    };

    // Two uncommon-path iterations are exactly enough to install and record
    // the trace, without exercising it long enough to hide a later phase
    // change from an adaptive policy.
    let mut cpu = prepare_cpu(1);
    let warm = cpu.run_batch(&mut bus, 14, &[0]);
    assert_eq!(warm.instructions, 14);
    assert_eq!(cpu.pc, CODE_BASE);

    cpu.set_d(1, 0);
    let start = Instant::now();
    let result = cpu.run_batch(&mut bus, INSTRS, &[0]);
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(result.instructions, INSTRS);
    println!(
        "batch     biased CMP/copy loop {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// Measure a compiler-shaped bounded record scan with a predicted interior
/// branch and a conditional loop latch. An outer two-instruction loop resets
/// the index so the benchmark repeatedly exercises both the hot path and the
/// terminal exit instead of turning the scan into an unbounded synthetic loop.
///
/// The measured 68k sequence is equivalent to this C-shaped loop:
///
/// ```c
/// struct Record {
///     uint8_t prefix[42];
///     int16_t state;
///     uint8_t suffix[12];
/// };
///
/// for (int16_t i = 0; i < 128; ++i) {
///     struct Record *records = *record_table;
///     if (records[(uint16_t)i].state > 0)
///         uncommon_path();
/// }
/// ```
fn bench_guarded_indexed_scan() {
    const CODE_BASE: u32 = 0x6000;
    const INSTRS: u32 = 100_000_000;
    let words = [
        0x7600, // outer: MOVEQ #0,D3
        0x7038, // scan: MOVEQ #56,D0
        0xC0C3, // MULU.W D3,D0
        0x2079, 0x0000, 0x4000, // MOVEA.L $00004000,A0
        0x41E8, 0x002A, // LEA 42(A0),A0
        0x4A70, 0x0800, // TST.W 0(A0,D0.L)
        0x6F02, // BLE.S latch (common path)
        0x4E71, // uncommon-path placeholder
        0x5243, // latch: ADDQ.W #1,D3
        0x0C43, 0x0080, // CMPI.W #128,D3
        0x6D00, 0xFFE2, // BLT.W scan
        0x7600, // MOVEQ #0,D3
        0x60DC, // BRA.S scan
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    bus.write_long(0x4000, 0x5000);

    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(7, 0xF000);
        cpu
    };

    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "batch     guarded indexed scan    {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// Measure a compiler-shaped address bound check whose longword limit lives
/// in a stack-frame-style displacement slot. The unconditional latch keeps
/// the CMPA memory form at the hot trace boundary without adding unrelated
/// address-register mutation to the measurement.
fn bench_displacement_address_compare() {
    const CODE_BASE: u32 = 0x6C00;
    const INSTRS: u32 = 100_000_000;
    let words = [
        0xB7E9, 0x0010, // CMPA.L 16(A1),A3
        0x60FA, // BRA.S loop
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    bus.write_long(0x4010, 0x5000);

    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(1, 0x4000);
        cpu.set_a(3, 0x5000);
        cpu.set_a(7, 0xF000);
        cpu
    };

    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "batch     displacement CMPA      {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// Measure a read-only compare through a compiler-shaped brief indexed
/// address in the middle of an otherwise traceable record loop. Keeping the
/// surrounding work in the benchmark measures the trace boundary rather than
/// letting native-call overhead dominate a two-instruction self-loop:
///
/// ```c
/// do {
///     uint32_t accumulator = seed3 + seed4 + seed5;
///     if (*(int16_t *)((uint8_t *)records + (int16_t)offset + 4) == value) {
///         ++accumulator;
///     }
/// } while (running);
/// ```
fn bench_indexed_value_compare() {
    const CODE_BASE: u32 = 0x6E00;
    const INSTRS: u32 = 100_000_000;
    let words = [
        0x7600, // MOVEQ #0,D3
        0x7801, // MOVEQ #1,D4
        0x7A02, // MOVEQ #2,D5
        0x7C03, // MOVEQ #3,D6
        0x7E04, // MOVEQ #4,D7
        0x2003, // MOVE.L D3,D0
        0xD084, // ADD.L D4,D0
        0xD085, // ADD.L D5,D0
        0xB270, 0x2004, // CMP.W 4(A0,D2.W),D1
        0x6602, // BNE.S skip increment (recorded not taken)
        0x5280, // ADDQ.L #1,D0
        0x60E6, // BRA.S loop
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    bus.write_word_at(0x4206, 0x1234);

    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(0, 0x4200);
        cpu.set_a(7, 0xF000);
        cpu.set_d(1, 0x1234);
        cpu.set_d(2, 2);
        cpu
    };

    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "batch     indexed value compare   {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// Mirror `bench_indexed_value_compare` with an immediate-to-memory CMPI.W so
/// the two generated traces differ only in the measured comparison. The
/// memory word equals the immediate, keeping the recorded branch not taken.
fn bench_indexed_immediate_compare() {
    const CODE_BASE: u32 = 0x6E00;
    const INSTRS: u32 = 100_000_000;
    let words = [
        0x7600, // MOVEQ #0,D3
        0x7801, // MOVEQ #1,D4
        0x7A02, // MOVEQ #2,D5
        0x7C03, // MOVEQ #3,D6
        0x7E04, // MOVEQ #4,D7
        0x2003, // MOVE.L D3,D0
        0xD084, // ADD.L D4,D0
        0xD085, // ADD.L D5,D0
        0x0C70, 0x1234, 0x2004, // CMPI.W #$1234,4(A0,D2.W)
        0x6602, // BNE.S skip increment (recorded not taken)
        0x5280, // ADDQ.L #1,D0
        0x60E4, // BRA.S loop
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    bus.write_word_at(0x4206, 0x1234);

    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(0, 0x4200);
        cpu.set_a(7, 0xF000);
        cpu.set_d(2, 2);
        cpu
    };

    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "batch     indexed immediate cmp   {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// Measure a JIT-compiled loop that pushes an address with `PEA (d16,A5)`
/// and pops it back with a postincrement load, keeping the stack balanced
/// across iterations while exercising the checked predecrement store. This
/// models a compiler materializing the address of an A5-relative global as
/// a by-reference argument; the popping load stands in for the callee
/// consuming the pushed pointer:
///
/// ```c
/// int32_t count = 0;                  /* MOVEQ #0,D0, once  */
/// for (;;) {
///     ++count;                        /* ADDQ.L #1,D0    */
///     int32_t *arg = &globals->field; /* PEA $40(A5)     */
///     sink = arg;                     /* MOVE.L (A7)+,D3 */
///     ++iterations;                   /* ADDQ.L #1,D1    */
/// }
/// ```
///
/// The instruction budget is one initializer plus a whole number of
/// five-instruction iterations, so the run ends at the loop head and the
/// final counter, pointer, and stack values are exact.
fn bench_pea_displacement_loop() {
    const CODE_BASE: u32 = 0x6E00;
    const ITERATIONS: u32 = 20_000_000;
    const INSTRS: u32 = 1 + ITERATIONS * 5;
    let words = [
        0x7000, // MOVEQ #0,D0
        0x5280, // loop: ADDQ.L #1,D0
        0x486D, 0x0040, // PEA $40(A5)
        0x261F, // MOVE.L (A7)+,D3
        0x5281, // ADDQ.L #1,D1
        0x60F4, // BRA.S loop
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }

    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(5, 0x4200);
        cpu.set_a(7, 0x8000);
        cpu
    };

    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(cpu.d(0), ITERATIONS, "count increments once per iteration");
    assert_eq!(cpu.d(1), ITERATIONS, "iterations increment once per pass");
    assert_eq!(cpu.d(3), 0x4240, "the popped value is the pushed address");
    assert_eq!(cpu.a(7), 0x8000, "the stack is balanced at the loop head");
    println!(
        "batch     pea displacement loop   {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// Measure a JIT-compiled loop whose address computation is an indexed
/// LEA — the register-only form that blocks a deterministic boot-phase
/// head in EV Override profiling. The loop sweeps the word index and
/// wraps it, so the run ends at an exact wrap boundary with fully
/// asserted final state:
///
/// ```c
/// for (;;) {
///     entry = &table[i];        /* LEA (4,A0,D2.W),A1 */
///     if (++i < 0x7FFF) continue; /* ADDQ; CMPI; BLT   */
///     i = 0;                    /* MOVEQ #0,D2         */
/// }
/// ```
fn bench_indexed_lea_loop() {
    const CODE_BASE: u32 = 0x6E00;
    const WRAP_INSTRS: u32 = 4 * 0x7FFF + 2;
    const WRAPS: u32 = 763;
    const INSTRS: u32 = WRAP_INSTRS * WRAPS;
    let words = [
        0x43F0, 0x2004, // loop: LEA (4,A0,D2.W),A1
        0x5282, // ADDQ.L #1,D2
        0x0C42, 0x7FFF, // CMPI.W #$7FFF,D2
        0x6DF4, // BLT.S loop
        0x7400, // MOVEQ #0,D2
        0x60F0, // BRA.S loop
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }

    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(0, 0x4200);
        cpu.set_a(7, 0x8000);
        cpu
    };

    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(cpu.d(2), 0, "the run ends exactly at a wrap boundary");
    assert_eq!(cpu.a(0), 0x4200, "the base register is untouched");
    assert_eq!(
        cpu.a(1),
        0x4200 + 0x8002,
        "the last computed address is the final table entry"
    );
    println!(
        "batch     indexed lea loop        {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// Register-count shifts, the form the profile showed blocking a
/// fixed-point table-lookup loop. The count lives in a register even
/// though it is loop-invariant, so the trace must compute the distance,
/// the shifted-out bit, and the cycle cost at run time.
///
/// C model of the loop body:
/// ```c
/// int32_t d0;          /* value being shifted */
/// int32_t d1 = 16;     /* shift distance, held in a register */
/// int16_t d2;          /* loop counter */
/// do {
///     d0 = table_word >> d1;   /* ASR.L D1,D0 -- arithmetic */
///     d2 += 1;
/// } while (d2 < 0x7FFF);
/// ```
fn bench_register_count_shift_loop() {
    const CODE_BASE: u32 = 0x7200;
    const WRAP_INSTRS: u32 = 4 * 0x7FFF + 2;
    const WRAPS: u32 = 763;
    const INSTRS: u32 = WRAP_INSTRS * WRAPS;
    let words = [
        0xE2A0, // loop: ASR.L D1,D0
        0x5282, // ADDQ.L #1,D2
        0x0C42, 0x7FFF, // CMPI.W #$7FFF,D2
        0x6DF6, // BLT.S loop
        0x7400, // MOVEQ #0,D2
        0x60F2, // BRA.S loop
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }

    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        // A count of one keeps the shifted value from saturating to zero
        // immediately, so every iteration does real work.
        cpu.set_d(0, 0x4000_0000);
        cpu.set_d(1, 1);
        cpu.set_a(7, 0x8000);
        cpu
    };

    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(cpu.d(2), 0, "the run ends exactly at a wrap boundary");
    assert_eq!(cpu.d(1), 1, "the count register is untouched");
    assert_eq!(
        cpu.d(0),
        0,
        "repeated arithmetic right shifts of a positive value saturate to zero"
    );
    println!(
        "batch     register count shift    {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// Measure decoded generic memory operations without allowing a backward
/// branch to turn the workload into a native JIT loop. Each pass walks the
/// same straight-line code, retaining the decoded-op cache while resetting
/// only the architectural state changed by the instruction stream.
fn bench_generic_memory(name: &str, words: &[u16], instrs_per_pattern: u32) {
    const CODE_BASE: usize = 0x100;
    const PATTERNS: u32 = 16_384;
    const PASSES: u32 = 512;
    let mut bus = LinearMemoryBus::new(0x10_0000);
    for pattern in 0..PATTERNS as usize {
        for (word, value) in words.iter().enumerate() {
            bus.write_word_at(
                (CODE_BASE + (pattern * words.len() + word) * 2) as u32,
                *value,
            );
        }
    }

    let instrs_per_pass = PATTERNS * instrs_per_pattern;
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);
    cpu.set_sr(0x2700);
    cpu.pc = CODE_BASE as u32;
    cpu.set_a(0, 0x80000);
    let warm = cpu.run_batch(&mut bus, instrs_per_pass, &[0]);
    assert_eq!(warm.instructions, instrs_per_pass);

    let start = Instant::now();
    for _ in 0..PASSES {
        cpu.pc = CODE_BASE as u32;
        cpu.set_a(0, 0x80000);
        let result = cpu.run_batch(&mut bus, instrs_per_pass, &[0]);
        assert_eq!(result.instructions, instrs_per_pass);
    }
    let elapsed = start.elapsed().as_secs_f64();
    let instrs = u64::from(instrs_per_pass) * u64::from(PASSES);
    println!(
        "batch     {name:18} {:8.1} M instr/s",
        instrs as f64 / elapsed / 1_000_000.0
    );
}

fn bench_set<B: BenchBus>(label: &str) {
    bench_linear::<B>(label, "linear NOP", 0x4E71, 4, 40_000_000);
    bench_linear::<B>(label, "linear ADDQ", 0x5280, 4, 40_000_000);
    bench_linear::<B>(label, "linear MOVEQ", 0x7001, 4, 40_000_000);
    bench_loop::<B>(label, "loop ADDQ/BRA", &[0x5280, 0x60FC], 14, 2, 30_000_000);
    bench_loop::<B>(label, "loop TST/BNE", &[0x4A80, 0x66FC], 14, 2, 30_000_000);
    bench_loop::<B>(
        label,
        "loop TST/BNE.W",
        &[0x4A80, 0x6600, 0xFFFC],
        14,
        2,
        30_000_000,
    );
    bench_loop::<B>(
        label,
        "loop reg mix",
        &[0x2400, 0xD481, 0x5282, 0xB182, 0x4A82, 0x60F4],
        30,
        6,
        12_500_000,
    );
}

/// Exercise the absolute-addressed TST forms the gameplay census
/// flagged (4A39/4A79 heads): all three widths against fixed targets.
fn bench_tst_abs_loop() {
    // Outer cycles of 514 instructions end exactly at the outer label
    // with the targets untouched (TST writes nothing).
    const CYCLES: u32 = 408_560;
    const INSTRS: u32 = CYCLES * 514;
    const CODE_BASE: u32 = 0x7000;
    let words = [
        0x727F, // outer: MOVEQ #127,D1
        0x4A78, 0x4000, // inner: TST.W ($4000).W
        0x4AB9, 0x0000, 0x4004, // TST.L ($4004).L
        0x4A39, 0x0000, 0x4003, // TST.B ($4003).L
        0x51C9, 0xFFEE, // DBRA D1,inner
        0x60E8, // BRA.S outer
    ];
    let mut bus = LinearMemoryBus::new(0x10000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    for address in 0x4000..0x4008 {
        bus.write_byte(address, 0xAA);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(cpu.pc, CODE_BASE, "the run ends exactly at the outer label");
    for address in 0x4000..0x4008 {
        assert_eq!(bus.read_byte(address), 0xAA, "TST writes nothing");
    }
    println!(
        "batch     absolute TST loop       {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// The ROM caller-save idiom the gameplay profile names as the top
/// blocked class after LINK admission: MOVEM.L push, a short body,
/// MOVEM.L pop, loop. Base cannot admit the register-mask forms and
/// interprets the whole loop.
fn bench_movem_frame_loop() {
    const INSTRS: u32 = 100_000_000;
    const CODE_BASE: u32 = 0x6000;
    let words = [
        0x48E7, 0x3020, // head: MOVEM.L D2/D3/A2,-(SP)
        0x5284, // ADDQ.L #1,D4
        0x5285, // ADDQ.L #1,D5
        0x3684, // MOVE.W D4,(A3)
        0x4CDF, 0x040C, // MOVEM.L (SP)+,D2/D3/A2
        0x51C8, 0xFFF0, // DBRA D0,head
        0x707F, // MOVEQ #127,D0
        0x60EA, // BRA.S head
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(3, 0x4000);
        cpu.set_a(7, 0x8000);
        cpu.set_d(0, 0x7F);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert!(cpu.d(4) > 5_000, "the loop actually iterated");
    assert!(
        cpu.d(4).abs_diff(cpu.d(5)) <= 1,
        "counters advance in lockstep"
    );
    assert!(
        cpu.a(7) == 0x8000 || cpu.a(7) == 0x7FF4,
        "stack balanced or exactly mid-bracket: {:#06x}",
        cpu.a(7)
    );
    println!(
        "batch     movem frame loop        {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// A trap-punctuated loop, a dominant profiled gameplay shape: short
/// straight-line runs separated by A-lines the host emulates. The bench's
/// dispatch loop plays the host (each A-line is a no-op that costs one
/// batch round trip), so the number reflects segment execution plus the
/// per-segment boundary -- the stage-1 trap-crossing question.
fn bench_trap_segment_loop() {
    const INSTRS: u64 = 50_000_000;
    const CODE_BASE: u32 = 0x6000;
    let words = [
        0x5282, // head: ADDQ.L #1,D2
        0x5283, // ADDQ.L #1,D3
        0xD682, // ADD.L D2,D3
        0x2003, // MOVE.L D3,D0- scratch
        0x4A41, // TST.W D1
        0xA123, // trap 1
        0x5285, // ADDQ.L #1,D5
        0xDA83, // ADD.L D3,D5
        0x2005, // MOVE.L D5,D0
        0x4A42, // TST.W D2
        0xA124, // trap 2
        0x5281, // ADDQ.L #1,D1
        0x4A43, // TST.W D3
        0x51CF, 0xFFE4, // DBRA D7,head
        0x707F, // MOVEQ #127,D7
        0x60DE, // BRA.S head
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(7, 0x8000);
        cpu.set_d(7, 0x7F);
        cpu
    };
    let drive = |cpu: &mut CpuCore, bus: &mut LinearMemoryBus, budget: u64| -> u64 {
        let mut retired = 0u64;
        while retired < budget {
            let chunk = (budget - retired).min(4_096) as u32;
            let result = cpu.run_batch(bus, chunk, &[]);
            retired += u64::from(result.instructions);
            match result.exit {
                m68k::BatchExit::BudgetExhausted => {}
                m68k::BatchExit::AlineTrap { .. } => {} // host no-op dispatch
                other => panic!("unexpected exit {other:?}"),
            }
        }
        retired
    };
    let mut warm_cpu = prepare_cpu();
    drive(&mut warm_cpu, &mut bus, 5_000_000);
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    let retired = drive(&mut cpu, &mut bus, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert!(cpu.d(2) > 5_000, "the loop actually iterated");
    assert!(
        cpu.d(1).abs_diff(cpu.d(2)) <= 1,
        "the two per-iteration counters advance in lockstep"
    );
    println!(
        "batch     trap segment loop       {:8.1} M instr/s",
        retired as f64 / elapsed / 1_000_000.0
    );
}

/// The measured real-world segment shape: ~9-op straight-line runs between
/// A-lines (a dominant profiled gameplay pattern), so the boundary
/// cost amortizes over twice the work of the short variant above.
fn bench_trap_segment_loop_long() {
    const INSTRS: u64 = 50_000_000;
    const CODE_BASE: u32 = 0x6000;
    let words = [
        0x5282, // head: ADDQ.L #1,D2
        0x5283, // ADDQ.L #1,D3
        0xD682, // ADD.L D2,D3
        0x2003, // MOVE.L D3,D0 - scratch
        0x4A41, // TST.W D1
        0x5284, // ADDQ.L #1,D4
        0xD883, // ADD.L D3,D4
        0x2004, // MOVE.L D4,D0
        0x4A42, // TST.W D2
        0xA123, // trap 1
        0x5285, // ADDQ.L #1,D5
        0xDA83, // ADD.L D3,D5
        0x2005, // MOVE.L D5,D0
        0x4A44, // TST.W D4
        0x5286, // ADDQ.L #1,D6
        0xDC85, // ADD.L D5,D6
        0x2006, // MOVE.L D6,D0
        0x4A45, // TST.W D5
        0x5287, // ADDQ.L #1,D7 - clobbered by the DBRA counter reload path
        0xA124, // trap 2
        0x5281, // ADDQ.L #1,D1
        0x4A43, // TST.W D3
        0x51CF, 0xFFD2, // DBRA D7,head
        0x7E7F, // MOVEQ #127,D7
        0x60CC, // BRA.S head
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(7, 0x8000);
        cpu.set_d(7, 0x7F);
        cpu
    };
    let drive = |cpu: &mut CpuCore, bus: &mut LinearMemoryBus, budget: u64| -> u64 {
        let mut retired = 0u64;
        while retired < budget {
            let chunk = (budget - retired).min(4_096) as u32;
            let result = cpu.run_batch(bus, chunk, &[]);
            retired += u64::from(result.instructions);
            match result.exit {
                m68k::BatchExit::BudgetExhausted => {}
                m68k::BatchExit::AlineTrap { .. } => {} // host no-op dispatch
                other => panic!("unexpected exit {other:?}"),
            }
        }
        retired
    };
    let mut warm_cpu = prepare_cpu();
    drive(&mut warm_cpu, &mut bus, 5_000_000);
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    let retired = drive(&mut cpu, &mut bus, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert!(cpu.d(2) > 5_000, "the loop actually iterated");
    assert!(
        cpu.d(1).abs_diff(cpu.d(2)) <= 1,
        "the two per-iteration counters advance in lockstep"
    );
    println!(
        "batch     trap segment loop long  {:8.1} M instr/s",
        retired as f64 / elapsed / 1_000_000.0
    );
}

/// PEA of an absolute address, rebalanced -- the profile's second-widest
/// blocked head class after MOVEM. Base blocks the head at the PEA.
fn bench_pea_abs_loop() {
    const INSTRS: u32 = 100_000_000;
    const CODE_BASE: u32 = 0x6000;
    let words = [
        0x4878, 0x3000, // head: PEA ($3000).W
        0x588F, // ADDQ.L #4,A7
        0x5283, // ADDQ.L #1,D3
        0x51C8, 0xFFF6, // DBRA D0,head
        0x707F, // MOVEQ #127,D0
        0x60F0, // BRA.S head
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(7, 0x8000);
        cpu.set_d(0, 0x7F);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert!(cpu.d(3) > 5_000, "the loop actually iterated");
    assert_eq!(
        bus.read_word(0x7FFE),
        0x3000,
        "the pushed constant lands below the rebalanced stack"
    );
    println!(
        "batch     pea abs loop            {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}
/// CMPI.W against a displaced structure field, with a counter mutation
/// so the wait classifier admits the loop. Base blocks the head at the
/// displacement form.
fn bench_cmpi_disp_loop() {
    const INSTRS: u32 = 100_000_000;
    const CODE_BASE: u32 = 0x6000;
    let words = [
        0x0C68, 0x0042, 0x0010, // head: CMPI.W #$42,($10,A0)
        0x6602, // BNE.S +2
        0x4E71, // NOP
        0x5283, // ADDQ.L #1,D3
        0x51C8, 0xFFF2, // DBRA D0,head
        0x707F, // MOVEQ #127,D0
        0x60EC, // BRA.S head
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    bus.write_word_at(0x3010, 0x0042);
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(0, 0x3000);
        cpu.set_a(7, 0x8000);
        cpu.set_d(0, 0x7F);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert!(cpu.d(3) > 5_000, "the loop actually iterated");
    println!(
        "batch     cmpi disp loop          {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}
/// Full-width immediate loads (the `203C` trap-selector shapes), with a
/// counter. Base blocks the head at the first load.
fn bench_move_imm_reg_loop() {
    const INSTRS: u32 = 100_000_000;
    const CODE_BASE: u32 = 0x6000;
    let words = [
        0x323C, 0x8123, // head: MOVE.W #$8123,D1
        0x2A3C, 0x0000, 0xA89F, // MOVE.L #$A89F,D5
        0x5283, // ADDQ.L #1,D3
        0x51C8, 0xFFF2, // DBRA D0,head
        0x707F, // MOVEQ #127,D0
        0x60EC, // BRA.S head
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(7, 0x8000);
        cpu.set_d(0, 0x7F);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert!(cpu.d(3) > 5_000, "the loop actually iterated");
    assert_eq!(cpu.d(5), 0x0000_A89F, "the long load lands");
    println!(
        "batch     move imm reg loop       {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

fn main() {
    println!("m68k microbench");
    let only = std::env::args().nth(1);
    if only.as_deref() == Some("trace-calls") {
        // The trace function returns to the Rust self-loop driver after each
        // iteration, isolating the native call-boundary break-even point.
        // `trace-roundtrips` additionally includes validation, cache probing,
        // and decoded-loop re-entry, as real non-self-loop traces do.
        for head_ops in 2..=9 {
            bench_one_shot_trace(head_ops, 50_000_000);
        }
        return;
    }
    if only.as_deref() == Some("trace-roundtrips") {
        for head_ops in 3..=9 {
            bench_trace_roundtrip(head_ops, 50_000_000);
        }
        return;
    }
    if only.as_deref() == Some("profiled-opportunities") {
        bench_profiled_opportunities();
        return;
    }
    if only.as_deref() == Some("profiled-self-loops") {
        bench_profiled_self_loops();
        return;
    }
    if only.as_deref() == Some("profiled-two-op-loops") {
        bench_profiled_two_op_memory_loops();
        return;
    }
    if only.as_deref() == Some("indexed-memory-loop") {
        bench_indexed_memory_loop();
        return;
    }
    if only.as_deref() == Some("movem-indexed-loop") {
        bench_movem_indexed_loop();
        return;
    }
    if only.as_deref() == Some("trace-branch-bias") {
        bench_trace_branch_bias();
        return;
    }
    if only.as_deref() == Some("guarded-indexed-scan") {
        bench_guarded_indexed_scan();
        return;
    }
    if only.as_deref() == Some("displacement-address-compare") {
        bench_displacement_address_compare();
        return;
    }
    if only.as_deref() == Some("indexed-value-compare") {
        bench_indexed_value_compare();
        return;
    }
    if only.as_deref() == Some("indexed-immediate-compare") {
        bench_indexed_immediate_compare();
        return;
    }
    if only.as_deref() == Some("pea-displacement") {
        bench_pea_displacement_loop();
        return;
    }
    if only.as_deref() == Some("register-count-shift") {
        bench_register_count_shift_loop();
        return;
    }
    if only.as_deref() == Some("link-unlk") {
        bench_link_unlk_frame_loop();
        return;
    }
    if only.as_deref() == Some("call-through") {
        bench_call_through_leaf();
        return;
    }
    if only.as_deref() == Some("far-call-through") {
        bench_far_call_through_leaf();
        return;
    }
    if only.as_deref() == Some("tst-abs") {
        bench_tst_abs_loop();
        return;
    }
    if only.as_deref() == Some("movem") {
        bench_movem_frame_loop();
        return;
    }
    if only.as_deref() == Some("trap-segments") {
        bench_trap_segment_loop();
        return;
    }
    if only.as_deref() == Some("trap-segments-long") {
        bench_trap_segment_loop_long();
        return;
    }
    if only.as_deref() == Some("pea-abs") {
        bench_pea_abs_loop();
        return;
    }
    if only.as_deref() == Some("cmpi-disp") {
        bench_cmpi_disp_loop();
        return;
    }
    if only.as_deref() == Some("move-imm-reg") {
        bench_move_imm_reg_loop();
        return;
    }
    if only.as_deref() == Some("clr-abs") {
        bench_clr_abs_loop();
        return;
    }
    if only.as_deref() == Some("memory-and") {
        bench_memory_and_loop();
        return;
    }
    if only.as_deref() == Some("salvage-prefix") {
        bench_salvaged_prefix_loop();
        return;
    }
    if only.as_deref() == Some("clr-predec") {
        bench_clr_predec_loop();
        return;
    }
    if only.as_deref() == Some("move-imm-store") {
        bench_move_imm_store_loop();
        return;
    }
    if only.as_deref() == Some("indexed-dest-store") {
        bench_indexed_dest_store_loop();
        return;
    }
    if only.as_deref() == Some("sub-reg-to-mem") {
        bench_sub_reg_to_mem_loop();
        return;
    }
    if only.as_deref() == Some("mixed-path") {
        bench_mixed_path_loop();
        return;
    }
    if only.as_deref() == Some("indexed-lea") {
        bench_indexed_lea_loop();
        return;
    }
    if only.as_deref() == Some("immediate-shifts") {
        bench_immediate_shift_trace();
        return;
    }
    if only.as_deref() == Some("memory-add") {
        bench_memory_add_trace();
        return;
    }
    if only.as_deref() == Some("memory-sub") {
        bench_memory_sub_trace();
        return;
    }
    if only.as_deref() == Some("fixed-point-update") {
        bench_fixed_point_state_update();
        return;
    }
    if only.as_deref() == Some("indirect-jsr") {
        match (std::env::args().nth(2), std::env::args().nth(3)) {
            (Some(mix), Some(head_ops)) => {
                let mix = IndirectJsrMix::parse(&mix)
                    .expect("indirect-jsr mix must be register, memory-alu, or memory-heavy");
                let head_ops = head_ops
                    .parse()
                    .expect("indirect-jsr op count must be an integer");
                let instrs = std::env::args()
                    .nth(4)
                    .map(|value| value.parse().expect("instruction count must be an integer"))
                    .unwrap_or(50_000_000);
                bench_indirect_jsr_case(mix, head_ops, instrs);
            }
            (None, None) => bench_indirect_jsr_regions(),
            _ => panic!("indirect-jsr requires both a mix and an operation count"),
        }
        return;
    }
    if only.as_deref() == Some("displacement-trace-calls") {
        for head_ops in 2..=9 {
            bench_one_shot_displacement_trace(head_ops, 50_000_000);
        }
        return;
    }
    if only.as_deref() == Some("generic-memory") {
        bench_generic_memory("TST.B (A0)", &[0x4A10], 1);
        bench_generic_memory("TST.B (A0)+", &[0x4A18], 1);
        bench_generic_memory("TST.B index", &[0x4A30, 0x0000], 1);
        bench_generic_memory("ADD.W (A0),D0", &[0xD050], 1);
        return;
    }
    if only.as_deref() == Some("region") {
        bench_batch_loop(
            "batch",
            "multi-block region",
            &[
                0x5280, // ADDQ.L #1,D0
                0x6602, // BNE.S skip
                0x4E71, // uncommon fallthrough
                0x5281, // skip: ADDQ.L #1,D1
                0x60F6, // BRA.S loop
            ],
            200_000_000,
        );
        return;
    }
    if only.as_deref() != Some("batch") {
        bench_set::<PlainBenchBus>("plain");
        bench_set::<LinearMemoryBus>("linearbus");
    }
    // Exercise displacement-based globals and stack temporaries in a
    // deterministic, self-contained loop.
    if only.as_deref() != Some("legacy") {
        bench_batch_loop(
            "batch",
            "displacement mix",
            &[
                0x4A2D, 0x0100, // TST.B $0100(A5)
                0x082D, 0x0003, 0x0100, // BTST #3,$0100(A5)
                0x1B40, 0x0100, // MOVE.B D0,$0100(A5)
                0x422D, 0x0100, // CLR.B $0100(A5)
                0x322D, 0x0100, // MOVE.W $0100(A5),D1
                0x526D, 0x0100, // ADDQ.W #1,$0100(A5)
                0x2F2D, 0x0100, // MOVE.L $0100(A5),-(A7)
                0x588F, // ADDQ.L #4,A7
                0x60DE, // BRA.B back to the first instruction
            ],
            200_000_000,
        );
    }
}

/// Exercise the absolute-addressed CLR forms the gameplay census
/// flagged (4238/4239 heads): all three widths against fixed targets.
fn bench_clr_abs_loop() {
    // Outer cycles of 514 instructions end exactly at the outer label
    // with the three targets cleared and the guard byte untouched.
    const CYCLES: u32 = 408_560;
    const INSTRS: u32 = CYCLES * 514;
    const CODE_BASE: u32 = 0x7000;
    let words = [
        0x727F, // outer: MOVEQ #127,D1
        0x4278, 0x4000, // inner: CLR.W ($4000).W
        0x42B9, 0x0000, 0x4004, // CLR.L ($4004).L
        0x4239, 0x0000, 0x4003, // CLR.B ($4003).L
        0x51C9, 0xFFEE, // DBRA D1,inner
        0x60E8, // BRA.S outer
    ];
    let mut bus = LinearMemoryBus::new(0x10000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    for address in 0x4000..0x4008 {
        bus.write_byte(address, 0xAA);
    }
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(cpu.pc, CODE_BASE, "the run ends exactly at the outer label");
    for address in (0x4000..0x4002).chain(0x4003..0x4008) {
        assert_eq!(bus.read_byte(address), 0, "target byte cleared");
    }
    assert_eq!(bus.read_byte(0x4002), 0xAA, "untouched byte kept");
    println!(
        "batch     absolute CLR loop       {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}

/// The census exemplar behind twenty-three of the top-forty blocked
/// heads: AND.W of a displaced structure field. Base blocks the head at
/// the AND.
fn bench_memory_and_loop() {
    const INSTRS: u32 = 100_000_000;
    const CODE_BASE: u32 = 0x6000;
    let words = [
        0xC268, 0x0010, // head: AND.W ($10,A0),D1
        0x5283, // ADDQ.L #1,D3
        0x51C8, 0xFFF8, // DBRA D0,head
        0x707F, // MOVEQ #127,D0
        0x60F2, // BRA.S head
    ];
    let mut bus = LinearMemoryBus::new(0x1_0000);
    for (index, word) in words.iter().enumerate() {
        bus.write_word_at(CODE_BASE + index as u32 * 2, *word);
    }
    bus.write_word_at(0x3010, 0x0FF0);
    let prepare_cpu = || {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.set_sr(0x2700);
        cpu.pc = CODE_BASE;
        cpu.set_a(0, 0x3000);
        cpu.set_a(7, 0x8000);
        cpu.set_d(1, 0xFFFF_F0F0);
        cpu.set_d(0, 0x7F);
        cpu
    };
    let mut warm_cpu = prepare_cpu();
    assert_eq!(
        warm_cpu.run_batch(&mut bus, 5_000_000, &[0]).instructions,
        5_000_000
    );
    let mut cpu = prepare_cpu();
    let start = Instant::now();
    assert_eq!(cpu.run_batch(&mut bus, INSTRS, &[0]).instructions, INSTRS);
    let elapsed = start.elapsed().as_secs_f64();
    assert!(cpu.d(3) > 5_000, "the loop actually iterated");
    println!(
        "batch     memory and loop         {:8.1} M instr/s",
        f64::from(INSTRS) / elapsed / 1_000_000.0
    );
}
