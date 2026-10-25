//! Opcode-word disassembly helpers.
//!
//! This module formats the information available in a single opcode word. It
//! does not read extension words or operand memory; see [`disassemble`] for the
//! resulting placeholders and size estimate.

mod format;

pub use format::*;
