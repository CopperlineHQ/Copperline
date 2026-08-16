// SPDX-License-Identifier: GPL-3.0-or-later

//! Zorro expansion bus: an ordered autoconfig chain of boards.
//!
//! Boards appear one at a time in the Zorro II configuration space at
//! $E80000 (the A1200/A600/A500 path; this emulator has no native Zorro III
//! backplane, so Zorro III boards configure through the same window, as
//! expansion.library does on real 32-bit machines). Each board exposes the
//! nibble-encoded autoconfig ROM until the system ROM either assigns it a base
//! address or shuts it up, at which point the next unconfigured board
//! appears.
//!
//! Boards are described by a [`BoardSpec`]; built-in RAM boards come from
//! `[memory] fast`/`z3` in the config, and additional boards can be loaded
//! from TOML metadata files via `[[zorro]]` sections (the "plugin" path:
//! the autoconfig identity lives in data, not code). RAM-backed storage and
//! functional device apertures share the same placement and dispatch path
//! through [`BoardBacking`].

use crate::net::NetConfig;
use crate::wasm_manifest::{WasmCaps, WasmManifest};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const AUTOCONFIG_BASE: u64 = 0x00E8_0000;
pub const AUTOCONFIG_SIZE: u64 = 0x0001_0000;

const AUTOCONFIG_ROM_BYTES: usize = 16;
const AUTOCONFIG_PHYSICAL_BYTES: u64 = 0x80;

// er_Type fields.
const ERT_ZORROII: u8 = 0xC0;
const ERT_ZORROIII: u8 = 0x80;
const ERTF_MEMLIST: u8 = 1 << 5;
const ERTF_DIAGVALID: u8 = 1 << 4;
const ERTF_CHAINEDCONFIG: u8 = 1 << 3;

// er_Flags fields.
const ERFF_MEMSPACE: u8 = 1 << 7;
const ERFF_EXTENDED: u8 = 1 << 5;
const ERFF_ZORRO_III: u8 = 1 << 4;

// Configuration registers (physical offsets in the config window).
const EC_Z3_BASEADDRESS_PHYS: u64 = 0x44;
const EC_BASEADDRESS_PHYS: u64 = 0x48;
const EC_BASEADDRESS_LO_PHYS: u64 = 0x4A;
const EC_SHUTUP_PHYS: u64 = 0x4C;

/// Copperline's registered Amiga expansion manufacturer ID (dec0de
/// Consulting, 5192 / 0x1448). The same ID labels the real ROMulus
/// flash-ROM board (product 1); the emulator's virtual boards take
/// distinct product numbers under it so tools like identify.library can
/// tell them apart.
pub const COPPERLINE_MANUFACTURER_ID: u16 = 0x1448;
/// The always-present identification board guest software finds (via
/// expansion.library FindConfigDev) to detect that it is running under
/// Copperline. Product 1 is the physical ROMulus board, so the emulator's
/// product numbering starts at 2.
const PRODUCT_COPPERLINE_ID: u8 = 0x02;
const PRODUCT_FAST_RAM: u8 = 0x03;
const PRODUCT_Z3_RAM: u8 = 0x04;
/// The services board (host filesystem and, later, other guest services).
const PRODUCT_SERVICES: u8 = 0x05;
/// The bundled HostSocket board (`bsdsocket.library` backed by a host-side
/// TCP/IP stack; see `crate::hostsocket`).
const PRODUCT_HOSTSOCKET: u8 = 0x06;
/// The MHI virtual MPEG audio decoder board (`crate::mhi`, `[mhi]`); see
/// `docs/internals/mhi.md`.
const PRODUCT_MHI: u8 = 0x07;

/// Village Tronic's registered expansion manufacturer ID.
pub const PICASSO2_MANUFACTURER_ID: u16 = 2167;
/// Picasso II linear VRAM aperture. Product 13 is the unsupported segmented
/// configuration selected by the physical board's jumper.
pub const PICASSO2_PRODUCT_VRAM: u8 = 11;
/// Picasso II VGA-register and monitor-switch window.
pub const PICASSO2_PRODUCT_REGS: u8 = 12;
#[allow(dead_code)]
pub const PICASSO2_PRODUCT_SEGMENTED: u8 = 13;
/// The original Picasso II serial number.
pub const PICASSO2_SERIAL: u32 = 0x0002_0000;
/// Picasso II+ serial number. The products stay 11 and 12; Picasso96 uses
/// this serial to distinguish the CL-GD5428 revision from the original card.
pub const PICASSO2PLUS_SERIAL: u32 = 0x0010_0000;

/// Atéo Concepts' registered expansion manufacturer ID (also used by the
/// Graffity boards' Zorro III identity).
pub const GRAFFITY_MANUFACTURER_ID: u16 = 2092;
/// Graffity [Zorro II] linear VRAM aperture.
pub const GRAFFITY_Z2_PRODUCT_VRAM: u8 = 34;
/// Graffity [Zorro II] VGA I/O and monitor-switch aperture. Physically 128 KB
/// (twice Picasso II's register window), though only the low VGA port range
/// within it is live.
pub const GRAFFITY_Z2_PRODUCT_REGS: u8 = 33;
const GRAFFITY_Z2_REGS_SIZE: usize = 0x2_0000;
/// Graffity [Zorro III]'s single autoconfig identity: one 16 MB window
/// holding the switch-strobe trap, VGA-register, and VRAM sub-apertures.
pub const GRAFFITY_Z3_PRODUCT: u8 = 33;
const GRAFFITY_Z3_WINDOW_BYTES: usize = 0x0100_0000;

/// Device-window offsets carry this tag to distinguish multiple autoconfig
/// apertures owned by one [`crate::zorro_device::ZorroDevice`]. Physical
/// in-tree device windows are at most 128 MB, so bits 31:28 are otherwise
/// clear.
pub const DEVICE_WINDOW_SHIFT: u32 = 28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// "II"/"III" are the bus generations' proper names (roman numerals), not
// acronyms to be case-folded.
#[allow(clippy::upper_case_acronyms)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum ZorroVersion {
    II,
    III,
}

/// What sits behind the board's address space once configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BoardBacking {
    /// RAM the CPU can read and write at external-bus speed.
    Ram,
    /// A functional device (registers + optional boot ROM): accesses route to
    /// `Bus::devices[usize]`, a [`crate::zorro_device::BoardDevice`], not to
    /// board RAM. The index is assigned when the board is added to the chain
    /// and matches the device attached to the bus.
    Device(usize),
}

/// The autoconfig identity and backing of one expansion board.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BoardSpec {
    pub name: String,
    pub version: ZorroVersion,
    pub manufacturer: u16,
    pub product: u8,
    pub serial: u32,
    pub size_bytes: usize,
    pub backing: BoardBacking,
    /// Link the board space into the system free memory list (ERTF_MEMLIST).
    pub memlist: bool,
    /// Request placement in autoconfig memory space (ERFF_MEMSPACE). Most
    /// functional boards are I/O windows, but framebuffer apertures can be
    /// device-backed memory windows.
    pub memory_space: bool,
    /// Another autoconfig identity belonging to this physical board follows.
    pub chained: bool,
    /// Logical aperture number passed to a shared device in offset bits 31:28.
    pub window: u8,
    /// Autoboot: ERTF_DIAGVALID with er_InitDiagVec pointing at the
    /// DiagArea inside the configured board space.
    pub diag_vec: Option<u16>,
}

impl BoardSpec {
    /// The built-in Zorro II fast RAM board (`[memory] fast`).
    pub fn fast_ram(size_bytes: usize) -> Self {
        Self {
            name: "fast RAM".into(),
            version: ZorroVersion::II,
            manufacturer: COPPERLINE_MANUFACTURER_ID,
            product: PRODUCT_FAST_RAM,
            serial: 0,
            size_bytes,
            backing: BoardBacking::Ram,
            memlist: true,
            memory_space: true,
            chained: false,
            window: 0,
            diag_vec: None,
        }
    }

    /// The built-in Zorro III fast RAM board (`[memory] z3`).
    pub fn z3_ram(size_bytes: usize) -> Self {
        Self {
            name: "Zorro III RAM".into(),
            version: ZorroVersion::III,
            manufacturer: COPPERLINE_MANUFACTURER_ID,
            product: PRODUCT_Z3_RAM,
            serial: 0,
            size_bytes,
            backing: BoardBacking::Ram,
            memlist: true,
            memory_space: true,
            chained: false,
            window: 0,
            diag_vec: None,
        }
    }

    /// The Copperline identification board: a tiny, inert Zorro II board
    /// kept on the autoconfig chain so guest software can detect the
    /// emulator by finding manufacturer [`COPPERLINE_MANUFACTURER_ID`] /
    /// product [`PRODUCT_COPPERLINE_ID`] (e.g. identify.library calling
    /// FindConfigDev). It is the smallest legal Zorro II size, is left out
    /// of the free memory list, and never autoboots, so it does not perturb
    /// the machine's memory map. Its serial number carries the running
    /// Copperline version (see [`copperline_ident_serial`]) so a tool can
    /// report the exact version, not just the emulator name.
    pub fn copperline_id() -> Self {
        Self {
            name: "Copperline".into(),
            version: ZorroVersion::II,
            manufacturer: COPPERLINE_MANUFACTURER_ID,
            product: PRODUCT_COPPERLINE_ID,
            serial: copperline_ident_serial(),
            size_bytes: 0x1_0000,
            backing: BoardBacking::Ram,
            memlist: false,
            memory_space: true,
            chained: false,
            window: 0,
            diag_vec: None,
        }
    }

