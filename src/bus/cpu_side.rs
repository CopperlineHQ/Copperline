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

    /// The longest span one timed-device tick may cover and leave every
    /// device exactly where a run of short ticks would, for everything a
    /// save state carries, not only what the CPU observes: `None` for no
    /// limit, `Some(0)` for none at all right now. Device ticks are linear
    /// in their colour clocks except:
    ///
    /// - while a device streams host audio (a CD drive playing CD-DA, a
    ///   sound card playing): it fills its mixer ring before Paula drains
    ///   the ring for the same span, and reads how full the ring is;
    /// - while a floppy platter winds up or coasts down with drive sounds
    ///   on: the drive-sound synthesizer takes the spin level at each tick;
    /// - across the floppy controller's own transition to idle
    ///   (`FloppyController::cck_until_idle`).
    pub fn device_batch_limit_cck(&self) -> Option<u32> {
        use crate::zorro_device::ZorroDevice;
        let streaming = self.gayle.as_ref().is_some_and(|g| g.cd_audio_playing())
            || self
                .ide_a4000
                .as_ref()
                .is_some_and(|ide| ide.cd_audio_playing())
            || self.sdmac.as_ref().is_some_and(|s| s.cd_audio_playing())
            || self
                .cdtv
                .as_ref()
                .is_some_and(crate::cdtv::CdtvController::audio_playing)
            || self
                .akiko
                .as_ref()
                .is_some_and(crate::akiko::Akiko::audio_playing)
            || self.devices.iter().any(ZorroDevice::streams_audio);
        if streaming || (self.paula.drive_sounds().enabled() && self.floppy.motor_changing_speed())
        {
            return Some(0);
        }
        self.floppy.cck_until_idle()
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

#[cfg(test)]
mod tests {
    use crate::zorro_device::{BoardDevice, DeviceHost, ZorroDevice};

    #[test]
    fn a_device_streaming_audio_stops_batching() {
        let mut bus = super::super::tests::empty_bus();
        assert_eq!(bus.device_batch_limit_cck(), None);
        let mut board = crate::toccata::Toccata::new();
        {
            let mut host = DeviceHost::new(&mut bus.mem);
            board.write(0x0000, 1, 0x14, &mut host); // ACTIVE | FIFO_PLAY
            board.write(0x6001, 1, 8, &mut host);
            board.write(0x6801, 1, 0x0f, &mut host);
            board.write(0x6001, 1, 9, &mut host);
            board.write(0x6801, 1, 0x10, &mut host);
            board.write(0x6801, 1, 0x11, &mut host); // play
        }
        assert!(ZorroDevice::streams_audio(&board));
        bus.attach_devices(vec![BoardDevice::Toccata(Box::new(board))]);
        assert_eq!(bus.device_batch_limit_cck(), Some(0));
    }
}
