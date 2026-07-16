// SPDX-License-Identifier: GPL-3.0-or-later
//
//! Culling ROM resident tags before Kickstart initialises them.
//!
//! Exec builds the ResModules list at coldstart by scanning the ROM for
//! resident tags, then walks it in priority order calling each rt_Init. A
//! module we do not want can be removed from that list in the window between
//! the two, without touching the ROM image.
//!
//! The window we use is expansion.library's DIAG pass, which our board's
//! DiagPoint traps into (see `guest/services/entry.s`). expansion.library is
//! priority +110; the ROM's scsi.device is +10, so it has not run yet.
//!
//! This is what UAE/amiberry call `scsidevice_disable`, and it exists for the
//! same reason: a machine whose disk controller we do not emulate yet is
//! better off with no scsi.device than with one that probes absent hardware.
//! On the A4000 that probe is an IDE presence test that stalls the boot for
//! several seconds; on the A3000 the ROM's SCSI driver hangs outright waiting
//! on an interrupt the missing WD33C93 never raises.
//!
//! It is a stopgap, not emulation: a real big-box Amiga has the controller
//! fitted and its scsi.device simply finds no drives. Once the A4000 IDE and
//! the WD33C93 are emulated, the machine profiles should stop setting it.

use m68k::AddressBus;

/// ExecBase.ResModules: the pointer to the resident list exec built at
/// coldstart.
const EXEC_RES_MODULES: u32 = 300;

/// A ResModules entry with bit 31 set is not a Resident but a "the list
/// continues here" pointer. Unlinking an entry means overwriting its slot
/// with one of these, aimed at the following slot.
const RESLIST_JUMP: u32 = 0x8000_0000;

/// struct Resident (exec/resident.h).
const RTC_MATCHWORD: u16 = 0x4AFC;
const RT_MATCH_TAG: u32 = 2;
const RT_TYPE: u32 = 12;
const RT_NAME: u32 = 14;
const NT_DEVICE: u8 = 3;

/// The ROM windows a resident tag can legitimately live in: the extended ROM
/// at $A80000 and Kickstart itself at $F00000. A tag outside them came from
/// RAM (a KickTagged module, say), and is none of our business.
fn in_rom(addr: u32) -> bool {
    (0x00A8_0000..0x00B8_0000).contains(&addr) || (0x00F0_0000..0x0100_0000).contains(&addr)
}

/// Whether the resident tag at `tag` is a ROM device named `name`.
fn is_rom_device(bus: &mut dyn AddressBus, tag: u32, name: &str) -> bool {
    if !in_rom(tag)
        || bus.read_word(tag) != RTC_MATCHWORD
        || bus.read_long(tag + RT_MATCH_TAG) != tag
        || bus.read_byte(tag + RT_TYPE) != NT_DEVICE
    {
        return false;
    }
    // The whole string we compare (name + NUL) must lie in ROM, not just its
    // first byte: a tag pointing at the very end of a window would otherwise
    // be compared against whatever the bus returns past it.
    let rt_name = bus.read_long(tag + RT_NAME);
    if !in_rom(rt_name) || !in_rom(rt_name + name.len() as u32) {
        return false;
    }
    name.bytes()
        .chain(std::iter::once(0)) // the NUL, so "scsi.device2" does not match
        .enumerate()
        .all(|(i, c)| bus.read_byte(rt_name + i as u32) == c)
}