    /// The A2065 Ethernet board: Commodore West Chester (manufacturer 514),
    /// product 0x70, a 64K Zorro II window with no autoboot ROM. `slot` is the
    /// index of the matching `A2065` device in `Bus::devices`.
    pub fn a2065(slot: usize) -> Self {
        Self {
            name: "A2065 Ethernet".into(),
            version: ZorroVersion::II,
            manufacturer: 514,
            product: 0x70,
            serial: 0,
            size_bytes: 0x1_0000,
            backing: BoardBacking::Device(slot),
            memlist: false,
            memory_space: false,
            chained: false,
            window: 0,
            diag_vec: None,
        }
    }

    /// The MacroSystem Toccata sound board: manufacturer 18260 (0x4754),
    /// product 12, a 64K Zorro II I/O window with no autoboot ROM (the
    /// board's own `romtype` is `ROMTYPE_NOT` -- it needs no ROM image and
    /// carries no DiagArea). `slot` is the index of the matching `Toccata`
    /// device in `Bus::devices`.
    pub fn toccata(slot: usize) -> Self {
        Self {
            name: "Toccata".into(),
            version: ZorroVersion::II,
            manufacturer: 18260,
            product: 12,
            serial: 0,
            size_bytes: 0x1_0000,
            backing: BoardBacking::Device(slot),
            memlist: false,
            memory_space: false,
            chained: false,
            window: 0,
            diag_vec: None,
        }
    }

    /// The MHI virtual MPEG audio decoder board (`[mhi]`): manufacturer
    /// [`COPPERLINE_MANUFACTURER_ID`], product [`PRODUCT_MHI`] (7, the next
    /// free number after HostSocket), a 64K Zorro II I/O window with no
    /// autoboot ROM -- the guest library (`guest/mhi/`) finds it via
    /// `FindConfigDev`, exactly like HostSocket. `slot` is the index of the
    /// matching `Mhi` device in `Bus::devices`. See `docs/internals/mhi.md`.
    #[cfg(feature = "mhi")]
    pub fn mhi(slot: usize) -> Self {
        Self {
            name: "MHI MPEG audio decoder".into(),
            version: ZorroVersion::II,
            manufacturer: COPPERLINE_MANUFACTURER_ID,
            product: PRODUCT_MHI,
            serial: 0,
            size_bytes: 0x1_0000,
            backing: BoardBacking::Device(slot),
            memlist: false,
            memory_space: false,
            chained: false,
            window: 0,
            diag_vec: None,
        }
    }

    /// The A2091 SCSI controller board: the DMAC supplies the autoconfig
    /// identity (Commodore West Chester, product 3) with a valid DiagArea
    /// vector pointing at the boot ROM, which appears at $2000 in the
    /// board space. `slot` is the index of the matching `A2091` device in
    /// `Bus::devices`.
    pub fn a2091(slot: usize) -> Self {
        Self {
            name: "A2091 SCSI".into(),
            version: ZorroVersion::II,
            manufacturer: 514,
            product: 3,
            serial: 0,
            size_bytes: 0x1_0000,
            backing: BoardBacking::Device(slot),
            memlist: false,
            memory_space: false,
            chained: false,
            window: 0,
            diag_vec: Some(0x2000),
        }
    }

    /// The A4091 SCSI-2 controller board: Commodore (manufacturer 514),
    /// product 84, a 16M Zorro III window with the DiagArea vector at $0200
    /// in the nibble-wide boot ROM. The same identity is baked into the
    /// first bytes of the physical EPROM; here the chain supplies it, as it
    /// does for every board. `slot` is the index of the matching `A4091`
    /// device in `Bus::devices`.
    pub fn a4091(slot: usize) -> Self {
        Self {
            name: "A4091 SCSI".into(),
            version: ZorroVersion::III,
            manufacturer: 514,
            product: 84,
            serial: 0,
            size_bytes: 0x0100_0000,
            backing: BoardBacking::Device(slot),
            memlist: false,
            memory_space: false,
            chained: false,
            window: 0,
            diag_vec: Some(0x0200),
        }
    }

    /// The Copperline services board: a 64K Zorro II window through which the
    /// emulator offers host-backed services to the guest -- currently the
    /// host filesystem (`HOSTFS0:`...), with room for more (RTG, mouse,
    /// clipboard) later. The window carries the guest-side handler, the mount
    /// table, per-unit host register banks, and a DiagArea (`diag_vec` points
    /// at [`crate::filesys::DIAG_OFFSET`]) whose DiagPoint mounts the
    /// configured host directories at expansion init; its packet pump then
    /// rings each DosPacket in through its unit's doorbell register. Backed
    /// by the [`crate::filesys::FilesysBoard`] device in `Bus::devices`.
    pub fn copperline_services(slot: usize) -> Self {
        Self {
            name: "Copperline services".into(),
            version: ZorroVersion::II,
            manufacturer: COPPERLINE_MANUFACTURER_ID,
            product: PRODUCT_SERVICES,
            serial: 0,
            size_bytes: 0x1_0000,
            backing: BoardBacking::Device(slot),
            memlist: false,
            memory_space: false,
            chained: false,
            window: 0,
            diag_vec: Some(crate::filesys::DIAG_OFFSET),
        }
    }

    /// The bundled HostSocket board (`[hostsocket]`): guest-facing
    /// `bsdsocket.library` backed by a host-side smoltcp stack, implemented
    /// as the embedded WASM plugin in `crate::hostsocket`. A 64K Zorro II
    /// window holding the RPC register bank and the stub ROM the DiagArea
    /// (`diag_vec` points at [`crate::hostsocket::DIAG_OFFSET`]) boots from.
    /// The placeholder device slot is reassigned when the plugin board is
    /// attached, same as any `[[zorro]]` metadata board.
    pub fn hostsocket() -> Self {
        Self {
            name: "HostSocket bsdsocket.library".into(),
            version: ZorroVersion::II,
            manufacturer: COPPERLINE_MANUFACTURER_ID,
            product: PRODUCT_HOSTSOCKET,
            serial: 0,
            size_bytes: 0x1_0000,
            backing: BoardBacking::Device(0),
            memlist: false,
            memory_space: false,
            chained: false,
            window: 0,
            diag_vec: Some(crate::hostsocket::DIAG_OFFSET),
        }
    }

    /// A `lide.device`-compatible Zorro II IDE board (`[lide]`):
    /// RIPPLE (LIV2, manufacturer 0x144A / product 7, 128K, two channels),
    /// RIDE (LIV2, manufacturer 0x144A / product 9, 128K, one channel), or
    /// AT-Bus 2008 (manufacturer 0x082C / product 6, 64K, one channel,
    /// odd-lane ROM). `diag_vec` is only set when a boot ROM is configured
    /// (hardware-only mode never autoboots). `slot` is the index of the
    /// matching [`crate::ide_zorro::IdeZorro`] device in `Bus::devices`.
    pub fn lide(
        personality: crate::ide_zorro::LidePersonality,
        slot: usize,
        has_rom: bool,
    ) -> Self {
        use crate::ide_zorro::LidePersonality;
        let (name, manufacturer, product, serial, size_bytes, diag_vec) = match personality {
            LidePersonality::Ripple => ("lide RIPPLE IDE", 0x144A, 7, 0, 0x2_0000, 0x0008),
            LidePersonality::Ride => ("lide RIDE IDE", 0x144A, 9, 1, 0x2_0000, 0x0008),
            LidePersonality::AtBus2008 => ("lide AT-Bus 2008 IDE", 0x082C, 6, 0, 0x1_0000, 0x0001),
        };
        Self {
            name: name.into(),
            version: ZorroVersion::II,
            manufacturer,
            product,
            serial,
            size_bytes,
            backing: BoardBacking::Device(slot),
            memlist: false,
            memory_space: false,
            chained: false,
            window: 0,
            diag_vec: has_rom.then_some(diag_vec),
        }
    }

    /// The Z3660 accelerator's FPGA RTG core: one 128 MB Zorro III window
    /// (manufacturer 0x144B, product 1) holding the register file, the P96
    /// VRAM, and the GFXData mailbox; no autoboot ROM (the Z3660.card
    /// driver is disk-loaded). `slot` is the index of the matching `Z3660`
    /// device in `Bus::devices`.
    pub fn z3660(slot: usize) -> Self {
        Self {
            name: "Z3660 RTG".into(),
            version: ZorroVersion::III,
            manufacturer: crate::z3660::Z3660_MANUFACTURER_ID,
            product: crate::z3660::Z3660_PRODUCT,
            serial: 0,
            size_bytes: crate::z3660::Z3660_WINDOW_BYTES,
            backing: BoardBacking::Device(slot),
            memlist: false,
            memory_space: false,
            chained: false,
            window: 0,
            diag_vec: None,
        }
    }

    /// Picasso II linear framebuffer aperture (product 11). It is a
    /// device-backed Zorro II memory-space board and is deliberately not
    /// linked into Exec's free-memory list: Picasso96 owns the VRAM.
    pub fn picasso2_vram(slot: usize, size_bytes: usize) -> Self {
        Self::picasso2_vram_with_serial(slot, size_bytes, PICASSO2_SERIAL, "Picasso II VRAM")
    }

    /// Picasso II+ linear framebuffer aperture. Its product identity and
    /// layout match the original board; the serial selects the revision.
    pub fn picasso2plus_vram(slot: usize, size_bytes: usize) -> Self {
        Self::picasso2_vram_with_serial(slot, size_bytes, PICASSO2PLUS_SERIAL, "Picasso II+ VRAM")
    }

