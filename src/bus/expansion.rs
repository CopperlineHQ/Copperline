// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::buster::Revision;
use crate::zorro_device::{BoardDevice, DeviceHost, ZorroDevice};

impl BoardDevice {
    /// Host-service registers receive the submitted CPU operand as one ABI call.
    pub(crate) fn whole_operand(&self) -> bool {
        match self {
            Self::Filesys(_) | Self::Copperhf(_) | Self::Z3660(_) => true,
            #[cfg(feature = "wasm-boards")]
            Self::Wasm(_) => true,
            #[cfg(feature = "mhi")]
            Self::Mhi(_) => true,
            _ => false,
        }
    }
}

impl Bus {
    pub(crate) fn wire_buster(&mut self) -> anyhow::Result<()> {
        self.mem.zorro.controller = self.buster.revision;
        self.mem.zorro.rebuild_regions();
        self.buster.devices = [None; 5];
        self.buster.wired = false;
        #[cfg(feature = "wasm-boards")]
        {
            self.buster.wasm_slots = 0;
        }
        let fitted = self.buster.revision != Revision::Absent;
        if let Some(local) = self.sdmac.as_mut() {
            local.arbitrated = fitted;
            self.buster.wired = fitted;
        }
        for (index, board) in self.devices.iter_mut().enumerate() {
            let slot = fitted
                .then(|| self.mem.zorro.physical_slot(index))
                .flatten();
            match board {
                BoardDevice::A2091(d) => d.arbitrated = slot.is_some(),
                BoardDevice::A4091(d) => d.arbitrated = slot.is_some(),
                #[cfg(feature = "wasm-boards")]
                BoardDevice::Wasm(d) if d.can_dma() => {
                    d.dma.version = slot.and_then(|_| self.mem.zorro.device_version(index));
                }
                _ => continue,
            }
            if let Some(slot) = slot {
                let pin = self.buster.devices.get_mut(slot).ok_or_else(|| {
                    anyhow::anyhow!("DMA controller exceeds the five Buster request lines")
                })?;
                *pin = Some(index);
                #[cfg(feature = "wasm-boards")]
                if matches!(board, BoardDevice::Wasm(_)) {
                    self.buster.wasm_slots |= 1 << slot;
                }
                self.buster.wired = true;
            }
        }
        self.sample_buster_requests();
        Ok(())
    }

    #[inline]
    pub(crate) fn sample_buster_requests(&mut self) {
        // Called on every device advance; a machine with no Buster-wired
        // DMA master has nothing to sample.
        if self.buster.wired {
            self.sample_wired_buster_requests();
        } else {
            self.buster.settle_idle();
        }
    }

    #[inline(never)]
    fn sample_wired_buster_requests(&mut self) {
        let mut z2 = 0;
        let mut z3 = 0;
        for (slot, index) in self.buster.devices.iter().enumerate() {
            let Some(index) = *index else { continue };
            match &self.devices[index] {
                BoardDevice::A2091(d) if d.request_address().is_some() => z2 |= 1 << slot,
                BoardDevice::A4091(d)
                    if d.request_address().is_some() && self.buster.revision.z3_master() =>
                {
                    z3 |= 1 << slot
                }
                #[cfg(feature = "wasm-boards")]
                BoardDevice::Wasm(d) if d.dma.request_address().is_some() => {
                    if d.dma.z3() {
                        if self.buster.revision.z3_master() {
                            z3 |= 1 << slot;
                        }
                    } else {
                        z2 |= 1 << slot;
                    }
                }
                _ => {}
            }
        }
        self.buster.note_requests_sampled();
        self.buster.z2 = z2;
        self.buster.sample_registration(z3);
        self.buster.local_request = self
            .sdmac
            .as_ref()
            .is_some_and(|d| d.request_address().is_some());
    }

