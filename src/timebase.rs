// SPDX-License-Identifier: GPL-3.0-or-later

//! Host clock imports for code shared with the browser build.
//!
//! On every native target (and on wasm32-wasip1, whose WASI clocks work)
//! this is a pure re-export of `std::time`, so it changes nothing. On
//! wasm32-unknown-unknown `std::time::Instant::now()` and
//! `SystemTime::now()` abort at runtime, so the `web-time` crate backs
//! them with `performance.now()` / `Date.now()` instead. Modules that can
//! be part of the headless core import their clocks from here rather than
//! from `std::time`.

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use web_time::{Duration, Instant, SystemTime, UNIX_EPOCH};
