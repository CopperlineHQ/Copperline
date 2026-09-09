// SPDX-License-Identifier: GPL-3.0-or-later

//! Gayle gate array (A600/A1200): the ID register at $DE1000, the IDE
//! interface at $DA0000, and the PCMCIA status/change/enable/config
//! registers at $DA8000-$DAB000.
//!
//! Decode and register layout follow the Commodore schematics as captured by
//! the Linux `gayle.c` IDE driver, the `amigayle.h` header (disassembled
//! from card.resource), and WinUAE's `gayle.cpp`: the IDE task file lives at
//! $DA2000 with a 4-byte stride (byte registers on the odd word half, offset
//! base+4*reg+2), and the control block register at base+$101A. None of
//! this is on the chip bus; the CPU reaches it through
//! `cpu_external_access`.
//!
//! The PCMCIA side: Gayle samples the slot's CD, BVD1, BVD2, WP, and
//! READY/IREQ pins into the status register at $DA8000, latches every
//! change of them into $DA9000 (write-to-clear, AND semantics), and drives
//! INT2 or INT6 for the latched sources the $DAA000 enable register admits
//! -- card detect always on INT6, write-enable and IDE on INT2, battery and
//! busy/interrupt on whichever of the two the enable register's level bits
//! pick. $DAB000 holds the programming-voltage and access-speed
//! configuration. The slot's address windows ($600000 common memory,
//! $A00000 attribute, $A20000/$A30000 I/O, $A40000 reset) are decoded by
//! the bus with the card in [`crate::pcmcia`]; Gayle only says whether the
//! slot is enabled.
//!
//! The drives, the task file, and the command engine are the shared ATA core
//! in [`crate::ata`]; Gayle is the front-end that decodes for it and adds its
//! own ID, interrupt, and PCMCIA registers.

use crate::ata::{task_file_reg, AtaBus, AtaDevice, IdeReg};
use crate::pcmcia::{PIN_BSY_IRQ, PIN_BVD1, PIN_BVD2, PIN_CCDET, PIN_WR};

pub use crate::ata::{AtapiDrive, IdeDrive, MAX_MULTIPLE, SECTOR_SIZE};

// Gayle interrupt/status bit layout (shared by the status, interrupt
// change, and interrupt enable registers).
pub const GAYLE_IRQ_IDE: u8 = 0x80;
/// Card detect changed ($DA9000) / card-detect interrupt enable ($DAA000).
pub const GAYLE_IRQ_CCDET: u8 = PIN_CCDET;
/// Battery voltage 1 / status change.
pub const GAYLE_IRQ_BVD1: u8 = PIN_BVD1;
/// Battery voltage 2 / digital audio.
pub const GAYLE_IRQ_BVD2: u8 = PIN_BVD2;
/// Write enable changed.
pub const GAYLE_IRQ_WR: u8 = PIN_WR;
/// Busy / card interrupt request.
pub const GAYLE_IRQ_BSY: u8 = PIN_BSY_IRQ;
/// The pin-change latches: every status bit that is a slot pin.
pub const GAYLE_PIN_MASK: u8 = PIN_CCDET | PIN_BVD1 | PIN_BVD2 | PIN_WR | PIN_BSY_IRQ;
/// $DA9000 bit 1: reset the machine when card detect changes (preliminary
/// Gayle datasheet; WinUAE `GAYLE_IRQ_RESET`).
pub const GAYLE_IRQ_RESET: u8 = 0x02;
/// $DA9000 bit 0: bus-error the access when card detect changes (WinUAE
/// `GAYLE_IRQ_BERR`). Both bits written together reset the card's
/// configuration instead.
pub const GAYLE_IRQ_BERR: u8 = 0x01;
/// $DAA000 bit 1: battery-voltage interrupts on INT6 instead of INT2.
pub const GAYLE_INT_BVD_LEV: u8 = 0x02;
/// $DAA000 bit 0: busy/IRQ interrupts on INT6 instead of INT2.
pub const GAYLE_INT_BSY_LEV: u8 = 0x01;
/// $DA8000 write bit 1: enable the card's digital-audio output.
pub const GAYLE_CS_DAEN: u8 = 0x02;
/// $DA8000 write bit 0: disable the slot (windows unmapped, pins read as an
/// empty socket).
pub const GAYLE_CS_DIS: u8 = 0x01;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Gayle {
    /// $DE1000 ID shifted out MSB-first on D7: $D0 (A600) / $D1 (A1200).
    id: u8,
    id_bit: u8,
    /// $DA9000 latched interrupt-change bits (write-to-clear with AND),
    /// plus the RESET/BERR control bits in 1:0.
    intreq: u8,
    /// $DAA000 interrupt enable, plus the BVD/BSY level-select bits in 1:0.
    intena: u8,
    /// $DAB000 config (PCMCIA programming voltage, access speed).
    config: u8,
    /// The IDE cable behind the gate array.
    ata: AtaBus,
    /// The slot pins as the card drives them, in status-register layout
    /// (zero for an empty socket). The bus refreshes this whenever the card
    /// changes or is accessed.
    #[serde(default)]
    raw_pins: u8,
    /// The pins as the status register last showed them: `raw_pins` while
    /// the slot is enabled, an empty socket otherwise. The change latches
    /// are the difference between successive values.
    #[serde(default)]
    pins: u8,
    /// $DA8000 as written: bits 1:0 are the DAEN/DIS controls, bits 7:2 read
    /// back OR-ed into the status (card.resource's CardMiscControl writes
    /// the write-protect-override bit there).
    #[serde(default)]
    status_control: u8,
    /// The slot windows overlap Zorro II space: with more than 4 MiB of
    /// fast RAM configured there, Gayle's PCMCIA decode gives way and the
    /// slot behaves as empty.
    #[serde(default)]
    slot_shadowed: bool,
    /// A write to $DA9000 with RESET and BERR both set asks for the card's
    /// configuration to be reset; the bus drains this to reach the card.
    #[serde(default)]
    card_reset_request: bool,
    /// Card detect changed with $DA9000's RESET bit set: reset the machine.
    #[serde(default)]
    machine_reset_request: bool,
}