    fn picasso2_vram_with_serial(slot: usize, size_bytes: usize, serial: u32, name: &str) -> Self {
        Self {
            name: name.into(),
            version: ZorroVersion::II,
            manufacturer: PICASSO2_MANUFACTURER_ID,
            product: PICASSO2_PRODUCT_VRAM,
            serial,
            size_bytes,
            backing: BoardBacking::Device(slot),
            memlist: false,
            memory_space: true,
            chained: true,
            window: 1,
            diag_vec: None,
        }
    }

    /// Picasso II VGA I/O and monitor-switch aperture (product 12).
    pub fn picasso2_regs(slot: usize) -> Self {
        Self::picasso2_regs_with_serial(slot, PICASSO2_SERIAL, "Picasso II registers")
    }

    /// Picasso II+ VGA I/O and monitor-switch aperture (product 12).
    pub fn picasso2plus_regs(slot: usize) -> Self {
        Self::picasso2_regs_with_serial(slot, PICASSO2PLUS_SERIAL, "Picasso II+ registers")
    }

    fn picasso2_regs_with_serial(slot: usize, serial: u32, name: &str) -> Self {
        Self {
            name: name.into(),
            version: ZorroVersion::II,
            manufacturer: PICASSO2_MANUFACTURER_ID,
            product: PICASSO2_PRODUCT_REGS,
            serial,
            size_bytes: 0x1_0000,
            backing: BoardBacking::Device(slot),
            memlist: false,
            memory_space: false,
            chained: false,
            window: 0,
            diag_vec: None,
        }
    }

    /// Graffity [Zorro II] linear framebuffer aperture (product 34), chained
    /// to a 128 KB register aperture. Like Picasso II's, it is left out of
    /// Exec's free-memory list: Picasso96 owns the VRAM.
    pub fn graffity_z2_vram(slot: usize, size_bytes: usize) -> Self {
        Self {
            name: "Graffity VRAM".into(),
            version: ZorroVersion::II,
            manufacturer: GRAFFITY_MANUFACTURER_ID,
            product: GRAFFITY_Z2_PRODUCT_VRAM,
            serial: 0,
            size_bytes,
            backing: BoardBacking::Device(slot),
            memlist: false,
            memory_space: true,
            chained: true,
            window: 1,
            diag_vec: None,
        }
    }

    /// Graffity [Zorro II] VGA I/O and monitor-switch aperture (product 33).
    pub fn graffity_z2_regs(slot: usize) -> Self {
        Self {
            name: "Graffity registers".into(),
            version: ZorroVersion::II,
            manufacturer: GRAFFITY_MANUFACTURER_ID,
            product: GRAFFITY_Z2_PRODUCT_REGS,
            serial: 0,
            size_bytes: GRAFFITY_Z2_REGS_SIZE,
            backing: BoardBacking::Device(slot),
            memlist: false,
            memory_space: false,
            chained: false,
            window: 0,
            diag_vec: None,
        }
    }

