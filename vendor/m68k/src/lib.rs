//! A safe, embeddable Motorola M68000-family CPU emulator.
//!
//! The core supports the M68000, M68010, M68EC020, M68020, M68EC030,
//! M68030, M68EC040, M68LC040, M68040, M68060, and SCC68070. It includes
//! generation-specific exception frames, instruction prefetch, integer and
//! floating-point execution, 68030/68040/68060 MMU translation, and
//! per-generation timing models.
//!
//! # Getting started
//!
//! Implement [`AddressBus`] for a machine with devices, or use
//! [`LinearMemoryBus`] for a contiguous RAM-backed address space:
//!
//! ```no_run
//! use m68k::{CpuCore, CpuType, LinearMemoryBus, StepResult};
//!
//! let mut memory = LinearMemoryBus::new(1024 * 1024);
//! memory.write_long_at(0, 0x0008_0000); // reset SSP
//! memory.write_long_at(4, 0x0000_1000); // reset PC
//! memory.write_word_at(0x1000, 0x4E71); // NOP
//!
//! let mut cpu = CpuCore::new();
//! cpu.set_cpu_type(CpuType::M68000);
//! cpu.reset(&mut memory);
//! assert!(matches!(cpu.step(&mut memory), StepResult::Ok { .. }));
//! ```
//!
//! [`CpuCore::step`] and [`CpuCore::execute`] preserve transaction-level bus
//! ordering and expose internal-clock gaps through [`AddressBus::sync`].
//! [`CpuCore::run_for_cycles`] retains cycle accounting with lower dispatch
//! overhead. [`CpuCore::run_for_cycles_with_hook`] additionally lets a host
//! synchronize devices and IRQ state between instructions, and
//! [`CpuCore::run_for_cycles_with_boundary_hook`] also reports interrupt
//! entry, while
//! [`CpuCore::run_batch`] is an instruction-budgeted throughput path that can
//! use [`FastMem`].
//!
//! # Cargo features
//!
//! - `serde` serializes the architectural CPU state for deterministic save
//!   states. Decode tables, FastMem pointers, and trace caches are deliberately
//!   omitted and rebuilt after loading.
//! - `jit` lowers eligible hot traces to native code with Cranelift on
//!   non-WebAssembly targets. Without it—and on WebAssembly—the same trace
//!   machinery uses the portable Rust executor.
//! - `trace-profile` adds opt-in trace and decoded-operation profiling,
//!   activated at runtime with `M68K_TRACE_PROFILE=1`.
//!
//! No features are enabled by default.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod core;
pub mod dasm;
pub mod fpu;
pub mod mmu;

// Re-export the embedding API at the crate root.
pub use core::cpu::CpuCore;
pub use core::cpu::{
    CACR_040_DE, CACR_040_IE, CACR_060_CABC, CACR_060_CUBC, CACR_060_EBC, CACR_060_EDC,
    CACR_060_EIC, CACR_060_ESB, CACR_CD, CACR_CED, CACR_CEI, CACR_CI, CACR_ED, CACR_EI, CACR_FD,
    CACR_FI, PCR_060_RESET, PCR_DFP, PCR_EC060_RESET, PCR_ESS,
};
pub use core::memory::{
    AddressBus, FastMem, FastMemRegion, LinearMemoryBus, MAX_FAST_MEM_REGIONS, TrackedMem,
};
pub use core::types::{
    BatchExit, BatchResult, CpuType, CycleBatchControl, CycleBatchExit, CycleBatchResult,
    CycleBoundaryEvent, HleHandler, NoOpHleHandler, Size, StepResult,
};
