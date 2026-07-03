# m68k-rs

This is Copperline's vendored, locally patched copy of `m68k-rs`. The root
crate uses it through a path dependency, not through crates.io. Do not treat
this directory as matching the published `m68k` crate unless the local patches
have first been reconciled upstream or published separately.

A safe, pure Rust implementation of the Motorola 68000 family CPU emulator,
used by Copperline for instruction execution and cycle counts.

[![Rust CI](https://github.com/benletchford/m68k-rs/actions/workflows/rust.yml/badge.svg)](https://github.com/benletchford/m68k-rs/actions/workflows/rust.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## Features

- **CPU family coverage**: M68000, M68010, M68020, M68030, M68040, and variants (EC/LC)
- **Small dependency set**: Pure Rust with serde support for state serialization
- **Safe Rust**: No unsafe code blocks
- **FPU emulation**: 68881/68882/68040 operations covered by the current tests; unsupported encodings still trap
- **MMU emulation**: 68030/68040 table walks and transparent translation; permission/status detail is still incomplete
- **HLE support**: Built-in trap interception for High-Level Emulation
- **Reference tests**: SingleStepTests, Musashi fixtures, and local cross-CPU cases

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
m68k = { path = "crates/m68k" }
```

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

| Method | Best For | Behavior on Trap |
| :--- | :--- | :--- |
| **`step()`** | Debuggers, Analyzers, Custom Control | Returns a `StepResult` variant (e.g., `AlineTrap`). The CPU **does not** take the exception automatically. If you do nothing, it acts like a NOP. You must manually call `cpu.take_exception(...)` if you want standard behavior. |
| **`step_with_hle_handler()`** | OS Emulation (Mac/Amiga/Atari) | Calls your `HleHandler` callback. If it returns `true`, execution continues. If it returns `false`, the CPU **automatically** triggers the standard hardware exception (stacks frame, jumps to vector). |

Use **`step()`** when you need full control over the execution loop or are building a debugger that needs to pause on every event.

Use **`step_with_hle_handler()`** when implementing a high-level emulator (like a Macintosh or Amiga emulator) where you want to patch specific system calls but otherwise let the guest OS run normally.

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
| `SCC68070` | Philips SCC68070 variant               |

## Validation & Testing

The test suite uses several reference sources:

### SingleStepTests (m68000)

The [SingleStepTests](https://github.com/SingleStepTests/m68000) project provides per-instruction test vectors derived from real hardware and cycle-accurate emulators. The suite runs **all 101 instruction categories** with thousands of test cases each, covering:

- All addressing modes and operand sizes
- Edge cases for condition codes (CCR/SR)
- BCD arithmetic (ABCD, SBCD, NBCD)
- Multiply/divide overflow handling
- Exception frame generation

### WinUAE cputest (68000-68060)

Toni Wilen's WinUAE cputest generator produces exhaustive per-opcode test
sets (hundreds of state combinations per opcode, hardware-validated over
years on real 68000-68060 silicon). The `crates/cputest-runner` harness
drives this core through those sets:

```sh
crates/cputest-runner/tools/cputest-gen.sh /tmp/cputest-data 68020
cd crates/cputest-runner
cargo run --release -- /tmp/cputest-data all 68020
```

All six CPU models pass their full sweeps (the sole exception is one 68060
LPSTOP round documented in the runner). This covers documented and
undocumented behavior alike: illegal-encoding maps per model, privilege
versus Line-F ordering, BCD invalid-digit results, register write order for
CAS2/MOVEM/MULL/DIVL collisions, and T0/T1 trace timing.

### Musashi Reference Implementation

The suite also checks against [Musashi](https://github.com/kstenerud/Musashi), an M68000 emulator used in MAME and other projects:

- Execute complete Musashi test binaries
- Verify register state, memory contents, and exception handling
- Cover 68000 through 68040 instruction sets

### Cross-CPU Verification

Additional test suites verify behavior across CPU generations:

- **68040 FPU tests**: Floating-point transcendental functions, rounding modes
- **MMU translation tests**: Table walks, TTR matching, fault handling
- **Privilege tests**: User/supervisor mode transitions, TRAP behavior
- **Exception tests**: Double-fault detection, address error frames

### Test Coverage

```
tests/
├── singlestep_m68000_v1_tests.rs   # 101 instruction test files
├── musashi_tests.rs                 # Musashi integration suite
├── cross_cpu_tests.rs               # Multi-generation verification
├── m68040_tests.rs                  # 68040-specific features
├── mmu_fault_tests.rs               # MMU and exception handling
├── hle_interception_tests.rs        # Trap handler API tests
└── fixtures/
    ├── m68000/                      # SingleStepTests submodule
    └── Musashi/                     # Musashi reference submodule
```

## Architecture

```
m68k/
├── core/           # CPU core, registers, execution loop
├── dasm/           # Disassembler
├── fpu/            # 68881/68882/68040 FPU emulation
└── mmu/            # 68030/68040 PMMU emulation
```

### Key Types

| Type                    | Description                        |
| ----------------------- | ---------------------------------- |
| `CpuCore`               | Main CPU state and execution       |
| `CpuType`               | CPU model selection enum           |
| `AddressBus`            | Trait for memory/IO implementation |
| `HleHandler`            | Trait for HLE interception         |
| `StepResult`            | Instruction execution result       |
| `CpuCore::is_stopped()` | STOP state check                   |
| `CpuCore::is_halted()`  | Double-fault halt check            |

## Performance

The emulator is designed for correctness first, with performance as a secondary goal. Typical use cases (classic computer emulation, game console emulation) run at many multiples of original hardware speed on modern CPUs.

## License

MIT License - see [LICENSE](LICENSE) for details.

## Contributing

Contributions are welcome! Please ensure:

1. All tests pass: `cargo test`
2. No clippy warnings: `cargo clippy -- -D warnings`
3. Code is formatted: `cargo fmt`

## Acknowledgments

- [Musashi](https://github.com/kstenerud/Musashi) - Reference implementation and test fixtures
- [SingleStepTests](https://github.com/SingleStepTests/m68000) - Exhaustive instruction test vectors
- The M68000 Programmer's Reference Manual
