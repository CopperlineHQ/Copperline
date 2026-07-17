//! Super DMAC (SDMAC): the SCSI DMA controller on the A3000 motherboard.
//!
//! The SDMAC sits between the CPU bus and a WD33C93 SCSI controller: it owns
//! the DMA FIFO and the interrupt plumbing, and it maps the WD33C93's own
//! register file into two of its addresses (a register-select latch and a data
//! port). Kickstart's built-in scsi.device drives the pair, so unlike the Zorro
//! controllers there is no boot ROM to load: the machine either has a working
//! SDMAC + WD33C93 or it hangs during scsi.device init.
//!
//! This is the same layering `crate::a2091` uses, and the same one amiberry
//! uses: the A2091's DMAC and the A3000's SDMAC are two front-ends onto one
//! WD33C93 core ([`crate::scsi::Wd33c93`]). Only the front-end differs -- the
//! register map, the ISTR bits, and a 32-bit DMA address counter instead of the
//! Zorro II DMAC's 24-bit one.
//!
//! The DMA engine has no FIFO of its own: a transfer moves every byte the
//! WD33C93 offers or wants within the access that completes the handshake, so
//! the FIFO always reads back empty and never full.

use crate::memory::Memory;
use crate::scsi::{DmaDir, ScsiTarget, Wd33c93};

/// Base of the SDMAC register file.
pub const SDMAC_BASE: u32 = 0x00DD_0000;
/// The register file repeats every $100 bytes; the second copy is the "ALT"
/// shadow that cdhooper's sdmac tool writes through to defeat CPU write
/// buffering. Decoding the window as two mirrors is what makes that work.
pub const SDMAC_SIZE: u32 = 0x0200;

// Register offsets within the $100-byte file.
const DAWR: u32 = 0x03; // W    DACK width
const WTC: u32 = 0x04; // R/W  Word transfer count (SDMAC-02 only), 32-bit
const CONTR: u32 = 0x0B; // R/W  Control
const ACR: u32 = 0x0C; // R/W  DMA address (physically in Ramsey), 32-bit
const ST_DMA: u32 = 0x13; // W    Strobe: start DMA
const FLUSH: u32 = 0x17; // W    Strobe: flush DMA FIFO
const CLR_INT: u32 = 0x1B; // W    Strobe: clear interrupts
const ISTR: u32 = 0x1F; // R    Interrupt status
const SP_DMA: u32 = 0x3F; // W    Strobe: stop DMA
const SCMD: u32 = 0x43; // R/W  WD33C93 data port
const SCMD_ALT: u32 = 0x47; // R/W  ... and its alias
const SASR: u32 = 0x49; // R/W  WD33C93 register select (reads: aux status)
const SASR_ALT: u32 = 0x41; // R/W  ... and its alias
const SSPBDAT: u32 = 0x58; // R/W  Synchronous serial periph. bus data, 32-bit
const SSPBCTL: u32 = 0x5C; // R/W  Synchronous serial periph. bus control, 32-bit

// ISTR bits.
/// DMA FIFO is empty. Kickstart waits on this during scsi.device init.
pub const ISTR_FIFOE: u8 = 0x01;
/// DMA FIFO is full.
pub const ISTR_FIFOF: u8 = 0x02;
/// An enabled interrupt is pending.
pub const ISTR_INT_P: u8 = 0x10;
/// DMA done (end of process).
pub const ISTR_INT_E: u8 = 0x20;
/// The SCSI peripheral raised an interrupt.
pub const ISTR_INT_S: u8 = 0x40;
/// Interrupt follow.
pub const ISTR_INT_F: u8 = 0x80;

// CONTR bits.
/// DMA data direction: 0 = read, 1 = write.
pub const CONTR_DMADIR: u8 = 0x02;
/// Interrupt enable.
pub const CONTR_INTEN: u8 = 0x04;
/// Strobe: reset the WD33C93.
pub const CONTR_RESET: u8 = 0x10;

