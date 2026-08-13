// SPDX-License-Identifier: GPL-3.0-or-later

//! Copperline: a cycle-driven Amiga emulator (OCS/ECS/AGA).
//!
//! This library crate holds the whole emulator: the deterministic core
//! (`bus`, `cpu`, `chipset`, `memory`, peripherals) and the frontend
//! building blocks (`video`, `audio`, `emulator`, `config`). The
//! `copperline` binary (`src/main.rs`) is a thin CLI wrapper around it;
//! alternative frontends, fuzzers, and test harnesses can depend on the
//! library directly. `emulator::build_machine` wires a validated
//! [`config::Config`] into a runnable machine.

pub mod a2065;
pub mod a2091;
pub mod a4091;
pub mod akiko;
pub mod amigaos;
pub mod ata;
pub mod audio;
// Host block devices: a real disk (an SD card, a CF card, an Amiga's own
// hard drive) standing in for a hard-drive image. Not built for wasm32,
// which has no such thing to reach.
#[cfg(not(target_arch = "wasm32"))]
pub mod blockdev;
pub mod bus;
pub mod cache;
pub mod cdrom;
pub mod cdtv;
pub mod chipset;
pub mod config;
#[cfg(feature = "control")]
pub mod control;
pub mod cpu;
pub mod crashlog;
pub mod debugger;
pub mod dirfs;
pub mod disasm;
pub mod diskimage;
pub mod dms;
pub mod drive_sounds;
pub mod emulator;
pub mod envcfg;
pub mod filesys;
pub mod floppy;
// Physical floppy drives over a Greaseweazle, through the pure-Rust
// FluxBridge library. Gated because it talks to a serial port, which a
// wasm32 browser build cannot do.
#[cfg(feature = "fluxbridge")]
pub mod fluxbridge;
// The WHDLoad game library: the launcher's Library page, the local game
// database, and the OpenRetro sync that fills it. Gated because it is the
// only part of Copperline that makes network requests of its own, and so
// the only part that needs an HTTP client and a TLS stack.
#[cfg(feature = "game-library")]
pub mod gamelib;
#[cfg(feature = "frontend")]
pub mod gamepad;
pub mod gary;
pub mod gayle;
pub mod gdbstub;
pub(crate) mod gzip;
pub mod harddrive;
pub mod heatmap;
pub mod hostsocket;
pub mod ide_a4000;
pub mod ide_zorro;
pub mod inputrec;
pub mod inputsched;
pub mod ipf;
// Host-keyboard controller bindings: a frontend concern (it speaks winit key
// codes and produces the same `JoystickState` the gamepad reader does), so it
// rides the same feature gate as `gamepad`. The autofire policy that pairs
// with it lives in `config`, which every build has.
#[cfg(feature = "frontend")]
pub mod keymap;
pub mod lha;
pub mod memory;
#[cfg(feature = "midi")]
pub mod midi;
#[cfg(feature = "mt32")]
pub mod mt32;
pub mod net;
pub mod package;
pub mod parallel;
pub mod paths;
pub mod picasso2;
pub mod pointer;
pub mod priority;
pub mod ramsey;
pub mod recorder;
pub mod regcheck;
pub mod romdb;
pub mod romsearch;
pub mod romtags;
pub mod rtc;
pub mod sampler;
pub mod savestate;
pub mod screenshot;
pub mod scsi;
pub mod sdmac;
pub mod serial;
pub mod smc;
pub mod timebase;
pub mod timestamp;
pub mod timetravel;
pub mod video;
pub mod wasm_manifest;
#[cfg(feature = "wasm-boards")]
pub mod wasmboard;
pub mod waveform;
pub mod whdload;
pub mod z3660;
pub mod zorro;
pub mod zorro_device;
