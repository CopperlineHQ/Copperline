// SPDX-License-Identifier: GPL-3.0-or-later

//! Freezer cartridge: an Action Replay-style system monitor that lives in
//! its own memory bank and is entered by a level-7 (non-maskable)
//! interrupt when the user presses the cartridge's button. One model is
//! fitted so far: HRTMon in its UAE cartridge build (`hrtmon-rom/`).
//!
//! The model follows the cartridge as the monitor's own source expects it
//! (`HRTmonV2.s` with `UAE`, `CARTRIDGE` and `SAVE_CUSTOM` set), which is
//! also how WinUAE hosts it:
//!
//! - A 1 MiB bank at `$A10000` holds the image, loaded at the bank start;
//!   the rest reads `$FF`. It is plain read/write memory on the CPU side
//!   (the monitor keeps its variables, stack and screen buffers in it) and
//!   is present at all times, not only after a freeze.
//! - A configuration block at the start of the image (`+20`..`+72`)
//!   describes the machine: colours, chipset, video standard, IDE
//!   interface, chip RAM size. The host writes it when the cartridge is
//!   fitted and again at every reset (WinUAE's `hrtmon_configure`), and
//!   clears the `entered` flag the monitor sets while it is active.
//! - The monitor cannot read the write-only custom registers back, so the
//!   host keeps a shadow of every custom-register write (CPU and Copper)
//!   and every driven custom-register read, plus the last byte written to
//!   each CIA register. A freeze copies the shadows into the bank at fixed
//!   offsets (`PTR_CUSTOM` = `$A9F000` for the 512-byte custom image; the
//!   CIA bytes at `$A9E000`/`$A9D000` on the real chips' `$100` register
//!   stride) before the monitor runs, so it can show and later restore the
//!   registers the interrupted program had set.
//! - A freeze then writes the level-7 autovector (VBR + `$7C`) to point at
//!   the `bra.w monitor` at `+12` and raises the non-maskable interrupt.
//!   The CPU takes it at the next instruction boundary whatever the SR
//!   mask says, and the monitor installs itself on first entry
//!   (`mon_install` through `init_code`). It leaves through the header's
//!   RTE when the user types `x`; the host does nothing on exit.
//!
//! Nothing here models Amiga hardware: a real Action Replay sits on the
//! expansion bus and snoops the chip-register writes itself. The bank,
//! the shadows and the pending interrupt are machine state (save-state
//! version 75) so run-ahead and rewind restore them with the guest.

use crate::memory::Memory;
use crate::zorro_device::dma_write_byte;

/// Base of the cartridge bank: the UAE cartridge address the image is
/// assembled for (`ORG $A10000`).
pub const HRTMON_BASE: u32 = 0x00A1_0000;
/// The bank is 1 MiB: `$A10000`-`$B0FFFF`.
pub const HRTMON_BANK_SIZE: usize = 0x10_0000;
/// At most this much of an image lands in the bank (WinUAE's loader reads
/// 512 KiB); the shadows live above it.
pub const HRTMON_IMAGE_MAX: usize = 0x8_0000;
/// The `bra.w monitor` the level-7 vector points at.
pub const HRTMON_ENTRY_OFFSET: u32 = 12;
/// Where the 512-byte custom-register shadow lands: `PTR_CUSTOM` in the
/// source, `$A9F000`.
pub const HRTMON_CUSTOM_SHADOW: usize = 0x8_F000;
/// The CIA-A shadow: byte `reg * $100 + 1`, the odd-byte lane the real
/// CIA-A answers on (`$BFE001 + reg * $100`).
pub const HRTMON_CIAA_SHADOW: usize = 0x8_E000;
/// The CIA-B shadow: byte `reg * $100`, the even-byte lane of `$BFD000`.
pub const HRTMON_CIAB_SHADOW: usize = 0x8_D000;
const CIA_SHADOW_STRIDE: usize = 0x100;
const CUSTOM_SHADOW_SIZE: usize = 0x200;