    /// Graffity [Zorro III]: one 16 MB window (manufacturer 2092, product
    /// 33) holding the switch-strobe trap, VGA-register, and VRAM
    /// sub-apertures; no autoboot ROM. `slot` is the index of the matching
    /// `GraffityZ3` device in `Bus::devices`.
    pub fn graffity_z3(slot: usize) -> Self {
        Self {
            name: "Graffity [Zorro III]".into(),
            version: ZorroVersion::III,
            manufacturer: GRAFFITY_MANUFACTURER_ID,
            product: GRAFFITY_Z3_PRODUCT,
            serial: 0,
            size_bytes: GRAFFITY_Z3_WINDOW_BYTES,
            backing: BoardBacking::Device(slot),
            memlist: false,
            memory_space: false,
            chained: false,
            window: 0,
            diag_vec: None,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.window > 0x0f {
            bail!(
                "board {:?}: device window tag must fit in four bits",
                self.name
            );
        }
        if self.window != 0 && !matches!(self.backing, BoardBacking::Device(_)) {
            bail!(
                "board {:?}: only device-backed boards may use a window tag",
                self.name
            );
        }
        // A tagged offset carries the aperture number in bits 31:28, so the
        // aperture itself must decode entirely below the tag; a larger one
        // would silently alias into a neighbouring window.
        if self.window != 0 && self.size_bytes > 1 << DEVICE_WINDOW_SHIFT {
            bail!(
                "board {:?}: a window-tagged aperture must fit below the tag bits \
                 ({} bytes exceeds the {} MiB window)",
                self.name,
                self.size_bytes,
                (1usize << DEVICE_WINDOW_SHIFT) >> 20,
            );
        }
        match self.version {
            ZorroVersion::II => {
                if zorro_ii_size_code(self.size_bytes).is_none() {
                    bail!(
                        "board {:?}: {} bytes is not a Zorro II autoconfig size \
                         (64K, 128K, 256K, 512K, 1M, 2M, 4M, or 8M)",
                        self.name,
                        self.size_bytes
                    );
                }
            }
            ZorroVersion::III => {
                if zorro_iii_size_bits(self.size_bytes).is_none() {
                    bail!(
                        "board {:?}: {} bytes is not a Zorro III autoconfig size \
                         (a power of two from 64K to 1G)",
                        self.name,
                        self.size_bytes
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum BoardState {
    Unconfigured,
    Configured { base: u32 },
    ShutUp,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Board {
    spec: BoardSpec,
    state: BoardState,
    ram: Vec<u8>,
}

/// The autoconfig chain plus the configured boards' address windows.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ZorroChain {
    boards: Vec<Board>,
    /// Configured RAM windows, rebuilt whenever a board's state changes:
    /// (base, len, board index). Kept flat so the CPU's per-access lookup
    /// is a scan over at most a handful of entries.
    regions: Vec<(u32, u32, usize)>,
    /// Configured device-board windows (base, len, board index); accesses
    /// route to the device on the bus instead of board RAM.
    device_regions: Vec<(u32, u32, usize)>,
}

impl ZorroChain {
    pub fn add_board(&mut self, spec: BoardSpec) -> Result<()> {
        spec.validate()?;
        let ram = match spec.backing {
            BoardBacking::Ram => vec![0u8; spec.size_bytes],
            BoardBacking::Device(_) => Vec::new(),
        };
        self.boards.push(Board {
            spec,
            state: BoardState::Unconfigured,
            ram,
        });
        Ok(())
    }

    /// Add a RAM-backed board with its window pre-seeded from `rom` (copied at
    /// offset 0, the remainder left zero). Used for the filesys rtarea,
    /// whose handler ROM must be present in the window from power-on. The
    /// board is still writable RAM, as the real rtarea is.
    pub fn add_board_with_rom(&mut self, spec: BoardSpec, rom: &[u8]) -> Result<()> {
        spec.validate()?;
        if !matches!(spec.backing, BoardBacking::Ram) {
            bail!(
                "board {:?}: add_board_with_rom requires RAM backing",
                spec.name
            );
        }
        if rom.len() > spec.size_bytes {
            bail!(
                "board {:?}: ROM image {} bytes exceeds the {}-byte window",
                spec.name,
                rom.len(),
                spec.size_bytes
            );
        }
        let mut ram = vec![0u8; spec.size_bytes];
        ram[..rom.len()].copy_from_slice(rom);
        self.boards.push(Board {
            spec,
            state: BoardState::Unconfigured,
            ram,
        });
        Ok(())
    }

    /// Add a board already configured at a fixed base, bypassing the
    /// autoconfig handshake (for fixtures and non-autoconfig mappings).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn add_board_configured_at(&mut self, spec: BoardSpec, base: u32) -> Result<()> {
        self.add_board(spec)?;
        let idx = self.boards.len() - 1;
        self.boards[idx].state = BoardState::Configured { base };
        self.rebuild_regions();
        Ok(())
    }

    /// Return all boards to unconfigured with zeroed RAM (cold power-on).
    pub fn power_on_reset(&mut self) {
        self.power_on_reset_with(|_, ram| ram.fill(0));
    }

    /// Return all boards to unconfigured and initialise each RAM-backed
    /// aperture through `fill`. The bank index gives a deterministic caller a
    /// stable stream discriminator without exposing the private Board layout.
    pub(crate) fn power_on_reset_with(&mut self, mut fill: impl FnMut(usize, &mut [u8])) {
        self.warm_reset();
        for (idx, board) in self.boards.iter_mut().enumerate() {
            fill(idx, &mut board.ram);
        }
    }

    /// Return all boards to unconfigured, preserving RAM contents. /RST on
    /// the expansion bus (keyboard reset or the CPU RESET instruction)
    /// clears each board's autoconfig latch, including shut-up boards, but
    /// expansion RAM keeps its contents so reset-surviving structures work.
    pub fn warm_reset(&mut self) {
        for board in &mut self.boards {
            board.state = BoardState::Unconfigured;
        }
        self.regions.clear();
        self.device_regions.clear();
    }

    fn current(&self) -> Option<usize> {
        self.boards
            .iter()
            .position(|b| b.state == BoardState::Unconfigured)
    }

    fn rebuild_regions(&mut self) {
        self.regions.clear();
        self.device_regions.clear();
        for (idx, board) in self.boards.iter().enumerate() {
            if let BoardState::Configured { base } = board.state {
                match board.spec.backing {
                    BoardBacking::Ram => {
                        if !board.ram.is_empty() {
                            self.regions.push((base, board.ram.len() as u32, idx));
                        }
                    }
                    BoardBacking::Device(_) => {
                        self.device_regions
                            .push((base, board.spec.size_bytes as u32, idx));
                    }
                }
            }
        }
    }

    // ----- configured RAM windows ----------------------------------------

    /// Locate `addr..addr+size` inside a configured board's RAM window.
    /// Returns the board index and byte offset.
    pub fn region_at(&self, addr: u32, size: usize) -> Option<(usize, usize)> {
        for &(base, len, idx) in &self.regions {
            let off = addr.wrapping_sub(base);
            if u64::from(off) + size as u64 <= u64::from(len) {
                return Some((idx, off as usize));
            }
        }
        None
    }

    /// The largest configured RAM window as (base, len, board index), for
    /// the CPU's JIT fastmem window. None until autoconfig has placed a
    /// RAM board.
    pub fn largest_ram_region(&self) -> Option<(u32, u32, usize)> {
        self.regions
            .iter()
            .max_by_key(|(_, len, _)| *len)
            .map(|&(base, len, idx)| (base, len, idx))
    }

    /// Configured RAM windows as (base, len) pairs, for the debugger's
    /// memory hunt.
    pub fn ram_regions(&self) -> Vec<(u32, u32)> {
        self.regions
            .iter()
            .filter(|(_, _, idx)| !self.boards[*idx].ram.is_empty())
            .map(|(base, len, _)| (*base, *len))
            .collect()
    }

    pub fn board_ram(&self, idx: usize) -> &[u8] {
        &self.boards[idx].ram
    }

    pub fn board_ram_mut(&mut self, idx: usize) -> &mut [u8] {
        &mut self.boards[idx].ram
    }

    /// Locate `addr..addr+size` inside a configured device board's window,
    /// returning the backing kind and byte offset.
    pub fn device_region_at(&self, addr: u32, size: usize) -> Option<(BoardBacking, u32)> {
        for &(base, len, idx) in &self.device_regions {
            let off = addr.wrapping_sub(base);
            if u64::from(off) + size as u64 <= u64::from(len) {
                let window = u32::from(self.boards[idx].spec.window) << DEVICE_WINDOW_SHIFT;
                return Some((self.boards[idx].spec.backing, off | window));
            }
        }
        None
    }

    // ----- autoconfig window ----------------------------------------------

    pub fn config_read(&self, addr: u64, size: usize) -> u64 {
        let Some(idx) = self.current() else {
            return floating_bus(size);
        };
        let off = addr.wrapping_sub(AUTOCONFIG_BASE) & (AUTOCONFIG_SIZE - 1);
        match size {
            1 => self.config_read_byte(idx, off) as u64,
            2 => {
                let hi = self.config_read_byte(idx, off) as u16;
                let lo = self.config_read_byte(idx, off + 1) as u16;
                ((hi << 8) | lo) as u64
            }
            4 => {
                let b0 = self.config_read_byte(idx, off) as u32;
                let b1 = self.config_read_byte(idx, off + 1) as u32;
                let b2 = self.config_read_byte(idx, off + 2) as u32;
                let b3 = self.config_read_byte(idx, off + 3) as u32;
                ((b0 << 24) | (b1 << 16) | (b2 << 8) | b3) as u64
            }
            _ => floating_bus(size),
        }
    }

    pub fn config_write(&mut self, addr: u64, size: usize, val: u64) {
        let Some(idx) = self.current() else {
            return;
        };
        let off = addr.wrapping_sub(AUTOCONFIG_BASE) & (AUTOCONFIG_SIZE - 1);
        let data_byte = match size {
            1 => (val & 0xFF) as u8,
            2 | 4 => ((val >> ((size - 1) * 8)) & 0xFF) as u8,
            _ => return,
        };
        let board = &mut self.boards[idx];
        match off & 0x7E {
            EC_Z3_BASEADDRESS_PHYS => {
                // Zorro III boards take a 16-bit write of the base address
                // high word here, which both assigns and configures.
                if board.spec.version == ZorroVersion::III && size >= 2 {
                    let base = ((val & 0xFFFF) as u32) << 16;
                    board.state = BoardState::Configured { base };
                    log::info!(
                        "zorro III board {:?} autoconfigured at {:#010X}",
                        board.spec.name,
                        base
                    );
                    self.rebuild_regions();
                }
            }
            EC_BASEADDRESS_PHYS => {
                // Zorro II boards take the base high byte here, which both
                // assigns and configures.
                if board.spec.version == ZorroVersion::II {
                    let base = u32::from(data_byte) << 16;
                    board.state = BoardState::Configured { base };
                    log::info!(
                        "zorro II board {:?} autoconfigured at {:#010X}",
                        board.spec.name,
                        base
                    );
                    self.rebuild_regions();
                }
            }
            EC_BASEADDRESS_LO_PHYS => {
                // The system ROM writes the low nibble here before the full byte
                // goes to ec_BaseAddress. Not needed for memory boards, but
                // accepting it keeps the register sequence faithful.
            }
            EC_SHUTUP_PHYS => {
                board.state = BoardState::ShutUp;
                log::debug!("zorro board {:?} shut up", board.spec.name);
                self.rebuild_regions();
            }
            _ => {}
        }
    }

    fn config_read_byte(&self, idx: usize, off: u64) -> u8 {
        if off >= AUTOCONFIG_PHYSICAL_BYTES || off & 1 != 0 {
            return 0xFF;
        }
        let Some(logical) = self.config_logical_byte(idx, (off / 4) as usize) else {
            return 0xFF;
        };
        let physical = if off / 4 == 0 { logical } else { !logical };
        let nibble = if off & 2 == 0 {
            physical >> 4
        } else {
            physical & 0x0F
        };
        nibble << 4
    }

    fn config_logical_byte(&self, idx: usize, byte: usize) -> Option<u8> {
        if byte >= AUTOCONFIG_ROM_BYTES {
            return None;
        }
        let spec = &self.boards[idx].spec;
        let mut rom = [0u8; AUTOCONFIG_ROM_BYTES];
        let memlist = if spec.memlist { ERTF_MEMLIST } else { 0 };
        let chained = if spec.chained { ERTF_CHAINEDCONFIG } else { 0 };
        let memspace = if spec.memory_space { ERFF_MEMSPACE } else { 0 };
        match spec.version {
            ZorroVersion::II => {
                rom[0] = ERT_ZORROII | memlist | chained | zorro_ii_size_code(spec.size_bytes)?;
                rom[2] = memspace;
            }
            ZorroVersion::III => {
                let (code, extended) = zorro_iii_size_bits(spec.size_bytes)?;
                rom[0] = ERT_ZORROIII | memlist | chained | code;
                rom[2] = memspace | ERFF_ZORRO_III | if extended { ERFF_EXTENDED } else { 0 };
            }
        }
        if let Some(vec) = spec.diag_vec {
            rom[0] |= ERTF_DIAGVALID;
            rom[10..12].copy_from_slice(&vec.to_be_bytes());
        }
        rom[1] = spec.product;
        rom[4..6].copy_from_slice(&spec.manufacturer.to_be_bytes());
        rom[6..10].copy_from_slice(&spec.serial.to_be_bytes());
        Some(rom[byte])
    }
}

/// Zorro II er_Type size code for a board space size, if representable.
pub fn zorro_ii_size_code(bytes: usize) -> Option<u8> {
    match bytes {
        0x0080_0000 => Some(0),
        0x0001_0000 => Some(1),
        0x0002_0000 => Some(2),
        0x0004_0000 => Some(3),
        0x0008_0000 => Some(4),
        0x0010_0000 => Some(5),
        0x0020_0000 => Some(6),
        0x0040_0000 => Some(7),
        _ => None,
    }
}

/// Zorro III er_Type size code and the er_Flags EXTENDED bit. Sizes up to
/// 8M use the Zorro II codes; 16M-1G use the extended encoding.
pub fn zorro_iii_size_bits(bytes: usize) -> Option<(u8, bool)> {
    if let Some(code) = zorro_ii_size_code(bytes) {
        return Some((code, false));
    }
    let code = match bytes {
        0x0100_0000 => 0, // 16M
        0x0200_0000 => 1, // 32M
        0x0400_0000 => 2, // 64M
        0x0800_0000 => 3, // 128M
        0x1000_0000 => 4, // 256M
        0x2000_0000 => 5, // 512M
        0x4000_0000 => 6, // 1G
        _ => return None,
    };
    Some((code, true))
}

fn floating_bus(size: usize) -> u64 {
    match size {
        1 => 0xFF,
        2 => 0xFFFF,
        4 => 0xFFFF_FFFF,
        _ => 0,
    }
}

// ----- metadata files -----------------------------------------------------

/// On-disk board metadata (`[[zorro]] metadata = "board.toml"`):
///
/// ```toml
/// name = "MegaRAM"
/// zorro = 3            # 2 or 3
/// type = "ram"         # "ram" or "wasm"
/// size = "64M"
/// manufacturer = 0x07DB
/// product = 0x20
/// serial = 0           # optional
/// memlist = true       # optional; defaults true for "ram", false for "wasm"
///
/// # For type = "wasm" (a plugin board), additionally:
/// wasm = "board.wasm"  # module path, relative to this metadata file
/// dma = true           # optional capabilities (default false)
/// int2 = true
/// # diag_vec = 0x40     # DiagArea offset in the board window, for a plugin
///                        # that ships its own autoboot ROM (see docs/zorro.md)
/// int6 = false
///
/// # Plugin settings passed to the module (defaults; the user overrides them
/// # per-board in the main config). A "file" option is loaded by the host and
/// # exposed to the plugin as a named resource.
/// [config]
/// mac = "02:00:10:00:00:01"
/// [[option]]
/// key = "mac"
/// label = "MAC address"
/// type = "string"      # string | bool | int | file | enum
/// [[option]]
/// key = "rom"
/// label = "Boot ROM"
/// type = "file"
/// ```
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBoardMeta {
    name: String,
    zorro: u8,
    #[serde(rename = "type")]
    backing: String,
    size: String,
    manufacturer: u32,
    product: u16,
    serial: Option<u32>,
    memlist: Option<bool>,
    // WASM plugin boards (type = "wasm"):
    wasm: Option<String>,
    dma: Option<bool>,
    int2: Option<bool>,
    int6: Option<bool>,
    /// Grants the `resolve` capability (the resolve_start/resolve_poll
    /// imports): host-OS-resolver DNS lookups on a background thread.
    resolve: Option<bool>,
    /// Grants the `host_sockets` capability (the sock_* imports): direct
    /// passthrough to real host OS sockets, on the host's own network
    /// identity. A materially bigger grant than `resolve`/`net` -- see
    /// `WasmCaps::host_sockets`'s own doc comment -- so this is opt-in per
    /// board, never implied by `net`/`resolve`.
    host_sockets: Option<bool>,
    /// Host network backend ("none"/"loopback"/"nat"/"bridge"); presence grants the
    /// `net` capability (the net_send/net_recv imports).
    net: Option<String>,
    /// Host adapter identifier for `net = "bridge"`.
    net_interface: Option<String>,
    /// Default plugin settings (free-form key/value).
    config: Option<toml::Table>,
    /// Config option schema, for the launcher's config panel.
    #[serde(default)]
    option: Vec<RawOption>,
    /// Offset of the plugin's DiagArea within its board window, for a WASM
    /// board that ships its own autoboot ROM (mirrors the in-tree A2091's
    /// `diag_vec`; see docs/zorro.md's "Plugin settings, files, and the
    /// config panel"). Only meaningful for `type = "wasm"`.
    diag_vec: Option<u16>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOption {
    key: String,
    label: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    default: Option<toml::Value>,
    choices: Option<Vec<String>>,
}

/// One editable plugin setting, declared by a `[[option]]` in the manifest, used
/// to render the launcher's config panel.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigOption {
    pub key: String,
    pub label: String,
    pub kind: ConfigOptionKind,
    pub default: Option<String>,
}

/// The editing widget a [`ConfigOption`] wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigOptionKind {
    String,
    Bool,
    Int,
    /// A host file path; loaded by the host and exposed to the plugin as a
    /// resource keyed by the option's `key`.
    File,
    /// A choice from a fixed list.
    Enum(Vec<String>),
}

/// Stringify a TOML scalar for the plugin's flat string-valued config map.
pub(crate) fn toml_value_to_string(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(f) => f.to_string(),
        other => other.to_string(),
    }
}

/// A board parsed from a TOML metadata file: a RAM board (fully described by
/// its autoconfig identity) or a WASM plugin board (identity plus the module
/// path and capabilities, instantiated at machine-build time).
#[derive(Debug)]
pub enum LoadedZorroBoard {
    Ram(BoardSpec),
    Wasm {
        /// Autoconfig identity; `backing` is a `Device` placeholder reassigned
        /// to the real slot index when the board is attached to the bus.
        spec: BoardSpec,
        wasm_path: PathBuf,
        manifest: WasmManifest,
        /// The config-option schema for the launcher panel.
        options: Vec<ConfigOption>,
        /// Default settings from `[config]` (option defaults applied, file
        /// values resolved relative to the metadata file); the user's per-board
        /// overrides are layered on top at config-resolution time.
        default_config: BTreeMap<String, String>,
    },
}

/// Load a board description from a TOML metadata file.
pub fn load_board_metadata(path: &Path) -> Result<LoadedZorroBoard> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading zorro board metadata {}", path.display()))?;
    let raw: RawBoardMeta = toml::from_str(&text)
        .with_context(|| format!("parsing zorro board metadata {}", path.display()))?;
    let version = match raw.zorro {
        2 => ZorroVersion::II,
        3 => ZorroVersion::III,
        other => bail!(
            "{}: zorro = {} is not supported (expected 2 or 3)",
            path.display(),
            other
        ),
    };
    if raw.manufacturer > 0xFFFF {
        bail!("{}: manufacturer must be a 16-bit ID", path.display());
    }
    if raw.product > 0xFF {
        bail!("{}: product must be an 8-bit ID", path.display());
    }
    let size_bytes = parse_board_size(&raw.size)
        .with_context(|| format!("{}: bad size {:?}", path.display(), raw.size))?;
    let kind = raw.backing.trim().to_ascii_lowercase();
    if kind != "wasm" && raw.diag_vec.is_some() {
        bail!(
            "{}: diag_vec is only valid for type = \"wasm\" boards",
            path.display()
        );
    }
    match kind.as_str() {
        "ram" => {
            let spec = BoardSpec {
                name: raw.name,
                version,
                manufacturer: raw.manufacturer as u16,
                product: raw.product as u8,
                serial: raw.serial.unwrap_or(0),
                size_bytes,
                backing: BoardBacking::Ram,
                memlist: raw.memlist.unwrap_or(true),
                memory_space: true,
                chained: false,
                window: 0,
                diag_vec: None,
            };
            spec.validate()
                .with_context(|| format!("{}: invalid board", path.display()))?;
            Ok(LoadedZorroBoard::Ram(spec))
        }
        "wasm" => {
            let Some(wasm) = raw.wasm else {
                bail!(
                    "{}: type = \"wasm\" needs a `wasm = \"module.wasm\"` path",
                    path.display()
                );
            };
            // The bundled-artifact sentinel is reserved for the board
            // [hostsocket] expands to; a metadata board naming it must fail
            // fast here (before the directory join below can obscure it), or
            // the plugin host would silently hand this board the embedded
            // HostSocket module under its own autoconfig identity.
            if wasm == crate::hostsocket::BUNDLED_HOSTSOCKET_WASM {
                bail!(
                    "{}: wasm = {:?} is reserved for the bundled [hostsocket] board",
                    path.display(),
                    wasm
                );
            }
            // Module path resolves relative to the metadata file's directory.
            let wasm_path = match path.parent() {
                Some(dir) => dir.join(&wasm),
                None => PathBuf::from(&wasm),
            };
            // The real slot index is assigned when the device is attached.
            let spec = BoardSpec {
                name: raw.name,
                version,
                manufacturer: raw.manufacturer as u16,
                product: raw.product as u8,
                serial: raw.serial.unwrap_or(0),
                size_bytes,
                backing: BoardBacking::Device(0),
                memlist: raw.memlist.unwrap_or(false),
                memory_space: false,
                chained: false,
                window: 0,
                diag_vec: raw.diag_vec,
            };
            spec.validate()
                .with_context(|| format!("{}: invalid board", path.display()))?;
            let net = match &raw.net {
                Some(s) => crate::net::parse_net_config(s, raw.net_interface.as_deref())
                    .with_context(|| format!("{}: invalid network backend", path.display()))?,
                None if raw.net_interface.is_none() => NetConfig::None,
                None => bail!("{}: net_interface needs net = \"bridge\"", path.display()),
            };
            if raw.net_interface.is_some() && !matches!(&net, NetConfig::Bridge { .. }) {
                bail!(
                    "{}: net_interface applies only to net = \"bridge\"",
                    path.display()
                );
            }
            let options = parse_options(&raw.option, path)?;
            let default_config = build_default_config(raw.config.as_ref(), &options, path);
            let manifest = WasmManifest {
                name: spec.name.clone(),
                caps: WasmCaps {
                    dma: raw.dma.unwrap_or(false),
                    int2: raw.int2.unwrap_or(false),
                    int6: raw.int6.unwrap_or(false),
                    net: raw.net.is_some(),
                    resolve: raw.resolve.unwrap_or(false),
                    host_sockets: raw.host_sockets.unwrap_or(false),
                },
                net,
                // The merge with the user's per-board overrides happens at
                // config-resolution time; defaults travel separately for now.
                config: BTreeMap::new(),
                file_keys: options
                    .iter()
                    .filter(|o| o.kind == ConfigOptionKind::File)
                    .map(|o| o.key.clone())
                    .collect(),
            };
            Ok(LoadedZorroBoard::Wasm {
                spec,
                wasm_path,
                manifest,
                options,
                default_config,
            })
        }
        other => bail!(
            "{}: type = {:?} is not supported (expected \"ram\" or \"wasm\")",
            path.display(),
            other
        ),
    }
}

/// Parse the `[[option]]` schema from a manifest.
fn parse_options(raw: &[RawOption], path: &Path) -> Result<Vec<ConfigOption>> {
    let mut out = Vec::with_capacity(raw.len());
    for o in raw {
        let kind = match o.kind.trim().to_ascii_lowercase().as_str() {
            "string" => ConfigOptionKind::String,
            "bool" => ConfigOptionKind::Bool,
            "int" => ConfigOptionKind::Int,
            "file" => ConfigOptionKind::File,
            "enum" => {
                let choices = o.choices.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}: option {:?} type = \"enum\" needs choices = [...]",
                        path.display(),
                        o.key
                    )
                })?;
                ConfigOptionKind::Enum(choices)
            }
            other => bail!(
                "{}: option {:?} has unknown type {:?} (expected string/bool/int/file/enum)",
                path.display(),
                o.key,
                other
            ),
        };
        out.push(ConfigOption {
            key: o.key.clone(),
            label: o.label.clone().unwrap_or_else(|| o.key.clone()),
            kind,
            default: o.default.as_ref().map(toml_value_to_string),
        });
    }
    Ok(out)
}