    /// Host-service mailboxes take whole CPU operands. Physical card
    /// registers and RAM see separate strobes at the physical port width.
    pub(crate) fn zorro_first_beat(&self, addr: u32, size: usize) -> usize {
        use crate::zorro::BoardBacking;
        let chain = &self.mem.zorro;
        // Adjacent boards have separate selects even when a misaligned CPU
        // operand spans their boundary. AutoConfig addresses are 64K aligned.
        let to_boundary = 0x10000 - (addr as usize & 0xffff);
        if to_boundary < size {
            let next = addr.wrapping_add(to_boundary as u32);
            if chain.region_at(addr, 1).is_some()
                || chain.device_region_at(addr, 1).is_some()
                || chain.region_at(next, 1).is_some()
                || chain.device_region_at(next, 1).is_some()
            {
                return to_boundary;
            }
        }
        let version = if let Some((idx, _)) = chain.region_at(addr, size) {
            chain.ram_version(idx)
        } else if let Some((BoardBacking::Device(index), _)) = chain.device_region_at(addr, size) {
            if self.devices[index].whole_operand() {
                return size;
            }
            chain.device_version(index).expect("mapped board")
        } else {
            return size;
        };
        let width = version.width();
        size.min(width - (addr as usize & (width - 1)))
    }

    pub(crate) fn quick_vector(&self, level: u8) -> Option<u8> {
        if level != 2 || !self.buster.revision.z3_master() {
            return None;
        }
        self.buster
            .devices
            .iter()
            .flatten()
            .find_map(|&index| match &self.devices[index] {
                BoardDevice::A4091(d) => d.interrupt_vector(),
                _ => None,
            })
    }

    pub(super) fn buster_pending(&self) -> bool {
        self.buster.pending()
    }

    pub(crate) fn set_cpu_rmw_lock(&mut self, asserted: bool) {
        if asserted && self.cpu_bus_arbitration_enabled {
            self.settle_cpu_posted_writes();
            if self.buster.wired {
                self.begin_buster_cpu_cycle();
            }
        }
        self.buster.cpu_locked = asserted;
        self.buster.cpu_driving = asserted;
    }

    /// Finish the outstanding grant before driving /AS. The final turnaround
    /// clock is part of the master's cycle. Between Z3 scheduling atoms the
    /// local CPU can complete its pending cycle.
    pub(crate) fn begin_buster_cpu_cycle(&mut self) {
        if self.buster_pending() && self.cpu_bus_arbitration_enabled {
            loop {
                self.clock_buster_wait();
                if self.buster.owner.is_none() {
                    break;
                }
            }
        }
        self.buster.cpu_driving = true;
    }

    pub(crate) fn clock_buster_wait(&mut self) {
        let tick = self.advance_chipset(1);
        if self.cpu_bus_arbitration_enabled {
            self.record_slice_bus_advance(1, tick);
        } else {
            self.pending_device_cck = self.pending_device_cck.saturating_add(1);
            add_agnus_tick(&mut self.pending_device_tick, tick);
        }
    }