/// The configuration block, offsets from the image start (`HRTmonV2.s`,
/// the labels after `start`).
pub const CFG_MON_SIZE: usize = 20;
pub const CFG_COLOR0: usize = 24;
pub const CFG_COLOR1: usize = 26;
pub const CFG_KEYBOARD: usize = 29;
pub const CFG_IDE: usize = 31;
pub const CFG_A1200: usize = 32;
pub const CFG_AGA: usize = 33;
pub const CFG_CD32: usize = 37;
pub const CFG_SCREEN: usize = 38;
pub const CFG_NOVBR: usize = 39;
pub const CFG_ENTERED: usize = 40;
pub const CFG_HEXMODE: usize = 41;
pub const CFG_ID: usize = 50;
pub const CFG_VERSION: usize = 56;
pub const CFG_REVISION: usize = 58;
pub const CFG_MAX_CHIP: usize = 68;
/// The `NEWHRT` tag at `+50` that marks the 2.x config-block layout.
const CFG_ID_TAG: &[u8; 6] = b"NEWHRT";
/// The image tag WinUAE's loader accepts at offset 0 (old layout) or 4.
const IMAGE_TAG: &[u8; 4] = b"HRT!";
/// The `mon_size` the host reports: WinUAE's value, the monitor only uses
/// it for a `FreeMem` it never performs in cartridge mode.
const CFG_MON_SIZE_VALUE: u32 = 0x0080_0000;
/// The monitor's screen colours: WinUAE's dark blue on white.
const CFG_COLOR0_VALUE: u16 = 0x005A;
const CFG_COLOR1_VALUE: u16 = 0x0FFF;

/// Which cartridge is fitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CartridgeModel {
    Hrtmon,
}

impl CartridgeModel {
    /// The config spelling (`[cartridge] model`, `--cartridge`).
    pub fn label(self) -> &'static str {
        match self {
            CartridgeModel::Hrtmon => "hrtmon",
        }
    }

    /// The name shown to the user (menu row, OSD, launcher).
    pub fn display_name(self) -> &'static str {
        match self {
            CartridgeModel::Hrtmon => "HRTMon",
        }
    }
}

/// What the configuration block tells the monitor about the machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MachineTraits {
    /// AGA chipset (Alice/Lisa): the monitor reads the banked palette and
    /// the AGA registers.
    pub aga: bool,
    /// A CD32: the IDE registers, when present, are the Akiko-decoded ones
    /// at `$EB8000`.
    pub cd32: bool,
    /// NTSC video: the monitor uses a shorter screen.
    pub ntsc: bool,
    /// A motherboard IDE interface is fitted (Gayle or the A4000's).
    pub ide: bool,
    /// That interface is the Gayle (A600/A1200) type at `$DA2000`; the
    /// A4000's sits at `$DD2020`.
    pub gayle_ide: bool,
    /// Chip RAM fitted, the monitor's upper bound for chip-memory hunts.
    pub chip_ram_bytes: u32,
}

/// Why an image cannot be fitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// Neither offset 0 nor offset 4 carries `HRT!`.
    NotHrtmon,
    /// Larger than the loader accepts.
    TooLarge(usize),
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::NotHrtmon => {
                write!(f, "not an HRTMon image (no HRT! tag at offset 0 or 4)")
            }
            ImageError::TooLarge(len) => write!(
                f,
                "image is {len} bytes; an HRTMon image is at most {HRTMON_IMAGE_MAX} bytes"
            ),
        }
    }
}

impl std::error::Error for ImageError {}

/// Why a freeze could not be delivered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FreezeError {
    /// No cartridge is fitted: there is no button to press.
    NotFitted,
    /// The level-7 vector slot is not in writable RAM, so the interrupt
    /// would dispatch through whatever is there instead of the monitor.
    VectorNotWritable(u32),
}

impl std::fmt::Display for FreezeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FreezeError::NotFitted => {
                write!(
                    f,
                    "no freezer cartridge fitted ([cartridge] model = \"hrtmon\")"
                )
            }
            FreezeError::VectorNotWritable(addr) => write!(
                f,
                "the level-7 vector at {addr:#010X} (VBR + $7C) is not in writable RAM"
            ),
        }
    }
}

impl std::error::Error for FreezeError {}

