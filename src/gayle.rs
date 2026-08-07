// SPDX-License-Identifier: GPL-3.0-or-later

//! Gayle gate array (A600/A1200): the ID register at $DE1000, the IDE
//! interface at $DA0000, the Gayle status/interrupt/config registers at
//! $DA8000-$DAA000, and empty-slot PCMCIA status.
//!
//! Decode and register layout follow the Commodore schematics as captured by
//! the Linux `gayle.c` IDE driver and the ROM scsi.device: the IDE task
//! file lives at $DA2000 with a 4-byte stride (byte registers on the odd
//! word half, offset base+4*reg+2), and the control block register at
//! base+$101A. None of this is on the chip bus; the CPU reaches it through
//! `cpu_external_access`.
//!
//! The drives, the task file, and the command engine are the shared ATA core
//! in [`crate::ata`]; Gayle is the front-end that decodes for it and adds its
//! own ID, interrupt, and PCMCIA registers.

use crate::ata::{task_file_reg, AtaBus, IdeReg};

pub use crate::ata::{IdeDrive, MAX_MULTIPLE, SECTOR_SIZE};

// Gayle interrupt/status bit layout (shared by the status, interrupt
// change, and interrupt enable registers).
pub const GAYLE_IRQ_IDE: u8 = 0x80;
// PCMCIA bits (CCDET/BVD1/BVD2/WR/BSY) stay clear: no card inserted.

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Gayle {
    /// $DE1000 ID shifted out MSB-first on D7: $D0 (A600) / $D1 (A1200).
    id: u8,
    id_bit: u8,
    /// $DA9000 latched interrupt-change bits (write-to-clear with AND).
    intreq: u8,
    /// $DA9800 interrupt enable.
    intena: u8,
    /// $DAA000 config (PCMCIA voltage/resistor config; stored only).
    config: u8,
    /// The IDE cable behind the gate array.
    ata: AtaBus,
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
        }
    }

    /// Drain the activity latch set by command issue and data-port traffic.
    /// The bus polls this after each Gayle access to time the HDD LED.
    pub fn take_activity(&mut self) -> bool {
        self.ata.take_activity()
    }

    pub fn attach_drive(&mut self, slot: usize, drive: IdeDrive) {
        self.ata.attach_drive(slot, drive);
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
    /// keep the mounted drives.
    pub fn reset(&mut self) {
        self.id_bit = 0;
        self.intreq = 0;
        self.intena = 0;
        self.config = 0;
        self.ata.reset();
    }

    /// The INT2 line into Paula (PORTS): the latched interrupt-change bits
    /// gated by the $DAA000 enable register (the ROM writes $EC there:
    /// IDE plus the PCMCIA detect/change sources). Paula's INTREQ latch is
    /// level-fed, so the bus re-asserts INTREQ.PORTS while this stays true.
    pub fn int2_line(&self) -> bool {
        self.intreq & self.intena != 0
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
                // Status: live IDE INTRQ on bit 7. The PCMCIA pins are
                // active-low and pulled up, so an EMPTY slot reads with the
                // card-detect/battery/write/busy bits SET (0x7C); all-zero
                // would tell card.resource a card is inserted and wedge boot
                // waiting for it to become ready.
                let pcmcia_empty = 0x7C;
                if self.ata.irq_level() {
                    GAYLE_IRQ_IDE | pcmcia_empty
                } else {
                    pcmcia_empty
                }
            }
            0x00DA_9000 => self.intreq,
            0x00DA_A000 => self.intena,
            0x00DA_B000 => self.config,
            _ => 0,
        }
    }

    fn register_write(&mut self, addr: u32, value: u8) {
        match addr & 0xFFFF_F000 {
            0x00DA_8000 => {
                // Status register writes only touch the PCMCIA control bits;
                // nothing modeled behind them with an empty slot.
            }
            0x00DA_9000 => {
                // Interrupt change: write-to-clear. Bits written as 1 are
                // kept, bits written as 0 are cleared.
                self.intreq &= value;
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
        gayle.attach_drive(0, IdeDrive::open(&path, 0, None, 0).unwrap());
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

        let mut drive = IdeDrive::open(&path, 0, None, 0).unwrap();
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
        let mut drive = IdeDrive::open(&path, 0, None, 0).unwrap();
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
        let err = match IdeDrive::open(&path, 0, None, 0) {
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
}
