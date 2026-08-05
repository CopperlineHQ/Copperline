// SPDX-License-Identifier: LGPL-2.1-or-later

//! MT-32 sound module emulation in Rust.
//!
//! A port of the synthesis engine from [Munt](https://github.com/munt/munt)'s
//! `mt32emu`, reduced to what emulating the module actually needs: ROM images
//! go in as bytes, MIDI goes in as bytes, stereo samples come out at the
//! module's native 32 kHz. No file abstraction, no C API, no host plumbing.
//!
//! The engine runs on control and PCM ROM images from a real unit, which are
//! Roland's copyright and are never distributed with this crate.
//!
//! The port's fidelity bar is bit-identical output to Munt at the native
//! sample rate, held by differential tests against the reference engine
//! itself (the `oracle` feature). Munt is the community's accepted yardstick
//! for how close to real hardware the emulation is, so matching it exactly is
//! what "sounds like an MT-32" means here.

pub mod analog;
pub mod demo;
pub mod display;
pub mod engine;
pub mod jitter;
pub mod la32;
pub mod layout;
pub mod memory;
pub mod midi;
pub mod note;
pub mod param;
pub mod part;
pub mod partial;
pub mod pcm;
pub mod reply;
pub mod reverb;
pub mod rom;
pub mod sha1;
pub mod sysex;
pub mod tables;
pub mod tva;
pub mod tvf;
pub mod tvp;

/// The rate the module's DAC runs at, and so the rate [`rom`]-fed synthesis
/// leaves the engine before any host resampling.
pub const SAMPLE_RATE: u32 = 32_000;

/// The crate's own version, for a host's About panel and log lines.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The width of the module's LCD in characters.
pub const LCD_WIDTH: usize = 20;
