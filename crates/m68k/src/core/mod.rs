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
pub mod memory;
pub mod registers;
pub mod status;
pub mod timing;
pub mod timing_020;
pub mod timing_060;
pub mod types;
