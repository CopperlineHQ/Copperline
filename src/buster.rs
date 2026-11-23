// SPDX-License-Identifier: GPL-3.0-or-later

//! Expansion bus ownership. Boards supply requests in physical slot order.
//! Service is clocked by the machine so CPU waits and DMA share one timeline.

use serde::{Deserialize, Serialize};

/// Data driven or sampled in a single bus transfer, in 68k byte order.
pub(crate) enum Transfer<'a> {
    Read(&'a mut [u8]),
    Write(&'a [u8]),
}

impl Transfer<'_> {
    pub(crate) fn at_device(
        self,
        device: &mut crate::zorro_device::BoardDevice,
        off: u32,
        host: &mut crate::zorro_device::DeviceHost,
    ) {
        use crate::zorro_device::ZorroDevice;
        match self {
            Self::Read(bytes) => {
                let value = device.read(off, bytes.len(), host).to_be_bytes();
                bytes.copy_from_slice(&value[4 - bytes.len()..]);
            }
            Self::Write(bytes) => {
                let value = bytes.iter().fold(0, |acc, &b| (acc << 8) | u32::from(b));
                device.write(off, bytes.len(), value, host);
            }
        }
    }
}

/// The A2000 DIP controller and the two generations of Fat Buster.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Revision {
    #[default]
    Absent,
    A2000,
    Rev7,
    Rev9,
    Rev11,
}

impl Revision {
    pub(crate) fn decode_ranges(
        self,
        version: crate::zorro::ZorroVersion,
    ) -> &'static [(u64, u64)] {
        use crate::zorro::ZorroVersion::*;
        match (self, version) {
            (Self::Absent, _) => &[(0, 1 << 32)],
            (Self::A2000, II) => &[(0, 1 << 24)],
            (Self::A2000, III) => &[],
            (_, II) => &[(0x200000, 0xb80000), (0xe80000, 0xf00000)],
            (_, III) => &[(0x10000000, 0x80000000), (0xff000000, 0xff010000)],
        }
    }

    pub(crate) fn super_buster(self) -> bool {
        matches!(self, Self::Rev7 | Self::Rev9 | Self::Rev11)
    }
    pub(crate) fn z3_master(self) -> bool {
        matches!(self, Self::Rev9 | Self::Rev11)
    }
}

/// Super Buster MEMZ2 input. The other decoded Zorro II ranges drive IOZ2.
pub(crate) const Z2_MEMORY: std::ops::Range<u32> = 0x0020_0000..0x00a0_0000;

// Inputs A23..19 of the DIP part, with /AS asserted and /OVR released.
pub(crate) const LOCAL_RANGES: [(u64, u64); 3] =
    [(0, 0x200000), (0xb80000, 0xe80000), (0xf80000, 0x1000000)];

/// At a grant boundary sample request lines. While a Zorro II master holds
/// /OWN its grant is sticky. Registered Zorro III masters rearbitrate after
/// eight completed bus cycles. Fairness flip-flops belong to each group.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Buster {
    pub(crate) revision: Revision,
    pub(crate) owner: Option<usize>,
    // Static slot wiring and live request pins are reconstructed on load.
    #[serde(skip)]
    pub(crate) devices: [Option<usize>; 5],
    #[serde(skip)]
    pub(crate) wired: bool,
    #[cfg(feature = "wasm-boards")]
    #[serde(skip)]
    pub(crate) wasm_slots: u8,
    #[serde(skip)]
    pub(crate) z2: u8,
    #[serde(skip)]
    pub(crate) z3: u8,
    #[serde(skip)]
    pub(crate) cpu_driving: bool,
    #[serde(skip)]
    pub(crate) cpu_locked: bool,
    #[serde(skip)]
    pub(crate) local_request: bool,
    #[serde(default)]
    registered: u8,
    #[serde(default)]
    rev9_lock: bool,
    #[serde(skip)]
    pub(crate) slave_start: bool,
    #[serde(default)]
    waiting_for_slave: bool,
    // Grant watchdog runs until the master asserts /FCS. A slow slave can
    // stretch DTACK after that without losing the grant.
    claim_by: Option<u64>,
    timed_out: u8,
    served_z2: u8,
    served_z3: u8,
    cycles: u8,
    pub(crate) ready_at: u64,
}