/// The SDMAC masters the full 32-bit address space, word-aligned -- unlike the
/// A2091's Zorro II DMAC, which is limited to 24 bits.
const DMA_ADDR_MASK: u32 = 0xFFFF_FFFE;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Sdmac {
    /// The SCSI controller behind the DMA front-end.
    pub wd: Wd33c93,
    /// Control register. Only the latched bits read back; RESET is a strobe.
    contr: u8,
    /// DACK width. Write-only on real silicon, so nothing ever reads it back.
    dawr: u8,
    /// DMA address register. It lives in Ramsey, not in the SDMAC, but it is
    /// addressed through the SDMAC window, so it is modelled here with the DMA
    /// engine that uses it. The low two bits are wired to zero: this is a
    /// longword address.
    acr: u32,
    /// Synchronous serial peripheral bus, used for the external clock/EEPROM
    /// on some boards. Nothing behind it here; the registers latch.
    sspbdat: u32,
    sspbctl: u32,
    /// Set by ST_DMA, cleared by SP_DMA and FLUSH.
    dma_active: bool,
    dma_warned: bool,
    activity: bool,
}

impl Default for Sdmac {
    fn default() -> Self {
        Self::new()
    }
}

impl Sdmac {
    pub fn new() -> Self {
        Self {
            wd: Wd33c93::new(),
            contr: 0,
            dawr: 0,
            acr: 0,
            sspbdat: 0,
            sspbctl: 0,
            dma_active: false,
            dma_warned: false,
            activity: false,
        }
    }

    /// Fit a drive at a SCSI ID (0-6; 7 is the controller itself).
    pub fn attach_drive(&mut self, unit: usize, target: impl Into<ScsiTarget>) {
        self.wd.attach_target(unit, target);
    }

    /// System reset: clear the DMAC and the SBIC, but keep the mounted drives.
    pub fn reset(&mut self) {
        self.wd.reset();
        self.contr = 0;
        self.dawr = 0;
        self.acr = 0;
        self.sspbdat = 0;
        self.sspbctl = 0;
        self.dma_active = false;
    }

    /// Drain the activity latch for the HDD LED.
    pub fn take_activity(&mut self) -> bool {
        std::mem::take(&mut self.activity) | self.wd.take_activity()
    }

    /// Advance emulated time: deliver delayed WD33C93 interrupts and pump any
    /// DMA handshake that became ready.
    pub fn tick(&mut self, cck: u32, mem: &mut Memory) {
        self.wd.tick(cck);
        self.pump_dma(mem);
    }

    /// Interrupt status. The DMA engine has no FIFO to fill, so it is empty
    /// whenever no transfer is running and never full.
    fn istr(&self) -> u8 {
        let mut v = 0;
        if !self.dma_active {
            v |= ISTR_FIFOE;
        }
        if self.wd.int_asserted() {
            v |= ISTR_INT_S | ISTR_INT_F;
        }
        if self.int_line() {
            v |= ISTR_INT_P;
        }
        v
    }

    /// The word transfer count exists on the SDMAC-02 and was dropped on the
    /// SDMAC-04, whose bit 2 always reads zero. Both Kickstart and cdhooper's
    /// sdmac tool identify the part by writing a pattern here and checking what
    /// fails to come back, so reading zero is what declares us an SDMAC-04.
    fn wtc(&self) -> u32 {
        0
    }

    /// Whether the SDMAC drives the bus for a byte access at `addr`.
    /// Undecoded addresses in the window are left floating, so they show up in
    /// `[debug] log_unmapped` rather than reading back a value we invented.
    pub fn decodes(addr: u32) -> bool {
        Self::offset(addr).is_some()
    }

    /// The register offset a CPU address lands in, or None if nothing decodes.
    /// The two 32-bit registers (WTC, ACR) and the two SSPB registers occupy
    /// four bytes each; everything else is a single byte.
    fn offset(addr: u32) -> Option<u32> {
        if !(SDMAC_BASE..SDMAC_BASE + SDMAC_SIZE).contains(&addr) {
            return None;
        }
        let off = addr & 0xFF;
        let in_long = |base: u32| (base..base + 4).contains(&off);
        let hit = matches!(
            off,
            DAWR | CONTR
                | ST_DMA
                | FLUSH
                | CLR_INT
                | ISTR
                | SP_DMA
                | SCMD
                | SCMD_ALT
                | SASR
                | SASR_ALT
        ) || in_long(WTC)
            || in_long(ACR)
            || in_long(SSPBDAT)
            || in_long(SSPBCTL);
        hit.then_some(off)
    }

