// SPDX-License-Identifier: GPL-3.0-or-later
//! CIA output phase and Paula external-request input pipeline.
use super::*;

const PAULA_CIA_INPUT_CCK: u64 = 4;

#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct CiaIrqRoute {
    pub(super) logical: bool,
    output_at: u64,
    request_at: u64,
    forwarded: bool,
}

impl CiaIrqRoute {
    fn observe(&mut self, asserted: bool, source_at: u64, output_delay: u32) {
        if asserted && !self.logical {
            self.output_at = source_at.saturating_add(u64::from(output_delay));
            self.request_at = self.output_at.saturating_add(PAULA_CIA_INPUT_CCK);
            self.forwarded = false;
        }
        self.logical = asserted;
    }

    pub(super) fn next_event(&self, now: u64) -> Option<u32> {
        self.logical
            .then_some(self.request_at)
            .filter(|at| *at > now)
            .map(|at| (at - now).min(u64::from(u32::MAX)) as u32)
    }
}

impl Bus {
    /// A CPU poll observes a due external input even when its final clock
    /// remains in the deferred-device batch. Do not flush ordinary accesses.
    pub(crate) fn cia_irq_forwarding_due(&self) -> bool {
        self.pending_device_cck != 0
            && self
                .cia_irq_routes
                .iter()
                .zip([INT_PORTS, INT_EXTER])
                .any(|(route, bit)| {
                    self.paula.intena & bit != 0
                        && route.logical
                        && !route.forwarded
                        && route.request_at <= self.emulated_cck
                })
    }