impl Gayle {
    pub fn new(id: u8) -> Self {
        Self {
            id,
            id_bit: 0,
            intreq: 0,
            intena: 0,
            config: 0,
            ata: AtaBus::new(),
            raw_pins: 0,
            pins: 0,
            status_control: 0,
            slot_shadowed: false,
            card_reset_request: false,
            machine_reset_request: false,
        }
    }

    /// Drain the activity latch set by command issue and data-port traffic.
    /// The bus polls this after each Gayle access to time the HDD LED.
    pub fn take_activity(&mut self) -> bool {
        self.ata.take_activity()
    }

    pub fn attach_drive(&mut self, slot: usize, drive: impl Into<AtaDevice>) {
        self.ata.attach_drive(slot, drive);
    }

    /// A numbered hard disk for frontend save persistence.
    pub fn hard_disk_mut(&mut self, slot: usize) -> Option<&mut crate::harddrive::HardDriveImage> {
        self.ata.hard_disk_mut(slot)
    }

    /// The ATAPI CD-ROM drive behind this port, if either slot holds one.
    pub fn first_atapi_ref(&self) -> Option<&crate::scsi::ScsiCdRom> {
        self.ata.first_atapi_ref()
    }

    /// Mutable counterpart of [`Self::first_atapi_ref`].
    pub fn first_atapi_mut(&mut self) -> Option<&mut crate::scsi::ScsiCdRom> {
        self.ata.first_atapi_mut()
    }

