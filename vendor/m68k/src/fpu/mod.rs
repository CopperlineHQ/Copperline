//! Floating-point emulation for the 68881/68882 and the on-chip
//! 68040/68060 FPUs.
//!
//! The public register value is [`FloatX80`]. Instruction execution uses a
//! pure-Rust 80-bit extended-precision engine, including FPCR rounding modes,
//! exception accumulation, packed-decimal conversion, and transcendental
//! operations. CPU-specific instruction availability and exception behavior
//! are enforced by [`CpuCore`](crate::CpuCore).

mod dd;
mod operations;
mod packed;
mod softfloat;
mod transcendental;
mod types;

pub use types::*;
