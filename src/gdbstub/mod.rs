// SPDX-License-Identifier: GPL-3.0-or-later

//! The GDB remote-protocol stub, split by driver:
//!
//! - [`headless`] owns the Emulator and drives it directly from the
//!   connection (`--gdb`), one blocking session at a time -- the shape
//!   the headless control server copies.
//!
//! The packet semantics shared by any driver live in [`core`]: a
//! transport-free [`core::GdbCore`] each driver runs against the machine
//! it holds.

pub(crate) mod core;
mod headless;
#[cfg(test)]
pub(crate) mod testkit;

pub use headless::{run, Config};