    /// Return true when this cycle uses the Agnus CPU port. Zorro-to-Zorro
    /// cycles and DMA to 32-bit local RAM advance alongside chipset DMA.
    pub(super) fn service_buster(&mut self, chip_free: bool) -> bool {
        let now = self.emulated_cck;
        let Some(slot) = self.buster.grant(now) else {
            return false;
        };
        if now < self.buster.ready_at {
            return false;
        }
        if slot == crate::buster::LOCAL_MASTER {
            let local = self.sdmac.as_mut().expect("local request without SDMAC");
            let Some(addr) = local.request_address() else {
                return false;
            };
            let chip = addr < self.mem.chip_ram.len() as u32;
            if chip && !chip_free {
                self.note_cpu_request_passed_over();
                return false;
            }
            local.transfer_word(&mut self.mem);
            let wrote = local.take_wrote_memory();
            self.devices_wrote_memory |= wrote;
            if local.int_line() {
                self.paula.intreq |= INT_PORTS;
            }
            if chip && wrote {
                self.record_expansion_chip_write(addr, 2);
            }
            self.buster.complete(slot, false, false, now);
            self.sample_buster_requests();
            return chip;
        }
        let Some(index) = self.buster.devices.get(slot).copied().flatten() else {
            return false;
        };
        let (addr, z3) = match &self.devices[index] {
            BoardDevice::A2091(d) => (d.request_address(), false),
            BoardDevice::A4091(d) => (d.request_address(), true),
            #[cfg(feature = "wasm-boards")]
            BoardDevice::Wasm(d) => (d.dma.request_address(), d.dma.z3()),
            _ => return false,
        };
        let Some(addr) = addr else { return false };
        self.buster.master_strobe();
        if self
            .buster
            .wait_for_slave(self.mem.zorro.slave_wait(addr), now)
        {
            return false;
        }
        let overridden = self.mem.zorro.override_ram(addr, 1).is_some();
        let chip = !overridden && addr < self.mem.chip_ram.len() as u32;
        let slow = !overridden
            && addr.wrapping_sub(crate::memory::SLOW_RAM_BASE as u32)
                < self.mem.slow_ram.len() as u32;
        if (chip || slow) && !chip_free {
            self.note_cpu_request_passed_over();
            return false;
        }

        let chain = &self.mem.zorro;
        let device = chain
            .device_region_at(addr, 1)
            .and_then(|(backing, off)| match backing {
                crate::zorro::BoardBacking::Device(slot) => Some((slot, off)),
                _ => None,
            });
        let rom = if overridden {
            None
        } else {
            [
                (crate::memory::ROM_BASE, self.mem.rom.as_slice()),
                (self.mem.extended_rom_base, self.mem.extended_rom.as_slice()),
            ]
            .into_iter()
            .find_map(|(base, rom)| {
                let off = u64::from(addr).checked_sub(base)?;
                (off < rom.len() as u64).then(|| &rom[off as usize..])
            })
        };
        let z3_port = chip
            || rom.is_some()
            || addr.wrapping_sub(self.mem.mb_ram_base() as u32) < self.mem.mb_ram.len() as u32
            || addr.wrapping_sub(crate::memory::ACCEL_RAM_BASE as u32)
                < self.mem.accel_ram.len() as u32
            || chain
                .region_at(addr, 1)
                .is_some_and(|(idx, _)| chain.ram_version(idx) == crate::zorro::ZorroVersion::III)
            || device.is_some_and(|(slot, _)| {
                chain.device_version(slot) == Some(crate::zorro::ZorroVersion::III)
            });
        let fault = (z3 && !z3_port) || chain.collision(self.buster.revision, addr, 1);
        #[cfg(feature = "wasm-boards")]
        if let BoardDevice::Wasm(board) = &mut self.devices[index] {
            let mut host = DeviceHost::for_slot(&mut self.mem, index);
            let queued = board.dma.writing();
            let written = board.dma.transfer(addr, !fault, &mut host);
            if queued && !board.dma.writing() {
                if board.int2_line() {
                    self.paula.intreq |= INT_PORTS;
                }
                if board.int6_line() {
                    self.paula.intreq |= INT_EXTER;
                }
            }
            self.devices_wrote_memory |= written != 0;
            if chip && written != 0 {
                self.record_expansion_chip_write(addr, written);
            }
            self.buster.complete(slot, z3, false, now);
            self.sample_buster_requests();
            return chip || slow;
        }
        let mut written = 0;
        if fault {
            match &mut self.devices[index] {
                BoardDevice::A2091(d) => d.release_on_bus_error(),
                BoardDevice::A4091(d) => d.dma_bus_error(),
                _ => unreachable!(),
            }
        } else if let Some(rom) = rom {
            match &mut self.devices[index] {
                BoardDevice::A2091(d) => d.transfer_with(|_, _, _| {
                    u16::from_be_bytes([rom[0], rom.get(1).copied().unwrap_or(0xff)])
                }),
                BoardDevice::A4091(d) => d.transfer_with(|_, _, data| {
                    if let crate::buster::Transfer::Read(bytes) = data {
                        for (offset, byte) in bytes.iter_mut().enumerate() {
                            *byte = rom.get(offset).copied().unwrap_or(0xff);
                        }
                    }
                }),
                _ => unreachable!(),
            }
        } else if let Some((target, off)) = device.filter(|&(target, _)| target != index) {
            let [master, slave] = self
                .devices
                .get_disjoint_mut([index, target])
                .expect("distinct board slots");
            let mut host = DeviceHost::for_slot(&mut self.mem, target);
            match master {
                BoardDevice::A2091(d) => d.transfer_with(|_, _, write| {
                    if let Some(value) = write {
                        slave.write(off, 2, u32::from(value), &mut host);
                        value
                    } else {
                        slave.read(off, 2, &mut host) as u16
                    }
                }),
                BoardDevice::A4091(d) => {
                    d.transfer_with(|_, _, data| data.at_device(slave, off, &mut host))
                }
                _ => unreachable!(),
            }
            self.devices_wrote_memory |= host.wrote_memory();
            if slave.int2_line() {
                self.paula.intreq |= INT_PORTS;
            }
        } else {
            match &mut self.devices[index] {
                BoardDevice::A2091(d) => {
                    d.transfer_word(&mut self.mem);
                    if d.take_dma_write() {
                        self.devices_wrote_memory = true;
                        written = 2;
                    }
                }
                BoardDevice::A4091(d) => {
                    let mut host = DeviceHost::for_slot(&mut self.mem, index);
                    written = d.transfer_cycle(&mut host);
                    self.devices_wrote_memory |= host.wrote_memory();
                }
                _ => unreachable!(),
            }
        }
        if chip && written != 0 {
            self.record_expansion_chip_write(addr, written);
        }
        if self.devices[index].int2_line() {
            self.paula.intreq |= INT_PORTS;
        }
        // Neither the A2091 nor the A4091 asserts Zorro III /LOCK.
        self.buster.complete(slot, z3, false, now);
        self.sample_buster_requests();
        chip || slow
    }