pub(crate) const LOCAL_MASTER: usize = 5;
// Haynie specifies "several microseconds". 16 PAL/NTSC CCK are ~4.5 us.
const GRANT_TIMEOUT_CCK: u64 = 16;

impl Buster {
    /// The card interface translates a change in demand into the Zorro III
    /// registration/deregistration pulse. A revision 9 registration sampled
    /// at the start of a slave cycle can wedge the expansion arbiter.
    pub(crate) fn sample_registration(&mut self, requests: u8) {
        let rising = requests & !self.z3;
        if self.revision == Revision::Rev9 && self.slave_start && rising != 0 {
            self.rev9_lock = true;
        }
        self.timed_out &= requests;
        let eligible = requests & !self.timed_out;
        self.registered &= eligible;
        if self.revision == Revision::Rev11 {
            if self.registered == 0 {
                self.registered = eligible & eligible.wrapping_neg();
            }
        } else if self.revision.z3_master() {
            self.registered |= eligible;
        }
        self.z3 = requests;
    }

    #[cfg(feature = "wasm-boards")]
    pub(crate) fn accepts_z3_request(&self) -> bool {
        self.revision.z3_master() && !self.rev9_lock
    }

    pub(crate) fn pending(&self) -> bool {
        self.local_request
            || self.z2 != 0
            || (!self.rev9_lock && self.registered != 0)
            || self.owner.is_some()
    }

    pub(crate) fn grant(&mut self, now: u64) -> Option<usize> {
        if self.claim_by.is_some_and(|deadline| now >= deadline) {
            if let Some(slot) = self.owner.take().filter(|&s| s < self.devices.len()) {
                self.timed_out |= 1 << slot;
                self.registered &= !(1 << slot);
                self.sample_registration(self.z3);
            }
            self.claim_by = None;
        }
        let z2 = self.z2;
        let z3 = if self.rev9_lock { 0 } else { self.registered };
        if self.owner == Some(LOCAL_MASTER) && self.local_request {
            return self.owner;
        }
        if let Some(slot) = self.owner.take().filter(|&s| s < self.devices.len()) {
            let bit = 1 << slot;
            if (z2 | z3) & bit != 0 {
                self.owner = Some(slot);
                return Some(slot);
            }
        }
        self.claim_by = None;
        if now < self.ready_at || self.revision == Revision::Absent {
            return None;
        }
        self.waiting_for_slave = false;
        if self.local_request {
            self.owner = Some(LOCAL_MASTER);
            self.ready_at = now + 1;
            return self.owner;
        }
        let eligible = if z3 != 0 {
            Self::eligible(z3, &mut self.served_z3)
        } else if self.revision == Revision::A2000 {
            z2
        } else {
            Self::eligible(z2, &mut self.served_z2)
        };
        if eligible == 0 {
            return None;
        }
        let slot = eligible.trailing_zeros() as usize;
        self.owner = Some(slot);
        if z3 != 0 {
            self.claim_by = Some(now.saturating_add(GRANT_TIMEOUT_CCK));
        }
        self.cycles = 0;
        // Request sampled, CPU completes its cycle and the grant propagates.
        self.ready_at = now + 1;
        Some(slot)
    }

    fn eligible(requests: u8, served: &mut u8) -> u8 {
        if requests & !*served == 0 {
            *served = 0;
        }
        requests & !*served
    }

    pub(crate) fn master_strobe(&mut self) {
        self.claim_by = None;
    }

    /// XRDY extends a Z2 transfer; a Z3 slave delays its DTACK. Keep the
    /// grant while the slave is stretching the current cycle.
    pub(crate) fn wait_for_slave(&mut self, wait: u32, now: u64) -> bool {
        if wait != 0 && !self.waiting_for_slave {
            self.waiting_for_slave = true;
            self.ready_at = now.saturating_add(u64::from(wait));
            return true;
        }
        false
    }