/// Build the default settings map: option defaults, overlaid by any `[config]`
/// entries, with file-typed values resolved relative to the metadata file.
fn build_default_config(
    config: Option<&toml::Table>,
    options: &[ConfigOption],
    path: &Path,
) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for o in options {
        if let Some(d) = &o.default {
            map.insert(o.key.clone(), d.clone());
        }
    }
    if let Some(table) = config {
        for (k, v) in table {
            map.insert(k.clone(), toml_value_to_string(v));
        }
    }
    if let Some(dir) = path.parent() {
        for o in options {
            if o.kind == ConfigOptionKind::File {
                if let Some(val) = map.get(&o.key) {
                    let resolved = dir.join(val).to_string_lossy().into_owned();
                    map.insert(o.key.clone(), resolved);
                }
            }
        }
    }
    map
}

fn parse_board_size(s: &str) -> Result<usize> {
    let raw = s.trim();
    let split = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    let (num_str, unit_str) = raw.split_at(split);
    let n: u64 = num_str.parse().context("bad number")?;
    let unit = unit_str.trim().to_ascii_uppercase().replace("IB", "B");
    let bytes = match unit.as_str() {
        "" | "B" => n,
        "K" | "KB" => n * 1024,
        "M" | "MB" => n * 1024 * 1024,
        "G" | "GB" => n * 1024 * 1024 * 1024,
        _ => bail!("unknown unit {:?}", unit_str),
    };
    Ok(bytes as usize)
}

