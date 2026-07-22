// SPDX-License-Identifier: GPL-3.0-or-later

//! Memory regions backing the Amiga bus.

use crate::zorro::ZorroChain;
use anyhow::{Context, Result};
use std::path::Path;

pub const ROM_SIZE: usize = 512 * 1024;
/// A 256 KiB Kickstart part (1.2/1.3) decodes one address line fewer than a
/// 512 KiB part, so it responds across the whole 512 KiB ROM window mirrored.
pub const ROM_SIZE_256K: usize = 256 * 1024;
pub const ROM_BASE: u64 = 0x00F8_0000;
pub const CHIP_RAM_BASE: u64 = 0x0000_0000;
/// Size of the chip-RAM select window the motherboard address decode (Gary
/// and equivalents) routes to Agnus: $000000-$1FFFFF. Agnus decodes fewer
/// address bits than the window on the smaller parts, so the fitted RAM
/// image repeats inside it (see CpuBus::chip_window_offset).
pub const CHIP_WINDOW_SIZE: u64 = 0x0020_0000;
/// Conventional base of the first Zorro II RAM board (the start of the
/// Zorro II expansion space, where the ROM assigns it). Test fixtures
/// pre-configure their fast RAM boards here.
#[cfg_attr(not(test), allow(dead_code))]
pub const FAST_RAM_BASE: u64 = 0x0020_0000;
pub const SLOW_RAM_BASE: u64 = 0x00C0_0000;
/// Exclusive top of the Ramsey-controlled motherboard fast RAM (A3000/A4000):
/// the bank ends here and grows downward, so a full 16 MiB reaches $07000000.
/// Kickstart sizes it from the top down; unpopulated space below the fitted
/// RAM stays undecoded (Fat Gary times the cycle out).
pub const MB_RAM_TOP: u64 = 0x0800_0000;
/// Ramsey's own four banks of 1Mx4 parts stop at 16 MiB ($07000000-$07FFFFFF),
/// but the big-box memory map reserves the whole $04000000-$07FFFFFF window
/// for motherboard RAM expansion, and Kickstart's top-down sizing probe walks
/// all of it. Fitting more than [`MB_RAM_RAMSEY_MAX`] models an expanded
/// motherboard decode filling that window (A4000/Ramsey-07 only; config
/// validation enforces the gate). The cap keeps the [`Memory::mb_ram_base`]
/// subtraction from ever underflowing.
pub const MB_RAM_MAX: usize = 64 * 1024 * 1024;
/// The most motherboard RAM Ramsey itself can drive: four 4 MiB banks of
/// 1Mx4 parts. Totals beyond this spill into the expansion window below
/// $07000000 (see [`MB_RAM_MAX`]).
pub const MB_RAM_RAMSEY_MAX: usize = 16 * 1024 * 1024;
/// Base of the CPU-slot (accelerator) fast RAM: the $08000000-$0FFFFFFF
/// coprocessor-slot expansion space of the big-box memory map, where
/// accelerator boards carry their local RAM. The bank starts here and grows
/// upward; Kickstart's sizing probe scans the space bottom-up.
pub const ACCEL_RAM_BASE: u64 = 0x0800_0000;
/// The whole coprocessor-slot space: $08000000 up to $10000000, where the
/// Zorro III expansion space begins.
pub const ACCEL_RAM_MAX: usize = 128 * 1024 * 1024;
/// Amiga 1000 WCS / WOM: 256 KiB of writable control store at $FC0000 that
/// the boot ROM loads Kickstart into and then write-protects. The 256 KiB
/// boot-ROM window ($F80000-$FBFFFF) sits immediately below it, so a boot
/// ROM echoed to this size and mapped at ROM_BASE ends exactly at WCS_BASE.
pub const WCS_BASE: u64 = 0x00FC_0000;
pub const WCS_SIZE: usize = 256 * 1024;
/// The A1000 bootstrap ROM is 64 KiB; the address latch echoes it across the
/// 256 KiB $F80000-$FBFFFF window.
pub const A1000_BOOT_ROM_SIZE: usize = 64 * 1024;
pub use crate::zorro::{AUTOCONFIG_BASE, AUTOCONFIG_SIZE};