    pub(crate) fn complete(&mut self, slot: usize, z3: bool, locked: bool, now: u64) {
        self.claim_by = None;
        self.waiting_for_slave = false;
        self.ready_at = now + if z3 { 1 } else { 2 };
        if slot == LOCAL_MASTER {
            return;
        }
        if z3 {
            self.served_z3 |= 1 << slot;
            self.cycles = self.cycles.saturating_add(1);
            if self.cycles >= 8 && !locked {
                self.owner = None;
            }
        } else {
            self.served_z2 |= 1 << slot;
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self {
            revision: self.revision,
            devices: self.devices,
            wired: self.wired,
            #[cfg(feature = "wasm-boards")]
            wasm_slots: self.wasm_slots,
            ..Self::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dip_grants_slot_order_and_holds_own_until_released() {
        let mut b = Buster {
            revision: Revision::A2000,
            z2: 0b11010,
            ..Default::default()
        };
        assert_eq!(b.grant(0), Some(1));
        b.complete(1, false, false, 1);
        b.z2 |= 1;
        assert_eq!(b.grant(3), Some(1));
        b.z2 &= !(1 << 1);
        assert_eq!(b.grant(4), Some(0));
    }

    #[test]
    fn local_master_precedes_z3_and_z2_fairness_is_per_slot() {
        let mut b = Buster {
            revision: Revision::Rev9,
            z2: 3,
            local_request: true,
            ..Default::default()
        };
        b.sample_registration(4);
        assert_eq!(b.grant(0), Some(LOCAL_MASTER));
        b.local_request = false;
        assert_eq!(b.grant(1), Some(2));
        b.sample_registration(0);
        assert_eq!(b.grant(2), Some(0));
        b.complete(0, false, false, 3);
        b.z2 = 2;
        assert_eq!(b.grant(4), None);
        b.z2 = 3;
        assert_eq!(b.grant(5), Some(1));
    }

    #[test]
    fn level_two_rearbitrates_on_the_eighth_completed_cycle() {
        for revision in [Revision::Rev9, Revision::Rev11] {
            let mut b = Buster {
                revision,
                ..Default::default()
            };
            b.sample_registration(0b10);
            assert_eq!(b.grant(0), Some(1));
            b.sample_registration(0b11);
            for now in 1..=8 {
                assert_eq!(b.grant(now), Some(1));
                b.complete(1, true, false, now);
                assert_eq!(b.owner.is_none(), now == 8);
            }
            // Rev 11 keeps one registered master until it deregisters.
            assert_eq!(
                b.grant(9),
                Some(if revision == Revision::Rev11 { 1 } else { 0 })
            );
            b.sample_registration(1);
            assert_eq!(b.grant(10), Some(0));
        }
    }

    #[test]
    fn locked_master_releases_after_the_first_unlocked_cycle() {
        let mut b = Buster {
            revision: Revision::Rev9,
            ..Default::default()
        };
        b.sample_registration(3);
        assert_eq!(b.grant(0), Some(0));
        for now in 1..=10 {
            b.master_strobe();
            b.complete(0, true, true, now);
            assert_eq!(b.grant(now + 1), Some(0));
        }
        b.complete(0, true, false, 11);
        assert_eq!(b.grant(12), Some(1));
    }

    #[test]
    fn unclaimed_grant_expires_but_slave_wait_does_not_unregister() {
        for revision in [Revision::Rev9, Revision::Rev11] {
            let mut b = Buster {
                revision,
                ..Default::default()
            };
            b.sample_registration(3);
            assert_eq!(b.grant(0), Some(0));
            assert_eq!(b.grant(GRANT_TIMEOUT_CCK - 1), Some(0));
            assert_eq!(b.grant(GRANT_TIMEOUT_CCK), Some(1));
            b.master_strobe();
            assert!(b.wait_for_slave(100, GRANT_TIMEOUT_CCK + 1));
            assert_eq!(b.grant(110), Some(1));
            b.complete(1, true, false, 117);
            b.sample_registration(1);
            assert_eq!(b.grant(118), None);
            b.sample_registration(0);
            b.sample_registration(1);
            assert_eq!(b.grant(119), Some(0));
        }
    }

    #[test]
    fn rev7_ignores_registration_and_rev9_overlap_requires_reset() {
        for revision in [Revision::Rev7, Revision::Rev9, Revision::Rev11] {
            let mut b = Buster {
                revision,
                slave_start: true,
                ..Default::default()
            };
            b.sample_registration(1);
            assert_eq!(b.grant(1).is_some(), revision == Revision::Rev11);
            b.reset();
            b.sample_registration(1);
            assert_eq!(b.grant(2).is_some(), revision != Revision::Rev7);
        }
    }
}