    pub(super) fn update_cia_irq_routes(&mut self, source_at: [Option<u64>; 2]) {
        if !self.cia_irq_routes.iter().any(|route| route.logical)
            && !self.cia_a.irq_line_asserted()
            && !self.cia_b.irq_line_asserted()
        {
            return;
        }
        let inputs = [
            (
                self.cia_a.irq_line_asserted(),
                self.cia_a.irq_output_delay_cck(),
                INT_PORTS,
            ),
            (
                self.cia_b.irq_line_asserted(),
                self.cia_b.irq_output_delay_cck(),
                INT_EXTER,
            ),
        ];
        for (index, (asserted, phase, bit)) in inputs.into_iter().enumerate() {
            let route = &mut self.cia_irq_routes[index];
            route.observe(
                asserted,
                source_at[index].unwrap_or(self.emulated_cck),
                phase,
            );
            if !route.logical || route.request_at > self.emulated_cck {
                continue;
            }
            let deadline = if route.forwarded {
                self.emulated_cck
                    .saturating_add(u64::from(self.irq_latency_setting))
            } else {
                // Preserve the measured combined input/CPU deadline while
                // qualifying the raw external request one clock later. Other
                // Paula sources and their read sampling remain unchanged.
                route
                    .request_at
                    .saturating_add(u64::from(self.irq_latency_setting.saturating_sub(2)))
            };
            route.forwarded = true;
            let newly = self.paula.intreq & bit == 0;
            self.paula.intreq |= bit;
            if newly {
                self.irq_cia_request_mask |= bit;
            }
            if newly
                && self.current_enabled_irq_sources() & bit != 0
                && self.irq_latency_setting != 0
            {
                // Preserve the actual deadline through a batched advance;
                // do not start another full delay at its observation time.
                let remaining = deadline
                    .saturating_sub(self.emulated_cck)
                    .min(u64::from(u32::MAX)) as u32;
                self.arm_irq_latency_bits(bit, remaining);
                self.irq_latency_visible_at[bit.trailing_zeros() as usize] = deadline;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chipset::cia::{REG_CRA, REG_ICR, REG_TAHI, REG_TALO};

    #[test]
    fn keyboard_cnt_timer_uses_the_common_cia_input_pipeline() {
        let mut bus = super::super::tests::empty_bus();
        bus.cia_a.write(REG_TALO, 0);
        bus.cia_a.write(REG_TAHI, 0);
        bus.cia_a.write(REG_ICR, 0x81);
        // One-shot timer A, counted by KCLK/CNT. The initial keyboard
        // synchronization bit supplies a real CNT edge.
        bus.cia_a.write(REG_CRA, 0x29);
        for _ in 0..200_000 {
            bus.advance_devices(1);
            if bus.cia_a.irq_line_asserted() {
                break;
            }
        }
        assert!(bus.cia_a.irq_line_asserted());
        assert_eq!(bus.paula.intreq & INT_PORTS, 0);
        assert_eq!(bus.irq_cia_request_mask & INT_PORTS, 0);
        let edge = bus.emulated_cck;
        assert_eq!(bus.cia_irq_routes[0].output_at, edge + 2);
        assert_eq!(bus.cia_irq_routes[0].request_at, edge + 6);
        bus.advance_devices(5);
        assert_eq!(bus.paula.intreq & INT_PORTS, 0);
        bus.advance_devices(1);
        assert_ne!(bus.paula.intreq & INT_PORTS, 0);
        assert_ne!(bus.irq_cia_request_mask & INT_PORTS, 0);
    }

    #[test]
    fn intreqr_does_not_expose_a_cia_request_before_input_forwarding() {
        let mut bus = super::super::tests::empty_bus();
        bus.irq_latency_setting = 6;
        bus.paula.intena = 0xc000 | INT_PORTS;
        bus.cia_a.write(REG_TALO, 0);
        bus.cia_a.write(REG_TAHI, 0);
        bus.cia_a.write(REG_ICR, 0x81);
        bus.cia_a.write(REG_CRA, 0x09);
        bus.advance_devices(20);
        assert_eq!(bus.paula.intreq & INT_PORTS, 0);
        assert_eq!(bus.custom_read(0x00df_f01e, 2) as u16 & INT_PORTS, 0);
        // Direct register reads do not allocate a CPU bus slot in this fixture.
        // Move explicitly across output_at=17 plus the four-CCK input stage.
        assert_eq!(bus.cia_irq_routes[0].request_at, 21);
        bus.advance_devices(1);
        assert_ne!(bus.paula.intreq & INT_PORTS, 0);
        assert_ne!(bus.custom_read(0x00df_f01e, 2) as u16 & INT_PORTS, 0);
    }

    #[test]
    fn cia_request_output_input_and_eligibility_are_distinct() {
        let mut bus = super::super::tests::empty_bus();
        bus.irq_latency_setting = 6;
        bus.paula.intena = 0xc000 | INT_PORTS;
        bus.cia_a.write(REG_TALO, 0);
        bus.cia_a.write(REG_TAHI, 0);
        bus.cia_a.write(REG_ICR, 0x81);
        bus.cia_a.write(REG_CRA, 0x09);
        bus.advance_devices(15);
        assert!(bus.cia_a.irq_line_asserted());
        assert_eq!(bus.cia_irq_routes[0].output_at, 17);
        assert_eq!(bus.paula.intreq & INT_PORTS, 0);
        bus.advance_devices(5);
        assert_eq!(bus.paula.intreq & INT_PORTS, 0);
        bus.advance_devices(1);
        assert_ne!(bus.paula.intreq & INT_PORTS, 0);
        assert_eq!(bus.cpu_poll_intreq(0) & INT_PORTS, 0);
        assert_ne!(bus.cpu_poll_intreq(4) & INT_PORTS, 0);
        assert_eq!(bus.cpu_poll_intreq(2) & INT_PORTS, 0);
        bus.advance_devices(2);
        assert_ne!(bus.cpu_poll_intreq(2) & INT_PORTS, 0);
        assert_eq!(bus.cpu_poll_intreq(0) & INT_PORTS, 0);
        bus.advance_devices(1);
        assert_eq!(bus.cpu_poll_intreq(0) & INT_PORTS, 0);
        bus.advance_devices(1);
        assert_ne!(bus.cpu_poll_intreq(0) & INT_PORTS, 0);
    }

    #[test]
    fn cia_route_cancels_unforwarded_edge_and_retains_asserted_input() {
        let mut route = CiaIrqRoute::default();
        route.observe(true, 10, 2);
        assert_eq!(route.next_event(11), Some(5));
        route.observe(false, 11, 2);
        assert_eq!(route.next_event(11), None);
        route.observe(true, 20, 0);
        assert_eq!(route.output_at, 20);
        assert_eq!(route.request_at, 24);
        route.observe(true, 40, 0);
        assert_eq!(
            route.request_at, 24,
            "held input must not restart its delay"
        );
    }

    #[test]
    fn cia_request_batched_source_stages_and_serialization_match_single_steps() {
        let mut batch = super::super::tests::empty_bus();
        batch.irq_latency_setting = 6;
        batch.paula.intena = 0xc000 | INT_PORTS;
        batch.cia_a.write(REG_TALO, 0);
        batch.cia_a.write(REG_TAHI, 0);
        batch.cia_a.write(REG_ICR, 0x81);
        batch.cia_a.write(REG_CRA, 0x09);
        let mut steps = super::super::tests::empty_bus();
        steps.irq_latency_setting = 6;
        steps.paula.intena = batch.paula.intena;
        steps.cia_a = batch.cia_a.clone();
        batch.advance_devices(24);
        for _ in 0..24 {
            steps.advance_devices(1);
        }
        assert_eq!(batch.paula.intreq, steps.paula.intreq);
        assert_eq!(
            batch.cia_irq_routes[0].output_at,
            steps.cia_irq_routes[0].output_at
        );
        assert_eq!(
            batch.cia_irq_routes[0].request_at,
            steps.cia_irq_routes[0].request_at
        );
        assert_eq!(batch.irq_latency_visible_at, steps.irq_latency_visible_at);
        batch.cia_a_read((REG_ICR as u64) << 8, 1);
        assert_ne!(batch.irq_cia_request_mask & INT_PORTS, 0);
        assert_ne!(
            batch.paula.intreq & INT_PORTS,
            0,
            "CIA acknowledgement does not clear Paula's latch"
        );
        batch.paula.intreq &= !INT_PORTS;
        batch.advance_devices(10);
        assert_eq!(batch.paula.intreq & INT_PORTS, 0);
        assert_eq!(batch.irq_cia_request_mask & INT_PORTS, 0);
        let restored: CiaIrqRoute =
            serde_json::from_str(&serde_json::to_string(&steps.cia_irq_routes[0]).unwrap())
                .unwrap();
        assert_eq!(restored.request_at, steps.cia_irq_routes[0].request_at);
        let restored: Bus =
            rmp_serde::from_slice(&rmp_serde::to_vec_named(&steps).unwrap()).unwrap();
        assert_eq!(restored.irq_cia_request_mask & INT_PORTS, INT_PORTS);
        assert_eq!(restored.cpu_poll_intreq(0) & INT_PORTS, 0);
        assert_ne!(restored.cpu_poll_intreq(2) & INT_PORTS, 0);
    }

    #[test]
    fn non_timer_sources_preserve_mask_and_acknowledgement_orders() {
        use crate::chipset::cia::{REG_CRB, REG_SDR, REG_TODHI, REG_TODMID};

        fn fire(cia: &mut Cia, source: u8) {
            match source {
                0x08 => {
                    cia.write(REG_CRA, 0x40); // Output mode before queuing SDR.
                    cia.write(REG_TALO, 3);
                    cia.write(REG_TAHI, 0);
                    cia.write(REG_SDR, 0xa5);
                    cia.write(REG_CRA, 0x51);
                }
                0x04 => {
                    cia.write(REG_CRB, 0x80);
                    cia.write(REG_TODHI, 0);
                    cia.write(REG_TODMID, 0);
                    cia.write(REG_TODLO, 1);
                    cia.write(REG_CRB, 0);
                    cia.write(REG_TODHI, 0);
                    cia.write(REG_TODMID, 0);
                    cia.write(REG_TODLO, 0);
                    cia.tick_tod();
                }
                0x10 => {
                    cia.release_flag();
                    cia.assert_flag();
                    cia.release_flag();
                }
                _ => unreachable!(),
            }
        }

        // A500 kind141 verifies these settled SP/TOD orders on both CIAs.
        // FLAG here is a synthetic input, not an electrical disk-index golden.
        for cia_b in [false, true] {
            for source in [0x04, 0x08, 0x10] {
                for mode in 0..4 {
                    let mut bus = super::super::tests::empty_bus();
                    let bit = if cia_b { INT_EXTER } else { INT_PORTS };
                    let cia = if cia_b {
                        &mut bus.cia_b
                    } else {
                        &mut bus.cia_a
                    };
                    if mode == 0 || mode == 3 {
                        cia.write(REG_ICR, 0x80 | source);
                    }
                    fire(cia, source);
                    bus.advance_devices(1024);
                    assert_eq!(bus.paula.intreq & bit != 0, mode == 0 || mode == 3);

                    let acknowledge = |bus: &mut Bus| {
                        let addr = (REG_ICR as u64) << 8;
                        if cia_b {
                            bus.cia_b_read(addr, 1)
                        } else {
                            bus.cia_a_read(addr, 1)
                        }
                    };
                    if mode >= 2 {
                        let flags = acknowledge(&mut bus) as u8;
                        assert_eq!(flags & source, source);
                        assert_eq!(flags & 0x80 != 0, mode == 3);
                    }
                    let cia = if cia_b {
                        &mut bus.cia_b
                    } else {
                        &mut bus.cia_a
                    };
                    cia.write(REG_ICR, 0x80 | source);
                    bus.advance_devices(32);
                    assert_eq!(bus.paula.intreq & bit != 0, mode != 2);

                    bus.custom_write(0x00df_f09c, 2, u64::from(bit));
                    bus.advance_devices(32);
                    assert_eq!(bus.paula.intreq & bit != 0, mode < 2);
                    let flags = acknowledge(&mut bus) as u8;
                    assert_eq!(
                        flags & (source | 0x80),
                        if mode < 2 { source | 0x80 } else { 0 }
                    );
                    bus.custom_write(0x00df_f09c, 2, u64::from(bit));
                    bus.advance_devices(64);
                    assert_eq!(bus.paula.intreq & bit, 0);

                    let cia = if cia_b {
                        &mut bus.cia_b
                    } else {
                        &mut bus.cia_a
                    };
                    fire(cia, source);
                    bus.advance_devices(1024);
                    assert_ne!(bus.paula.intreq & bit, 0, "fresh source must reassert");
                }
            }
        }
    }
}