/// Restore big-endian byte order in a byte-swapped ROM image.
///
/// Every bootable Amiga ROM -- Kickstart parts of both sizes, the CDTV/CD32
/// extended ROMs, the A1000 bootstrap, AROS -- opens with the same big-endian
/// header: a $11xx ROM magic word followed by a JMP absolute long ($4EF9) to
/// the reset entry. Images prepared for an EPROM programmer store the same
/// content with the two bytes of every 16-bit word exchanged (Hyperion's
/// OS 3.1.4/3.2 releases ship their single-chip `.bin` ROM files this way,
/// alongside the `.rom` files in CPU order). Such a file opens $xx11 $F94E
/// instead -- bytes no big-endian ROM can start with, since $F94E in the
/// opcode slot would be an F-line instruction -- so the orientation is
/// detected from the header and the CPU byte order restored before use.
pub fn normalize_rom_byte_order(mut rom: Vec<u8>) -> Vec<u8> {
    let swapped = rom.len() >= 4
        && rom.len().is_multiple_of(2)
        && rom[1] == 0x11
        && rom[2..4] == [0xF9, 0x4E];
    if swapped {
        for pair in rom.chunks_exact_mut(2) {
            pair.swap(0, 1);
        }
        log::info!("byte-swapped (EPROM programmer) ROM image detected; restoring byte order");
    }
    rom
}

/// Normalise a boot-ROM image to the full 512 KiB ROM window. A 512 KiB
/// image is taken as-is. A 256 KiB image (Kickstart 1.2/1.3) does not decode
/// A18, so on real hardware the part appears mirrored across the whole
/// $F80000-$FFFFFF space; the 512 KiB images distributed for these versions
/// are simply that 256 KiB ROM doubled. Mirroring a 256 KiB image up to
/// ROM_SIZE makes both forms behave identically. Byte-swapped EPROM-burner
/// images are accepted and restored (see [`normalize_rom_byte_order`]).
pub fn normalize_boot_rom(rom: Vec<u8>) -> Result<Vec<u8>> {
    let rom = normalize_rom_byte_order(rom);
    match rom.len() {
        ROM_SIZE => Ok(rom),
        ROM_SIZE_256K => {
            let mut full = Vec::with_capacity(ROM_SIZE);
            full.extend_from_slice(&rom);
            full.extend_from_slice(&rom);
            Ok(full)
        }
        other => anyhow::bail!(
            "ROM size is {} bytes; expected {} (512 KiB) or {} (256 KiB)",
            other,
            ROM_SIZE,
            ROM_SIZE_256K
        ),
    }
}