/// Pack a `major.minor.patch` version string into a 32-bit autoconfig
/// serial number: byte 2 holds major, byte 1 minor, byte 0 patch. Missing
/// or non-numeric components (e.g. a `-dev` pre-release suffix) count as 0.
fn pack_version_serial(version: &str) -> u32 {
    let mut parts = version.split('.');
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let patch: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major << 16) | (minor << 8) | patch
}

/// The running Copperline version packed into a board serial number, so the
/// identification board ([`BoardSpec::copperline_id`]) can advertise it.
fn copperline_ident_serial() -> u32 {
    pack_version_serial(env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain_with(specs: Vec<BoardSpec>) -> ZorroChain {
        let mut chain = ZorroChain::default();
        for spec in specs {
            chain.add_board(spec).unwrap();
        }
        chain
    }

    #[test]
    fn zorro_ii_size_codes_match_autoconfig_slots() {
        assert_eq!(zorro_ii_size_code(64 * 1024), Some(1));
        assert_eq!(zorro_ii_size_code(512 * 1024), Some(4));
        assert_eq!(zorro_ii_size_code(8 * 1024 * 1024), Some(0));
        assert_eq!(zorro_ii_size_code(768 * 1024), None);
    }

    #[test]
    fn zorro_iii_sizes_cover_extended_range() {
        assert_eq!(zorro_iii_size_bits(2 * 1024 * 1024), Some((6, false)));
        assert_eq!(zorro_iii_size_bits(16 * 1024 * 1024), Some((0, true)));
        assert_eq!(zorro_iii_size_bits(1024 * 1024 * 1024), Some((6, true)));
        assert_eq!(zorro_iii_size_bits(3 * 1024 * 1024), None);
    }

    #[test]
    fn fast_ram_autoconfig_rom_is_nibble_encoded() {
        let chain = chain_with(vec![BoardSpec::fast_ram(512 * 1024)]);

        // er_Type is not inverted. 512K => size code 4, with Zorro II
        // and MEMLIST bits set: 0xC0 | 0x20 | 0x04 = 0xE4.
        assert_eq!(chain.config_read(AUTOCONFIG_BASE, 1), 0xE0);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 2, 1), 0x40);

        // er_Product is inverted before being exposed on the physical
        // nibbles: product 0x03 appears as 0xFC.
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 4, 1), 0xF0);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 6, 1), 0xC0);
    }

    #[test]
    fn copperline_id_board_advertises_registered_manufacturer() {
        let spec = BoardSpec::copperline_id();
        // dec0de Consulting's registered manufacturer ID, with a product
        // number distinct from the physical ROMulus board (product 1).
        assert_eq!(spec.manufacturer, COPPERLINE_MANUFACTURER_ID);
        assert_eq!(spec.manufacturer, 5192);
        assert_eq!(spec.product, PRODUCT_COPPERLINE_ID);
        // Inert: smallest Zorro II size, out of the free memory list, and
        // no autoboot, so it does not perturb the machine's memory map.
        assert_eq!(spec.version, ZorroVersion::II);
        assert_eq!(spec.size_bytes, 64 * 1024);
        assert!(!spec.memlist);
        assert!(spec.diag_vec.is_none());
        // Serial advertises the running Copperline version.
        assert_eq!(spec.serial, copperline_ident_serial());
        spec.validate().unwrap();
    }

    #[test]
    fn version_serial_packs_major_minor_patch() {
        assert_eq!(pack_version_serial("0.6.0"), 0x0000_0600);
        assert_eq!(pack_version_serial("1.2.3"), 0x0001_0203);
        assert_eq!(pack_version_serial("0.5"), 0x0000_0500);
        assert_eq!(pack_version_serial("12.34.56"), (12 << 16) | (34 << 8) | 56);
        // A pre-release suffix on the patch component counts as 0.
        assert_eq!(pack_version_serial("0.6.0-dev"), 0x0000_0600);
    }

    #[test]
    fn z2_base_write_configures_board_and_advances_chain() {
        let mut chain = chain_with(vec![
            BoardSpec::fast_ram(512 * 1024),
            BoardSpec::z3_ram(16 * 1024 * 1024),
        ]);

        chain.config_write(AUTOCONFIG_BASE + EC_BASEADDRESS_PHYS, 2, 0x2000);

        // The fast RAM board is now configured at $200000 and its RAM
        // window responds there.
        assert_eq!(chain.region_at(0x0020_0000, 2), Some((0, 0)));
        assert_eq!(chain.region_at(0x0027_FFFE, 2), Some((0, 0x7_FFFE)));
        assert_eq!(chain.region_at(0x0028_0000, 2), None);

        // The next board (Zorro III) is now in the config window:
        // er_Type = ERT_ZORROIII | ERTF_MEMLIST | 0 (16M extended).
        assert_eq!(chain.config_read(AUTOCONFIG_BASE, 1), 0xA0);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 2, 1), 0x00);
    }

    #[test]
    fn z3_board_configures_via_word_write_to_44() {
        let mut chain = chain_with(vec![BoardSpec::z3_ram(16 * 1024 * 1024)]);

        // er_Flags: MEMSPACE | ZORRO_III | EXTENDED = 0xB0, inverted to
        // 0x4F on the physical nibbles.
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 8, 1), 0x40);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 10, 1), 0xF0);

        chain.config_write(AUTOCONFIG_BASE + EC_Z3_BASEADDRESS_PHYS, 2, 0x4000);

        assert_eq!(chain.region_at(0x4000_0000, 4), Some((0, 0)));
        assert_eq!(chain.region_at(0x40FF_FFFC, 4), Some((0, 0x00FF_FFFC)));
        assert_eq!(chain.region_at(0x4100_0000, 4), None);
        // Chain exhausted: config space floats.
        assert_eq!(chain.config_read(AUTOCONFIG_BASE, 1), 0xFF);
    }

    #[test]
    fn shutup_disables_board_without_mapping_it() {
        let mut chain = chain_with(vec![BoardSpec::fast_ram(512 * 1024)]);

        chain.config_write(AUTOCONFIG_BASE + EC_SHUTUP_PHYS, 1, 0);

        assert_eq!(chain.region_at(0x0020_0000, 2), None);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE, 1), 0xFF);
    }

    #[test]
    fn power_on_reset_unconfigures_and_clears_ram() {
        let mut chain = chain_with(vec![BoardSpec::fast_ram(512 * 1024)]);
        chain.config_write(AUTOCONFIG_BASE + EC_BASEADDRESS_PHYS, 2, 0x2000);
        let (idx, off) = chain.region_at(0x0020_0000, 1).unwrap();
        chain.board_ram_mut(idx)[off] = 0xAA;

        chain.power_on_reset();

        assert_eq!(chain.region_at(0x0020_0000, 1), None);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE, 1), 0xE0);
        assert_eq!(chain.board_ram(0)[0], 0);
    }

    #[test]
    fn warm_reset_unconfigures_and_preserves_ram() {
        let mut chain = chain_with(vec![
            BoardSpec::fast_ram(512 * 1024),
            BoardSpec::fast_ram(512 * 1024),
        ]);
        chain.config_write(AUTOCONFIG_BASE + EC_BASEADDRESS_PHYS, 2, 0x2000);
        let (idx, off) = chain.region_at(0x0020_0000, 1).unwrap();
        chain.board_ram_mut(idx)[off] = 0xAA;
        // Shut up the second board; /RST must revive it too.
        chain.config_write(AUTOCONFIG_BASE + EC_SHUTUP_PHYS, 1, 0);

        chain.warm_reset();

        // Both boards are back in the autoconfig chain, RAM intact.
        assert_eq!(chain.region_at(0x0020_0000, 1), None);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE, 1), 0xE0);
        assert_eq!(chain.board_ram(0)[0], 0xAA);
        chain.config_write(AUTOCONFIG_BASE + EC_BASEADDRESS_PHYS, 2, 0x2000);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE, 1), 0xE0);
        chain.config_write(AUTOCONFIG_BASE + EC_BASEADDRESS_PHYS, 2, 0x2800);
        assert_eq!(chain.region_at(0x0020_0000, 1), Some((0, 0)));
        assert_eq!(chain.region_at(0x0028_0000, 1), Some((1, 0)));
    }

    #[test]
    fn board_metadata_file_round_trips() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "copperline-zorro-meta-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            r#"
            name = "MegaRAM"
            zorro = 3
            type = "ram"
            size = "64M"
            manufacturer = 2011
            product = 32
            "#,
        )
        .unwrap();
        let loaded = load_board_metadata(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        let LoadedZorroBoard::Ram(spec) = loaded else {
            panic!("expected a RAM board");
        };
        assert_eq!(spec.name, "MegaRAM");
        assert_eq!(spec.version, ZorroVersion::III);
        assert_eq!(spec.backing, BoardBacking::Ram);
        assert_eq!(spec.size_bytes, 64 * 1024 * 1024);
        assert_eq!(spec.manufacturer, 2011);
        assert_eq!(spec.product, 32);
        assert!(spec.memlist);
    }

    #[test]
    fn wasm_board_metadata_parses_path_and_caps() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "copperline-zorro-wasm-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            r#"
            name = "Test NIC"
            zorro = 2
            type = "wasm"
            size = "64K"
            manufacturer = 5192
            product = 16
            wasm = "nic.wasm"
            dma = true
            int2 = true
            diag_vec = 0x40
            "#,
        )
        .unwrap();
        let loaded = load_board_metadata(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        let LoadedZorroBoard::Wasm {
            spec,
            wasm_path,
            manifest,
            ..
        } = loaded
        else {
            panic!("expected a WASM board");
        };
        assert_eq!(spec.name, "Test NIC");
        assert_eq!(spec.version, ZorroVersion::II);
        assert!(matches!(spec.backing, BoardBacking::Device(_)));
        assert!(!spec.memlist);
        assert_eq!(spec.diag_vec, Some(0x40));
        // Module path is resolved next to the metadata file.
        assert_eq!(wasm_path, dir.join("nic.wasm"));
        assert_eq!(manifest.name, "Test NIC");
        assert!(manifest.caps.dma);
        assert!(manifest.caps.int2);
        assert!(!manifest.caps.int6);
    }

    #[test]
    fn diag_vec_rejected_for_ram_board() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "copperline-zorro-ram-diagvec-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            r#"
            name = "MegaRAM"
            zorro = 3
            type = "ram"
            size = "64M"
            manufacturer = 2011
            product = 32
            diag_vec = 0x40
            "#,
        )
        .unwrap();
        let err = load_board_metadata(&path).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(err.to_string().contains("diag_vec"), "{err:#}");
    }

    #[test]
    fn bad_metadata_is_rejected() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "copperline-zorro-badmeta-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"
            name = "Weird"
            zorro = 4
            type = "ram"
            size = "64M"
            manufacturer = 2011
            product = 32
            "#,
        )
        .unwrap();
        let err = load_board_metadata(&path).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(err.to_string().contains("zorro = 4"), "{err:#}");
    }

    #[test]
    fn serial_number_is_exposed_in_inverted_nibbles() {
        let mut spec = BoardSpec::fast_ram(512 * 1024);
        spec.serial = 0x1234_5678;
        let chain = chain_with(vec![spec]);

        // er_SerialNumber occupies logical bytes 6..10; every byte after
        // er_Type reads back inverted, one nibble per physical word.
        let mut serial = 0u32;
        for byte in 0..4u64 {
            let off = AUTOCONFIG_BASE + (6 + byte) * 4;
            let hi = (chain.config_read(off, 1) as u8) >> 4;
            let lo = (chain.config_read(off + 2, 1) as u8) >> 4;
            serial = (serial << 8) | u32::from(!((hi << 4) | lo));
        }
        assert_eq!(serial, 0x1234_5678);
    }

    #[test]
    fn diag_vector_sets_diagvalid_and_init_diag_vec() {
        let chain = chain_with(vec![BoardSpec::a2091(0)]);

        // er_Type carries ERTF_DIAGVALID (autoboot ROM present)...
        let hi = (chain.config_read(AUTOCONFIG_BASE, 1) as u8) >> 4;
        let lo = (chain.config_read(AUTOCONFIG_BASE + 2, 1) as u8) >> 4;
        let er_type = (hi << 4) | lo;
        assert_ne!(er_type & ERTF_DIAGVALID, 0);

        // ...and er_InitDiagVec (logical bytes 10..12) holds the DiagArea
        // offset inside the board space, inverted on the wire.
        let mut vec = 0u16;
        for byte in 0..2u64 {
            let off = AUTOCONFIG_BASE + (10 + byte) * 4;
            let hi = (chain.config_read(off, 1) as u8) >> 4;
            let lo = (chain.config_read(off + 2, 1) as u8) >> 4;
            vec = (vec << 8) | u16::from(!((hi << 4) | lo));
        }
        assert_eq!(vec, 0x2000);
    }

    #[test]
    fn word_and_long_config_reads_assemble_byte_lanes() {
        let chain = chain_with(vec![BoardSpec::fast_ram(512 * 1024)]);

        let b0 = chain.config_read(AUTOCONFIG_BASE, 1);
        let b1 = chain.config_read(AUTOCONFIG_BASE + 1, 1);
        let b2 = chain.config_read(AUTOCONFIG_BASE + 2, 1);
        let b3 = chain.config_read(AUTOCONFIG_BASE + 3, 1);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE, 2), (b0 << 8) | b1);
        assert_eq!(
            chain.config_read(AUTOCONFIG_BASE, 4),
            (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
        );
        // Odd physical bytes float high, as on the real bus.
        assert_eq!(b1, 0xFF);
        assert_eq!(b3, 0xFF);
    }

    #[test]
    fn low_nibble_then_high_byte_base_write_sequence_configures() {
        // The system ROM writes ec_BaseAddress low nibble ($4A) before the
        // high byte ($48); the sequence must configure exactly once.
        let mut chain = chain_with(vec![BoardSpec::fast_ram(512 * 1024)]);
        chain.config_write(AUTOCONFIG_BASE + EC_BASEADDRESS_LO_PHYS, 1, 0x00);
        assert_eq!(chain.region_at(0x0020_0000, 2), None, "not yet configured");
        chain.config_write(AUTOCONFIG_BASE + EC_BASEADDRESS_PHYS, 1, 0x20);
        assert_eq!(chain.region_at(0x0020_0000, 2), Some((0, 0)));
    }

    #[test]
    fn three_board_chain_configures_in_order() {
        let mut chain = chain_with(vec![
            BoardSpec::fast_ram(2 * 1024 * 1024),
            BoardSpec::a2091(0),
            BoardSpec::z3_ram(16 * 1024 * 1024),
        ]);

        // Board 1: Zorro II RAM at $200000.
        chain.config_write(AUTOCONFIG_BASE + EC_BASEADDRESS_PHYS, 1, 0x20);
        assert_eq!(chain.region_at(0x0020_0000, 2), Some((0, 0)));

        // Board 2: the A2091 device window at $E90000.
        chain.config_write(AUTOCONFIG_BASE + EC_BASEADDRESS_PHYS, 1, 0xE9);
        assert_eq!(
            chain.device_region_at(0x00E9_0000, 2),
            Some((BoardBacking::Device(0), 0))
        );

        // Board 3: Zorro III RAM via the 16-bit write to $44.
        chain.config_write(AUTOCONFIG_BASE + EC_Z3_BASEADDRESS_PHYS, 2, 0x4000);
        assert_eq!(chain.region_at(0x4000_0000, 2), Some((2, 0)));

        // The chain is exhausted: the config window floats.
        assert_eq!(chain.config_read(AUTOCONFIG_BASE, 1), 0xFF);
    }

    #[test]
    fn picasso2_chained_identities_share_one_tagged_device() {
        let mut chain = chain_with(vec![
            BoardSpec::picasso2_vram(3, 2 * 1024 * 1024),
            BoardSpec::picasso2_regs(3),
        ]);

        // Product 11 is a 2 MB Zorro II memory-space aperture and announces
        // that another identity belonging to this physical board follows.
        assert_eq!(chain.config_logical_byte(0, 0), Some(0xce));
        assert_eq!(chain.config_logical_byte(0, 1), Some(PICASSO2_PRODUCT_VRAM));
        assert_eq!(chain.config_logical_byte(0, 2), Some(ERFF_MEMSPACE));
        assert_eq!(chain.config_logical_byte(0, 4), Some(0x08));
        assert_eq!(chain.config_logical_byte(0, 5), Some(0x77));
        assert_eq!(chain.config_logical_byte(0, 6), Some(0x00));
        assert_eq!(chain.config_logical_byte(0, 7), Some(0x02));

        chain.config_write(AUTOCONFIG_BASE + EC_BASEADDRESS_PHYS, 1, 0x20);
        assert_eq!(
            chain.device_region_at(0x0020_0123, 1),
            Some((BoardBacking::Device(3), 1 << DEVICE_WINDOW_SHIFT | 0x123))
        );

        // Product 12 follows as a normal 64 KB I/O aperture with no tag.
        assert_eq!(chain.config_logical_byte(1, 0), Some(0xc1));
        assert_eq!(chain.config_logical_byte(1, 1), Some(PICASSO2_PRODUCT_REGS));
        assert_eq!(chain.config_logical_byte(1, 2), Some(0));
        chain.config_write(AUTOCONFIG_BASE + EC_BASEADDRESS_PHYS, 1, 0xe9);
        assert_eq!(
            chain.device_region_at(0x00e9_03c4, 2),
            Some((BoardBacking::Device(3), 0x3c4))
        );
    }

    #[test]
    fn picasso2plus_keeps_products_and_uses_revision_serial() {
        let specs = [
            BoardSpec::picasso2plus_vram(4, 1024 * 1024),
            BoardSpec::picasso2plus_regs(4),
        ];
        for (spec, product) in specs
            .iter()
            .zip([PICASSO2_PRODUCT_VRAM, PICASSO2_PRODUCT_REGS])
        {
            assert_eq!(spec.manufacturer, PICASSO2_MANUFACTURER_ID);
            assert_eq!(spec.product, product);
            assert_eq!(spec.serial, PICASSO2PLUS_SERIAL);
            assert_eq!(spec.backing, BoardBacking::Device(4));
        }

        let chain = chain_with(specs.into());
        assert_eq!(chain.config_logical_byte(0, 6), Some(0x00));
        assert_eq!(chain.config_logical_byte(0, 7), Some(0x10));
        assert_eq!(chain.config_logical_byte(0, 8), Some(0x00));
        assert_eq!(chain.config_logical_byte(0, 9), Some(0x00));
        assert_eq!(chain.config_logical_byte(1, 6), Some(0x00));
        assert_eq!(chain.config_logical_byte(1, 7), Some(0x10));
    }

    #[test]
    fn graffity_z2_chained_identities_share_one_tagged_device() {
        let mut chain = chain_with(vec![
            BoardSpec::graffity_z2_vram(3, 2 * 1024 * 1024),
            BoardSpec::graffity_z2_regs(3),
        ]);

        // Product 34 is a 2 MB Zorro II memory-space aperture (same size
        // code as Picasso II's) and announces the chained register board.
        assert_eq!(chain.config_logical_byte(0, 0), Some(0xce));
        assert_eq!(
            chain.config_logical_byte(0, 1),
            Some(GRAFFITY_Z2_PRODUCT_VRAM)
        );
        assert_eq!(chain.config_logical_byte(0, 2), Some(ERFF_MEMSPACE));
        assert_eq!(chain.config_logical_byte(0, 4), Some(0x08));
        assert_eq!(chain.config_logical_byte(0, 5), Some(0x2c));

        chain.config_write(AUTOCONFIG_BASE + EC_BASEADDRESS_PHYS, 1, 0x20);
        assert_eq!(
            chain.device_region_at(0x0020_0123, 1),
            Some((BoardBacking::Device(3), 1 << DEVICE_WINDOW_SHIFT | 0x123))
        );

        // Product 33 follows as a 128 KB I/O aperture with no tag.
        assert_eq!(chain.config_logical_byte(1, 0), Some(0xc2));
        assert_eq!(
            chain.config_logical_byte(1, 1),
            Some(GRAFFITY_Z2_PRODUCT_REGS)
        );
        assert_eq!(chain.config_logical_byte(1, 2), Some(0));
        chain.config_write(AUTOCONFIG_BASE + EC_BASEADDRESS_PHYS, 1, 0xe9);
        assert_eq!(
            chain.device_region_at(0x00e9_03c4, 2),
            Some((BoardBacking::Device(3), 0x3c4))
        );
    }

    #[test]
    fn graffity_z3_is_a_single_extended_size_zorro_iii_window() {
        let mut chain = chain_with(vec![BoardSpec::graffity_z3(5)]);

        // 16 MB needs the extended Zorro III size encoding: er_Type carries
        // no size bits of its own (code 0) and er_Flags sets both the
        // Zorro III and extended-size bits.
        assert_eq!(chain.config_logical_byte(0, 0), Some(ERT_ZORROIII));
        assert_eq!(chain.config_logical_byte(0, 1), Some(GRAFFITY_Z3_PRODUCT));
        assert_eq!(
            chain.config_logical_byte(0, 2),
            Some(ERFF_ZORRO_III | ERFF_EXTENDED)
        );
        assert_eq!(chain.config_logical_byte(0, 4), Some(0x08));
        assert_eq!(chain.config_logical_byte(0, 5), Some(0x2c));

        chain.config_write(AUTOCONFIG_BASE + EC_Z3_BASEADDRESS_PHYS, 2, 0x4000);
        assert_eq!(
            chain.device_region_at(0x4080_03c4, 2),
            Some((BoardBacking::Device(5), 0x0080_03c4))
        );
        assert_eq!(
            chain.device_region_at(0x4000_0000 + 0x00c0_0002, 4),
            Some((BoardBacking::Device(5), 0x00c0_0002))
        );
    }

    #[test]
    fn lide_identities_match_each_personality() {
        use crate::ide_zorro::LidePersonality;

        let ripple = BoardSpec::lide(LidePersonality::Ripple, 0, true);
        assert_eq!(ripple.manufacturer, 0x144A);
        assert_eq!(ripple.product, 7);
        assert_eq!(ripple.serial, 0);
        assert_eq!(ripple.size_bytes, 0x2_0000);
        assert_eq!(ripple.diag_vec, Some(0x0008));

        let ride = BoardSpec::lide(LidePersonality::Ride, 0, true);
        assert_eq!(ride.manufacturer, 0x144A);
        assert_eq!(ride.product, 9);
        assert_eq!(ride.serial, 1);
        assert_eq!(ride.size_bytes, 0x2_0000);
        assert_eq!(ride.diag_vec, Some(0x0008));

        let atbus = BoardSpec::lide(LidePersonality::AtBus2008, 0, true);
        assert_eq!(atbus.manufacturer, 0x082C);
        assert_eq!(atbus.product, 6);
        assert_eq!(atbus.size_bytes, 0x1_0000);
        assert_eq!(atbus.diag_vec, Some(0x0001));

        // Hardware-only mode (no ROM configured): no DiagArea, no autoboot.
        let no_rom = BoardSpec::lide(LidePersonality::Ripple, 0, false);
        assert_eq!(no_rom.diag_vec, None);
    }

    #[test]
    fn lide_ripple_autoconfig_rom_is_nibble_encoded() {
        use crate::ide_zorro::LidePersonality;

        let chain = chain_with(vec![BoardSpec::lide(LidePersonality::Ripple, 0, true)]);
        // er_Type = Zorro II | DIAGVALID | 128K size code (2): 0xC0|0x10|0x02 = 0xD2.
        assert_eq!(chain.config_read(AUTOCONFIG_BASE, 1), 0xD0);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 2, 1), 0x20);
        // er_Product 7, inverted: 0xF8.
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 4, 1), 0xF0);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 6, 1), 0x80);
        // er_Manufacturer 0x144A, inverted: 0xEB 0xB5.
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 0x10, 1), 0xE0);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 0x12, 1), 0xB0);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 0x14, 1), 0xB0);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 0x16, 1), 0x50);
        // er_InitDiagVec 0x0008, inverted: FF F7.
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 0x28, 1), 0xF0);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 0x2A, 1), 0xF0);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 0x2C, 1), 0xF0);
        assert_eq!(chain.config_read(AUTOCONFIG_BASE + 0x2E, 1), 0x70);
    }

    #[test]
    fn toccata_autoconfig_identity_matches_the_reference() {
        let chain = chain_with(vec![BoardSpec::toccata(0)]);
        // Zorro II | 64K size code 1, no MEMLIST/chained/DIAGVALID: 0xC0|1 = 0xC1.
        assert_eq!(chain.config_logical_byte(0, 0), Some(0xc1));
        assert_eq!(chain.config_logical_byte(0, 1), Some(12));
        assert_eq!(chain.config_logical_byte(0, 2), Some(0)); // not memory-space
                                                              // Manufacturer 18260 (MacroSystem) = 0x4754, big-endian.
        assert_eq!(chain.config_logical_byte(0, 4), Some(0x47));
        assert_eq!(chain.config_logical_byte(0, 5), Some(0x54));
        // No autoboot ROM: InitDiagVec stays zero, and no DIAGVALID bit
        // above (0xC1 has no 0x10 set).
        assert_eq!(chain.config_logical_byte(0, 10), Some(0));
        assert_eq!(chain.config_logical_byte(0, 11), Some(0));
    }

    #[test]
    #[cfg(feature = "mhi")]
    fn mhi_autoconfig_identity_matches_the_spec() {
        let chain = chain_with(vec![BoardSpec::mhi(0)]);
        // Zorro II | 64K size code 1, no MEMLIST/chained/DIAGVALID: 0xC0|1 = 0xC1.
        assert_eq!(chain.config_logical_byte(0, 0), Some(0xc1));
        assert_eq!(chain.config_logical_byte(0, 1), Some(7)); // product 7
        assert_eq!(chain.config_logical_byte(0, 2), Some(0)); // not memory-space
                                                              // Manufacturer 5192 (0x1448), big-endian.
        assert_eq!(chain.config_logical_byte(0, 4), Some(0x14));
        assert_eq!(chain.config_logical_byte(0, 5), Some(0x48));
        // No autoboot ROM.
        assert_eq!(chain.config_logical_byte(0, 10), Some(0));
        assert_eq!(chain.config_logical_byte(0, 11), Some(0));
    }

    #[test]
    fn window_tagged_apertures_must_fit_below_the_tag_bits() {
        let mut spec = BoardSpec::picasso2_vram(0, 2 * 1024 * 1024);
        spec.version = ZorroVersion::III;
        spec.size_bytes = 2 << DEVICE_WINDOW_SHIFT;
        let err = ZorroChain::default().add_board(spec).unwrap_err();
        assert!(
            err.to_string().contains("window-tagged aperture"),
            "{err:#}"
        );
    }
}