    /// Byte `n` (0 = most significant) of a 32-bit register.
    fn long_byte(value: u32, off: u32, base: u32) -> u8 {
        (value >> (8 * (3 - (off - base)))) as u8
    }

    fn set_long_byte(value: &mut u32, off: u32, base: u32, byte: u8) {
        let shift = 8 * (3 - (off - base));
        *value = (*value & !(0xFFu32 << shift)) | (u32::from(byte) << shift);
    }

    pub fn read_byte(&mut self, addr: u32) -> u8 {
        let Some(off) = Self::offset(addr) else {
            return 0xFF;
        };
        match off {
            ISTR => self.istr(),
            CONTR => self.contr,
            // Reading the select address gives the chip's auxiliary status;
            // reading the data port gives the selected register.
            SASR | SASR_ALT => self.wd.read_aux_status(),
            SCMD | SCMD_ALT => self.wd.read_data_port(),
            _ if (WTC..WTC + 4).contains(&off) => Self::long_byte(self.wtc(), off, WTC),
            _ if (ACR..ACR + 4).contains(&off) => Self::long_byte(self.acr, off, ACR),
            _ if (SSPBDAT..SSPBDAT + 4).contains(&off) => {
                Self::long_byte(self.sspbdat, off, SSPBDAT)
            }
            _ if (SSPBCTL..SSPBCTL + 4).contains(&off) => {
                Self::long_byte(self.sspbctl, off, SSPBCTL)
            }
            // DAWR is write-only; the strobes read back nothing.
            _ => 0xFF,
        }
    }

    pub fn write_byte(&mut self, addr: u32, value: u8) {
        let Some(off) = Self::offset(addr) else {
            return;
        };
        match off {
            DAWR => self.dawr = value,
            CONTR => {
                // RESET is a strobe on the WD33C93's reset line: it does not
                // latch, and it drops the drives' state, not the drives.
                self.contr = value & !CONTR_RESET;
                if value & CONTR_RESET != 0 {
                    self.wd.reset();
                }
            }
            SASR | SASR_ALT => self.wd.write_sasr(value),
            SCMD | SCMD_ALT => self.wd.write_data_port(value),
            // DMA start/stop. The transfer itself is pumped from `tick`, which
            // runs before the driver can look at the result.
            ST_DMA => self.dma_active = true,
            SP_DMA | FLUSH => self.dma_active = false,
            // Nothing to clear: ISTR is computed from the chip's own interrupt
            // state, which the driver clears by reading the SCSI status
            // register through SCMD.
            CLR_INT => {}
            // The word transfer count does not exist on the SDMAC-04.
            _ if (WTC..WTC + 4).contains(&off) => {}
            _ if (ACR..ACR + 4).contains(&off) => {
                Self::set_long_byte(&mut self.acr, off, ACR, value);
                self.acr &= !0b11; // longword address: the low two bits are zero
            }
            _ if (SSPBDAT..SSPBDAT + 4).contains(&off) => {
                Self::set_long_byte(&mut self.sspbdat, off, SSPBDAT, value);
            }
            _ if (SSPBCTL..SSPBCTL + 4).contains(&off) => {
                Self::set_long_byte(&mut self.sspbctl, off, SSPBCTL, value);
            }
            _ => {}
        }
    }

    /// The INT2 line into Paula (PORTS), gated by the control register's
    /// interrupt enable.
    pub fn int_line(&self) -> bool {
        self.contr & CONTR_INTEN != 0 && self.wd.int_asserted()
    }