    fn record_expansion_chip_write(&mut self, addr: u32, size: usize) {
        let off = addr as usize;
        if let Some(bytes) = self.mem.chip_ram.get(off..off + size) {
            self.current_frame_chip_ram_writes
                .push(BeamChipRamWrite::from_bytes(
                    self.agnus.vpos,
                    self.agnus.hpos,
                    addr,
                    bytes,
                ));
            if bytes.len() >= 2 {
                let n = bytes.len();
                self.data_bus = u16::from_be_bytes([bytes[n - 2], bytes[n - 1]]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::tests::empty_bus;
    use crate::zorro::BoardSpec;

    fn scripted(revision: Revision, dest: u32) -> Bus {
        let mut bus = empty_bus();
        bus.mem
            .zorro
            .add_board_configured_at(BoardSpec::a4091(0), 0x4000_0000)
            .unwrap();
        bus.devices.push(BoardDevice::A4091(
            crate::a4091::A4091::new(vec![0; 65536]).unwrap(),
        ));
        bus.buster.revision = revision;
        bus.wire_buster().unwrap();
        // COPY 256 bytes from chip RAM, then INT.
        for (i, v) in [0xc000_0100u32, 0x2000, dest, 0x9808_0000, 17]
            .into_iter()
            .enumerate()
        {
            bus.mem.chip_ram[0x1000 + i * 4..0x1004 + i * 4].copy_from_slice(&v.to_be_bytes());
        }
        bus.mem.chip_ram[0x2000..0x2100].fill(0xa5);
        let mut host = DeviceHost::for_slot(&mut bus.mem, 0);
        let d = &mut bus.devices[0];
        d.write(0x880003, 1, 0x63, &mut host);
        d.write(0x80003a, 1, 0x24, &mut host); // DIEN.SIR | BF
        d.write(0x80002c, 4, 0x1000, &mut host);
        bus.sample_buster_requests();
        bus
    }

    #[test]
    fn script_dma_restores_an_inflight_grant_and_quick_interrupt() {
        let mut bus = scripted(Revision::Rev11, 0x3000);
        bus.advance_chipset(34);
        assert_eq!(bus.mem.chip_ram[0x3000], 0xa5);
        assert_eq!(bus.mem.chip_ram[0x30ff], 0);
        let bytes = rmp_serde::to_vec_named(&bus).unwrap();
        let mut copy: Bus = rmp_serde::from_slice(&bytes).unwrap();
        copy.wire_buster().unwrap();
        for _ in 0..800 {
            bus.advance_chipset(1);
            copy.advance_chipset(1);
            assert_eq!(bus.buster.owner, copy.buster.owner);
            assert_eq!(
                bus.mem.chip_ram[0x3000..0x3100],
                copy.mem.chip_ram[0x3000..0x3100]
            );
            assert_eq!(bus.quick_vector(2), copy.quick_vector(2));
        }
        assert_eq!(bus.quick_vector(2), Some(0x63));
        assert_eq!(bus.quick_vector(3), None);
        assert!(bus.mem.chip_ram[0x3000..0x3100].iter().all(|&b| b == 0xa5));
    }

    #[test]
    fn z3_master_can_write_another_z3_cards_register_window() {
        let mut bus = scripted(Revision::Rev11, 0x42800034);
        bus.mem.chip_ram[0x1003] = 4;
        bus.mem.chip_ram[0x1002] = 0;
        bus.mem
            .zorro
            .add_board_configured_at(BoardSpec::a4091(1), 0x42000000)
            .unwrap();
        bus.devices.push(BoardDevice::A4091(
            crate::a4091::A4091::new(vec![0; 65536]).unwrap(),
        ));
        bus.wire_buster().unwrap();
        bus.advance_chipset(64);
        let mut host = DeviceHost::for_slot(&mut bus.mem, 1);
        assert_eq!(bus.devices[1].read(0x800034, 4, &mut host), 0xa5a5a5a5);
        assert_eq!(bus.quick_vector(2), Some(0x63));
    }

    #[test]
    fn script_can_fetch_data_from_system_rom() {
        let mut bus = scripted(Revision::Rev11, 0x3000);
        bus.mem.chip_ram[0x1004..0x1008]
            .copy_from_slice(&(crate::memory::ROM_BASE as u32).to_be_bytes());
        bus.mem.rom[..256].fill(0x56);
        bus.advance_chipset(2000);
        assert_eq!(bus.quick_vector(2), Some(0x63));
        assert_eq!(&bus.mem.chip_ram[0x3000..0x3100], &bus.mem.rom[..256]);
    }

    #[test]
    fn fixed_display_slots_delay_dma_to_chip_ram() {
        fn clocks(display: bool) -> u64 {
            let mut bus = scripted(Revision::Rev11, 0x3000);
            bus.agnus.vpos = 0x2c;
            bus.agnus.hpos = 0x16;
            bus.denise.diwstrt = 0x2c81;
            bus.denise.diwstop = 0x40c1;
            bus.denise.ddfstrt = 0x28;
            bus.denise.ddfstop = 0xc0;
            bus.denise.bplcon0 = 0x6200;
            if display {
                bus.agnus.dmacon = DMACON_DMAEN | DMACON_BPLEN;
            }
            for _ in 0..2000 {
                bus.advance_chipset(1);
                if bus.quick_vector(2).is_some() {
                    return bus.emulated_cck();
                }
            }
            panic!("DMA did not finish");
        }
        assert!(clocks(true) > clocks(false));
    }

    #[test]
    fn level_one_does_not_run_a_z3_script() {
        let mut bus = scripted(Revision::Rev7, 0x3000);
        bus.advance_chipset(600);
        assert_eq!(bus.mem.chip_ram[0x3000], 0);
        assert_eq!(bus.quick_vector(2), None);
        assert!(!bus.buster_pending());
    }

    #[test]
    fn z3_dma_cannot_bridge_to_z2_ram() {
        let mut bus = scripted(Revision::Rev11, 0x200000);
        bus.mem
            .zorro
            .add_board_configured_at(BoardSpec::fast_ram(512 * 1024), 0x200000)
            .unwrap();
        bus.advance_chipset(64);
        let mut host = DeviceHost::for_slot(&mut bus.mem, 0);
        assert_eq!(bus.devices[0].read(0x80000f, 1, &mut host) & 0x24, 0x20);
        assert_eq!(bus.mem.zorro.board_ram(1)[0], 0);
    }
}
