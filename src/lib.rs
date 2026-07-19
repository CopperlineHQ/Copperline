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
pub mod dms;
pub mod drive_sounds;
pub mod emulator;
pub mod envcfg;
pub mod filesys;
pub mod floppy;
#[cfg(feature = "frontend")]
pub mod gamepad;
pub mod gary;
pub mod gayle;
pub mod gdbstub;
pub mod harddrive;
pub mod ide_a4000;
pub mod inputrec;
pub mod inputsched;
pub mod memory;
#[cfg(feature = "midi")]
pub mod midi;
pub mod net;
pub mod parallel;
pub mod priority;
pub mod ramsey;
pub mod recorder;
pub mod romsearch;
pub mod romtags;
pub mod rtc;
pub mod sampler;
pub mod savestate;
pub mod screenshot;
pub mod scsi;
pub mod sdmac;
pub mod serial;
pub mod timebase;
pub mod timestamp;
pub mod timetravel;
pub mod video;
pub mod wasm_manifest;
#[cfg(feature = "wasm-boards")]
pub mod wasmboard;
pub mod waveform;
pub mod zorro;
pub mod zorro_device;