    /// Move every byte the WD33C93 currently offers or wants between its data
    /// path and Amiga memory, a word at a time, while DMA is started.
    fn pump_dma(&mut self, mem: &mut Memory) {
        if !self.dma_active {
            return;
        }
        while let Some(dir) = self.wd.dma_request() {
            if self.wd.dma_remaining() == 0 {
                break;
            }
            self.activity = true;
            let addr = self.acr & DMA_ADDR_MASK;
            match dir {
                DmaDir::In => {
                    let b0 = self.wd.dma_in_byte();
                    let b1 = if self.wd.dma_request().is_some() && self.wd.dma_remaining() > 0 {
                        self.wd.dma_in_byte()
                    } else {
                        // An odd byte count still writes a full word; the pad
                        // byte is whatever the FIFO carries (zero here).
                        0
                    };
                    self.dma_write_word(mem, addr, (u16::from(b0) << 8) | u16::from(b1));
                }
                DmaDir::Out => {
                    let w = self.dma_read_word(mem, addr);
                    self.wd.dma_out_byte((w >> 8) as u8);
                    if self.wd.dma_request().is_some() && self.wd.dma_remaining() > 0 {
                        self.wd.dma_out_byte(w as u8);
                    }
                }
            }
            self.acr = self.acr.wrapping_add(2);
        }
    }

    fn dma_read_word(&mut self, mem: &Memory, addr: u32) -> u16 {
        crate::zorro_device::dma_read_word(mem, addr).unwrap_or_else(|| {
            self.warn_dma_target(addr);
            0xFFFF
        })
    }

    fn dma_write_word(&mut self, mem: &mut Memory, addr: u32, w: u16) {
        if !crate::zorro_device::dma_write_word(mem, addr, w) {
            self.warn_dma_target(addr);
        }
    }