/// Normalise an Amiga 1000 bootstrap ROM (the 64 KiB "Amiga ROM Bootstrap"
/// that loads Kickstart from the Kickstart disk into the WCS). On real
/// hardware the address latch echoes the 64 KiB part across the whole 256 KiB
/// $F80000-$FBFFFF boot-ROM window; echoing it here lets the standard ROM
/// decode at ROM_BASE cover that window, leaving $FC0000-$FFFFFF for the WCS.
pub fn normalize_a1000_boot_rom(rom: Vec<u8>) -> Result<Vec<u8>> {
    let rom = normalize_rom_byte_order(rom);
    if rom.len() != A1000_BOOT_ROM_SIZE {
        anyhow::bail!(
            "A1000 boot ROM is {} bytes; expected {} (64 KiB)",
            rom.len(),
            A1000_BOOT_ROM_SIZE
        );
    }
    let mut full = Vec::with_capacity(WCS_SIZE);
    while full.len() < WCS_SIZE {
        full.extend_from_slice(&rom);
    }
    Ok(full)
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Memory {
    pub chip_ram: Vec<u8>,
    pub slow_ram: Vec<u8>,
    /// Ramsey-controlled motherboard fast RAM (A3000/A4000): 32-bit local
    /// RAM off the chip bus, ending at [`MB_RAM_TOP`] and growing downward.
    /// Empty on machines without a Ramsey; fitted via [`Memory::fit_mb_ram`].
    pub mb_ram: Vec<u8>,
    /// CPU-slot (accelerator) fast RAM: 32-bit local RAM in the big-box
    /// coprocessor-slot space, starting at [`ACCEL_RAM_BASE`] and growing
    /// upward. Empty when no accelerator RAM is fitted; fitted via
    /// [`Memory::fit_accel_ram`].
    pub accel_ram: Vec<u8>,
    pub rom: Vec<u8>,
    pub overlay: bool,
    /// Zorro expansion boards (autoconfig chain plus their RAM windows).
    pub zorro: ZorroChain,
    /// Extended ROM image (CD32 at $E00000, CDTV at $F00000); empty when
    /// no extended ROM is fitted.
    pub extended_rom: Vec<u8>,
    pub extended_rom_base: u64,
    /// Amiga 1000 WCS / WOM at $FC0000 (256 KiB writable control store the
    /// boot ROM loads Kickstart into). Empty on every other machine, which is
    /// how the address decode tells an A1000 apart.
    pub wcs: Vec<u8>,
    /// A1000 WCS write-protect latch. A 68000 RESET clears it (WCS writable,
    /// boot ROM mapped at $F80000); a CPU write anywhere in $F80000-$FBFFFF
    /// sets it, after which the boot code runs the Kickstart it loaded.
    pub wcs_write_protected: bool,
}

impl Memory {
    /// Load the ROM image and allocate chip/slow RAM. At reset the
    /// CPU bus overlays the ROM into the low address range; the chip RAM
    /// backing store itself remains RAM and becomes CPU-visible when CIA-A
    /// releases /OVL. Expansion RAM lives on the `zorro` chain.
    pub fn load(
        rom_path: &Path,
        chip_ram_bytes: usize,
        slow_ram_bytes: usize,
        zorro: ZorroChain,
    ) -> Result<Self> {
        let rom = std::fs::read(rom_path)
            .with_context(|| format!("reading ROM {}", rom_path.display()))?;
        let rom = normalize_boot_rom(rom)?;
        Ok(Self::with_rom(
            rom,
            chip_ram_bytes,
            slow_ram_bytes,
            zorro,
            Vec::new(),
        ))
    }

    /// Load an Amiga 1000: the `rom_path` is the 64 KiB bootstrap ROM (echoed
    /// across the $F80000 boot-ROM window), and a 256 KiB WCS is allocated at
    /// $FC0000 for the boot ROM to load Kickstart into from the Kickstart disk.
    pub fn load_a1000(
        rom_path: &Path,
        chip_ram_bytes: usize,
        slow_ram_bytes: usize,
        zorro: ZorroChain,
    ) -> Result<Self> {
        let rom = std::fs::read(rom_path)
            .with_context(|| format!("reading A1000 boot ROM {}", rom_path.display()))?;
        let rom = normalize_a1000_boot_rom(rom)?;
        let wcs = vec![0u8; WCS_SIZE];
        Ok(Self::with_rom(
            rom,
            chip_ram_bytes,
            slow_ram_bytes,
            zorro,
            wcs,
        ))
    }

    /// Build a machine with a minimal placeholder ROM for the `--load-state`
    /// path. A save state carries the full ROM image and replaces it on load,
    /// so the machine only has to be constructible first; this avoids requiring
    /// the original Kickstart (or the bundled AROS) just to restore a state.
    /// The stub vectors the reset PC into a `bra.s` self-loop, so a machine that
    /// is somehow run before the state is applied stays inert rather than
    /// executing unmapped memory.
    pub fn placeholder(chip_ram_bytes: usize, slow_ram_bytes: usize, zorro: ZorroChain) -> Self {
        let mut rom = vec![0u8; ROM_SIZE];
        rom[0..4].copy_from_slice(&0x0000_4000u32.to_be_bytes()); // initial SP
        rom[4..8].copy_from_slice(&0x00F8_0010u32.to_be_bytes()); // initial PC
        rom[0x10..0x12].copy_from_slice(&0x60FEu16.to_be_bytes()); // bra.s self
        Self::with_rom(rom, chip_ram_bytes, slow_ram_bytes, zorro, Vec::new())
    }

    fn with_rom(
        rom: Vec<u8>,
        chip_ram_bytes: usize,
        slow_ram_bytes: usize,
        zorro: ZorroChain,
        wcs: Vec<u8>,
    ) -> Self {
        Self {
            chip_ram: vec![0u8; chip_ram_bytes],
            slow_ram: vec![0u8; slow_ram_bytes],
            mb_ram: Vec::new(),
            accel_ram: Vec::new(),
            rom,
            overlay: true,
            zorro,
            extended_rom: Vec::new(),
            extended_rom_base: 0,
            wcs,
            wcs_write_protected: false,
        }
    }

    /// Fit `bytes` of Ramsey-controlled motherboard fast RAM (see
    /// [`Memory::mb_ram`]). Zero removes the bank. Panics beyond
    /// [`MB_RAM_MAX`]: config validation rejects such sizes long before
    /// this, and enforcing the bound at the only mutation point keeps
    /// [`Memory::mb_ram_base`] a plain subtraction.
    pub fn fit_mb_ram(&mut self, bytes: usize) {
        assert!(
            bytes <= MB_RAM_MAX,
            "motherboard RAM {bytes} bytes exceeds the {MB_RAM_MAX}-byte expansion window"
        );
        self.mb_ram = vec![0u8; bytes];
    }

    /// Base address of the fitted motherboard RAM: the bank ends at
    /// [`MB_RAM_TOP`] and grows downward, so the base depends on its size.
    pub fn mb_ram_base(&self) -> u64 {
        MB_RAM_TOP - self.mb_ram.len() as u64
    }

    /// Fit `bytes` of CPU-slot (accelerator) fast RAM (see
    /// [`Memory::accel_ram`]). Zero removes the bank. Panics beyond
    /// [`ACCEL_RAM_MAX`]: config validation rejects such sizes long before
    /// this, and enforcing the bound at the only mutation point keeps the
    /// bank inside the coprocessor-slot space.
    pub fn fit_accel_ram(&mut self, bytes: usize) {
        assert!(
            bytes <= ACCEL_RAM_MAX,
            "accelerator RAM {bytes} bytes exceeds the {ACCEL_RAM_MAX}-byte CPU-slot space"
        );
        self.accel_ram = vec![0u8; bytes];
    }

    /// Attach an extended ROM image: 512 KiB maps at $E00000 (CD32),
    /// 256 KiB at $F00000 (CDTV). Byte-swapped EPROM-burner images are
    /// accepted and restored (see [`normalize_rom_byte_order`]).
    pub fn attach_extended_rom(&mut self, image: Vec<u8>) -> Result<()> {
        let image = normalize_rom_byte_order(image);
        self.extended_rom_base = match image.len() {
            0x8_0000 => 0x00E0_0000,
            0x4_0000 => 0x00F0_0000,
            other => anyhow::bail!(
                "extended ROM is {} bytes; expected 512 KiB (CD32, $E00000) \
                 or 256 KiB (CDTV, $F00000)",
                other
            ),
        };
        self.extended_rom = image;
        Ok(())
    }

    /// Remove any fitted extended ROM, returning the $E00000/$F00000 window
    /// to nothing (open bus). Used when a freshly loaded main ROM is not
    /// accompanied by an extended image.
    pub fn detach_extended_rom(&mut self) {
        self.extended_rom = Vec::new();
        self.extended_rom_base = 0;
    }

    /// Return memory to its cold power-on state: clear all RAM and restore
    /// the boot-time ROM overlay and Zorro autoconfig state. Unlike a
    /// warm (keyboard) reset, this does not preserve RAM contents, so the
    /// machine boots as if power had been cycled.
    pub fn power_on_reset(&mut self) {
        self.chip_ram.fill(0);
        self.slow_ram.fill(0);
        self.mb_ram.fill(0);
        self.accel_ram.fill(0);
        // Cold boot loses the WCS contents and returns the latch to boot mode
        // (WCS writable), so the A1000 reloads Kickstart from disk. A warm
        // (keyboard) reset preserves the WCS, as the real machine does.
        self.wcs.fill(0);
        self.wcs_write_protected = false;
        self.overlay = true;
        self.zorro.power_on_reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_rom_512k_is_taken_as_is() {
        let image: Vec<u8> = (0..ROM_SIZE).map(|i| i as u8).collect();
        let out = normalize_boot_rom(image.clone()).unwrap();
        assert_eq!(out, image);
    }

    #[test]
    fn boot_rom_256k_is_mirrored_into_full_window() {
        // A 256 KiB Kickstart 1.x part does not decode A18, so it appears
        // mirrored across the whole 512 KiB ROM window. A truncated 256 KiB
        // image must therefore expand to the same bytes as the doubled
        // 512 KiB images distributed for those versions.
        let half: Vec<u8> = (0..ROM_SIZE_256K).map(|i| i as u8).collect();
        let out = normalize_boot_rom(half.clone()).unwrap();
        assert_eq!(out.len(), ROM_SIZE);
        assert_eq!(&out[..ROM_SIZE_256K], &half[..]);
        assert_eq!(&out[ROM_SIZE_256K..], &half[..]);
    }

    /// A patterned image of `len` bytes opening with the ROM header: the
    /// $1114 magic word and a JMP absolute long to the reset entry.
    fn headered_rom(len: usize) -> Vec<u8> {
        let mut rom: Vec<u8> = (0..len).map(|i| (i / 3) as u8).collect();
        rom[..8].copy_from_slice(&[0x11, 0x14, 0x4E, 0xF9, 0x00, 0xF8, 0x00, 0xD2]);
        rom
    }

    /// `rom` with the two bytes of every 16-bit word exchanged, as an EPROM
    /// programmer image stores it.
    fn byte_swapped(rom: &[u8]) -> Vec<u8> {
        rom.chunks(2).flat_map(|p| [p[1], p[0]]).collect()
    }

    #[test]
    fn byte_swapped_boot_rom_is_restored_to_cpu_order() {
        // Hyperion's single-chip `.bin` ROM files are the `.rom` images with
        // every 16-bit word byte-swapped for the EPROM programmer; the header
        // then opens $1411 $F94E, which no big-endian ROM can start with.
        let rom = headered_rom(ROM_SIZE);
        let out = normalize_boot_rom(byte_swapped(&rom)).unwrap();
        assert_eq!(out, rom);
    }

    #[test]
    fn byte_swapped_256k_boot_rom_is_restored_then_mirrored() {
        let rom = headered_rom(ROM_SIZE_256K);
        let out = normalize_boot_rom(byte_swapped(&rom)).unwrap();
        assert_eq!(&out[..ROM_SIZE_256K], &rom[..]);
        assert_eq!(&out[ROM_SIZE_256K..], &rom[..]);
    }

    #[test]
    fn rom_without_the_swapped_header_is_left_alone() {
        // The swapped-JMP bytes alone are not enough: without the ROM magic
        // word the image is taken at face value. This keeps arbitrary data
        // (the A1000 WCS-loaded Kickstart disk path feeds this function too)
        // from being scrambled by a coincidental match.
        let mut rom = headered_rom(ROM_SIZE);
        rom[..4].copy_from_slice(&[0x12, 0x34, 0xF9, 0x4E]);
        assert_eq!(normalize_rom_byte_order(rom.clone()), rom);
        // Likewise a swapped magic word without the swapped JMP.
        rom[..4].copy_from_slice(&[0x14, 0x11, 0x4E, 0xF9]);
        assert_eq!(normalize_rom_byte_order(rom.clone()), rom);
    }

    #[test]
    fn byte_swapped_extended_rom_is_restored_on_attach() {
        // The CDTV extended ROM opens with the same $111x + JMP header as a
        // Kickstart, so a byte-swapped dump is detected the same way.
        let mut image = headered_rom(0x4_0000);
        image[1] = 0x11;
        let mut mem = Memory::placeholder(1024, 0, ZorroChain::default());
        mem.attach_extended_rom(byte_swapped(&image)).unwrap();
        assert_eq!(mem.extended_rom, image);
        assert_eq!(mem.extended_rom_base, 0x00F0_0000);
    }

    #[test]
    fn byte_swapped_a1000_boot_rom_is_restored_before_echoing() {
        let rom = headered_rom(A1000_BOOT_ROM_SIZE);
        let out = normalize_a1000_boot_rom(byte_swapped(&rom)).unwrap();
        assert_eq!(&out[..A1000_BOOT_ROM_SIZE], &rom[..]);
    }

    #[test]
    fn boot_rom_other_sizes_are_rejected() {
        assert!(normalize_boot_rom(vec![0u8; 1024]).is_err());
        assert!(normalize_boot_rom(vec![0u8; ROM_SIZE + 1]).is_err());
        assert!(normalize_boot_rom(Vec::new()).is_err());
    }

    #[test]
    fn placeholder_rom_is_full_size_and_self_loops() {
        // The --load-state placeholder must be a valid full-size ROM with sane
        // reset vectors and an inert self-loop, so a machine built from it is
        // constructible and harmless until the save state replaces the image.
        let mem = Memory::placeholder(512 * 1024, 256 * 1024, ZorroChain::default());
        assert_eq!(mem.rom.len(), ROM_SIZE);
        assert_eq!(mem.chip_ram.len(), 512 * 1024);
        assert_eq!(mem.slow_ram.len(), 256 * 1024);
        assert_eq!(&mem.rom[0..4], &0x0000_4000u32.to_be_bytes()); // initial SP
        assert_eq!(&mem.rom[4..8], &0x00F8_0010u32.to_be_bytes()); // initial PC
        assert_eq!(&mem.rom[0x10..0x12], &0x60FEu16.to_be_bytes()); // bra.s self
    }

    #[test]
    fn mb_ram_ends_at_its_top_and_clears_on_power_on() {
        let mut mem = Memory::placeholder(1024, 0, ZorroChain::default());
        assert_eq!(mem.mb_ram_base(), MB_RAM_TOP); // empty bank: zero-length
        mem.fit_mb_ram(2 * 1024 * 1024);
        // The bank grows downward from MB_RAM_TOP, so the base tracks size.
        assert_eq!(mem.mb_ram_base(), MB_RAM_TOP - 2 * 1024 * 1024);
        mem.mb_ram[0] = 0xAA;
        // Power-on loses the contents like every other RAM.
        mem.power_on_reset();
        assert_eq!(mem.mb_ram[0], 0);
        assert_eq!(mem.mb_ram.len(), 2 * 1024 * 1024);
    }

    /// The only mutation point enforces the 64 MiB expansion-window maximum,
    /// which is what keeps `mb_ram_base` a plain subtraction.
    #[test]
    #[should_panic(expected = "expansion window")]
    fn mb_ram_beyond_the_expansion_window_is_refused() {
        let mut mem = Memory::placeholder(1024, 0, ZorroChain::default());
        mem.fit_mb_ram(MB_RAM_MAX + 1);
    }

    /// A full 64 MiB motherboard fit reaches the bottom of the expansion
    /// window at $04000000.
    #[test]
    fn mb_ram_expansion_window_reaches_04000000() {
        let mut mem = Memory::placeholder(1024, 0, ZorroChain::default());
        mem.fit_mb_ram(MB_RAM_MAX);
        assert_eq!(mem.mb_ram_base(), 0x0400_0000);
    }

    #[test]
    fn accel_ram_starts_at_its_base_and_clears_on_power_on() {
        let mut mem = Memory::placeholder(1024, 0, ZorroChain::default());
        mem.fit_accel_ram(16 * 1024 * 1024);
        assert_eq!(mem.accel_ram.len(), 16 * 1024 * 1024);
        mem.accel_ram[0] = 0xAA;
        mem.power_on_reset();
        assert_eq!(mem.accel_ram[0], 0);
        assert_eq!(mem.accel_ram.len(), 16 * 1024 * 1024);
    }

    /// The CPU-slot space ends at $10000000 where Zorro III begins; the only
    /// mutation point refuses a bank that would cross it.
    #[test]
    #[should_panic(expected = "CPU-slot space")]
    fn accel_ram_beyond_the_cpu_slot_space_is_refused() {
        let mut mem = Memory::placeholder(1024, 0, ZorroChain::default());
        mem.fit_accel_ram(ACCEL_RAM_MAX + 1);
    }

    #[test]
    fn a1000_boot_rom_echoes_64k_across_the_256k_window() {
        // The 64 KiB A1000 bootstrap ROM is echoed four times to fill the
        // 256 KiB $F80000-$FBFFFF window, so the standard ROM decode covers it.
        let boot: Vec<u8> = (0..A1000_BOOT_ROM_SIZE).map(|i| i as u8).collect();
        let out = normalize_a1000_boot_rom(boot.clone()).unwrap();
        assert_eq!(out.len(), WCS_SIZE);
        for chunk in out.chunks(A1000_BOOT_ROM_SIZE) {
            assert_eq!(chunk, &boot[..]);
        }
        // Only the 64 KiB part is accepted (a 256/512 KiB Kickstart is not it).
        assert!(normalize_a1000_boot_rom(vec![0u8; ROM_SIZE]).is_err());
        assert!(normalize_a1000_boot_rom(vec![0u8; 32 * 1024]).is_err());
    }
}
