// SPDX-License-Identifier: GPL-3.0-or-later

//! The CPU's event horizon: the earliest moment something other than the
//! CPU changes state the CPU can observe without touching a device -- an
//! interrupt request on its way to the IPL pins, guest memory written by a
//! bus master, a request for the bus. A halted CPU's `STOP` fast-forward
//! naps to it and a `[cpu] jit` batch runs up to it; both read this one
//! function, so a source added here bounds both. Split out of `bus.rs` for
//! size; this is the same `Bus`, with full access to its private state.

use super::*;
use crate::zorro_device::DeviceDeadline;

/// Fail-safe horizon for a device that cannot predict its next event (see
/// [`DeviceDeadline::Unknown`]): about 18 microseconds, so a host-driven
/// arrival is seen within a few instructions of time, as the `[cpu] jit`
/// batch ceiling always bounded it.
pub(crate) const UNPREDICTABLE_DEVICE_CAP_CCK: u32 = 64;

/// Horizon while a serial sink with a live host input side (TCP, pty, the
/// browser channel) is attached: it can start a reception at any wall-clock
/// moment. 256 cck is about 72 microseconds, well under one character time
/// even at 115200 baud (about 10 x 31 cck), so a byte arriving from the host
/// mid-nap raises RBF and wakes the CPU before a second byte can complete and
/// overrun Paula's one-word receive buffer.
pub(crate) const SERIAL_LIVE_CAP_CCK: u32 = 256;

/// See [`Bus::cpu_event_horizon`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EventHorizon {
    /// Colour clocks from now to the earliest such event, at least 1, or
    /// `None` when no source has anything scheduled. Unpredictable sources
    /// contribute their fail-safe cap.
    pub cck: Option<u32>,
    /// True when every source predicted its next event, so nothing can
    /// happen before `cck`. False when some source is host-driven or has no
    /// deadline model: `cck` then holds a fail-safe cap rather than a bound.
    pub exact: bool,
}

impl EventHorizon {
    fn bound(&mut self, cck: Option<u32>) {
        if let Some(cck) = cck {
            let cck = cck.max(1);
            self.cck = Some(self.cck.map_or(cck, |earliest| earliest.min(cck)));
        }
    }

    fn deadline(&mut self, deadline: DeviceDeadline) {
        match deadline {
            DeviceDeadline::Idle => {}
            DeviceDeadline::In(cck) => self.bound(Some(cck)),
            DeviceDeadline::Unknown => self.unpredictable(UNPREDICTABLE_DEVICE_CAP_CCK),
        }
    }

    fn unpredictable(&mut self, cap_cck: u32) {
        self.exact = false;
        self.bound(Some(cap_cck));
    }
}

impl Bus {
    /// The earliest moment, in colour clocks from now, at which any device
    /// can raise an interrupt the CPU would see, write guest memory, or ask
    /// for the bus, with no CPU access in between (see the module comment).
    ///
    /// The frame structure is left to the callers: the `STOP` fast-forward
    /// also stops at the frame wrap and the display-start snapshot line,
    /// and a JIT batch at the end of the scanline.
    pub(crate) fn cpu_event_horizon(&self) -> EventHorizon {
        let mut horizon = EventHorizon {
            cck: None,
            exact: true,
        };
        // A physical plugin can request the bus from its very next tick.
        #[cfg(feature = "wasm-boards")]
        if self.buster.wasm_slots != 0 {
            horizon.unpredictable(1);
        }
        // CIA timer and alarm events, and a staged interrupt's arrival at
        // Paula's external request input.
        horizon.bound(self.next_cia_event_cck());
        horizon.bound(self.next_cia_b_tod_alarm_cck());
        // An interrupt already latched in INTREQ but still inside its
        // IPL-pin recognition pipe is invisible to the CPU right now.
        horizon.bound(self.next_irq_visible_cck());
        horizon.bound(self.next_copper_wakeup_cck());
        horizon.bound(self.next_blitter_completion_cck());
        horizon.bound(self.next_blitter_irq_cck());
        horizon.bound(self.next_audio_irq_cck());
        horizon.bound(self.next_pot_event_cck());
        horizon.bound(self.floppy.next_completion_cck(self.agnus.dmacon));
        horizon.bound(self.floppy.next_sync_irq_cck(self.agnus.dmacon));
        horizon.bound(self.floppy.next_index_pulse_cck());
        horizon.bound(self.next_serial_event_cck());
        if self.paula.serial.can_produce_input() {
            horizon.unpredictable(SERIAL_LIVE_CAP_CCK);
        }
        horizon.bound(self.next_keyboard_event_cck());
        horizon.deadline(self.storage_deadline());
        horizon
    }

