//! # Core
//!
//! Core M68000 family CPU emulation engine.

pub mod addressing;
pub mod cpu;
pub mod decode;
pub mod ea;
pub mod exceptions;
pub mod execute;
pub mod instructions;
pub mod interrupts;
pub(crate) mod mem_ops;
pub mod memory;
pub mod op_cache;
pub mod registers;
pub mod status;
pub mod timing;
pub mod timing_020;
pub mod timing_040;
pub mod timing_060;
pub mod trace_jit;
#[cfg(feature = "trace-profile")]
pub mod trace_profile;
pub mod types;
pub(crate) mod window;