/// Unlink every ROM resident named `name` from ExecBase's ResModules list, so
/// Kickstart never calls its rt_Init. Returns how many were culled.
///
/// Only valid during the DIAG pass: earlier the list does not exist, later the
/// module has already initialised and unlinking it achieves nothing.
pub fn cull_rom_device(bus: &mut dyn AddressBus, name: &str) -> usize {
    let exec_base = bus.read_long(4);
    let mut slot = bus.read_long(exec_base + EXEC_RES_MODULES);
    let mut culled = 0;
    loop {
        let entry = bus.read_long(slot);
        if entry == 0 {
            return culled;
        }
        if entry & RESLIST_JUMP != 0 {
            slot = entry & !RESLIST_JUMP;
            continue;
        }
        if is_rom_device(bus, entry, name) {
            // Skip this slot by turning it into a jump to the next one. The
            // final slot holds the terminating 0, so this is safe even for
            // the last entry.
            bus.write_long(slot, RESLIST_JUMP | (slot + 4));
            culled += 1;
        }
        slot += 4;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat 16 MB memory, so a tag can sit at a ROM address.
    struct FlatBus(Vec<u8>);

    impl FlatBus {
        fn new() -> Self {
            FlatBus(vec![0; 0x0100_0000])
        }
        fn poke(&mut self, addr: u32, bytes: &[u8]) {
            let at = addr as usize;
            self.0[at..at + bytes.len()].copy_from_slice(bytes);
        }
        /// A resident tag for device `name`, with its rt_Name string just past
        /// the tag itself.
        fn poke_device(&mut self, tag: u32, name: &str) {
            let rt_name = tag + 32;
            self.poke(tag, &RTC_MATCHWORD.to_be_bytes());
            self.poke(tag + RT_MATCH_TAG, &tag.to_be_bytes());
            self.poke(tag + RT_TYPE, &[NT_DEVICE]);
            self.poke(tag + RT_NAME, &rt_name.to_be_bytes());
            self.poke(rt_name, name.as_bytes());
            self.poke(rt_name + name.len() as u32, &[0]);
        }
        /// ExecBase at $400 with a ResModules list of `tags` at $600.
        fn poke_res_modules(&mut self, tags: &[u32]) {
            let (exec_base, list) = (0x400u32, 0x600u32);
            self.poke(4, &exec_base.to_be_bytes());
            self.poke(exec_base + EXEC_RES_MODULES, &list.to_be_bytes());
            for (i, tag) in tags.iter().enumerate() {
                self.poke(list + 4 * i as u32, &tag.to_be_bytes());
            }
            self.poke(list + 4 * tags.len() as u32, &0u32.to_be_bytes());
        }
        /// The tags exec would still initialise, in list order.
        fn residents(&mut self) -> Vec<u32> {
            let exec_base = self.read_long(4);
            let mut slot = self.read_long(exec_base + EXEC_RES_MODULES);
            let mut out = Vec::new();
            loop {
                let entry = self.read_long(slot);
                if entry == 0 {
                    return out;
                }
                if entry & RESLIST_JUMP != 0 {
                    slot = entry & !RESLIST_JUMP;
                    continue;
                }
                out.push(entry);
                slot += 4;
            }
        }
    }

    impl AddressBus for FlatBus {
        fn read_byte(&mut self, addr: u32) -> u8 {
            self.0[addr as usize]
        }
        fn read_word(&mut self, addr: u32) -> u16 {
            u16::from_be_bytes(self.0[addr as usize..addr as usize + 2].try_into().unwrap())
        }
        fn read_long(&mut self, addr: u32) -> u32 {
            u32::from_be_bytes(self.0[addr as usize..addr as usize + 4].try_into().unwrap())
        }
        fn write_byte(&mut self, addr: u32, value: u8) {
            self.0[addr as usize] = value;
        }
        fn write_word(&mut self, addr: u32, value: u16) {
            self.poke(addr, &value.to_be_bytes());
        }
        fn write_long(&mut self, addr: u32, value: u32) {
            self.poke(addr, &value.to_be_bytes());
        }
    }

    const EXEC: u32 = 0x00F8_0042;
    const SCSI: u32 = 0x00F8_590C;
    const AUDIO: u32 = 0x00F8_968C;

    #[test]
    fn the_named_device_is_unlinked_and_its_neighbours_survive() {
        let mut bus = FlatBus::new();
        bus.poke_device(EXEC, "exec.library");
        bus.poke_device(SCSI, "scsi.device");
        bus.poke_device(AUDIO, "audio.device");
        bus.poke_res_modules(&[EXEC, SCSI, AUDIO]);

        assert_eq!(cull_rom_device(&mut bus, "scsi.device"), 1);
        assert_eq!(bus.residents(), vec![EXEC, AUDIO]);
    }

    #[test]
    fn a_device_in_the_last_slot_is_unlinked_without_running_off_the_list() {
        let mut bus = FlatBus::new();
        bus.poke_device(EXEC, "exec.library");
        bus.poke_device(SCSI, "scsi.device");
        bus.poke_res_modules(&[EXEC, SCSI]);

        assert_eq!(cull_rom_device(&mut bus, "scsi.device"), 1);
        assert_eq!(bus.residents(), vec![EXEC]);
    }

    #[test]
    fn culling_is_idempotent_and_reports_nothing_when_the_device_is_absent() {
        let mut bus = FlatBus::new();
        bus.poke_device(EXEC, "exec.library");
        bus.poke_res_modules(&[EXEC]);

        assert_eq!(cull_rom_device(&mut bus, "scsi.device"), 0);
        assert_eq!(bus.residents(), vec![EXEC]);
    }

    #[test]
    fn a_ram_resident_of_the_same_name_is_left_alone() {
        // A KickTagged scsi.device in RAM belongs to whoever installed it.
        let mut bus = FlatBus::new();
        let ram_scsi = 0x0020_0000;
        bus.poke_device(ram_scsi, "scsi.device");
        bus.poke_res_modules(&[ram_scsi]);

        assert_eq!(cull_rom_device(&mut bus, "scsi.device"), 0);
        assert_eq!(bus.residents(), vec![ram_scsi]);
    }

    #[test]
    fn the_list_is_followed_across_a_jump_entry() {
        // Exec's list is not always contiguous: an entry with bit 31 set
        // continues it elsewhere, and a tag can sit past one.
        let mut bus = FlatBus::new();
        bus.poke_device(EXEC, "exec.library");
        bus.poke_device(SCSI, "scsi.device");
        bus.poke_res_modules(&[EXEC]);
        let (list, tail) = (0x600u32, 0x700u32);
        bus.poke(list + 4, &(RESLIST_JUMP | tail).to_be_bytes());
        bus.poke(tail, &SCSI.to_be_bytes());
        bus.poke(tail + 4, &0u32.to_be_bytes());
        assert_eq!(bus.residents(), vec![EXEC, SCSI]);

        assert_eq!(cull_rom_device(&mut bus, "scsi.device"), 1);
        assert_eq!(bus.residents(), vec![EXEC]);
    }

    #[test]
    fn a_library_of_the_same_name_is_not_a_device() {
        let mut bus = FlatBus::new();
        bus.poke_device(SCSI, "scsi.device");
        bus.poke(SCSI + RT_TYPE, &[9]); // NT_LIBRARY
        bus.poke_res_modules(&[SCSI]);

        assert_eq!(cull_rom_device(&mut bus, "scsi.device"), 0);
    }

    #[test]
    fn a_longer_name_with_the_same_prefix_does_not_match() {
        let mut bus = FlatBus::new();
        bus.poke_device(SCSI, "scsi.device.old");
        bus.poke_res_modules(&[SCSI]);

        assert_eq!(cull_rom_device(&mut bus, "scsi.device"), 0);
    }

    #[test]
    fn a_name_running_off_the_rom_window_is_never_compared() {
        // rt_Name so close to the end of the ROM that the string would spill
        // past it: the tag is ignored rather than compared against whatever
        // sits beyond the window.
        let mut bus = FlatBus::new();
        bus.poke_device(SCSI, "scsi.device");
        let near_end = 0x00FF_FFF8u32;
        bus.poke(SCSI + RT_NAME, &near_end.to_be_bytes());
        bus.poke(near_end, b"scsi.de"); // truncated by the window edge
        bus.poke_res_modules(&[SCSI]);

        assert_eq!(cull_rom_device(&mut bus, "scsi.device"), 0);
    }
}
