# m68k-rs

A safe, pure Rust implementation of the Motorola 68000 family CPU emulator.

One core for transaction-accurate hardware emulation and high-throughput
high-level emulation (HLE).

[![Rust CI](https://github.com/benletchford/m68k-rs/actions/workflows/rust.yml/badge.svg)](https://github.com/benletchford/m68k-rs/actions/workflows/rust.yml)
[![Crates.io](https://img.shields.io/crates/v/m68k.svg)](https://crates.io/crates/m68k)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## Features

- **Complete CPU family support**: M68000 through M68060, including EC/LC variants and the SCC68070
- **Two explicit execution contracts**: transaction-accurate cycle scheduling for hardware emulators, and an instruction-budgeted fast path for HLE
- **Bus-visible accuracy**: 68000/68010 two-word prefetch, model-specific access ordering, and internal clock synchronization through `AddressBus::sync`
- **Memory-safe core**: The interpreter — instruction semantics, decode, exceptions, MMU, FPU — is 100% safe Rust. The optional fast paths (fastmem batch execution and the trace JIT) use a small, contract-documented `unsafe` perimeter, fenced by step-vs-batch equivalence tests
- **FPU emulation**: Software 80-bit extended precision, packed decimal, and model-specific 68881/68882/68040/68060 behavior
- **MMU emulation**: 68030/68040/68060 translation, ATCs, transparent translation, `PTEST`, fault frames, and writeback
- **HLE-ready**: Built-in trap interception for High-Level Emulation
- **Save-state ready**: Optional `serde` support serializes architectural state while rebuilding runtime caches on load
- **Extensively tested**: Validated against multiple industry-standard test suites

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
m68k = "0.14.3"
```

The default build has no JIT compiler dependency. Native applications that use
`run_batch()` can enable Cranelift compilation explicitly:

```toml
[dependencies]
m68k = { version = "0.14.3", features = ["jit"] }
```

With `jit` enabled on x86-64, eligible indirect-dispatch loops and short
return arms are combined in the background by default, including on Windows,
Linux and Intel macOS. Set `M68K_NATIVE_REGIONS=off` before starting the process
to use ordinary trace execution without the extra combination IR or compiler
worker. `0`, empty and unrecognized values also disable combining; `public`,
`on` and `1` explicitly enable it. Configuration is read when the thread-local
JIT is created, so restart the process to change it. Other architectures,
including native Apple Silicon, keep their existing execution path.

The architecture limit is implemented in code, not just a lack of performance
measurements: combining requires checked trace wrappers and retained inline
IR currently generated only on x86-64, and worker startup is also gated to
x86-64. Setting the override on ARM64 does not activate this tier. This does
not disable the ordinary JIT or imply that the combining algorithm cannot be
ported to ARM64; that path needs implementation and validation separately.

This tier preserves exact `run_batch()` instruction limits and observable
memory, watch, trap and fault behavior. As already documented for
`run_batch()`, it does not provide cycle accounting; precise execution APIs
are unchanged. Promotion compiles immutable IR snapshots on a background
worker while existing native traces continue executing. Publication rechecks
the captured trace identities; stale or failed results keep the old tier.
The worker owns its executable memory, with leases keeping installed code
alive. Submission, polling and shutdown do not wait for compilation.
Thread creation, snapshot capture and publication still require foreground
work, and ordinary first-tier compilation remains synchronous. The
`trace-profile` feature retains the original per-head instrumentation and
does not start this worker.


### Basic Usage

```rust
use m68k::{CpuCore, CpuType, AddressBus, StepResult};

// Implement your memory bus
struct MyBus { memory: Vec<u8> }

impl AddressBus for MyBus {
    fn read_byte(&mut self, addr: u32) -> u8 {
        self.memory.get(addr as usize).copied().unwrap_or(0)
    }
    fn write_byte(&mut self, addr: u32, val: u8) {
        if let Some(m) = self.memory.get_mut(addr as usize) { *m = val; }
    }
    fn read_word(&mut self, addr: u32) -> u16 {
        u16::from_be_bytes([self.read_byte(addr), self.read_byte(addr + 1)])
    }
    fn write_word(&mut self, addr: u32, val: u16) {
        let bytes = val.to_be_bytes();
        self.write_byte(addr, bytes[0]);
        self.write_byte(addr + 1, bytes[1]);
    }
    fn read_long(&mut self, addr: u32) -> u32 {
        ((self.read_word(addr) as u32) << 16) | self.read_word(addr + 2) as u32
    }
    fn write_long(&mut self, addr: u32, val: u32) {
        self.write_word(addr, (val >> 16) as u16);
        self.write_word(addr + 2, val as u16);
    }
}

fn main() {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68000);

    let mut bus = MyBus { memory: vec![0; 0x10000] };

    // Set up vectors: SSP at 0x1000, PC at 0x400
    bus.write_long(0, 0x1000);
    bus.write_long(4, 0x400);

    // Write a NOP instruction at 0x400
    bus.write_word(0x400, 0x4E71);

    cpu.reset(&mut bus);

    loop {
        match cpu.step(&mut bus) {
            StepResult::Ok { cycles } => println!("Executed: {} cycles", cycles),
            StepResult::Stopped => break,
            StepResult::AlineTrap { opcode } => println!("A-line trap: {:04X}", opcode),
            StepResult::FlineTrap { opcode } => println!("F-line trap: {:04X}", opcode),
            StepResult::TrapInstruction { trap_num } => println!("TRAP #{}", trap_num),
            StepResult::Breakpoint { bp_num } => println!("BKPT #{}", bp_num),
            StepResult::IllegalInstruction { opcode } => println!("Illegal instruction: {:04X}", opcode),
        }
    }
}
```

For cycle-accurate MC68000 interrupt integration, the host must retain the
instruction's IPL sample until its boundary interrupt decision. Ordinary
polls use a bus access's sample; `AddressBus::ipl_hold_sample` retains an
earlier access when necessary. `AddressBus::ipl_poll_internal` instead takes
and retains a **fresh** sample at the current synchronized internal-cycle
boundary. DIVS uses this for a nonnegative dividend and negative divisor,
except early absolute overflow: the poll precedes the final prefetch by two
CPU clocks, while the fetch position and total instruction duration stay
unchanged. Timing-aware hosts must implement this hook; its default is a
no-op for functional buses. Exception dispatch releases either sample hold
through `AddressBus::ipl_release_sample`.

### High-Level Emulation (HLE)

Intercept traps for OS emulation or debugger integration with CPU/bus access:

```rust
use m68k::{AddressBus, CpuCore, HleHandler};

struct MacToolbox;

// All methods in HleHandler are optional (default return is false).
// Return `true` to indicate the HLE handled the trap (suppressing the hardware exception).
// Return `false` to let the CPU take the standard hardware exception.
impl HleHandler for MacToolbox {
    // Optional: Intercept A-line traps (0xAxxx)
    fn handle_aline(
        &mut self,
        cpu: &mut CpuCore,
        bus: &mut dyn AddressBus,
        opcode: u16,
    ) -> bool {
        println!("A-line trap: {:04X} at PC=0x{:08X}", opcode, cpu.pc);
        // ... implement HLE logic ...
        true // Handled: do NOT take the standard Line-A exception
    }

    // Optional: Intercept TRAP #n instructions
    fn handle_trap(&mut self, _cpu: &mut CpuCore, _bus: &mut dyn AddressBus, trap: u8) -> bool {
        // Example: Only intercept TRAP #0, let #1-15 go to real hardware vectors
        if trap == 0 {
            println!("OS Call (TRAP #0)");
            true // Handled
        } else {
            false // Not handled: CPU will take exception vector 32+n
        }
    }

    // Optional: Intercept F-line traps (coprocessor instructions)
    fn handle_fline(&mut self, _cpu: &mut CpuCore, _bus: &mut dyn AddressBus, opcode: u16) -> bool {
        println!("Generic Coprocessor instruction: {:04X}", opcode);
        true // Handled
    }

    // Optional: Intercept BKPT #n instructions
    fn handle_breakpoint(&mut self, _cpu: &mut CpuCore, _bus: &mut dyn AddressBus, bp: u8) -> bool {
        println!("Breakpoint #{}", bp);
        true // Handled
    }

    // Optional: Intercept ILLEGAL instructions (0x4AFC)
    fn handle_illegal(&mut self, _cpu: &mut CpuCore, _bus: &mut dyn AddressBus, opcode: u16) -> bool {
        println!("Illegal instruction: {:04X}", opcode);
        false // Not handled: CPU will take illegal instruction exception
    }
}

fn emulate(cpu: &mut CpuCore, bus: &mut impl AddressBus) {
    let mut hle = MacToolbox;
    let result = cpu.step_with_hle_handler(bus, &mut hle);
}
```

### Choosing an Approach

| Method | Budget | Execution contract | Host-visible exit |
| :--- | :--- | :--- | :--- |
| **`step()`** | One instruction | Transaction-accurate `AddressBus` accesses and cycles | Surfaces A-line, F-line, `TRAP`, `BKPT`, and illegal instructions without taking their exception |
| **`step_with_hle_handler()`** | One instruction | Same precise path as `step()` | Offers traps to `HleHandler`; unhandled traps take the hardware exception |
| **`execute()`** | CPU cycles | Precise path; whole instructions may overshoot the requested cycles | Takes traps as hardware exceptions and returns consumed cycles |
| **`run_for_cycles()`** | CPU cycles | Precise path with actual cycle and instruction totals | Surfaces traps, STOP, and bus-requested instruction boundaries separately |
| **`run_for_cycles_with_hook()`** | CPU cycles | Same precise path, with host synchronization after each normally completed instruction | The hook can continue or request an instruction-boundary return |
| **`run_for_cycles_with_boundary_hook()`** | CPU cycles | Same precise path, with separate instruction and interrupt-entry events | The hook can synchronize or return before the first handler instruction |
| **`run_batch()`** | Instructions | Throughput path using decoded-op caching, optional direct RAM, and portable or `jit`-enabled native hot-loop traces | Surfaces traps, STOP, watched PCs, or budget exhaustion |

Use **`step()`** for debugger-style control. Use **`run_for_cycles()`** when a
machine scheduler needs to advance the CPU by a clock budget without losing
bus ordering or trap state:

```rust
use m68k::CycleBatchExit;

let result = cpu.run_for_cycles(&mut bus, 512);
match result.exit {
    CycleBatchExit::BudgetExhausted => {
        // result.cycles may be greater than 512: instructions are never split.
    }
    CycleBatchExit::Stopped => {
        // Wait for a serviceable interrupt.
    }
    CycleBatchExit::BoundaryRequested => {
        // Apply work queued by the bus before resuming the CPU.
    }
    event => {
        // Handle a surfaced trap. The trapping instruction is not included
        // in result.instructions and no exception-entry cycles were charged.
        println!("{event:?}");
    }
}
```

An `AddressBus` can return `true` from `take_boundary_request()` when a bus
access discovers host work that must run after the current instruction or
interrupt entry and before another instruction executes. The request is
checked after a normally completed instruction and after an interrupt serviced
on batch entry. Completed cycles are included; interrupt entry does not add to
the retirement count. The request takes precedence over cycle budget
exhaustion. Implementations should consume the request while retaining the
associated work until the host handles the boundary exit.

The 68000 and 68010 may already have instruction words in their hardware
prefetch queue at this boundary. If the host work changes instruction-visible
memory or mapping and those queued words must not be used, call
`cpu.invalidate_prefetch()` before resuming.

Use **`run_for_cycles_with_hook()`** when devices or IRQ lines must be updated
between instructions rather than after an aggregate batch:

```rust
use m68k::CycleBatchControl;

let result = cpu.run_for_cycles_with_hook(&mut bus, 512, |cpu, bus, cycles| {
    bus.advance_devices(cycles);
    cpu.set_irq(bus.interrupt_level());

    if bus.monitor_requested() {
        CycleBatchControl::Return
    } else {
        CycleBatchControl::Continue
    }
});
```

The hook receives the cycles for the just-completed instruction and can inspect
`cpu.ppc` and `cpu.pc`. Its CPU and bus updates are applied before another
instruction is fetched. A hook-requested return uses
`CycleBatchExit::BoundaryRequested`, includes the completed instruction exactly
once, and resumes at the following instruction. Interrupt-entry cycles are
included in `result.cycles` but are not reported as instruction cycles to the
hook. The original `run_for_cycles()` path has no runtime hook check.

Use **`run_for_cycles_with_boundary_hook()`** when the host must also advance
devices for interrupt-entry cycles before the first handler instruction:

```rust
use m68k::{CycleBatchControl, CycleBoundaryEvent};

let result = cpu.run_for_cycles_with_boundary_hook(&mut bus, 512, |cpu, bus, event| {
    let cycles = match event {
        CycleBoundaryEvent::Instruction { cycles }
        | CycleBoundaryEvent::InterruptEntry { cycles } => cycles,
    };
    bus.advance_devices(cycles);
    cpu.set_irq(bus.interrupt_level());
    CycleBatchControl::Continue
});
```

Interrupt entry contributes no retired instruction. Returning from its event
stops before the first handler instruction, and resuming executes that
instruction normally.

Use **`step_with_hle_handler()`** when patching selected guest OS calls while
allowing every unhandled trap to follow hardware behavior. Use
**`run_batch()`** for HLE workloads where host-call latency and instruction
throughput matter more than observing the physical prefetch bus.

### Direct RAM and observed writes

`run_batch()` prefers `AddressBus::fast_mem()`, whose `FastMem` window permits
side-effect-free direct reads and writes. With native JIT enabled on 68020 and
later CPUs, a bus that needs write bookkeeping can instead return `TrackedMem`
from `AddressBus::tracked_mem()`. Compiled code reads RAM directly and calls
`write_byte/word/long` for stores; memory-to-memory MOVE also retains
`begin_memory_copy`/`end_memory_copy` notifications. This supports metadata such
as retained graphics coverage and write journals without interpreting the loop.

Both capabilities are opt-in. `TrackedMem` requires stable, side-effect-free
reads and infallible writes with the restrictions documented on the type;
faulting, relocating, MMIO, and read-observing buses must decline it. Pre-68020
store sequencing retains the ordinary path. A compiled memory trace is
specialized for its write mode and re-recorded if that mode changes between
batches, keeping notification checks out of direct stores.

Either window is indexed by physical address. With the PMMU enabled, each
access through it is translated first by the transparent-translation registers
and the ATC alone, never a table search. An access they cannot map into the
window -- an ATC miss, a protection violation, a physical address outside it,
one spanning two pages -- takes the ordinary path, which searches the tables
and raises any fault with its exact frame. While the window translates, no ATC
hit records a use in the 68030's replacement order -- not through the window,
a compiled trace or the ordinary path -- so entries age by when table searches
loaded them, and the saved order does not depend on what the trace JIT
compiled. Outside `run_batch` replacement stays least recently used.
Memory-to-memory MOVE notifications
are omitted under translation, as on the ordinary path. Traces are compiled
for translated or physical addresses and re-recorded when the PMMU is switched
between batches.

## Supported CPU Types

| CPU        | Description                            |
| ---------- | -------------------------------------- |
| `M68000`   | Original 68000 (24-bit address bus)    |
| `M68010`   | 68010 with virtual memory support      |
| `M68EC020` | 68020 embedded controller (no MMU)     |
| `M68020`   | Full 68020 with 32-bit address bus     |
| `M68EC030` | 68030 embedded controller (no MMU)     |
| `M68030`   | Full 68030 with on-chip MMU            |
| `M68EC040` | 68040 embedded controller (no FPU/MMU) |
| `M68LC040` | 68040 lite (no FPU)                    |
| `M68040`   | Full 68040 with FPU and MMU            |
| `M68060`   | Superscalar 68060 with FPU and MMU     |
| `SCC68070` | Philips SCC68070 variant               |

## Validation & Testing

This emulator has been rigorously validated against multiple industry-standard test suites to ensure correctness:

### SingleStepTests (m68000)

The [SingleStepTests](https://github.com/SingleStepTests/m68000) project
provides exhaustive per-instruction fixtures from MAME's microcoded 68000 core.
The suite covers every supplied instruction file and 261,894 cases, including:

- All addressing modes and operand sizes
- Edge cases for condition codes (CCR/SR)
- BCD arithmetic (ABCD, SBCD, NBCD)
- Multiply/divide overflow handling
- Exception frame generation
- **Cycle and transaction auditing**: all 261,894 cases match their fixture cycle and bus-access totals, with separate ignored audit tools for access sequence and per-access timing analysis

### Musashi Reference Implementation

We also run binaries from [Musashi](https://github.com/kstenerud/Musashi), a
widely deployed M68000 emulator. The integration tests:

- Execute complete Musashi test binaries
- Verify register state, memory contents, and exception handling
- Cover 68000 through 68040 instruction sets
- Explicitly exclude legacy cases whose undefined BCD flags or illegal
  68000 encodings conflict with the hardware-oriented SingleStepTests model

### Cross-CPU Verification

Additional test suites verify behavior across CPU generations:

- **FPU tests**: 80-bit arithmetic, transcendental functions, packed decimal, memory operands, rounding modes, and save/restore
- **MMU translation tests**: 68030/68040/68060 table walks, ATCs, TTR matching, page crossings, fault frames, and writeback
- **Privilege tests**: User/supervisor mode transitions, TRAP behavior
- **Exception tests**: Per-model frames, resumable bus faults, double faults, and address errors
- **Execution-path differential tests**: Cold/warm cache state, self-modifying code, cycle batches, fast batches, and native traces

### Test Coverage

```
tests/
├── singlestep_m68000_v1_tests.rs  # Exhaustive 68000 fixture suite
├── musashi_tests.rs               # Musashi integration binaries
├── m68020_tests.rs ...            # Generation-specific behavior
├── m68060_tests.rs                # 68060 integer, FPU, and MMU behavior
├── fpu_accuracy.rs                # Extended-precision differential tests
├── run_for_cycles_tests.rs        # Precise cycle-batch boundary contract
├── run_batch_tests.rs             # HLE fast-path equivalence and exits
└── fixtures/
    ├── m68000/                    # External SingleStepTests checkout
    └── Musashi/                   # Musashi reference binaries
```

## Architecture

```
m68k/
├── core/           # CPU state, interpreter, timing, caches, and trace JIT
├── dasm/           # Disassembler
├── fpu/            # 80-bit FPU, packed decimal, and transcendental operations
└── mmu/            # 68030/68040/68060 translation and ATCs
```

### Key Types

| Type | Description |
| :--- | :--- |
| `CpuCore` | Main CPU state and execution APIs |
| `CpuType` | CPU model selection enum |
| `AddressBus` | Extensible memory, device, fetch-cache, timing, and fast-RAM contract |
| `LinearMemoryBus` / `FastMem` | Ready-made flat memory and optional direct-RAM window |
| `HleHandler` | Trap interception callbacks |
| `StepResult` | Single-instruction result |
| `CycleBatchResult` / `CycleBatchExit` / `CycleBatchControl` / `CycleBoundaryEvent` | Precise cycle-scheduled execution result, hook control, and boundary kind |
| `BatchResult` / `BatchExit` | High-throughput instruction-batch result |
| `CpuCore::is_stopped()` | STOP state check |
| `CpuCore::is_halted()` | Double-fault halt check |

## Performance

Accuracy and throughput are separate, deliberate contracts:

- **Precise execution** — `step`, `step_with_hle_handler`, `execute`, and
  `run_for_cycles` use the interpreter and ordinary `AddressBus` calls. These
  paths preserve bus-visible prefetch, access ordering, internal-clock
  synchronization, fault state, and model-specific cycle accounting.
- **Throughput execution** — `run_batch` reuses decoded operations and executes
  eligible hot backward-branch traces through a portable micro-op loop. The
  opt-in `jit` feature compiles those traces with Cranelift on native targets.
  A bus may expose a contiguous `FastMem` window to keep eligible RAM operands
  inside the trace. Guarded exits and self-modifying-code checks fall back
  without partially committing an instruction.

The two paths share instruction semantics and are continuously compared in
cold-cache, warm-cache, memory, control-flow, and fault tests. This keeps the
hardware contract simple for machine emulators while allowing HLE systems to
opt into lower dispatch overhead explicitly.

## License

MIT License - see [LICENSE](LICENSE) for details.

## Contributing

Contributions are welcome! Please see [CONTRIBUTING.md](CONTRIBUTING.md)

## Acknowledgments

- [Musashi](https://github.com/kstenerud/Musashi) - Reference implementation and test fixtures
- [SingleStepTests](https://github.com/SingleStepTests/m68000) - Exhaustive instruction test vectors
- The M68000 Programmer's Reference Manual