    /// When a storage controller or expansion board next changes state on
    /// its own -- a timed drive presenting data or completing, a delayed
    /// controller interrupt, a CD frame -- so a CPU halted in STOP waiting
    /// for it does not nap past it. A Zorro board without a deadline model
    /// is [`DeviceDeadline::Unknown`].
    pub fn storage_deadline(&self) -> DeviceDeadline {
        use crate::zorro_device::ZorroDevice;
        let mut deadline = [
            self.gayle.as_ref().and_then(|g| g.next_drive_event_cck()),
            self.ide_a4000
                .as_ref()
                .and_then(|ide| ide.next_drive_event_cck()),
            self.sdmac.as_ref().and_then(|s| s.next_event_cck()),
            // The CDTV drive's subcode clock and play cadence: a guest
            // asleep between subcode bytes is woken by each one.
            self.cdtv
                .as_ref()
                .and_then(crate::cdtv::CdtvController::next_event_cck),
            self.buster_pending().then_some(1),
        ]
        .into_iter()
        .map(DeviceDeadline::from_cck)
        .fold(DeviceDeadline::Idle, DeviceDeadline::min);
        if let Some(akiko) = self.akiko.as_ref() {
            deadline = deadline.min(akiko.next_deadline());
        }
        self.devices
            .iter()
            .map(ZorroDevice::next_deadline)
            .fold(deadline, DeviceDeadline::min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zorro_device::BoardDevice;

    fn horizon_bus() -> Bus {
        let mut bus = super::super::tests::empty_bus();
        bus.agnus.dmacon = 0;
        bus
    }

    #[test]
    fn a_machine_with_only_predictable_sources_has_an_exact_horizon() {
        let bus = horizon_bus();
        assert!(bus.cpu_event_horizon().exact);
    }

    #[cfg(feature = "cd32-fmv")]
    #[test]
    fn a_board_with_no_deadline_model_fails_safe() {
        let mut bus = horizon_bus();
        let fmv = crate::cd32_fmv::Cd32Fmv::new(vec![0; 256 * 1024]).unwrap();
        bus.attach_devices(vec![BoardDevice::Cd32Fmv(Box::new(fmv))]);
        let horizon = bus.cpu_event_horizon();
        assert!(!horizon.exact, "an unknown board makes the horizon a cap");
        assert_eq!(horizon.cck, Some(UNPREDICTABLE_DEVICE_CAP_CCK));
    }

    #[test]
    fn an_idle_board_leaves_the_horizon_as_it_was() {
        let mut bus = horizon_bus();
        let before = bus.cpu_event_horizon();
        bus.attach_devices(vec![BoardDevice::Z3660(crate::z3660::Z3660::new())]);
        assert_eq!(bus.cpu_event_horizon(), before);
    }

    #[test]
    fn deadlines_combine_with_unknown_winning() {
        use DeviceDeadline::*;
        assert_eq!(Idle.min(Idle), Idle);
        assert_eq!(Idle.min(In(5)), In(5));
        assert_eq!(In(9).min(In(5)), In(5));
        assert_eq!(In(9).min(Unknown), Unknown);
        assert_eq!(Unknown.min(Idle), Unknown);
        assert_eq!(DeviceDeadline::from_cck(None), Idle);
        assert_eq!(DeviceDeadline::from_cck(Some(0)), In(1));
    }
}