    /// Advance every ATAPI drive behind this port (master and slave alike).
    pub fn tick_atapi(&mut self, cck: u32, cd_audio: &mut crate::chipset::paula::CdAudioRing) {
        self.ata.tick_atapi(cck, cd_audio);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn pending_host_disks(&self, out: &mut Vec<(String, String, bool)>) {
        self.ata.pending_host_disks(out);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn materialize_host_disks(&mut self) -> anyhow::Result<()> {
        self.ata.materialize_host_disks()
    }

    /// Let go of any real disk of the host's, and say how many went.
    pub fn release_host_disks(&mut self) -> usize {
        self.ata.release_host_disks()
    }

    /// System reset: clear the register file and any in-flight transfer but
    /// keep the mounted drives. The card stays in its socket: its pins are
    /// sampled afresh with no change latched, as on a real power-on.
    pub fn reset(&mut self) {
        self.id_bit = 0;
        self.intreq = 0;
        self.intena = 0;
        self.config = 0;
        self.status_control = 0;
        self.card_reset_request = false;
        self.machine_reset_request = false;
        self.pins = self.effective_pins();
        self.ata.reset();
    }

    // ----- PCMCIA slot state -----------------------------------------------

    /// Whether the slot's address windows are decoded: not shadowed by
    /// Zorro II RAM, and not disabled through the status register.
    pub fn slot_enabled(&self) -> bool {
        !self.slot_shadowed && self.status_control & GAYLE_CS_DIS == 0
    }

    pub fn slot_shadowed(&self) -> bool {
        self.slot_shadowed
    }

    /// Decide the fast-RAM conflict rule at machine build: with more than
    /// 4 MiB of Zorro II fast RAM the common window is RAM and the whole
    /// slot goes dark.
    pub fn set_slot_shadowed(&mut self, shadowed: bool) {
        self.slot_shadowed = shadowed;
        self.sync_pins();
    }

    /// The card's pins as the bus last sampled them.
    pub fn card_pins(&self) -> u8 {
        self.raw_pins
    }

    /// Report the pins the card in the socket drives (zero for an empty
    /// socket). Any bit that differs from what the status register showed
    /// is latched into $DA9000; a high busy/IRQ pin is latched for as long
    /// as it stays high (it is a level, as the IDE bit is).
    pub fn set_card_pins(&mut self, raw: u8) {
        self.raw_pins = raw & GAYLE_PIN_MASK;
        self.sync_pins();
    }

    fn effective_pins(&self) -> u8 {
        if self.slot_enabled() {
            self.raw_pins
        } else {
            0
        }
    }

    fn sync_pins(&mut self) {
        let now = self.effective_pins();
        let changed = now ^ self.pins;
        self.pins = now;
        self.intreq |= changed;
        if now & PIN_BSY_IRQ != 0 {
            self.intreq |= GAYLE_IRQ_BSY;
        }
        if changed & PIN_CCDET != 0 {
            // The preliminary datasheet's card-change actions, as WinUAE
            // reads them: RESET alone reboots the machine, BERR alone would
            // bus-error the access (TODO: not modelled; it needs a
            // deferred bus-error injection into the running instruction),
            // both together mean neither.
            match self.intreq & (GAYLE_IRQ_RESET | GAYLE_IRQ_BERR) {
                GAYLE_IRQ_RESET => self.machine_reset_request = true,
                GAYLE_IRQ_BERR => {
                    log::warn!("gayle: card-detect change with BERR armed is not modelled")
                }
                _ => {}
            }
        }
    }

    /// Drain the request a $DA9000 RESET+BERR write made to reset the
    /// card's configuration.
    pub fn take_card_reset_request(&mut self) -> bool {
        std::mem::take(&mut self.card_reset_request)
    }

    /// Drain the request a card-detect change made to reset the machine.
    pub fn take_machine_reset_request(&mut self) -> bool {
        std::mem::take(&mut self.machine_reset_request)
    }

    /// The bits of $DA9000 currently latched.
    pub fn change_latches(&self) -> u8 {
        self.intreq
    }

    // ----- interrupt lines ------------------------------------------------

    /// The latched sources the enable register admits.
    fn enabled_sources(&self) -> u8 {
        self.intreq & self.intena & (GAYLE_IRQ_IDE | GAYLE_PIN_MASK)
    }

    /// The INT2 line into Paula (PORTS): IDE and write-enable changes, plus
    /// battery and busy/IRQ changes unless their level bits send them to
    /// INT6. Paula's INTREQ latch is level-fed, so the bus re-asserts
    /// INTREQ.PORTS while this stays true.
    pub fn int2_line(&self) -> bool {
        let sources = self.enabled_sources();
        let mut mask = GAYLE_IRQ_IDE | GAYLE_IRQ_WR;
        if self.intena & GAYLE_INT_BVD_LEV == 0 {
            mask |= GAYLE_IRQ_BVD1 | GAYLE_IRQ_BVD2;
        }
        if self.intena & GAYLE_INT_BSY_LEV == 0 {
            mask |= GAYLE_IRQ_BSY;
        }
        sources & mask != 0
    }

    /// The INT6 line into Paula (EXTER): card detect always, battery and
    /// busy/IRQ when their level bits say so.
    pub fn int6_line(&self) -> bool {
        let sources = self.enabled_sources();
        let mut mask = GAYLE_IRQ_CCDET;
        if self.intena & GAYLE_INT_BVD_LEV != 0 {
            mask |= GAYLE_IRQ_BVD1 | GAYLE_IRQ_BVD2;
        }
        if self.intena & GAYLE_INT_BSY_LEV != 0 {
            mask |= GAYLE_IRQ_BSY;
        }
        sources & mask != 0
    }

    /// Latch an IDE interrupt the cable raised during this access. Unlike the
    /// A4000's interface, Gayle records the edge in a register of its own,
    /// which the driver clears by writing it back.
    fn latch_ide_irq(&mut self) {
        if self.ata.take_irq_edge() {
            self.intreq |= GAYLE_IRQ_IDE;
        }
    }

    // ----- $DE1000 ID shift register -------------------------------------

    fn id_read(&mut self) -> u8 {
        let bit = (self.id >> (7 - self.id_bit)) & 1;
        self.id_bit = (self.id_bit + 1) & 7;
        if bit != 0 {
            0x80
        } else {
            0x00
        }
    }

    fn id_reset(&mut self) {
        self.id_bit = 0;
    }

    // ----- memory-mapped access ------------------------------------------

    /// Byte/word read anywhere in $DA0000-$DBFFFF or $DE0000-$DEFFFF.
    /// `addr` is the full masked CPU address.
    pub fn read(&mut self, addr: u32, size: usize) -> u32 {
        if size == 4 {
            let hi = self.read(addr, 2);
            let lo = self.read(addr.wrapping_add(2), 2);
            return (hi << 16) | lo;
        }
        let value = self.read_inner(addr, size);
        self.latch_ide_irq();
        if crate::envcfg::flag("COPPERLINE_DIAG_GAYLE") {
            log::info!("gayle rd {addr:#08X}/{size} -> {value:#06X}");
        }
        value
    }

    fn read_inner(&mut self, addr: u32, size: usize) -> u32 {
        match (addr, size) {
            (0x00DE_1000..=0x00DE_1003, _) => {
                let v = u32::from(self.id_read());
                // A word read shifts one bit only; it appears on D15-D8.
                if size == 2 {
                    v << 8
                } else {
                    v
                }
            }
            _ if (0x00DA_8000..0x00DB_0000).contains(&addr) => {
                let v = u32::from(self.register_read(addr));
                if size == 2 {
                    v << 8
                } else {
                    v
                }
            }
            _ if (0x00DA_0000..0x00DA_8000).contains(&addr) => {
                self.ata.read_reg(Self::ide_reg(addr), size)
            }
            _ => 0,
        }
    }

    pub fn write(&mut self, addr: u32, size: usize, value: u32) {
        if size == 4 {
            self.write(addr, 2, value >> 16);
            self.write(addr.wrapping_add(2), 2, value & 0xFFFF);
            return;
        }
        if crate::envcfg::flag("COPPERLINE_DIAG_GAYLE") {
            log::info!("gayle wr {addr:#08X}/{size} <- {value:#06X}");
        }
        match addr {
            0x00DE_1000..=0x00DE_1003 => self.id_reset(),
            _ if (0x00DA_8000..0x00DB_0000).contains(&addr) => {
                let byte = if size == 2 {
                    (value >> 8) as u8
                } else {
                    value as u8
                };
                self.register_write(addr, byte);
            }
            _ if (0x00DA_0000..0x00DA_8000).contains(&addr) => {
                self.ata.write_reg(Self::ide_reg(addr), size, value);
            }
            _ => {}
        }
        self.latch_ide_irq();
    }

    fn register_read(&mut self, addr: u32) -> u8 {
        match addr & 0xFFFF_F000 {
            0x00DA_8000 => {
                // Status: the slot pins (all clear for an empty socket, so
                // card.resource sees no card), the control bits as written,
                // and live IDE INTRQ on bit 7.
                let mut v = self.pins | self.status_control;
                if self.ata.irq_level() {
                    v |= GAYLE_IRQ_IDE;
                }
                v
            }
            0x00DA_9000 => self.intreq,
            0x00DA_A000 => self.intena,
            0x00DA_B000 => self.config & 0x0F,
            _ => 0,
        }
    }

    fn register_write(&mut self, addr: u32, value: u8) {
        match addr & 0xFFFF_F000 {
            0x00DA_8000 => {
                let was_disabled = self.status_control & GAYLE_CS_DIS;
                self.status_control = value;
                if was_disabled != value & GAYLE_CS_DIS {
                    // Disabling the slot pulls the pins to the empty-socket
                    // state and enabling it samples the card again, so both
                    // edges latch a card-detect change.
                    self.sync_pins();
                }
            }
            0x00DA_9000 => {
                // Interrupt change: write-to-clear. Bits written as 1 are
                // kept, bits written as 0 are cleared; the two control bits
                // are set by writing them, and both together reset the
                // card's configuration.
                self.intreq = (self.intreq & value) | (value & (GAYLE_IRQ_RESET | GAYLE_IRQ_BERR));
                if self.intreq & (GAYLE_IRQ_RESET | GAYLE_IRQ_BERR)
                    == GAYLE_IRQ_RESET | GAYLE_IRQ_BERR
                {
                    self.card_reset_request = true;
                }
            }
            0x00DA_A000 => self.intena = value,
            0x00DA_B000 => self.config = value,
            _ => {}
        }
    }

    /// A600/A1200 IDE decode, as the ROM scsi.device drives it (verified
    /// against ROM 40.063 boot probes): task file at $DA2000 with a 4-byte
    /// stride, byte registers on the even (D15-D8) byte, the 16-bit data
    /// port at $DA2000, and the control block one A12 page up ($DA3018).
    fn ide_reg(addr: u32) -> Option<IdeReg> {
        match addr & 0x7FFF {
            off @ 0x2000..=0x201F => task_file_reg(off - 0x2000),
            0x3018 | 0x301A => Some(IdeReg::AltStatusDevCtl),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ata::{DH_LBA, ERR_ABRT, ST_DRDY, ST_DRQ, ST_DSC, ST_ERR};
    use crate::harddrive::{CYL_SECTORS, RDB_HEADS, RDB_SPT};
    use crate::pcmcia::{PIN_BSY_IRQ, PIN_BVD1, PIN_BVD2, PIN_CCDET, PIN_WR};
    use std::path::PathBuf;

    fn temp_image(sectors: u64) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "copperline-gayle-test-{}-{}.hdf",
            std::process::id(),
            rand_suffix()
        ));
        let data = vec![0u8; (sectors * SECTOR_SIZE as u64) as usize];
        std::fs::write(&path, data).unwrap();
        path
    }

    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};
        // Parallel tests can hit the same nanosecond timestamp; a
        // process-wide counter keeps the image paths distinct.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64;
        (nanos << 16) | NEXT.fetch_add(1, Ordering::Relaxed)
    }

    fn gayle_with_drive(sectors: u64) -> (Gayle, PathBuf) {
        let path = temp_image(sectors);
        let mut gayle = Gayle::new(0xD0);
        gayle.attach_drive(
            0,
            IdeDrive::open(&path, 0, None, 0, crate::diskimage::FileSystem::FFS).unwrap(),
        );
        (gayle, path)
    }

    const IDE_DATA: u32 = 0x00DA_2000;
    const IDE_ERROR: u32 = 0x00DA_2004;
    const IDE_NSECTOR: u32 = 0x00DA_2008;
    const IDE_SECTOR: u32 = 0x00DA_200C;
    const IDE_LCYL: u32 = 0x00DA_2010;
    const IDE_HCYL: u32 = 0x00DA_2014;
    const IDE_SELECT: u32 = 0x00DA_2018;
    const IDE_STATUS: u32 = 0x00DA_201C;
    const GAYLE_INTREQ: u32 = 0x00DA_9000;
    const GAYLE_INTENA: u32 = 0x00DA_A000;
    const GAYLE_STATUS_REG: u32 = 0x00DA_8000;
    const GAYLE_ID_REG: u32 = 0x00DE_1000;

    fn set_lba(g: &mut Gayle, lba: u32, count: u8) {
        g.write(
            IDE_SELECT,
            1,
            u32::from(DH_LBA | ((lba >> 24) as u8 & 0x0F)),
        );
        g.write(IDE_HCYL, 1, (lba >> 16) & 0xFF);
        g.write(IDE_LCYL, 1, (lba >> 8) & 0xFF);
        g.write(IDE_SECTOR, 1, lba & 0xFF);
        g.write(IDE_NSECTOR, 1, u32::from(count));
    }

    fn be32(block: &[u8], offset: usize) -> u32 {
        u32::from_be_bytes(block[offset..offset + 4].try_into().unwrap())
    }

    fn rdb_block_sums_to_zero(block: &[u8]) -> bool {
        (0..64)
            .map(|i| be32(block, i * 4))
            .fold(0u32, |a, v| a.wrapping_add(v))
            == 0
    }

    #[test]
    fn bare_partition_hardfile_gets_synthesized_rdb() {
        // One cylinder (256 KiB) of FFS partition: boot block 'DOS\x03'.
        let path = temp_image(CYL_SECTORS as u64);
        let mut data = std::fs::read(&path).unwrap();
        data[..4].copy_from_slice(b"DOS\x03");
        data[SECTOR_SIZE] = 0xA5; // marker in partition sector 1
        std::fs::write(&path, &data).unwrap();

        let mut drive =
            IdeDrive::open(&path, 0, None, 0, crate::diskimage::FileSystem::FFS).unwrap();
        // One synthesized RDB cylinder plus the partition cylinder.
        assert_eq!(drive.disk.total_sectors(), 2 * u64::from(CYL_SECTORS));

        let mut sector = [0u8; SECTOR_SIZE];
        drive.disk.read_sector(0, &mut sector).unwrap();
        assert_eq!(&sector[..4], b"RDSK");
        assert!(rdb_block_sums_to_zero(&sector));
        assert_eq!(be32(&sector, 64), 2); // cylinders
        assert_eq!(be32(&sector, 68), RDB_SPT);
        assert_eq!(be32(&sector, 72), RDB_HEADS);

        drive.disk.read_sector(1, &mut sector).unwrap();
        assert_eq!(&sector[..4], b"PART");
        assert!(rdb_block_sums_to_zero(&sector));
        assert_eq!(&sector[36..40], b"\x03DH0"); // BSTR drive name
        assert_eq!(be32(&sector, 128 + 9 * 4), 1); // low cylinder
        assert_eq!(be32(&sector, 128 + 10 * 4), 1); // high cylinder
        assert_eq!(be32(&sector, 128 + 16 * 4), 0x444F_5303); // dostype DOS\x03

        // Partition LBAs shift down one cylinder onto the file.
        drive
            .disk
            .read_sector(u64::from(CYL_SECTORS), &mut sector)
            .unwrap();
        assert_eq!(&sector[..4], b"DOS\x03");
        drive
            .disk
            .read_sector(u64::from(CYL_SECTORS) + 1, &mut sector)
            .unwrap();
        assert_eq!(sector[0], 0xA5);

        // Writes to the partition persist in the file at the shifted offset;
        // writes to the synthesized RDB stay in memory.
        let mut payload = [0u8; SECTOR_SIZE];
        payload[..4].copy_from_slice(b"WRIT");
        drive
            .disk
            .write_sector(u64::from(CYL_SECTORS) + 2, &payload)
            .unwrap();
        drive.disk.write_sector(0, &payload).unwrap();
        drive.disk.read_sector(0, &mut sector).unwrap();
        assert_eq!(&sector[..4], b"WRIT");
        let on_disk = std::fs::read(&path).unwrap();
        assert_eq!(&on_disk[2 * SECTOR_SIZE..2 * SECTOR_SIZE + 4], b"WRIT");
        assert_eq!(&on_disk[..4], b"DOS\x03"); // RDB write did not hit the file

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn image_with_own_rdsk_is_not_wrapped() {
        let path = temp_image(CYL_SECTORS as u64);
        let mut data = std::fs::read(&path).unwrap();
        data[..4].copy_from_slice(b"RDSK");
        std::fs::write(&path, &data).unwrap();
        let mut drive =
            IdeDrive::open(&path, 0, None, 0, crate::diskimage::FileSystem::FFS).unwrap();
        assert_eq!(drive.disk.total_sectors(), u64::from(CYL_SECTORS));
        let mut sector = [0u8; SECTOR_SIZE];
        drive.disk.read_sector(0, &mut sector).unwrap();
        assert_eq!(&sector[..4], b"RDSK");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn bare_partition_with_uneven_size_is_rejected() {
        // Half a cylinder: detected as a bare partition but not wrappable.
        let path = temp_image(u64::from(CYL_SECTORS) / 2);
        let mut data = std::fs::read(&path).unwrap();
        data[..4].copy_from_slice(b"DOS\x00");
        std::fs::write(&path, &data).unwrap();
        let err = match IdeDrive::open(&path, 0, None, 0, crate::diskimage::FileSystem::FFS) {
            Ok(_) => panic!("expected open to fail"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("bare partition"), "unexpected error: {err}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn gayle_id_shifts_out_msb_first_on_d7() {
        let mut gayle = Gayle::new(0xD0);
        gayle.write(GAYLE_ID_REG, 1, 0xFF); // any write resets the shifter
        let bits: Vec<u32> = (0..8).map(|_| gayle.read(GAYLE_ID_REG, 1)).collect();
        // 0xD0 = 1101 0000.
        assert_eq!(bits, [0x80, 0x80, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00]);
        // A fresh write restarts the sequence.
        gayle.write(GAYLE_ID_REG, 1, 0x00);
        assert_eq!(gayle.read(GAYLE_ID_REG, 1), 0x80);

        let mut a1200 = Gayle::new(0xD1);
        a1200.write(GAYLE_ID_REG, 1, 0);
        let bits: Vec<u32> = (0..8).map(|_| a1200.read(GAYLE_ID_REG, 1)).collect();
        assert_eq!(bits, [0x80, 0x80, 0x00, 0x80, 0x00, 0x00, 0x00, 0x80]);
    }

    #[test]
    fn identify_reports_geometry_lba_and_multiple() {
        let (mut g, path) = gayle_with_drive(16 * 32 * 4); // 4 cylinders
        g.write(IDE_SELECT, 1, 0xA0);
        g.write(IDE_STATUS, 1, 0xEC);
        let status = g.read(0x00DA_3018, 1); // alt status: no irq clear
        assert_eq!(status as u8, ST_DRDY | ST_DSC | ST_DRQ);
        g.write(GAYLE_INTENA, 1, u32::from(GAYLE_IRQ_IDE));
        assert!(g.int2_line());

        // The CPU sees every ATA word byte-swapped (Gayle's IDE data bus
        // wiring); undo the swap to check the ATA-defined values.
        let mut words = [0u16; 256];
        for w in words.iter_mut() {
            *w = (g.read(IDE_DATA, 2) as u16).swap_bytes();
        }
        assert_eq!(words[0], 0x045A, "Conner-style configuration word");
        assert_eq!(words[1], 4, "cylinders");
        assert_eq!(words[3], 16, "heads");
        assert_eq!(words[6], 32, "sectors per track");
        assert_eq!(words[47] & 0xFF, u16::from(MAX_MULTIPLE));
        assert_ne!(words[49] & 0x0200, 0, "LBA capability");
        let lba = u32::from(words[60]) | (u32::from(words[61]) << 16);
        assert_eq!(lba, 16 * 32 * 4);
        // ATA string convention: first char of each pair in bits 15-8.
        assert_eq!(words[27], u16::from_be_bytes(*b"CO"));
        // Transfer complete: DRQ clears.
        assert_eq!(g.read(IDE_STATUS, 1) as u8, ST_DRDY | ST_DSC);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn write_then_read_sectors_round_trips_through_the_image() {
        let (mut g, path) = gayle_with_drive(64);
        g.write(GAYLE_INTENA, 1, u32::from(GAYLE_IRQ_IDE));

        // WRITE SECTORS, 2 sectors at LBA 5.
        set_lba(&mut g, 5, 2);
        g.write(IDE_STATUS, 1, 0x30);
        assert_eq!(
            g.read(0x00DA_3018, 1) as u8,
            ST_DRDY | ST_DSC | ST_DRQ,
            "first DRQ block ready without IRQ"
        );
        assert!(!g.int2_line(), "no IRQ before first block is consumed");
        for i in 0..512u32 {
            g.write(IDE_DATA, 2, (i * 7) & 0xFFFF);
        }
        assert_eq!(g.read(IDE_STATUS, 1) as u8, ST_DRDY | ST_DSC);

        // READ SECTORS back.
        set_lba(&mut g, 5, 2);
        g.write(IDE_STATUS, 1, 0x20);
        assert!(g.int2_line(), "read data ready raises INT2");
        let mut got = Vec::with_capacity(512);
        for _ in 0..512 {
            got.push(g.read(IDE_DATA, 2) as u16);
        }
        for (i, w) in got.iter().enumerate() {
            assert_eq!(u32::from(*w), (i as u32 * 7) & 0xFFFF, "word {i}");
        }
        assert_eq!(g.read(IDE_STATUS, 1) as u8, ST_DRDY | ST_DSC);

        // The bytes really hit the backing file (big-endian word order).
        let data = std::fs::read(&path).unwrap();
        let off = 5 * SECTOR_SIZE;
        assert_eq!(data[off], 0);
        assert_eq!(data[off + 1], 0);
        assert_eq!(data[off + 2], 0);
        assert_eq!(data[off + 3], 7);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn set_multiple_and_read_multiple_transfer_in_blocks() {
        let (mut g, path) = gayle_with_drive(64);
        g.write(GAYLE_INTENA, 1, u32::from(GAYLE_IRQ_IDE));

        // SET MULTIPLE = 4.
        g.write(IDE_NSECTOR, 1, 4);
        g.write(IDE_STATUS, 1, 0xC6);
        assert_eq!(g.read(IDE_STATUS, 1) as u8, ST_DRDY | ST_DSC);

        // READ MULTIPLE of 8 sectors: expect 2 DRQ blocks of 4.
        set_lba(&mut g, 0, 8);
        g.write(IDE_STATUS, 1, 0xC4);
        let mut blocks = 0;
        while g.read(0x00DA_3018, 1) as u8 & ST_DRQ != 0 {
            blocks += 1;
            assert!(blocks <= 2, "expected exactly two DRQ blocks");
            for _ in 0..(4 * 256) {
                g.read(IDE_DATA, 2);
            }
        }
        assert_eq!(blocks, 2);

        // SET MULTIPLE beyond the advertised maximum aborts.
        g.write(IDE_NSECTOR, 1, 64);
        g.write(IDE_STATUS, 1, 0xC6);
        assert_ne!(g.read(IDE_STATUS, 1) as u8 & ST_ERR, 0);
        assert_ne!(g.read(IDE_ERROR, 1) as u8 & ERR_ABRT, 0);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn chs_addressing_follows_initialize_device_parameters() {
        let (mut g, path) = gayle_with_drive(64);
        // INITIALIZE DEVICE PARAMETERS: 2 heads, 8 sectors per track.
        g.write(IDE_SELECT, 1, 0xA0 | 1); // heads - 1
        g.write(IDE_NSECTOR, 1, 8);
        g.write(IDE_STATUS, 1, 0x91);
        assert_eq!(g.read(IDE_STATUS, 1) as u8, ST_DRDY | ST_DSC);

        // Write one sector at C/H/S = 1/1/3 -> LBA (1*2+1)*8 + 2 = 26.
        g.write(IDE_SELECT, 1, 0xA0 | 1);
        g.write(IDE_HCYL, 1, 0);
        g.write(IDE_LCYL, 1, 1);
        g.write(IDE_SECTOR, 1, 3);
        g.write(IDE_NSECTOR, 1, 1);
        g.write(IDE_STATUS, 1, 0x30);
        for i in 0..256u32 {
            g.write(IDE_DATA, 2, 0xBEE0 + (i & 0xF));
        }
        assert_eq!(g.read(IDE_STATUS, 1) as u8, ST_DRDY | ST_DSC);
        let data = std::fs::read(&path).unwrap();
        let off = 26 * SECTOR_SIZE;
        assert_eq!(data[off], 0xBE);
        assert_eq!(data[off + 1], 0xE0);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn gayle_interrupt_latch_is_write_to_clear_and_gates_int2() {
        let (mut g, path) = gayle_with_drive(64);
        // The latch records the IRQ regardless; the $DAA000 enable gates
        // its delivery to INT2.
        set_lba(&mut g, 0, 1);
        g.write(IDE_STATUS, 1, 0x20);
        assert_eq!(g.read(GAYLE_INTREQ, 1) as u8 & GAYLE_IRQ_IDE, GAYLE_IRQ_IDE);
        assert!(!g.int2_line(), "INTENA clear blocks INT2");
        g.write(GAYLE_INTENA, 1, u32::from(GAYLE_IRQ_IDE));
        assert!(g.int2_line());
        // Live INTRQ shows in the status register.
        assert_eq!(
            g.read(GAYLE_STATUS_REG, 1) as u8 & GAYLE_IRQ_IDE,
            GAYLE_IRQ_IDE
        );
        // Write-to-clear: writing 0 to bit 7 clears the latch.
        g.write(GAYLE_INTREQ, 1, u32::from(!GAYLE_IRQ_IDE));
        assert_eq!(g.read(GAYLE_INTREQ, 1) as u8 & GAYLE_IRQ_IDE, 0);
        assert!(!g.int2_line());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn missing_device_status_follows_winuae_pair_semantics() {
        // Empty cable: every status read floats to 0xFF.
        let mut g = Gayle::new(0xD0);
        g.write(IDE_SELECT, 1, 0xB0);
        assert_eq!(g.read(IDE_STATUS, 1) as u8, 0xFF, "empty cable floats");
        assert_eq!(g.read(IDE_ERROR, 1), 0, "non-status registers read 0");

        // Master present, slave selected: status reads 0x01, commands abort.
        let (mut g, path) = gayle_with_drive(64);
        g.write(GAYLE_INTENA, 1, u32::from(GAYLE_IRQ_IDE));
        g.write(IDE_SELECT, 1, 0xB0);
        assert_eq!(g.read(IDE_STATUS, 1) as u8, 0x01, "pair present");
        assert_eq!(g.read(IDE_ERROR, 1), 0, "non-status registers read 0");
        g.write(IDE_STATUS, 1, 0xEC);
        assert!(g.int2_line(), "aborted command still raises the IRQ");
        assert_eq!(
            g.read(IDE_STATUS, 1) as u8,
            0x01,
            "no phantom IDENTIFY: status stays at the pair-present pattern"
        );
        std::fs::remove_file(path).ok();
    }

    const GAYLE_CONFIG_REG: u32 = 0x00DA_B000;
    const CARD_PINS: u8 = PIN_CCDET | PIN_BVD1 | PIN_BVD2 | PIN_WR;

    /// An empty socket reads with every pin bit clear (card.resource sees
    /// no card); inserting a card raises the pins it drives and latches
    /// each as a change, and card detect goes out on INT6 once enabled.
    #[test]
    fn card_insert_sets_status_pins_latches_changes_and_raises_int6() {
        let mut g = Gayle::new(0xD1);
        assert_eq!(g.read(GAYLE_STATUS_REG, 1) as u8 & GAYLE_PIN_MASK, 0);
        assert_eq!(g.read(GAYLE_INTREQ, 1) as u8 & GAYLE_PIN_MASK, 0);

        g.set_card_pins(CARD_PINS);
        assert_eq!(
            g.read(GAYLE_STATUS_REG, 1) as u8 & GAYLE_PIN_MASK,
            CARD_PINS
        );
        assert_eq!(g.read(GAYLE_INTREQ, 1) as u8 & GAYLE_PIN_MASK, CARD_PINS);
        // Nothing enabled: nothing on either line.
        assert!(!g.int2_line());
        assert!(!g.int6_line());

        // card.resource's enable word: IDE plus every card source.
        g.write(GAYLE_INTENA, 1, 0xEC);
        assert!(g.int6_line(), "card detect is an INT6 source");
        assert!(g.int2_line(), "WR change is an INT2 source");

        // Write-to-clear: dropping the detect and write-enable latches
        // while keeping the rest.
        g.write(
            GAYLE_INTREQ,
            1,
            u32::from(!(GAYLE_IRQ_CCDET | GAYLE_IRQ_WR)),
        );
        assert_eq!(
            g.read(GAYLE_INTREQ, 1) as u8 & GAYLE_PIN_MASK,
            PIN_BVD1 | PIN_BVD2
        );
        assert!(!g.int6_line());
        // BVD changes default to INT2 (level bit clear).
        assert!(g.int2_line());
        g.write(GAYLE_INTREQ, 1, 0);
        assert!(!g.int2_line());

        // Removing the card latches every pin again.
        g.set_card_pins(0);
        assert_eq!(g.read(GAYLE_STATUS_REG, 1) as u8 & GAYLE_PIN_MASK, 0);
        assert_eq!(g.read(GAYLE_INTREQ, 1) as u8 & GAYLE_PIN_MASK, CARD_PINS);
        assert!(g.int6_line());
    }

    /// The $DAA000 level bits steer the battery and busy/IRQ sources
    /// between INT2 and INT6; card detect stays on INT6 regardless.
    #[test]
    fn enable_register_level_bits_route_bvd_and_bsy_between_int2_and_int6() {
        let mut g = Gayle::new(0xD1);
        g.set_card_pins(CARD_PINS);
        g.write(GAYLE_INTREQ, 1, 0); // drop the insertion latches
        g.write(GAYLE_INTENA, 1, u32::from(GAYLE_IRQ_BVD1 | GAYLE_IRQ_BSY));

        // Battery pin drops: INT2 by default, INT6 with BVD_LEV.
        g.set_card_pins(CARD_PINS & !PIN_BVD1);
        assert!(g.int2_line());
        assert!(!g.int6_line());
        g.write(
            GAYLE_INTENA,
            1,
            u32::from(GAYLE_IRQ_BVD1 | GAYLE_IRQ_BSY | GAYLE_INT_BVD_LEV),
        );
        assert!(!g.int2_line());
        assert!(g.int6_line());
        g.write(GAYLE_INTREQ, 1, u32::from(!GAYLE_IRQ_BVD1));

        // A card interrupt (IREQ#) is a level: it stays latched while the
        // pin is high even after a clear, and follows BSY_LEV.
        g.set_card_pins(CARD_PINS & !PIN_BVD1 | PIN_BSY_IRQ);
        assert!(g.int2_line(), "BSY_LEV clear: INT2");
        g.write(GAYLE_INTREQ, 1, u32::from(!GAYLE_IRQ_BSY));
        assert_eq!(g.read(GAYLE_INTREQ, 1) as u8 & GAYLE_IRQ_BSY, 0);
        g.set_card_pins(CARD_PINS & !PIN_BVD1 | PIN_BSY_IRQ);
        assert_ne!(
            g.read(GAYLE_INTREQ, 1) as u8 & GAYLE_IRQ_BSY,
            0,
            "a high IRQ pin re-latches on the next sample"
        );
        g.write(
            GAYLE_INTENA,
            1,
            u32::from(GAYLE_IRQ_BVD1 | GAYLE_IRQ_BSY | GAYLE_INT_BSY_LEV),
        );
        assert!(!g.int2_line());
        assert!(g.int6_line(), "BSY_LEV set: INT6");
        // Card detect always rides INT6, whatever the level bits say.
        g.write(GAYLE_INTENA, 1, u32::from(GAYLE_IRQ_CCDET));
        g.set_card_pins(0);
        assert!(g.int6_line());
        assert!(!g.int2_line());
    }

    /// $DA8000 writes: DIS pulls the socket to empty (latching the detect
    /// change) and re-enabling samples the card again; the other written
    /// bits read back OR-ed into the status (CardMiscControl's
    /// write-protect override); $DAB000 keeps only its four config bits.
    #[test]
    fn status_control_bits_disable_the_slot_and_read_back() {
        let mut g = Gayle::new(0xD1);
        g.set_card_pins(CARD_PINS & !PIN_WR); // write-protected card
        g.write(GAYLE_INTREQ, 1, 0);
        assert!(g.slot_enabled());

        g.write(GAYLE_STATUS_REG, 1, u32::from(GAYLE_CS_DIS));
        assert!(!g.slot_enabled());
        assert_eq!(g.read(GAYLE_STATUS_REG, 1) as u8 & GAYLE_PIN_MASK, 0);
        assert_ne!(g.read(GAYLE_INTREQ, 1) as u8 & GAYLE_IRQ_CCDET, 0);
        assert_ne!(g.read(GAYLE_STATUS_REG, 1) as u8 & GAYLE_CS_DIS, 0);
        g.write(GAYLE_INTREQ, 1, 0);

        // Write-protect override: bit 3 written reads back set.
        g.write(GAYLE_STATUS_REG, 1, u32::from(PIN_WR | GAYLE_CS_DAEN));
        assert!(g.slot_enabled());
        assert_eq!(
            g.read(GAYLE_STATUS_REG, 1) as u8 & (GAYLE_PIN_MASK | GAYLE_CS_DAEN),
            CARD_PINS | GAYLE_CS_DAEN
        );
        assert_ne!(g.read(GAYLE_INTREQ, 1) as u8 & GAYLE_IRQ_CCDET, 0);

        g.write(GAYLE_CONFIG_REG, 1, 0xF9);
        assert_eq!(g.read(GAYLE_CONFIG_REG, 1), 0x09, "voltage/speed bits only");
        // A word write lands on the even byte.
        g.write(GAYLE_CONFIG_REG, 2, 0x0500);
        assert_eq!(g.read(GAYLE_CONFIG_REG, 2), 0x0500);
    }

    /// $DA9000 bits 1:0 are set by writing them: both together ask for a
    /// card configuration reset, RESET alone reboots the machine on the
    /// next card-detect change.
    #[test]
    fn change_register_control_bits_request_card_and_machine_resets() {
        let mut g = Gayle::new(0xD1);
        g.write(GAYLE_INTREQ, 1, u32::from(GAYLE_IRQ_RESET | GAYLE_IRQ_BERR));
        assert_eq!(
            g.read(GAYLE_INTREQ, 1) as u8 & 3,
            GAYLE_IRQ_RESET | GAYLE_IRQ_BERR
        );
        assert!(g.take_card_reset_request());
        assert!(!g.take_card_reset_request(), "drained");
        assert!(!g.take_machine_reset_request());

        g.write(GAYLE_INTREQ, 1, u32::from(GAYLE_IRQ_RESET));
        assert!(!g.take_card_reset_request());
        g.set_card_pins(CARD_PINS);
        assert!(g.take_machine_reset_request());
        // A system reset clears every latch and control bit but keeps the
        // card visible, with no change pending.
        g.reset();
        assert_eq!(g.read(GAYLE_INTREQ, 1), 0);
        assert_eq!(
            g.read(GAYLE_STATUS_REG, 1) as u8 & GAYLE_PIN_MASK,
            CARD_PINS
        );
    }

    /// More than 4 MiB of Zorro II fast RAM covers the slot's window: the
    /// socket reads empty whatever is in it, and nothing is latched.
    #[test]
    fn fast_ram_shadowing_makes_the_slot_read_empty() {
        let mut g = Gayle::new(0xD1);
        g.set_slot_shadowed(true);
        g.set_card_pins(CARD_PINS);
        assert!(!g.slot_enabled());
        assert_eq!(g.read(GAYLE_STATUS_REG, 1) as u8 & GAYLE_PIN_MASK, 0);
        assert_eq!(g.read(GAYLE_INTREQ, 1), 0);
        assert!(g.card_pins() == CARD_PINS, "the card is still there");
    }

    /// A `.iso` path attaches as an ATAPI drive rather than being rejected:
    /// IDENTIFY PACKET DEVICE (0xA1) answers, and plain IDENTIFY DEVICE
    /// (0xEC) aborts. The full PACKET protocol is exercised in `ata.rs`.
    #[test]
    fn a_cd_image_path_attaches_as_atapi() {
        let path = std::env::temp_dir().join(format!(
            "copperline-gayle-test-{}-{}.iso",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::write(&path, vec![0u8; 2048]).unwrap();
        let mut g = Gayle::new(0xD0);
        g.attach_drive(0, crate::ata::AtapiDrive::open(&path).unwrap());
        g.write(IDE_SELECT, 1, 0xA0);
        g.write(IDE_STATUS, 1, 0xA1);
        assert_eq!(g.read(IDE_STATUS, 1) as u8, ST_DRDY | ST_DSC | ST_DRQ);
        g.write(IDE_SELECT, 1, 0xA0);
        g.write(IDE_STATUS, 1, 0xEC);
        assert_eq!(
            g.read(IDE_STATUS, 1) as u8,
            ST_DRDY | ST_DSC | ST_ERR,
            "IDENTIFY DEVICE must abort against an ATAPI slot"
        );
        std::fs::remove_file(path).ok();
    }
}
