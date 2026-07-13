//! Super DMAC (SDMAC): the SCSI DMA controller of the A3000.
//!
//! The SDMAC sits between the CPU bus and a WD33C93 SCSI controller: it owns a
//! DMA FIFO and the interrupt plumbing, and it maps the WD33C93's own register
//! file into two of its addresses (a register-select latch and a data port).
//!
//! Only the register file is modelled: no DMA engine, and no WD33C93 behind
//! it. This is not yet enough to boot an A3000, and the machine it leaves is
//! not an A3000 with an empty SCSI socket either -- see below.
//!
//! What it does fix is a hang. Kickstart's scsi.device spins on the interrupt
//! status register waiting for the DMA FIFO to report itself empty, and with
//! nothing decoding that address the FIFO-empty bit reads back as zero
//! forever, so the ROM never reaches a display. An idle SDMAC reports an empty
//! FIFO and no interrupt pending, which gets the driver moving again.
//!
//! It then deadlocks one step further along: the driver arms interrupts, sends
//! the WD33C93 a command, and waits for the completion interrupt, which nothing
//! here can raise. Exec ends up in its idle loop with no runnable task. Neither
//! an all-ones nor an all-zeroes auxiliary status persuades it to give up on
//! the missing chip, and amiberry offers no guidance because it has no
//! absent-WD33C93 path at all: its SDMAC always routes the register window
//! straight into a WD33C93 core.
//!
//! So the way out is to fit the chip. We already have a WD33C93 in
//! `crate::scsi`, driven by the A2091's DMAC in `crate::a2091` -- exactly the
//! layering amiberry uses, where the A2091 DMAC and the A3000 SDMAC are two
//! front-ends onto one shared core. Wiring `SASR`/`SCMD` through to it, and
//! its interrupt to INT2, is the next step and the one that should boot.

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
const SASR: u32 = 0x49; // R/W  WD33C93 register select
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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Sdmac {
    /// Control register. Only the latched bits read back; RESET is a strobe.
    contr: u8,
    /// DACK width. Write-only on real silicon, so nothing ever reads it back.
    dawr: u8,
    /// DMA address register. It lives in Ramsey, not in the SDMAC, but it is
    /// addressed through the SDMAC window, so it is modelled here with the DMA
    /// engine that would use it. The low two bits are wired to zero: this is a
    /// longword address.
    acr: u32,
    /// Synchronous serial peripheral bus, used for the external clock/EEPROM
    /// on some boards. Nothing behind it here; the registers latch.
    sspbdat: u32,
    sspbctl: u32,
    /// WD33C93 register-select latch. The chip it selects is not fitted.
    sasr: u8,
}

impl Sdmac {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Interrupt status. With no DMA engine the FIFO is always empty and never
    /// full, and with no WD33C93 nothing ever raises an interrupt.
    fn istr(&self) -> u8 {
        ISTR_FIFOE
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
            DAWR | CONTR | ST_DMA | FLUSH | CLR_INT | ISTR | SP_DMA | SCMD | SASR
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
            // TODO(codewiz): route these to a crate::scsi WD33C93. Reading the
            // select address returns the chip's auxiliary status; the data
            // port returns the selected register. Until a chip is fitted these
            // are a guess, and no value works: scsi.device gets far enough to
            // wait for an interrupt no one can raise. $00 at least does not
            // claim a chip that is permanently busy (CIP and BSY set).
            SASR => 0x00,
            SCMD => 0xFF,
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
            // RESET would reset the WD33C93, which is not fitted. The bit is a
            // strobe and does not latch.
            CONTR => self.contr = value & !CONTR_RESET,
            SASR => self.sasr = value,
            // Writes to the missing WD33C93 go nowhere. Its register-select
            // latch still holds, so a driver can write a register number and
            // read $FF back from the data port, which is how it concludes the
            // socket is empty.
            SCMD => {}
            // The strobes have no DMA engine to act on.
            ST_DMA | SP_DMA | FLUSH | CLR_INT => {}
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

    /// The SDMAC never raises an interrupt without a WD33C93 to raise one for.
    pub fn int_line(&self) -> bool {
        false
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

    /// With no WD33C93 fitted the auxiliary status at least must not claim a
    /// chip that is permanently busy: $FF sets CIP and BSY, and a driver then
    /// waits forever for a command to finish. This does not make the machine
    /// boot -- only fitting the chip will -- but it pins down the one value we
    /// know to be wrong.
    #[test]
    fn an_absent_wd33c93_does_not_claim_to_be_permanently_busy() {
        use crate::scsi::{ASR_BSY, ASR_CIP, ASR_INT};
        let mut s = sdmac();
        s.write_byte(SDMAC_BASE + SASR, 0x15);
        assert_eq!(s.read_byte(SDMAC_BASE + SCMD), 0xFF);

        let aux = s.read_byte(SDMAC_BASE + SASR);
        assert_eq!(aux & (ASR_CIP | ASR_BSY | ASR_INT), 0, "aux {aux:#04X}");
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