/// The fitted cartridge: its bank, the register shadows the host keeps
/// for it, and the pending freeze interrupt.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Cartridge {
    model: CartridgeModel,
    /// The 1 MiB bank at [`HRTMON_BASE`].
    bank: Vec<u8>,
    /// The last value written to, or read from, each custom register
    /// (`$DFF000` + offset), 512 bytes big-endian.
    custom_shadow: Vec<u8>,
    /// The last byte written to each CIA register.
    ciaa_shadow: [u8; 16],
    ciab_shadow: [u8; 16],
    /// A freeze has been requested and the CPU has not yet acknowledged
    /// the level-7 interrupt.
    nmi_pending: bool,
    /// Freezes delivered since power-on.
    freezes: u64,
}

impl Cartridge {
    /// Fit an HRTMon image: `HRT!` at offset 4 (the cartridge header) or at
    /// offset 0 (the older layout WinUAE's loader also accepts); either
    /// way the image lands at the bank start.
    pub fn hrtmon(image: &[u8]) -> Result<Self, ImageError> {
        let tagged_at = |at: usize| image.len() >= at + 4 && &image[at..at + 4] == IMAGE_TAG;
        if !tagged_at(4) && !tagged_at(0) {
            return Err(ImageError::NotHrtmon);
        }
        if image.len() > HRTMON_IMAGE_MAX {
            return Err(ImageError::TooLarge(image.len()));
        }
        let mut bank = vec![0xFF; HRTMON_BANK_SIZE];
        bank[..image.len()].copy_from_slice(image);
        Ok(Self {
            model: CartridgeModel::Hrtmon,
            bank,
            custom_shadow: vec![0; CUSTOM_SHADOW_SIZE],
            ciaa_shadow: [0; 16],
            ciab_shadow: [0; 16],
            nmi_pending: false,
            freezes: 0,
        })
    }

    pub fn model(&self) -> CartridgeModel {
        self.model
    }

    /// The bank's base address.
    pub fn base(&self) -> u32 {
        HRTMON_BASE
    }

    /// The bank's size in bytes.
    pub fn size(&self) -> usize {
        self.bank.len()
    }

    /// Whether `addr` falls in the bank.
    #[inline]
    pub fn decodes(&self, addr: u32) -> bool {
        addr.wrapping_sub(self.base()) < self.bank.len() as u32
    }

    /// Byte offset into the bank of a `size`-byte access at `addr`, when
    /// the whole access lies inside it.
    #[inline]
    pub fn offset(&self, addr: u32, size: usize) -> Option<usize> {
        let off = addr.wrapping_sub(self.base()) as usize;
        (self.decodes(addr) && off + size <= self.bank.len()).then_some(off)
    }

    pub fn bank(&self) -> &[u8] {
        &self.bank
    }

    pub fn bank_mut(&mut self) -> &mut [u8] {
        &mut self.bank
    }

    /// Whether the monitor is active: bit 0 of `entered`, which it sets on
    /// entry and clears on exit.
    pub fn entered(&self) -> bool {
        self.bank[CFG_ENTERED] & 1 != 0
    }

    /// The monitor's version and revision words, when the image carries
    /// the `NEWHRT` config-block tag.
    pub fn version(&self) -> Option<(u16, u16)> {
        (&self.bank[CFG_ID..CFG_ID + 6] == CFG_ID_TAG).then(|| {
            let word = |at: usize| u16::from_be_bytes([self.bank[at], self.bank[at + 1]]);
            (word(CFG_VERSION), word(CFG_REVISION))
        })
    }

    /// The address the level-7 vector is pointed at: the `bra.w monitor`
    /// in the cartridge header.
    pub fn entry(&self) -> u32 {
        self.base() + HRTMON_ENTRY_OFFSET
    }

    pub fn nmi_pending(&self) -> bool {
        self.nmi_pending
    }

    /// Freezes delivered since power-on.
    pub fn freezes(&self) -> u64 {
        self.freezes
    }