    fn warn_dma_target(&mut self, addr: u32) {
        if !self.dma_warned {
            self.dma_warned = true;
            log::warn!("sdmac: DMA to unmapped address {addr:#08X}; ignoring");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sdmac() -> Sdmac {
        Sdmac::new()
    }

    /// The register that hangs the A3000 boot. Kickstart's scsi.device spins
    /// here waiting for the DMA FIFO to report itself empty; an address that
    /// decodes to nothing reads back 0, meaning "not empty", forever.
    #[test]
    fn an_idle_sdmac_reports_an_empty_fifo_and_no_interrupt() {
        let mut s = sdmac();
        let istr = s.read_byte(SDMAC_BASE + ISTR);
        assert_eq!(istr & ISTR_FIFOE, ISTR_FIFOE);
        assert_eq!(istr & ISTR_FIFOF, 0);
        assert_eq!(
            istr & (ISTR_INT_P | ISTR_INT_E | ISTR_INT_S | ISTR_INT_F),
            0
        );
        assert!(!s.int_line());
    }

    /// Kickstart writes $0C, reads it back, then writes $00. RESET is a strobe
    /// and must not latch.
    #[test]
    fn the_control_register_reads_back_except_for_the_reset_strobe() {
        let mut s = sdmac();
        s.write_byte(SDMAC_BASE + CONTR, CONTR_INTEN | CONTR_DMADIR);
        assert_eq!(s.read_byte(SDMAC_BASE + CONTR), CONTR_INTEN | CONTR_DMADIR);
        s.write_byte(SDMAC_BASE + CONTR, CONTR_RESET);
        assert_eq!(s.read_byte(SDMAC_BASE + CONTR), 0);
    }

    /// Both Kickstart and cdhooper's sdmac tool identify the part by writing a
    /// pattern to WTC and seeing what comes back. Reading zero (bit 2 clear
    /// against a written bit 2) is what says "SDMAC-04".
    #[test]
    fn the_word_transfer_count_is_absent_on_the_sdmac_04() {
        let mut s = sdmac();
        for off in WTC..WTC + 4 {
            s.write_byte(SDMAC_BASE + off, 0xFF);
        }
        for off in WTC..WTC + 4 {
            assert_eq!(s.read_byte(SDMAC_BASE + off), 0x00);
        }
    }

    /// The DMA address register is a longword address: the low two bits are
    /// wired to zero. cdhooper's Ramsey test writes patterns and expects them
    /// back masked -- amiberry fails this, reading 0.
    #[test]
    fn the_dma_address_register_masks_its_low_two_bits() {
        let mut s = sdmac();
        for (written, expected) in [
            (0xFFFF_FFFFu32, 0xFFFF_FFFCu32),
            (0xA5A5_A5A5, 0xA5A5_A5A4),
            (0x5A5A_5A5A, 0x5A5A_5A58),
        ] {
            for i in 0..4 {
                s.write_byte(SDMAC_BASE + ACR + i, (written >> (8 * (3 - i))) as u8);
            }
            let mut got = 0u32;
            for i in 0..4 {
                got = (got << 8) | u32::from(s.read_byte(SDMAC_BASE + ACR + i));
            }
            assert_eq!(got, expected, "wrote {written:#010X}");
        }
    }

    /// The serial peripheral bus data register is a plain latch. amiberry
    /// reads $FF here and fails cdhooper's SDMAC test.
    #[test]
    fn the_serial_bus_data_register_latches() {
        let mut s = sdmac();
        for byte in [0x00u8, 0xA5, 0x5A] {
            s.write_byte(SDMAC_BASE + SSPBDAT + 3, byte);
            assert_eq!(s.read_byte(SDMAC_BASE + SSPBDAT + 3), byte);
        }
    }

    /// An idle WD33C93 is neither busy nor mid-command, and has nothing to say.
    /// $FF here (CIP | BSY | INT set) leaves scsi.device waiting forever for a
    /// command that never finishes.
    #[test]
    fn the_auxiliary_status_of_an_idle_chip_is_quiet() {
        use crate::scsi::{ASR_BSY, ASR_CIP, ASR_INT};
        let mut s = sdmac();
        let aux = s.read_byte(SDMAC_BASE + SASR);
        assert_eq!(aux & (ASR_CIP | ASR_BSY | ASR_INT), 0, "aux {aux:#04X}");
    }

    /// The register file ends at the data register: $1F is the auxiliary status
    /// aliased in read-only, and $1A-$1E are not registers and float. This is
    /// how cdhooper's sdmac tool identifies the chip -- with both writable, it
    /// reported "SCSI Controller: Not detected: INVALID" and failed its WDC
    /// test.
    #[test]
    fn the_registers_past_the_data_register_are_read_only_or_float() {
        use crate::scsi::WD_AUX_STATUS;
        let mut s = sdmac();
        let aux = s.read_byte(SDMAC_BASE + SASR);

        for value in [0xFFu8, 0xA5, 0x5A] {
            // The auxiliary status ignores writes and keeps reading the status.
            s.write_byte(SDMAC_BASE + SASR, WD_AUX_STATUS);
            s.write_byte(SDMAC_BASE + SCMD, value);
            s.write_byte(SDMAC_BASE + SASR, WD_AUX_STATUS);
            assert_eq!(s.read_byte(SDMAC_BASE + SCMD), aux, "wrote {value:#04X}");

            // An undefined register never drives the bus.
            s.write_byte(SDMAC_BASE + SASR, 0x1E);
            s.write_byte(SDMAC_BASE + SCMD, value);
            s.write_byte(SDMAC_BASE + SASR, 0x1E);
            assert_eq!(s.read_byte(SDMAC_BASE + SCMD), 0xFF, "wrote {value:#04X}");
        }
    }

    /// The WD33C93's registers are reached by writing a register number to the
    /// select latch and reading or writing the data port. Both addresses have
    /// an alias four bytes below (the SDMAC decodes them loosely) -- SysInfo
    /// polls the auxiliary status through the $41 alias, and read 250,000
    /// floating $00s before this decoded.
    #[test]
    fn the_wd33c93_register_file_is_reachable_through_both_aliases() {
        use crate::scsi::WD_OWN_ID;
        for select in [SASR, SASR_ALT] {
            for data in [SCMD, SCMD_ALT] {
                let mut s = sdmac();
                s.write_byte(SDMAC_BASE + select, WD_OWN_ID);
                s.write_byte(SDMAC_BASE + data, 0x07);
                s.write_byte(SDMAC_BASE + select, WD_OWN_ID);
                assert_eq!(
                    s.read_byte(SDMAC_BASE + data),
                    0x07,
                    "select {select:#04X} data {data:#04X}"
                );
            }
        }
    }

    /// The control register's interrupt enable gates the INT2 line, and the
    /// chip's interrupt shows up in ISTR whether or not it is enabled.
    #[test]
    fn the_interrupt_enable_gates_int2_but_not_the_status_register() {
        let mut s = sdmac();
        assert!(!s.int_line());
        assert_eq!(s.istr() & (ISTR_INT_S | ISTR_INT_P), 0);

        // Reset-complete raises the chip's interrupt without any drive.
        s.write_byte(SDMAC_BASE + CONTR, CONTR_RESET);
        s.write_byte(SDMAC_BASE + SASR, crate::scsi::WD_OWN_ID);
        s.write_byte(SDMAC_BASE + SCMD, 0x07);
        s.write_byte(SDMAC_BASE + SASR, crate::scsi::WD_COMMAND);
        s.write_byte(SDMAC_BASE + SCMD, 0x00); // CSR_RESET
        s.wd.tick(1000);
        assert!(s.wd.int_asserted(), "the chip should interrupt after reset");

        assert_eq!(s.istr() & ISTR_INT_S, ISTR_INT_S);
        assert!(!s.int_line(), "INT2 is masked until CONTR enables it");

        s.write_byte(SDMAC_BASE + CONTR, CONTR_INTEN);
        assert!(s.int_line());
        assert_eq!(s.istr() & ISTR_INT_P, ISTR_INT_P);
    }

    /// A started transfer takes the FIFO out of the empty state; stopping or
    /// flushing it puts it back. Kickstart waits on this bit.
    #[test]
    fn the_dma_strobes_drive_the_fifo_empty_flag() {
        let mut s = sdmac();
        assert_eq!(s.read_byte(SDMAC_BASE + ISTR) & ISTR_FIFOE, ISTR_FIFOE);
        s.write_byte(SDMAC_BASE + ST_DMA, 0);
        assert_eq!(s.read_byte(SDMAC_BASE + ISTR) & ISTR_FIFOE, 0);
        s.write_byte(SDMAC_BASE + FLUSH, 0);
        assert_eq!(s.read_byte(SDMAC_BASE + ISTR) & ISTR_FIFOE, ISTR_FIFOE);
        s.write_byte(SDMAC_BASE + ST_DMA, 0);
        s.write_byte(SDMAC_BASE + SP_DMA, 0);
        assert_eq!(s.read_byte(SDMAC_BASE + ISTR) & ISTR_FIFOE, ISTR_FIFOE);
    }

    /// The file repeats every $100; cdhooper's tool writes through the shadow
    /// to defeat CPU write buffering, so both copies must be the same register.
    #[test]
    fn the_register_file_is_mirrored_every_256_bytes() {
        let mut s = sdmac();
        s.write_byte(SDMAC_BASE + 0x100 + CONTR, CONTR_INTEN);
        assert_eq!(s.read_byte(SDMAC_BASE + CONTR), CONTR_INTEN);
        assert!(Sdmac::decodes(SDMAC_BASE + 0x100 + ISTR));
    }

    /// Undecoded addresses in the window stay undriven, so they float and get
    /// logged rather than reading back something we made up.
    #[test]
    fn undecoded_addresses_are_not_claimed() {
        assert!(Sdmac::decodes(SDMAC_BASE + ISTR));
        assert!(Sdmac::decodes(SDMAC_BASE + ACR + 2));
        assert!(!Sdmac::decodes(SDMAC_BASE));
        assert!(!Sdmac::decodes(SDMAC_BASE + 0x20));
        // Only inside the window: the offsets must not match everywhere.
        assert!(!Sdmac::decodes(0x0000_001F));
        assert!(!Sdmac::decodes(SDMAC_BASE + SDMAC_SIZE + ISTR));
    }
}
