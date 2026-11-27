// SPDX-License-Identifier: GPL-3.0-or-later

//! The bus state a CPU instruction reads or leaves behind without touching
//! the chip bus or a device, for the precise core's wait-loop skip
//! (`src/emulator/wait_skip.rs`): the sub-colour-clock carries and phases
//! that time a local-memory access, the floating data bus, the ROM
//! controller's latch. Split out of `bus.rs` for size; this is the same
//! `Bus`, with full access to its private state.

use super::*;

/// See [`Bus::cpu_side_state`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CpuSideBusState {
    cpu_pc: u32,
    cpu_clock_carry: u32,
    ext_clock_carry_x100: u32,
    cpu_chip_clock_phase: u32,
    cpu_bus_overlap_clocks: u32,
    cpu_posted_write_debt: u32,
    data_bus: u16,
    gary_rom: Option<(u32, Option<u32>)>,
    buster_cpu: (bool, bool),
}

impl Bus {
    /// What a CPU instruction that stays off the chip bus and away from the
    /// devices reads from, or leaves in, the bus. Two instruction
    /// boundaries with equal states (and equal CPU state) time such an
    /// instruction identically.
    pub(crate) fn cpu_side_state(&self) -> CpuSideBusState {
        CpuSideBusState {
            cpu_pc: self.cpu_pc,
            cpu_clock_carry: self.cpu_clock_carry,
            ext_clock_carry_x100: self.ext_clock_carry_x100,
            cpu_chip_clock_phase: self.cpu_chip_clock_phase,
            cpu_bus_overlap_clocks: self.cpu_bus_overlap_clocks,
            cpu_posted_write_debt: self.cpu_posted_write_debt,
            data_bus: self.data_bus,
            gary_rom: self
                .gary
                .as_ref()
                .map(|gary| (gary.rom_clock_remainder, gary.rom_fetch_latch)),
            buster_cpu: (self.buster.cpu_driving, self.buster.cpu_locked),
        }
    }

    /// Whether one long timed-device tick leaves every device exactly where
    /// a run of short ones would, for everything a save state carries, not
    /// only what the CPU can observe. It does except while a floppy platter
    /// winds up or coasts down with drive sounds on: the drive-sound
    /// synthesizer takes the platter's spin level at each tick, so the ramp
    /// it plays depends on how the time was cut up.
    pub fn device_ticks_batch_exactly(&self) -> bool {
        !(self.paula.drive_sounds().enabled() && self.floppy.motor_changing_speed())
    }

    /// Whether nothing on the bus watches the CPU's or the chipset's
    /// progress clock by clock: no frame analyzer or bus-event observer, no
    /// debugger beam trap, no waveform capture, no heat map and no freezer
    /// cartridge watching the program counter. The wait-loop skip advances
    /// the chipset in one span, which only these could tell apart from the
    /// instructions it stands for.
    pub(crate) fn cpu_progress_unobserved(&self) -> bool {
        !self.precise_timed_device_trace()
            && !self.chip_bus_observers_on
            && self.ui_beam_traps.is_empty()
            && self.wave.is_none()
            && !self.heat_map_armed()
            && self.cartridge.is_none()
    }
}