    /// Write the configuration block: what the monitor is told about the
    /// machine, exactly the fields WinUAE's `hrtmon_configure` writes and
    /// no others (the `whd_*` fields belong to WHDLoad).
    pub fn configure(&mut self, traits: MachineTraits) {
        let bank = &mut self.bank;
        bank[CFG_MON_SIZE..CFG_MON_SIZE + 4].copy_from_slice(&CFG_MON_SIZE_VALUE.to_be_bytes());
        bank[CFG_COLOR0..CFG_COLOR0 + 2].copy_from_slice(&CFG_COLOR0_VALUE.to_be_bytes());
        bank[CFG_COLOR1..CFG_COLOR1 + 2].copy_from_slice(&CFG_COLOR1_VALUE.to_be_bytes());
        bank[CFG_KEYBOARD] = 0;
        bank[CFG_IDE] = u8::from(traits.ide);
        bank[CFG_A1200] = u8::from(traits.ide && traits.gayle_ide);
        bank[CFG_AGA] = u8::from(traits.aga);
        bank[CFG_CD32] = u8::from(traits.cd32);
        bank[CFG_SCREEN] = u8::from(traits.ntsc);
        bank[CFG_NOVBR] = 1;
        bank[CFG_ENTERED] = 0;
        bank[CFG_HEXMODE] = 1;
        bank[CFG_MAX_CHIP..CFG_MAX_CHIP + 4].copy_from_slice(&traits.chip_ram_bytes.to_be_bytes());
    }

    /// Reset: the block is rewritten and the monitor is no longer inside
    /// (WinUAE's `action_replay_memory_reset` + `hrtmon_configure`). A
    /// freeze still pending is dropped with the interrupt state the reset
    /// clears.
    pub fn reset(&mut self, traits: MachineTraits) {
        self.configure(traits);
        self.nmi_pending = false;
    }

    /// A custom-register write landed (CPU or Copper): keep its value.
    #[inline]
    pub fn note_custom_write(&mut self, off: u16, value: u16) {
        let at = usize::from(off & 0x1FE);
        self.custom_shadow[at..at + 2].copy_from_slice(&value.to_be_bytes());
    }

    /// A custom register drove a read: keep the value read.
    #[inline]
    pub fn note_custom_read(&mut self, off: u16, value: u16) {
        self.note_custom_write(off, value);
    }

    /// A CIA register was written.
    #[inline]
    pub fn note_cia_write(&mut self, cia_b: bool, reg: usize, value: u8) {
        let reg = reg & 15;
        if cia_b {
            self.ciab_shadow[reg] = value;
        } else {
            self.ciaa_shadow[reg] = value;
        }
    }

    /// The button: copy the shadows into the bank, point the level-7
    /// vector at the monitor and raise the non-maskable interrupt.
    /// Returns the entry address the vector now holds. Nothing changes
    /// when the vector slot is not writable RAM.
    pub fn freeze(&mut self, mem: &mut Memory, vbr: u32) -> Result<u32, FreezeError> {
        let vector = vbr.wrapping_add(0x7C);
        let entry = self.entry();
        for (i, byte) in entry.to_be_bytes().into_iter().enumerate() {
            if !dma_write_byte(mem, vector.wrapping_add(i as u32), byte) {
                return Err(FreezeError::VectorNotWritable(vector));
            }
        }
        self.bank[HRTMON_CUSTOM_SHADOW..HRTMON_CUSTOM_SHADOW + CUSTOM_SHADOW_SIZE]
            .copy_from_slice(&self.custom_shadow);
        for reg in 0..16 {
            self.bank[HRTMON_CIAA_SHADOW + reg * CIA_SHADOW_STRIDE + 1] = self.ciaa_shadow[reg];
            self.bank[HRTMON_CIAB_SHADOW + reg * CIA_SHADOW_STRIDE] = self.ciab_shadow[reg];
        }
        self.nmi_pending = true;
        self.freezes += 1;
        Ok(entry)
    }

    /// The CPU acknowledged the level-7 interrupt: the request is
    /// consumed, so the monitor is entered exactly once per freeze (the
    /// 68000 recognises level 7 on its transition, not its level).
    pub fn take_nmi(&mut self) -> bool {
        std::mem::take(&mut self.nmi_pending)
    }

    /// The shadows as the monitor will see them, for tests and diagnostics.
    pub fn custom_shadow(&self) -> &[u8] {
        &self.custom_shadow
    }

    pub fn cia_shadow(&self, cia_b: bool) -> &[u8; 16] {
        if cia_b {
            &self.ciab_shadow
        } else {
            &self.ciaa_shadow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_image() -> Vec<u8> {
        let mut image = vec![0u8; 0x60];
        image[4..8].copy_from_slice(b"HRT!");
        image[CFG_ID..CFG_ID + 6].copy_from_slice(b"NEWHRT");
        image[CFG_VERSION..CFG_VERSION + 2].copy_from_slice(&2u16.to_be_bytes());
        image[CFG_REVISION..CFG_REVISION + 2].copy_from_slice(&39u16.to_be_bytes());
        image
    }

    fn memory() -> Memory {
        Memory {
            chip_ram: vec![0; 0x8_0000],
            slow_ram: Vec::new(),
            mb_ram: Vec::new(),
            accel_ram: Vec::new(),
            rom: vec![0; 0x8_0000],
            overlay: false,
            zorro: crate::zorro::ZorroChain::default(),
            extended_rom: Vec::new(),
            extended_rom_base: 0,
            wcs: Vec::new(),
            wcs_write_protected: false,
        }
    }

    #[test]
    fn loader_accepts_the_tag_at_offset_four_or_zero_and_nothing_else() {
        let cart = Cartridge::hrtmon(&stub_image()).unwrap();
        assert_eq!(cart.version(), Some((2, 39)));
        assert_eq!(cart.size(), HRTMON_BANK_SIZE);
        assert_eq!(cart.bank()[0x60], 0xFF, "the rest of the bank reads $FF");
        assert_eq!(cart.entry(), 0x00A1_000C);

        let mut old = vec![0u8; 0x40];
        old[..4].copy_from_slice(b"HRT!");
        let cart = Cartridge::hrtmon(&old).unwrap();
        assert_eq!(
            &cart.bank()[..4],
            b"HRT!",
            "old layout loads at the bank start"
        );
        assert_eq!(cart.version(), None);

        assert_eq!(
            Cartridge::hrtmon(b"ATZ!....").unwrap_err(),
            ImageError::NotHrtmon
        );
        assert_eq!(Cartridge::hrtmon(b"HR").unwrap_err(), ImageError::NotHrtmon);
        let mut huge = vec![0u8; HRTMON_IMAGE_MAX + 1];
        huge[4..8].copy_from_slice(b"HRT!");
        assert_eq!(
            Cartridge::hrtmon(&huge).unwrap_err(),
            ImageError::TooLarge(HRTMON_IMAGE_MAX + 1)
        );
    }

    #[test]
    fn decode_covers_the_whole_bank_and_no_more() {
        let cart = Cartridge::hrtmon(&stub_image()).unwrap();
        assert!(cart.decodes(0x00A1_0000));
        assert!(cart.decodes(0x00B0_FFFF));
        assert!(!cart.decodes(0x00A0_FFFF));
        assert!(!cart.decodes(0x00B1_0000));
        assert_eq!(cart.offset(0x00B0_FFFE, 2), Some(0xF_FFFE));
        assert_eq!(cart.offset(0x00B0_FFFF, 2), None);
        assert_eq!(cart.offset(0x00A1_0000, 4), Some(0));
    }

    #[test]
    fn configure_writes_winuae_fields_for_an_aga_ntsc_gayle_machine() {
        let mut cart = Cartridge::hrtmon(&stub_image()).unwrap();
        cart.bank_mut()[CFG_ENTERED] = 1;
        cart.configure(MachineTraits {
            aga: true,
            cd32: false,
            ntsc: true,
            ide: true,
            gayle_ide: true,
            chip_ram_bytes: 0x20_0000,
        });
        let bank = cart.bank();
        assert_eq!(
            &bank[CFG_MON_SIZE..CFG_MON_SIZE + 4],
            &[0x00, 0x80, 0x00, 0x00]
        );
        assert_eq!(&bank[CFG_COLOR0..CFG_COLOR0 + 2], &[0x00, 0x5A]);
        assert_eq!(&bank[CFG_COLOR1..CFG_COLOR1 + 2], &[0x0F, 0xFF]);
        assert_eq!(bank[CFG_KEYBOARD], 0);
        assert_eq!(bank[CFG_IDE], 1);
        assert_eq!(bank[CFG_A1200], 1);
        assert_eq!(bank[CFG_AGA], 1);
        assert_eq!(bank[CFG_CD32], 0);
        assert_eq!(bank[CFG_SCREEN], 1);
        assert_eq!(bank[CFG_NOVBR], 1);
        assert_eq!(bank[CFG_ENTERED], 0, "reset clears the entered flag");
        assert_eq!(bank[CFG_HEXMODE], 1);
        assert_eq!(
            &bank[CFG_MAX_CHIP..CFG_MAX_CHIP + 4],
            &[0x00, 0x20, 0x00, 0x00]
        );
        // Untouched by the host: the fields WHDLoad owns and the header.
        assert_eq!(&bank[60..64], &[0; 4]);
        assert_eq!(&bank[4..8], b"HRT!");
        assert!(!cart.entered());

        // An OCS PAL machine with an A4000 IDE: a1200 stays clear.
        cart.configure(MachineTraits {
            ide: true,
            gayle_ide: false,
            chip_ram_bytes: 0x8_0000,
            ..Default::default()
        });
        let bank = cart.bank();
        assert_eq!(
            (
                bank[CFG_IDE],
                bank[CFG_A1200],
                bank[CFG_AGA],
                bank[CFG_SCREEN]
            ),
            (1, 0, 0, 0)
        );
    }

    #[test]
    fn freeze_copies_the_shadows_writes_the_vector_and_arms_one_nmi() {
        let mut cart = Cartridge::hrtmon(&stub_image()).unwrap();
        let mut mem = memory();
        cart.note_custom_write(0x180, 0x0F00);
        cart.note_custom_write(0x181, 0x0ABC); // odd offsets land on the word
        cart.note_custom_read(0x002, 0x8210);
        cart.note_cia_write(false, 0x0E, 0x11);
        cart.note_cia_write(true, 0x00, 0x7F);
        cart.note_cia_write(true, 0x1F, 0x22); // register index wraps at 16

        assert_eq!(cart.freeze(&mut mem, 0x400).unwrap(), 0x00A1_000C);
        assert_eq!(&mem.chip_ram[0x47C..0x480], &0x00A1_000Cu32.to_be_bytes());
        let bank = cart.bank();
        assert_eq!(
            &bank[HRTMON_CUSTOM_SHADOW + 0x180..HRTMON_CUSTOM_SHADOW + 0x182],
            &[0x0A, 0xBC]
        );
        assert_eq!(
            &bank[HRTMON_CUSTOM_SHADOW + 0x002..HRTMON_CUSTOM_SHADOW + 0x004],
            &[0x82, 0x10]
        );
        assert_eq!(bank[HRTMON_CIAA_SHADOW + 0x0E * 0x100 + 1], 0x11);
        assert_eq!(
            bank[HRTMON_CIAA_SHADOW + 0x0E * 0x100],
            0xFF,
            "CIA-A uses the odd lane"
        );
        assert_eq!(bank[HRTMON_CIAB_SHADOW], 0x7F);
        assert_eq!(bank[HRTMON_CIAB_SHADOW + 0x0F * 0x100], 0x22);
        assert_eq!(
            bank[HRTMON_CIAB_SHADOW + 1],
            0xFF,
            "CIA-B uses the even lane"
        );
        assert!(cart.nmi_pending());
        assert_eq!(cart.freezes(), 1);
        assert!(cart.take_nmi());
        assert!(!cart.take_nmi(), "acknowledged once");
        assert!(!cart.nmi_pending());

        // A vector slot outside RAM refuses the freeze and arms nothing.
        assert_eq!(
            cart.freeze(&mut mem, 0x00F8_0000).unwrap_err(),
            FreezeError::VectorNotWritable(0x00F8_007C)
        );
        assert!(!cart.nmi_pending());
        assert_eq!(cart.freezes(), 1);
    }
}
