// SPDX-License-Identifier: GPL-3.0-or-later

//! Loadable configuration. The file format is TOML; see
//! `copperline.example.toml` (or the README) for the full schema.

use crate::bus::PortDevice;
use crate::chipset::agnus::{AgnusRevision, VideoStandard};
use crate::chipset::denise::DeniseRevision;
use crate::zorro::{zorro_ii_size_code, zorro_iii_size_bits, BoardSpec, ZorroChain, ZorroVersion};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Skip-serializing predicate for the raw config's nested `[section]` structs:
/// a section that still equals its default carries no user-set field, so it is
/// omitted from saved TOML entirely (keeping written files minimal, like the
/// hand-written `*.example.toml`). Referenced from `#[serde(skip_serializing_if
/// = "is_default")]` on each section field.
fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// Sentinel `rom_path` meaning "the user named no ROM": boot the bundled
/// AROS open-source Kickstart replacement if it can be found, otherwise fail
/// with a message telling the user to supply a Kickstart. A real path (from
/// `rom = "..."` or the CLI argument) always replaces it.
pub const BUNDLED_AROS_ROM: &str = "<bundled-aros>";

/// Sentinel `[scsi] rom` for an A4091 fitted without a named ROM: resolve to
/// the bundled A4091 ROM, or fail. A real `rom = "..."` replaces it.
pub const BUNDLED_A4091_ROM: &str = "<bundled-a4091>";

/// A WASM plugin Zorro board resolved from config: its autoconfig identity
/// (`spec`, with a placeholder device slot reassigned at build time), the
/// `.wasm` module path, and the plugin manifest (name + capabilities).
#[derive(Debug, Clone)]
pub struct WasmBoardConfig {
    pub spec: BoardSpec,
    pub wasm_path: PathBuf,
    pub manifest: crate::wasm_manifest::WasmManifest,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub rom_path: PathBuf,
    pub cpu: CpuModel,
    pub fpu: bool,
    /// CPU clock in MHz. Defaults to the model's stock speed
    /// ([`CpuModel::default_clock_mhz`]), or the machine profile's pinned
    /// clock (the A1200/CD32's authentic 14.18 MHz) when the profile names
    /// one; overridable via `[cpu] clock_mhz`.
    pub cpu_clock_mhz: f64,
    /// Model the 68020/030 on-chip instruction cache (CACR-controlled).
    /// Defaults on for the parts that have one (68EC020/68020/68030), as on
    /// real silicon; `[cpu] icache = false` opts a 020/030 back out.
    pub cpu_icache: bool,
    /// Model the 68030 on-chip data cache (CACR-controlled). Defaults on for
    /// the 68030. Only caches expansion RAM and ROM (chip/slow RAM get
    /// cache inhibit, as on real Amigas, because DMA writes them).
    pub cpu_dcache: bool,
    /// 68060 unimplemented-instruction policy (faithful traps by default).
    pub cpu_unimplemented: UnimplementedPolicy,
    /// Fast CPU execution through the m68k core's batch/trace-JIT path
    /// (`[cpu] jit`). Trades the cycle-exact CPU timing model for host
    /// speed: instructions retire in batches at an approximate cost, so
    /// the machine behaves like one with an accelerator card fitted.
    /// Defaults off.
    pub cpu_jit: bool,
    pub emulation: Emulation,
    pub chip_ram_bytes: usize,
    pub fast_ram_bytes: usize,
    pub slow_ram_bytes: usize,
    /// Ramsey-controlled motherboard fast RAM (`[memory] motherboard`):
    /// 32-bit local RAM ending at $08000000 and growing downward, sized by
    /// Kickstart's own probe rather than autoconfig. Needs a Ramsey
    /// ([`MemController`]) and a CPU with a 32-bit address bus. The
    /// A3000/A4000 profiles fit 4 MiB by default. Ramsey's four banks stop
    /// at 16 MiB; larger totals (up to 64 MiB, Ramsey-07/A4000 only) fill
    /// the $04000000-$06FFFFFF motherboard RAM expansion space below them.
    pub mb_ram_bytes: usize,
    /// CPU-slot (accelerator) fast RAM (`[memory] accelerator`): 32-bit
    /// local RAM in the big-box coprocessor-slot space, starting at
    /// $08000000 and growing upward (up to 128 MiB, ending where Zorro III
    /// space begins), sized by Kickstart's own probe rather than autoconfig.
    /// Needs a CPU with a 32-bit address bus.
    pub accel_ram_bytes: usize,
    /// Zorro III autoconfig RAM (`[memory] z3`). Needs a CPU with a 32-bit
    /// address bus (68020/030/040; not the 24-bit 68000/68EC020).
    pub z3_ram_bytes: usize,
    /// Extra Zorro RAM boards loaded from `[[zorro]]` metadata files, in
    /// autoconfig chain order after the built-in RAM boards.
    pub zorro_boards: Vec<BoardSpec>,
    /// WASM plugin boards loaded from `[[zorro]]` metadata files. Instantiated
    /// and attached to the bus at machine-build time (their device-slot index
    /// is assigned then); kept separate from RAM boards because they carry a
    /// module and capabilities, not just an autoconfig identity.
    pub wasm_boards: Vec<WasmBoardConfig>,
    /// Advertise the Copperline identification board on the Zorro autoconfig
    /// chain (manufacturer 5192 / product 2) so guest software such as
    /// identify.library can detect the emulator. Defaults to true; set
    /// `identify = false` for a chain with no emulator-identifying board.
    pub identify_board: bool,
    /// `[[filesys]]` host directories exported to the guest as
    /// AmigaDOS devices `HOSTFS0:`, `HOSTFS1:`, ... (experimental). Empty:
    /// no services board on the autoconfig chain.
    pub filesys: Vec<crate::filesys::MountSpec>,
    pub chipset: Chipset,
    /// Concrete chip revisions derived from the `[chipset] revision` preset,
    /// installed chip RAM, and the optional `agnus`/`denise` overrides.
    pub agnus_revision: AgnusRevision,
    pub denise_revision: DeniseRevision,
    /// Selected machine profile, if a `[machine]` section was given.
    pub machine: Option<MachineModel>,
    pub gate_array: GateArray,
    /// Memory controller fitted: a Ramsey on the big-box machines.
    pub mem_controller: MemController,
    /// Log every CPU access that no device decodes, within this address range.
    /// Set by `[debug] log_unmapped`. Off by default: on a booting machine the
    /// ROM probes enough empty space to make this a firehose, so it is meant
    /// to be pointed at one window (e.g. the A4000 IDE at $DD2020).
    pub log_unmapped: Option<std::ops::RangeInclusive<u32>>,
    /// Arm the custom-register access validator and last-writer table.
    /// Set by `[debug] validate_chipset`. Off by default: it is a
    /// diagnostic, and an unarmed machine pays nothing for it.
    pub validate_chipset: bool,
    /// Report self-modifying writes. Set by `[debug] detect_smc`.
    pub detect_smc: bool,
    /// A4000 motherboard IDE fitted (A4000 profile): the ATA task file at
    /// $DD2020, driven by Kickstart's own scsi.device. Takes its drives from
    /// `[ide]`, like Gayle's.
    pub ide_a4000: bool,
    /// Super DMAC fitted (A3000 profile): the SCSI DMA controller at $DD0000
    /// and the WD33C93 behind it. Kickstart hangs outright if nothing answers
    /// there. Drives go on its bus through `[scsi] controller = "a3000"`.
    pub sdmac: bool,
    /// Keep the ROM's scsi.device from initialising. Defaults to set when
    /// the machine's built-in disk controller (Gayle or A4000 IDE, A3000
    /// SDMAC SCSI) has no drives configured: the driver would only cost boot
    /// time probing an empty bus. With drives configured the default is
    /// false and the driver runs -- scsi.device is their boot path. Set by
    /// `[machine] rom_scsi_device_disable`; see [`crate::romtags`].
    pub rom_scsi_device_disable: bool,
    /// Akiko gate array fitted (CD32 profile): ID + C2P port at $B80000.
    pub akiko: bool,
    /// CDTV DMAC/CD controller fitted (CDTV profile): a Zorro II
    /// autoconfig board carrying the 6525 TPI and the Matshita drive.
    pub cdtv_cd: bool,
    /// Extended ROM image (`extended_rom = "path"`): 512 KiB maps at
    /// $E00000 (CD32), 256 KiB at $F00000 (CDTV).
    pub extended_rom_path: Option<PathBuf>,
    /// CD image (`[cd] image = "disc.cue"`), mounted on the machine's CD
    /// controller (CD32 Akiko or CDTV DMAC).
    pub cd_image_path: Option<PathBuf>,
    /// Emulated seconds after power-on at which the CD is inserted
    /// (0 = present at boot). Some CDTV discs need a post-boot insert.
    pub cd_insert_delay_secs: f64,
    /// CD32 NVRAM backing file (None = session-only EEPROM).
    pub cd32_nvram_path: Option<PathBuf>,
    /// Whether the battery RTC at $DC0000 is fitted. Defaults to false:
    /// the base A500/A500OCS, A600, A1200, A1000, and CD32 shipped without a
    /// battery-backed clock. Only the A500+ (soldered on the Rev 8A board),
    /// the CDTV, and the big-box A3000/A4000 carry one by default; the A600HD
    /// and a clock-equipped A1200 set `[machine] rtc = true`.
    pub rtc_present: bool,
    /// Which clock part answers there (`[machine] rtc_chip`): the OKI
    /// MSM6242 on most boards, the Ricoh RP5C01 on the A3000/A4000 -- a
    /// different register protocol, which battclock.resource probes for
    /// but Linux/m68k assumes from the machine model. Defaults per
    /// profile; setting it implies `rtc = true`.
    pub rtc_chip: crate::rtc::RtcChip,
    /// Power-on RTC value in Unix seconds (`[machine] rtc_time` /
    /// `--rtc-time`). When set, the clock starts here and ticks with
    /// *emulated* time instead of following the host wall clock, so the
    /// guest-visible time is deterministic and reproducible. Setting a time
    /// implies fitting the clock (`rtc = true`).
    pub rtc_seed_unix: Option<u64>,
    /// Stop the seeded RTC (`[machine] rtc_frozen`): every read returns
    /// exactly `rtc_seed_unix`, for pinning a guest to one time window.
    pub rtc_frozen: bool,
    /// RP5C01 battery-RAM (battmem) backing file (`[machine] battmem`),
    /// in the WinUAE/Amiberry `.nvram` layout; `None` keeps the battery
    /// registers session-only. Defaults to `battmem.nvram` when an
    /// RP5C01 is fitted.
    pub battmem_path: Option<PathBuf>,
    pub video_standard: VideoStandard,
    pub audio: AudioConfig,
    /// Gayle IDE drive images (raw flat HDF, RDB inside), opened
    /// read/write. Only valid on machines with a Gayle gate array.
    pub ide: IdeConfig,
    /// SCSI controller (`[scsi]`): the `controller` selects an A2091 (Zorro II),
    /// an A4091 (Zorro III), or the A3000's motherboard SCSI, plus up to seven
    /// drive images on SCSI IDs 0-6. The Zorro boards autoconfig on the chain
    /// and carry their own boot ROM and scsi.device; the A3000's does not.
    pub scsi: ScsiConfig,
    /// A2065 Ethernet board (`[a2065]`): when set, an A2065 NIC autoconfigs on
    /// the Zorro chain using the named host network backend. Networking is
    /// non-deterministic, so a fitted A2065 breaks byte-identical replay.
    pub a2065_net: Option<crate::net::NetConfig>,
    /// HostSocket board backend (`[hostsocket] net`): when set, the bundled
    /// bsdsocket.library plugin board is fitted with this backend. The board
    /// itself travels in [`Config::wasm_boards`]; this field records the
    /// resolved backend for surfaces (the launcher) that need to read it
    /// without digging the bundled entry back out of that list.
    pub hostsocket_net: Option<crate::net::NetConfig>,
    /// RTG graphics card (`[rtg] card`): when set, the card autoconfigs on
    /// the Zorro chain and presents RTG screens (all pixel formats, core
    /// blitter ops, hardware mouse sprite) to its Picasso96 driver.
    pub rtg: RtgCard,
    /// Picasso II/II+ display memory. Ignored by other RTG cards.
    pub rtg_vram_bytes: usize,
    pub floppy: FloppyConfig,
    /// Which floppy drive slots are electrically present. DF0 is the
    /// internal drive and is always present; DF1-DF3 are external drives
    /// that answer the standard Amiga external-drive ID protocol when true.
    pub floppy_connected: [bool; 4],
    /// Per-drive disk-swap playlists. Entry `i` is the ordered list of
    /// image paths configured for `dfI` (via `path`/`paths` in TOML); the
    /// first entry is the boot disk. A list with two or more entries lets
    /// the user cycle disks in that drive with the disk-swap key, so a
    /// multi-disk demo runs on a single drive. Empty for unused drives.
    pub floppy_playlists: [Vec<PathBuf>; 4],
    /// Presentation-level overscan handling for the window and
    /// screenshots (the emulated framebuffer always carries the full
    /// overscan field). See [`Overscan`].
    pub overscan: Overscan,
    /// Presentation pixel aspect: how emulated scanlines map to host
    /// rows in the window and in screenshots. See [`PixelAspect`].
    pub pixel_aspect: PixelAspect,
    /// How the presentation canvas is scaled into the window: aspect-fit
    /// with filtering, or whole-number multiples only. See
    /// [`DisplayScaling`]. Orthogonal to `pixel_aspect`, which decides what
    /// the canvas itself is.
    pub scaling: DisplayScaling,
    /// Motion-adaptive deinterlacing of LACE content (on by default).
    /// Off, every field is plain line-doubled as it arrives, which shows
    /// interlace bob/flicker like a real TV without persistence.
    pub deinterlace: bool,
    /// CRT phosphor persistence: the fraction of the previous presented
    /// frame each new frame keeps (0.0 = off). Approximates the phosphor
    /// decay that fuses field-rate dither and interlace flicker on a
    /// real CRT.
    pub phosphor: f32,
    /// GPU shader pass applied to the window image. See [`ShaderMode`].
    pub shader: ShaderMode,
    /// How strongly the shader pass is mixed in, 0.0 (invisible) to 1.0
    /// (the preset's full effect, the default). A single knob for every
    /// preset so the effect can be dialled back without editing shaders.
    pub shader_strength: f32,
    /// Draw a monitor-style front bezel around the window picture
    /// (`[display] bezel`): the display shrinks into the rounded opening of
    /// a procedural plastic frame in the spirit of the 1084 the Amiga
    /// shipped with. Independent of `shader`, and a presentation stage like
    /// it: screenshots, frame dumps, recordings and headless runs never
    /// include the bezel.
    pub bezel: bool,
    /// Show the performance overlay at start (`[display] perf_overlay`, or
    /// `--perf-overlay`): a live emulation-performance readout in the
    /// top-right of the display. The `Cmd+P` / `Alt+P` toggle flips it live
    /// without affecting this start-up value.
    pub perf_overlay: bool,
    /// Screen tint applied to the window image: the phosphor colour of a
    /// monochrome monitor, or a sepia treatment. See [`Tint`].
    pub tint: Tint,
    /// How large the pop-up menu is drawn (`[display] menu_scale`).
    pub menu_scale: MenuScale,
    /// Open the window in fullscreen at start (`[display] full_screen`, or
    /// `--full-screen` / `--windowed`). The `Cmd+F` / `Alt+F` toggle flips it
    /// live without affecting this start-up value.
    pub full_screen: bool,
    /// Show the status bar at start (`[display] status_bar`, or
    /// `--show-status-bar` / `--hide-status-bar`). `Cmd+Shift+F` /
    /// `Alt+Shift+F` toggles it live.
    pub status_bar: bool,
    /// Initial host input source for the emulated joystick/CD32-pad port
    /// (`[input] joystick` / `--joystick`). Defaults to
    /// [`JoystickInputMode::Gamepad`]; the runtime status-bar toggle, `Cmd+J` /
    /// `Alt+J`, and the menu's Joystick Input item flip it live without
    /// affecting this start-up value.
    pub joystick_input_mode: JoystickInputMode,
    /// Host mouse sensitivity, 0-100 (`[input] mouse_sensitivity` /
    /// `--mouse-sensitivity`). 50 (default) is 1:1 with the host mouse; 0 is a
    /// quarter speed and 100 quadruple, on an exponential scale. A host-input
    /// scale only -- it does not affect the emulated machine or scripted mouse
    /// input.
    pub mouse_sensitivity: u8,
    /// When the host mouse is grabbed (`[input] mouse_capture` /
    /// `--mouse-capture`). Defaults to [`MouseCapture::Click`], the
    /// historical click-the-display behaviour; `auto` grabs on focus, which
    /// suits a fullscreen session where no host cursor is wanted.
    pub mouse_capture: MouseCapture,
    /// `[input] autofire_hz`: how fast a held fire button is pulsed, or 0 for
    /// off (the default). A host input convenience, not machine state -- the
    /// emulated port sees an ordinary button being pressed and released.
    pub autofire_hz: u8,
    /// Controller devices plugged into the two game ports at power-on
    /// (`[input] port1` / `port2`, `--port1` / `--port2`); index 0 = port 1.
    /// Defaults to a mouse in port 1 and a joystick in port 2 -- a CD32
    /// joypad on the CD32 profile, whose serial button protocol
    /// lowlevel.library expects. Runtime hot-plug (menu, CCP
    /// `input.set_port`) changes the live machine without affecting this
    /// start-up value.
    pub port_devices: [PortDevice; 2],
    /// Host wiring for Paula's serial port (`[serial]` / `--serial`).
    /// Defaults to [`SerialMode::Stdout`], preserving the historical
    /// terminal-diagnostics behaviour.
    pub serial: SerialConfig,
    /// The peripheral on the Centronics parallel port (printer capture or audio
    /// sampler) and its settings. [`ParallelDevice::None`] leaves the port
    /// electrically disconnected, so CIA-A strobes receive no FLAG acknowledge
    /// and port-B reads see the CIA's own pin state.
    pub parallel: ParallelConfig,
}

/// How much of the overscan field the window presents. The
/// `COPPERLINE_OVERSCAN` env var (full/tv) overrides the config for one
/// run (the image-regression harness pins "full" so its baselines keep
/// the whole field).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Overscan {
    /// Present the full 716x285 overscan field the renderer produces
    /// (everything a real Denise can display).
    Full,
    /// Mask the deep horizontal overscan margins with black, like a CRT
    /// bezel, while preserving vertical border colour changes. Demos often
    /// leave junk in the deep horizontal overscan (e.g. HAM streams
    /// converging off-screen); a real TV hides it behind the bezel, and so
    /// does this mode. The default.
    #[default]
    Tv,
}

/// How emulated scanlines map to host rows in the window and in
/// screenshots. The `COPPERLINE_PIXEL_ASPECT` env var (tv/square)
/// overrides the config for one run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PixelAspect {
    /// Present the field with the non-square pixel aspect of a 4:3 CRT:
    /// the full overscan scan maps onto a 4:3 output, so PAL lo-res
    /// pixels come out slightly wider than tall, exactly as a real TV
    /// shows them. The default.
    #[default]
    Tv,
    /// Present with square pixels: one host row per woven scanline, so a
    /// standard lo-res display is an exact 2x2 of its 320-wide bitmap
    /// (e.g. 320x256 PAL occupies precisely 640x512 window pixels).
    /// Slightly taller than a real 4:3 CRT picture, but every pixel is
    /// an integer square, which suits side-by-side pixel comparisons.
    Square,
}

/// How the presentation canvas is scaled into the host window
/// (`[display] scaling`). A window-presentation setting only: the
/// framebuffer, screenshots, frame dumps and recordings are the canvas
/// itself and never see it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DisplayScaling {
    /// Fit the canvas to the window preserving its aspect ratio, with
    /// linear filtering -- the whole window height (or width) is used
    /// whatever the ratio works out to. The default.
    #[default]
    Smooth,
    /// Draw the canvas at the largest whole-number multiple of itself that
    /// fits the window, centred in black borders and point-sampled, so
    /// every canvas pixel is the same square block of host pixels. Falls
    /// back to the smooth fit when the window is too small for even 1x,
    /// which crops rather than shrinks.
    Integer,
}

impl DisplayScaling {
    /// Every mode, in the order a picker offers them.
    pub const MENU_ORDER: [DisplayScaling; 2] = [DisplayScaling::Smooth, DisplayScaling::Integer];

    /// Picker label: the config name of the mode (round-trips through
    /// [`parse_display_scaling`]).
    pub fn label(self) -> &'static str {
        match self {
            DisplayScaling::Smooth => "Smooth",
            DisplayScaling::Integer => "Integer",
        }
    }
}

/// The GPU shader pass the window applies to the presented image. The
/// `COPPERLINE_SHADER` env var overrides the config for one run. A
/// presentation stage only: screenshots, frame dumps, recordings and
/// headless runs never see it, so captures stay comparable whatever is
/// selected here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ShaderMode {
    /// Present the deinterlaced image untouched. The default. Spelled
    /// "none" or "off" in the config.
    #[default]
    None,
    /// Darken between the emulated scan lines -- one dark band per
    /// emulated line whatever the window scale, the line structure a
    /// 15 kHz CRT leaves.
    Scanlines,
    /// Modulate the output through a staggered RGB dot/shadow mask,
    /// like a slot-mask consumer tube.
    Mask,
    /// Scanlines and an aperture-grille phosphor mask together with
    /// tube curvature and a corner vignette: the full CRT look.
    Crt,
    /// A user WGSL fragment shader loaded from this path at start-up.
    Custom(PathBuf),
}

impl ShaderMode {
    /// The mode without its custom path, for callers that only name the
    /// selection (menu labels, status text).
    pub fn kind(&self) -> ShaderKind {
        match self {
            ShaderMode::None => ShaderKind::None,
            ShaderMode::Scanlines => ShaderKind::Scanlines,
            ShaderMode::Mask => ShaderKind::Mask,
            ShaderMode::Crt => ShaderKind::Crt,
            ShaderMode::Custom(_) => ShaderKind::Custom,
        }
    }
}

/// A [`ShaderMode`] stripped of its custom-shader path, so it is `Copy`
/// and can sit in the `Copy` label structs the menu and status bar build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderKind {
    None,
    Scanlines,
    Mask,
    Crt,
    Custom,
}

impl ShaderKind {
    /// Every shader, in the order a picker offers them. `Custom` is last
    /// because it is the one that depends on a file being configured.
    pub const MENU_ORDER: [ShaderKind; 5] = [
        ShaderKind::None,
        ShaderKind::Scanlines,
        ShaderKind::Mask,
        ShaderKind::Crt,
        ShaderKind::Custom,
    ];

    /// Picker label: the config name of the preset (round-trips through
    /// [`parse_shader`], which takes "off" as well as "none"), or
    /// "custom" for a user shader, whose path is too long to name here.
    pub fn label(self) -> &'static str {
        match self {
            ShaderKind::None => "off",
            ShaderKind::Scanlines => "scanlines",
            ShaderKind::Mask => "mask",
            ShaderKind::Crt => "crt",
            ShaderKind::Custom => "custom",
        }
    }

    /// What a picker shows the user, as against the config name [`label`]
    /// round-trips. Both pickers read this, so they cannot drift apart.
    ///
    /// [`label`]: ShaderKind::label
    pub fn menu_label(self) -> &'static str {
        match self {
            ShaderKind::None => "Disabled",
            ShaderKind::Scanlines => "Scanlines",
            ShaderKind::Mask => "Mask",
            // Named for the monitor the preset is modelled on; the path of a
            // user shader is too long for a value column.
            ShaderKind::Crt => "CRT (1084)",
            ShaderKind::Custom => "Custom",
        }
    }
}

/// Screen tint the window applies to the presented chipset display: a
/// monochrome-monitor phosphor look or a sepia treatment, matching the web
/// front-end's screen filter. The `COPPERLINE_TINT` env var overrides the
/// config for one run. A presentation stage only, like [`ShaderMode`]:
/// screenshots, frame dumps, recordings and headless runs are never
/// tinted, so captures stay comparable whatever is selected here. RTG
/// board scanout is presented untinted too: the tint models the monitor
/// on the Amiga's video output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tint {
    /// Full colour, untinted. The default. Spelled "none" or "off" in the
    /// config.
    #[default]
    None,
    /// Black and white: luminance only, like a mono composite feed.
    Bw,
    /// Green phosphor, the classic P1 monochrome monitor look.
    Green,
    /// Amber phosphor, the other common monochrome monitor look.
    Amber,
    /// Sepia-toned monochrome.
    Sepia,
}

/// How large the pop-up menu is drawn. The panel font is a bitmap, so the
/// sizes are whole multiples of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MenuScale {
    /// The font at its own size. The default.
    #[default]
    Normal,
    /// Twice up, for a large display or a distant seat.
    Large,
}

impl MenuScale {
    /// Every size, in the order a picker offers them.
    pub const MENU_ORDER: [MenuScale; 2] = [MenuScale::Normal, MenuScale::Large];

    /// Picker label: the config name of the size (round-trips through
    /// [`parse_menu_scale`]).
    pub fn label(self) -> &'static str {
        match self {
            MenuScale::Normal => "1x",
            MenuScale::Large => "2x",
        }
    }

    /// What a picker with room shows: the name and the figure together. The
    /// menu itself shows [`label`] alone, having no width to spare.
    ///
    /// [`label`]: MenuScale::label
    pub fn menu_label(self) -> &'static str {
        match self {
            MenuScale::Normal => "Normal (1x)",
            MenuScale::Large => "Large (2x)",
        }
    }

    /// What every length in the menu is multiplied by.
    pub fn factor(self) -> usize {
        match self {
            MenuScale::Normal => 1,
            MenuScale::Large => 2,
        }
    }
}

impl Tint {
    /// Every tint, in the order a picker offers them.
    pub const MENU_ORDER: [Tint; 5] = [Tint::None, Tint::Bw, Tint::Green, Tint::Amber, Tint::Sepia];

    /// Picker label: the config name of the tint (round-trips through
    /// [`parse_tint`], which takes "off" as well as "none").
    pub fn label(self) -> &'static str {
        match self {
            Tint::None => "off",
            Tint::Bw => "bw",
            Tint::Green => "green",
            Tint::Amber => "amber",
            Tint::Sepia => "sepia",
        }
    }

    /// What a picker shows the user, as against the config name [`label`]
    /// round-trips. "Colour" rather than "Off": it says what the picture
    /// looks like, and it is the web front-end's wording for the same picker.
    ///
    /// [`label`]: Tint::label
    pub fn menu_label(self) -> &'static str {
        match self {
            Tint::None => "Colour",
            Tint::Bw => "Black & white",
            Tint::Green => "Green",
            Tint::Amber => "Amber",
            Tint::Sepia => "Sepia",
        }
    }
}

/// Host input source for the emulated port-2 joystick/CD32 pad. `Gamepad` (the
/// default) uses only a physical pad, so the keyboard passes straight through to
/// the Amiga (and with no pad connected there is simply no port-2 input).
/// `Keyboard` always uses the keyboard-joystick mapping, capturing the arrow
/// keys and fire keys. There are deliberately only these two explicit modes: the
/// status-bar toggle and `Cmd+J` / `Alt+J` flip between them, so the active mode
/// is always visible rather than depending on hidden gamepad-presence state. Set
/// the start-up mode with `[input] joystick` (or `--joystick`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JoystickInputMode {
    #[default]
    Gamepad,
    Keyboard,
}

impl JoystickInputMode {
    /// Flip the two-state toggle (status bar, `Cmd+J`/`Alt+J`, launcher stepper).
    pub fn next(self) -> Self {
        match self {
            Self::Gamepad => Self::Keyboard,
            Self::Keyboard => Self::Gamepad,
        }
    }

    /// Short label for menus, the on-screen flash, and the config string
    /// (round-trips through [`parse_joystick_input_mode`]).
    pub fn label(self) -> &'static str {
        match self {
            Self::Gamepad => "gamepad",
            Self::Keyboard => "keyboard",
        }
    }

    /// What a picker shows the user, as against the config name [`label`]
    /// round-trips.
    ///
    /// [`label`]: JoystickInputMode::label
    pub fn menu_label(self) -> &'static str {
        match self {
            Self::Gamepad => "Gamepad",
            Self::Keyboard => "Keyboard",
        }
    }
}

/// When the host mouse is grabbed: the pointer is confined to the window
/// and the host cursor hidden, so the emulated mouse is the only one on
/// screen (`[input] mouse_capture` / `--mouse-capture`).
///
/// Uncaptured, host cursor motion over the display still drives the
/// emulated mouse; this setting only decides when the grab is taken, not
/// whether motion reaches the machine. `Cmd+G` / `Alt+G` releases and
/// re-takes it by hand in every mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MouseCapture {
    /// Clicking the display takes the grab (the default). That click is a
    /// window action and is not passed to the Amiga.
    #[default]
    Click,
    /// Grab as soon as the window has the focus, and again whenever it
    /// regains it, so there is never a host cursor loose over the display.
    Auto,
    /// Only the shortcut grabs. Clicks on the display go straight to the
    /// Amiga with the host cursor left alone.
    Manual,
}

impl MouseCapture {
    /// Short label for menus and the config string (round-trips through
    /// [`parse_mouse_capture`]).
    pub fn label(self) -> &'static str {
        match self {
            Self::Click => "click",
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

/// Where Paula's serial port is wired on the host (`[serial] mode` /
/// `--serial`). The Amiga serial port is also the MIDI port, so the MIDI
/// backend is one of these modes rather than a separate device.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SerialMode {
    /// Serial output is discarded and there is no serial input.
    Off,
    /// Serial output is written to the host terminal. The historical
    /// default (DiagROM and other tools print diagnostics here), kept as
    /// the default so an unconfigured machine behaves exactly as before.
    #[default]
    Stdout,
    /// Serial in/out is bridged to host MIDI endpoints. Requires a build
    /// with the `midi` feature; without it, resolving this mode is an error.
    Midi,
    /// Serial in/out is bridged to a host TCP port, like UAE's `TCP:`
    /// serial device. With an `AUX:` shell on the Amiga side, a connected
    /// client gets a remote AmigaDOS console.
    Tcp,
    /// Serial in/out dials out to a remote TCP service at startup (the
    /// address in [`SerialConfig::connect`]): a telnet BBS, a `tcpser`
    /// modem bridge, a `socat`-exposed device. The outbound counterpart
    /// of [`Tcp`].
    ///
    /// [`Tcp`]: SerialMode::Tcp
    TcpConnect,
    /// Serial in/out is bridged to a host pseudo-terminal. The emulator
    /// allocates a pty and logs the slave path (`/dev/pts/N`); a terminal
    /// program (`minicom`, `screen`, `cu`) attaches to it. Unix hosts only.
    Pty,
}

impl SerialMode {
    /// Config-string label (round-trips through [`parse_serial_mode`]).
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Stdout => "stdout",
            Self::Midi => "midi",
            Self::Tcp => "tcp",
            Self::TcpConnect => "tcp-connect",
            Self::Pty => "pty",
        }
    }
}

/// Resolved `[serial]` settings. `midi_out`/`midi_in` name the host MIDI
/// endpoints (substring match) and are only consulted when `mode` is
/// [`SerialMode::Midi`]; they are carried through in the other modes so the
/// configuration screen round-trips them unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SerialConfig {
    pub mode: SerialMode,
    pub midi_out: Option<String>,
    pub midi_in: Option<String>,
    /// The MT-32's two ROM images. Not Copperline's to ship, so the user
    /// supplies them; without both, `midi_out = "mt32"` has nothing to fit
    /// and says so.
    pub mt32_control_rom: Option<PathBuf>,
    pub mt32_pcm_rom: Option<PathBuf>,
    /// Show the MT-32's front panel under the status bar (default false).
    pub mt32_panel: bool,
    /// How that panel's display is lit.
    pub mt32_lcd: Mt32Lcd,
    /// TCP listen address for [`SerialMode::Tcp`]; `None` means the
    /// default `127.0.0.1:1234` (the port UAE's `TCP:` serial uses).
    pub listen: Option<String>,
    /// Remote `host:port` for [`SerialMode::TcpConnect`]. Required in that
    /// mode (there is no sensible default host to dial).
    pub connect: Option<String>,
}

/// How the MT-32's front-panel display is lit.
///
/// The engine draws the same twenty characters whichever this is; only the
/// glass and the characters on it change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mt32Lcd {
    /// The unit's own: a dark green backlight under lighter green
    /// characters. The default, since this is an MT-32. Unlit it goes to
    /// bare glass, a shade off the surround around it.
    #[default]
    Mt32,
    /// A Super JV's: deep blue under pale green characters. Unlit the
    /// blue stays, a shade darker, as that panel does.
    SuperJv,
    /// An S Series sampler's: a sky-blue backlight with the characters
    /// almost black on it. Unlit it keeps a paler blue, plainly a lamp
    /// gone out rather than a dark screen.
    SSeries,
    /// Black glass under the green the status bar's track counter uses, as
    /// one of the OLED panels sold to replace a tired original looks.
    Oled,
}

impl Mt32Lcd {
    /// Every style, in the order a picker offers them.
    pub const MENU_ORDER: [Mt32Lcd; 4] = [
        Mt32Lcd::Mt32,
        Mt32Lcd::SuperJv,
        Mt32Lcd::SSeries,
        Mt32Lcd::Oled,
    ];

    /// Config name, which round-trips through [`parse_mt32_lcd`].
    pub fn label(self) -> &'static str {
        match self {
            Mt32Lcd::Oled => "oled",
            Mt32Lcd::Mt32 => "mt32",
            Mt32Lcd::SuperJv => "superjv",
            Mt32Lcd::SSeries => "sseries",
        }
    }

    /// What a picker shows.
    pub fn menu_label(self) -> &'static str {
        match self {
            Mt32Lcd::Oled => "OLED",
            Mt32Lcd::Mt32 => "MT-32",
            Mt32Lcd::SuperJv => "Super JV",
            Mt32Lcd::SSeries => "S Series",
        }
    }
}

/// Parse a `[serial] mt32_lcd` value. The numbers are taken as well as the
/// names, since the styles are as often thought of as first, second, third.
pub(crate) fn parse_mt32_lcd(s: &str) -> Result<Mt32Lcd> {
    match s.trim().to_ascii_lowercase().as_str() {
        "mt32" | "mt-32" | "1" => Ok(Mt32Lcd::Mt32),
        "superjv" | "super-jv" | "2" => Ok(Mt32Lcd::SuperJv),
        "sseries" | "s-series" | "3" => Ok(Mt32Lcd::SSeries),
        "oled" | "4" => Ok(Mt32Lcd::Oled),
        other => bail!(
            "[serial] mt32_lcd must be \"mt32\", \"superjv\", \"sseries\" \
             or \"oled\" (or 1, 2, 3, 4), got \"{other}\""
        ),
    }
}

/// The `[serial] midi_out` value that means the built-in MT-32 rather
/// than a host endpoint. Matched whole and case-insensitively, so a host
/// device whose name merely contains it is still reachable.
pub const MIDI_OUT_MT32: &str = "mt32";

/// Whether a `midi_out` value asks for the built-in MT-32.
pub fn midi_out_is_mt32(midi_out: Option<&str>) -> bool {
    midi_out.is_some_and(|name| name.trim().eq_ignore_ascii_case(MIDI_OUT_MT32))
}

/// Which peripheral is plugged into the Amiga's Centronics parallel port. The
/// port carries one device at a time, chosen by `[parallel] device`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ParallelDevice {
    /// Nothing plugged in (an unplugged cable). The default.
    #[default]
    None,
    /// A Centronics printer whose raw byte stream is captured to a host file
    /// (`[parallel] output`).
    Printer,
    /// An 8-bit audio sampler (digitizer) on the data lines, fed from a host
    /// capture device. Needs a build with the `frontend` feature (cpal).
    Sampler,
}

impl ParallelDevice {
    /// Config-string label (round-trips through [`parse_parallel_device`]).
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Printer => "printer",
            Self::Sampler => "sampler",
        }
    }
}

/// Resolved `[parallel]` settings. `printer_output` is consulted only for
/// [`ParallelDevice::Printer`], and `sampler_input`/`sampler_gain_db` only for
/// [`ParallelDevice::Sampler`]; the inactive fields are carried through so the
/// configuration screen round-trips them unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct ParallelConfig {
    pub device: ParallelDevice,
    /// Raw printer-byte capture path for [`ParallelDevice::Printer`].
    pub printer_output: Option<PathBuf>,
    /// Host capture device for [`ParallelDevice::Sampler`]; `None` = default.
    pub sampler_input: Option<String>,
    /// Sampler input gain in decibels (preamp); 0 dB = unity.
    pub sampler_gain_db: f32,
}

impl Default for ParallelConfig {
    fn default() -> Self {
        Self {
            device: ParallelDevice::None,
            printer_output: None,
            sampler_input: None,
            sampler_gain_db: 0.0,
        }
    }
}

/// A configured hard-drive image: the host path plus an optional volume-name
/// override. The override only changes a host *directory* mounted as an
/// in-memory FFS volume -- it sets the FFS volume label instead of deriving it
/// from the directory name. A raw HDF carries its own label inside the image
/// and ignores the override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveImage {
    pub path: PathBuf,
    pub volume_name: Option<String>,
    /// `de_BootPri` for the partition Copperline synthesizes in front of a
    /// bare hardfile. Only reaches the guest for such images: an HDF that
    /// carries its own RDB keeps the priorities recorded inside it.
    pub boot_pri: i8,
}

/// Priority a synthesized hard-disk partition boots at when the config says
/// nothing, matching what HDToolBox writes for a plain hard-disk partition.
/// Kickstart's own DF0: boot node sits at 5, so a hard disk loses the tie to a
/// bootable floppy unless it is raised.
pub const HARDFILE_DEFAULT_BOOT_PRI: i8 = 0;

/// `de_BootPri` value that mounts a partition without offering it for boot,
/// the same sentinel `[[filesys]] bootpri` uses.
pub const BOOT_PRI_NEVER: i8 = -128;

/// Whether a drive-image path names a CD image (a cue sheet, a bare ISO,
/// or a CHD). On the SCSI bus such an entry attaches a CD-ROM drive
/// instead of a hard disk; the file extension is the format signal,
/// exactly as it is for the hard-drive back ends (HDF vs. directory).
pub fn is_cd_image_path(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        e.eq_ignore_ascii_case("cue")
            || e.eq_ignore_ascii_case("iso")
            || e.eq_ignore_ascii_case("chd")
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdeConfig {
    pub master: Option<DriveImage>,
    pub slave: Option<DriveImage>,
}

/// Which RTG graphics card the `[rtg]` section fits. A machine has at most
/// one: RTG screens come from whichever card the P96 driver finds, so a
/// second board would only compete for the display.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RtgCard {
    /// No RTG board; the chipset drives the display. The default.
    #[default]
    None,
    /// The Z3660 accelerator's FPGA RTG core, driven by the open-source
    /// Z3660.card Picasso96 driver.
    Z3660,
    /// Village Tronic Picasso II: Zorro II, CL-GD5426, 1 or 2 MB VRAM.
    Picasso2,
    /// Village Tronic Picasso II+: Zorro II, CL-GD5428, 1 or 2 MB VRAM,
    /// with vertical blank wired to INT2.
    Picasso2Plus,
}

/// Which SCSI host adapter the `[scsi]` section fits: one of the two Zorro
/// autoconfig boards, which carry their own boot ROM and scsi.device, or the
/// A3000's motherboard SCSI, which has neither (Kickstart's own scsi.device
/// drives it) and is only there on a machine with a Super DMAC.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScsiController {
    /// Commodore A2091/A590: Zorro II, WD33C93. The default.
    #[default]
    A2091,
    /// Commodore A4091: Zorro III, NCR 53C710.
    A4091,
    /// A3000 motherboard SCSI: Super DMAC + WD33C93 at $DD0000. The default on
    /// a machine that has one.
    A3000,
}

impl ScsiController {
    /// Whether the controller is a Zorro board (it autoconfigs and needs a boot
    /// ROM) rather than motherboard silicon.
    pub fn is_zorro_board(self) -> bool {
        !matches!(self, ScsiController::A3000)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScsiConfig {
    /// Which host adapter the section fits (`controller`). Only meaningful
    /// when `enabled()`.
    pub controller: ScsiController,
    /// Boot ROM image. For the A2091's split even/odd EPROM dumps, `rom` is
    /// the even half and `rom_odd` the other; the A4091 has a single ROM.
    pub rom: Option<PathBuf>,
    /// Odd-byte EPROM half for split A2091 dumps.
    pub rom_odd: Option<PathBuf>,
    /// Drive images by SCSI ID (0-6; ID 7 is the controller).
    pub units: [Option<DriveImage>; 7],
}

impl ScsiConfig {
    /// Whether a `[scsi]` section asked for a board at all. A bare
    /// `controller` with no ROM or drives fits nothing -- except an A4091,
    /// which validation gives the bundled ROM, so `rom` is then set.
    pub fn enabled(&self) -> bool {
        self.rom.is_some() || self.units.iter().any(Option::is_some)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Emulation {
    /// Whether the machine starts running (powered on) at launch. When
    /// false, the emulator sits powered off showing a test screen until
    /// the status-bar power button is clicked -- handy for arming video
    /// capture beforehand. The power button cold-boots the machine.
    pub power_on: bool,
    /// How real-mode pacing debits its per-frame instruction budget. See
    /// `PacingBudget`. The `COPPERLINE_REAL_PACING_BUDGET` env var overrides
    /// this for one run.
    pub pacing_budget: PacingBudget,
    /// Ask the OS to schedule the latency-critical threads (the wall-clock
    /// pacer and the audio callback) above normal, to reduce stutter and audio
    /// glitches under host load. Best effort and off by default; see
    /// [`crate::priority`]. The `COPPERLINE_REALTIME_PRIORITY` env var
    /// overrides this for one run.
    pub realtime_priority: bool,
    /// How fast the UI "Warp Speed" (turbo) mode runs when engaged, expressed
    /// as an output frame-skip level. See [`WarpSpeed`]. Adjustable at runtime
    /// from the Emulator menu and the keyboard.
    pub warp_speed: WarpSpeed,
    /// Record rewind history from power-on, so the rewind hotkey and menu item
    /// work without opening the debugger. Off by default: capturing costs a
    /// whole-machine serialize every `rewind_interval_frames` and the retained
    /// snapshots cost `rewind_budget_mb` of host memory.
    pub rewind: bool,
    /// Host-memory cap on the retained rewind snapshots. Oldest snapshots are
    /// evicted first, so this sets how far back rewind can reach: how much
    /// emulated time that buys depends on the machine's RAM size.
    pub rewind_budget_mb: usize,
    /// Emulated frames between rewind snapshots, and therefore the granularity
    /// of one rewind step. Larger is cheaper but coarser.
    pub rewind_interval_frames: u64,
}

/// Default rewind snapshot budget in MiB when `[emulation] rewind` is on.
pub const REWIND_DEFAULT_BUDGET_MB: usize = 256;
/// Default emulated frames between rewind snapshots: half a second of PAL,
/// which is a comfortable step size for a rewind hotkey.
pub const REWIND_DEFAULT_INTERVAL_FRAMES: u64 = 25;

// ---------------------------------------------------------------------------
// Autofire
//
// The `[input] autofire_hz` policy: how a held fire button is turned into a
// pulse train. It lives here rather than with the keyboard bindings in
// `keymap` because it is not a host-key concern -- the phase is a function of
// emulated time alone, and every input source (gamepad, keyboard, and any
// future one) is gated through it.
// ---------------------------------------------------------------------------

/// Autofire rates offered by the menu, in Hz. 0 is off.
pub const AUTOFIRE_RATES: [u8; 6] = [0, 3, 5, 8, 12, 16];

/// Fastest configurable autofire. Above roughly this the assert window is
/// shorter than the video frame the guest samples the port on, so the button
/// reads as noise rather than as a fast tap.
pub const AUTOFIRE_MAX_HZ: u8 = 30;

/// Label for an autofire rate.
pub fn autofire_label(hz: u8) -> String {
    if hz == 0 {
        "off".to_string()
    } else {
        format!("{hz} Hz")
    }
}

/// Whether a held fire button should be *asserted* right now, given the
/// autofire rate and how much emulated time has passed.
///
/// The phase is taken from emulated seconds rather than host frames, so the
/// rate is the same under warp, on a paced run, and on PAL or NTSC -- an
/// autofire that sped up in warp would be a different game.
pub fn autofire_asserted(hz: u8, emulated_seconds: f64) -> bool {
    if hz == 0 {
        return true; // Off: the button is simply held.
    }
    // One full press+release per 1/hz second: assert on the first half.
    let half_periods = emulated_seconds * f64::from(hz) * 2.0;
    (half_periods as i64).rem_euclid(2) == 0
}

/// Real-mode pacing budget model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacingBudget {
    /// Debit the budget by each instruction's actual returned m68k cycle
    /// count plus the chip-bus waits it incurred, clocking the CPU at its
    /// true cycles-per-instruction. The m68k core's 68000 cycle counts are
    /// validated against the SingleStepTests corpus, so this is the correct
    /// hardware-rate model and the default. (A separate
    /// blitter/raster-sync timing issue can make some area fills flicker
    /// under cycle pacing; tracked independently.)
    Cycles,
    /// Debit a flat `COPPERLINE_REAL_CPU_CPI` (default 4.0) cycles per retired
    /// instruction, regardless of the instruction's real cost. Cheaper and
    /// pacing-robust, but runs the CPU faster than hardware for instruction
    /// mixes that average more than the assumed flat cost. Opt in via
    /// `pacing_budget = "instructions"` or `COPPERLINE_REAL_PACING_BUDGET=instructions`.
    Instructions,
}

/// Hard upper bound on emulated frames per presented frame in `WarpSpeed::Max`,
/// so a host that emulates faster than it presents cannot spin the event loop
/// arbitrarily long between input polls. `Max` is normally bounded first by its
/// wall-clock budget (see `WarpSpeed::time_budget_ms`); this cap only matters
/// when the host is fast enough to retire this many frames inside that budget.
pub const WARP_MAX_FRAME_CAP: usize = 1024;

/// Wall-clock budget (milliseconds) for one presented frame in `WarpSpeed::Max`.
/// The event loop emulates frames back-to-back until this much host time has
/// elapsed, then presents one frame at vsync. Kept under a 60 Hz refresh
/// interval (16.6 ms) so input is still polled and a frame still presented every
/// host refresh while the core runs flat out.
pub const WARP_MAX_BUDGET_MS: u64 = 12;

/// How fast the UI "Warp Speed" (turbo) mode runs when engaged.
///
/// Presentation is gated to the host monitor's refresh rate (the wgpu surface
/// presents with vsync), so emulating exactly one frame per presented frame
/// caps warp at the monitor rate -- about 1.2x for a 50 Hz PAL machine on a
/// 60 Hz display. To decouple emulation speed from the monitor, warp emulates
/// several frames per *presented* frame (output frame skip): the intermediate
/// frames are computed but never rendered or presented, so the effective speed
/// is the level times the refresh rate, host CPU permitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WarpSpeed {
    /// Two emulated frames per presented frame.
    X2,
    /// Four emulated frames per presented frame.
    X4,
    /// Eight emulated frames per presented frame.
    X8,
    /// Sixteen emulated frames per presented frame.
    X16,
    /// As many frames as fit in `WARP_MAX_BUDGET_MS` of host time per presented
    /// frame (bounded by `WARP_MAX_FRAME_CAP`): run flat out, present at vsync.
    #[default]
    Max,
}

impl WarpSpeed {
    /// Every limit, in the order a picker offers them.
    pub const MENU_ORDER: [WarpSpeed; 5] = [
        WarpSpeed::X2,
        WarpSpeed::X4,
        WarpSpeed::X8,
        WarpSpeed::X16,
        WarpSpeed::Max,
    ];

    /// Cycle to the next level for the menu/keyboard "cycle" control:
    /// 2x -> 4x -> 8x -> 16x -> Max -> 2x.
    pub fn next(self) -> Self {
        match self {
            Self::X2 => Self::X4,
            Self::X4 => Self::X8,
            Self::X8 => Self::X16,
            Self::X16 => Self::Max,
            Self::Max => Self::X2,
        }
    }

    /// Short label for menus and the on-screen status flash.
    pub fn label(self) -> &'static str {
        match self {
            Self::X2 => "2x",
            Self::X4 => "4x",
            Self::X8 => "8x",
            Self::X16 => "16x",
            Self::Max => "Max",
        }
    }

    /// Maximum emulated frames to retire per presented frame while warping.
    pub fn frame_cap(self) -> usize {
        match self {
            Self::X2 => 2,
            Self::X4 => 4,
            Self::X8 => 8,
            Self::X16 => 16,
            Self::Max => WARP_MAX_FRAME_CAP,
        }
    }

    /// Wall-clock budget (milliseconds) per presented frame, or `None` for the
    /// fixed levels, which simply retire `frame_cap` frames then present.
    pub fn time_budget_ms(self) -> Option<u64> {
        match self {
            Self::Max => Some(WARP_MAX_BUDGET_MS),
            _ => None,
        }
    }
}

/// How Paula's stereo output is presented to the host. The Amiga hardware pans
/// channels 0/3 hard left and 1/2 hard right; `Mono` averages them into both
/// output channels for listeners who dislike that hard separation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelMode {
    #[default]
    Stereo,
    Mono,
}

impl ChannelMode {
    pub fn label(self) -> &'static str {
        match self {
            ChannelMode::Stereo => "stereo",
            ChannelMode::Mono => "mono",
        }
    }

    pub fn is_mono(self) -> bool {
        matches!(self, ChannelMode::Mono)
    }
}

pub(crate) fn parse_channel_mode(s: &str) -> Result<ChannelMode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "stereo" => Ok(ChannelMode::Stereo),
        "mono" => Ok(ChannelMode::Mono),
        other => bail!("unknown [audio] channel_mode {other:?}; expected \"stereo\" or \"mono\""),
    }
}

/// Control over Paula's analogue low-pass ("power LED") filter. `Auto` lets the
/// guest drive it through CIA-A's /LED line, as real hardware does; `On`/`Off`
/// force it regardless of what the software asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AudioFilterMode {
    #[default]
    Auto,
    On,
    Off,
}

impl AudioFilterMode {
    pub fn label(self) -> &'static str {
        match self {
            AudioFilterMode::Auto => "auto",
            AudioFilterMode::On => "on",
            AudioFilterMode::Off => "off",
        }
    }
}

pub(crate) fn parse_audio_filter_mode(s: &str) -> Result<AudioFilterMode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(AudioFilterMode::Auto),
        "on" | "enabled" | "true" => Ok(AudioFilterMode::On),
        "off" | "disabled" | "false" => Ok(AudioFilterMode::Off),
        other => {
            bail!("unknown [audio] audio_filter {other:?}; expected \"auto\", \"on\", or \"off\"")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioConfig {
    /// Synthesized floppy-drive sound effects: motor hum, head-step
    /// clacks and seek buzz (and the empty-drive change-line poll
    /// click).
    pub floppy_sounds: bool,
    /// Drive sound level, 0-100, relative to Paula's output.
    pub floppy_sounds_volume: u8,
    /// Host audio output device, matched by case-insensitive substring against
    /// the names cpal enumerates. `None` uses the system default output.
    pub output_device: Option<String>,
    /// Whether live audio output is produced at all. `false` runs with a null
    /// sink (no sound), the GUI's "Disabled" picker option; it is separate from
    /// the `--noaudio`/`--audio` CLI flags, which still override it.
    pub output_enabled: bool,
    /// Stereo (hardware panning) or mono (L/R averaged into both channels).
    pub channel_mode: ChannelMode,
    /// Stereo width, 0-100. 100 keeps the hardware left/right panning (default),
    /// 0 collapses to mono; values between narrow the separation.
    pub stereo_separation: u8,
    /// Paula's analogue low-pass filter: guest-driven (`Auto`) or forced.
    pub filter: AudioFilterMode,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            floppy_sounds: true,
            floppy_sounds_volume: 100,
            output_device: None,
            output_enabled: true,
            channel_mode: ChannelMode::Stereo,
            stereo_separation: 100,
            filter: AudioFilterMode::Auto,
        }
    }
}

/// Which FluxBridge driver backs a bridged drive.
///
/// Named rather than indexed so a config file does not depend on the order the
/// installed library happens to enumerate its drivers in; the name is resolved
/// to an index when the drive is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BridgeDriver {
    /// Rob Smith's Arduino-based DrawBridge.
    DrawBridge,
    /// Keir Fraser's Greaseweazle.
    #[default]
    Greaseweazle,
    /// Jim Drew's Supercard Pro.
    SupercardPro,
}

impl BridgeDriver {
    /// The library's own configuration token for this driver, which is what
    /// `fluxbridge::driver_named` resolves.
    pub fn match_token(self) -> &'static str {
        match self {
            BridgeDriver::DrawBridge => "drawbridge",
            BridgeDriver::Greaseweazle => "greaseweazle",
            BridgeDriver::SupercardPro => "supercardpro",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            BridgeDriver::DrawBridge => "DrawBridge",
            BridgeDriver::Greaseweazle => "Greaseweazle",
            BridgeDriver::SupercardPro => "Supercard Pro",
        }
    }
}

/// How hard the driver works to reproduce the disk's real timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BridgeReadMode {
    /// Captures wherever the head happens to be, saving the wait for the index
    /// -- most of a revolution per track -- and serves the capture while the
    /// platter is still turning it, exactly as a real head does. The
    /// revolution's two ends are joined where the recording repeats; a join
    /// the library can prove, or a capture verified as a whole AmigaDOS
    /// track, replays indefinitely, and one it cannot prove is served once
    /// and re-read fresh. Reads the same disks as `Compatible` and reaches a
    /// Workbench desktop appreciably sooner.
    /// The default.
    #[default]
    Normal,
    /// Captures each track from the index, so a revolution begins where the
    /// real one does and its two ends meet in the gap between sectors, exactly
    /// as a captured image's do. Waiting for the index costs most of a
    /// revolution on every track, which is why `Normal` is the default; reach
    /// for this one if a disk reads badly without the index to anchor it.
    Compatible,
    /// As `Compatible`, but the driver holds the caller up until the track is
    /// ready instead of answering "not yet". The wait lands on the emulated
    /// machine, which stops -- pointer and all -- for as long as it takes.
    Stalling,
}

/// Whether to force a density rather than sensing it from the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BridgeDensity {
    #[default]
    Auto,
    Dd,
    Hd,
}

/// Which drive the interface selects on the cable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BridgeCable {
    /// IBM PC cabling, drive A: the usual case for a PC drive on a
    /// Greaseweazle or DrawBridge.
    #[default]
    DriveA,
    DriveB,
    /// Shugart cabling, for a real Amiga drive.
    Shugart0,
    Shugart1,
    Shugart2,
    Shugart3,
}

/// Replay speeds a bridged bay accepts, as percentages of the platter's
/// real speed. Shared by the config parser, the CLI, and the launcher's
/// cycle row so all three offer the same set.
pub const SUPPORTED_BRIDGE_SPEED_PERCENTS: [u16; 2] = [100, 200];

/// The replay speed a bridged bay uses unless told otherwise: fast. A
/// track's first read always arrives at the platter's own pace, so this only
/// decides how re-reads are served -- and waiting out a full rotation to
/// replay bits already in hand helps nobody. `normal` is the opt-in for
/// software that times its own drive.
pub const DEFAULT_BRIDGE_SPEED_PERCENT: u16 = 200;

/// A real drive attached to one floppy bay, from `[floppy.dfN] bridge = ...`.
///
/// Held apart from [`FloppyDriveConfig`] because a bridged drive has no image
/// path: whichever of the two is present for a bay supplies its media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FluxBridgeConfig {
    pub driver: BridgeDriver,
    /// Emulator-level write protection, on top of the disk's own tab.
    /// Defaults to true, exactly as it does for an image, so writing to a real
    /// disk takes a deliberate `write_protected = false` as well as the tab
    /// being open -- two independent things to get wrong before anything is
    /// laid on physical media.
    pub write_protected: bool,
    /// `None` auto-detects the interface, which every current driver supports.
    pub port: Option<String>,
    pub mode: BridgeReadMode,
    pub density: BridgeDensity,
    pub cable: BridgeCable,
    /// How fast replays of already-captured revolutions are served to the
    /// guest, as a percentage of the platter's real speed: `200` ("fast",
    /// the default) or `100` ("normal"). A track's first read always streams
    /// at the platter's own pace; this only compresses the wait when the
    /// guest asks for a track already in hand. As with `[floppy] speed`,
    /// software that times its own loading can notice.
    pub speed: u16,
}

impl Default for FluxBridgeConfig {
    fn default() -> Self {
        Self {
            driver: BridgeDriver::default(),
            // Protected unless told otherwise, matching both the parser and an
            // image-backed drive. A derived `Default` would make this false and
            // quietly hand out a writable real disk.
            write_protected: true,
            port: None,
            mode: BridgeReadMode::default(),
            density: BridgeDensity::default(),
            cable: BridgeCable::default(),
            speed: DEFAULT_BRIDGE_SPEED_PERCENT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloppyConfig {
    pub drives: [Option<FloppyDriveConfig>; 4],
    /// Real drives, by bay. A bay with a bridge here has no entry in
    /// `drives`: the physical disk is its media.
    pub bridges: [Option<FluxBridgeConfig>; 4],
    /// Emulated drive speed as a data-rate percentage: 100 (real speed),
    /// 200/400/800 (that many times faster), or 0 for turbo, where DMA
    /// transfers complete almost instantly. Values above 100 keep the full
    /// bit-level pipeline, only compressed in time; drive mechanics (motor
    /// spin-up, stepping) always run at real speed.
    pub speed: u16,
}

impl Default for FloppyConfig {
    fn default() -> Self {
        Self {
            drives: std::array::from_fn(|_| None),
            bridges: std::array::from_fn(|_| None),
            speed: 100,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloppyDriveConfig {
    pub path: PathBuf,
    pub write_protected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CpuModel {
    M68000,
    M68010,
    M68EC020,
    M68020,
    M68030,
    M68040,
    M68060,
}

impl CpuModel {
    /// Whether the model ships with an FPU by default: the full 68040 has
    /// its floating-point unit on-die (the FPU-less variants are the LC/EC
    /// parts, which Copperline does not model); 68881/68882 boards for the
    /// other CPUs are opt-in via `[cpu] fpu = true`.
    pub fn default_fpu(self) -> bool {
        matches!(self, CpuModel::M68040 | CpuModel::M68060)
    }

    /// Default CPU clock in MHz for this model: a stock 68000/68010 runs at
    /// the PAL system clock (~7.09 MHz, 2x the colour clock); accelerated
    /// parts default to representative speeds (020 ~14 MHz, 030/040 ~25 MHz).
    /// Fast RAM runs at the CPU clock; chip/slow RAM stays chip-bus bound.
    pub fn default_clock_mhz(self) -> f64 {
        match self {
            CpuModel::M68000 | CpuModel::M68010 => 7.09,
            CpuModel::M68EC020 | CpuModel::M68020 => 14.0,
            CpuModel::M68030 | CpuModel::M68040 => 25.0,
            CpuModel::M68060 => 50.0,
        }
    }

    /// Whether this model has the on-chip instruction cache Copperline models.
    /// The 68020/68EC020/68030 ship a 256-byte direct-mapped instruction cache
    /// and the 68040 a 4 KB one; AmigaOS enables it (CACR) at boot. Real
    /// A1200/A4000 software (demos especially) leans on it: code looping out of
    /// chip RAM otherwise contends with bitplane DMA on every fetch and runs
    /// roughly half-speed.
    pub fn has_instruction_cache(self) -> bool {
        matches!(
            self,
            CpuModel::M68EC020
                | CpuModel::M68020
                | CpuModel::M68030
                | CpuModel::M68040
                | CpuModel::M68060
        )
    }

    /// Whether this model has the on-chip data cache Copperline models. The
    /// 68030 (256 bytes) and 68040 (4 KB) have one; the 020 has none.
    pub fn has_data_cache(self) -> bool {
        matches!(self, CpuModel::M68030 | CpuModel::M68040 | CpuModel::M68060)
    }
}

/// What a 68060 does with the instructions dropped from its silicon:
/// faithful traps (the OS-side 68060.library emulates them, as on real
/// accelerator boards) or direct native execution for systems without
/// the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum UnimplementedPolicy {
    #[default]
    Trap,
    Native,
}

/// PAL Amiga colour clock (CCK), in Hz. The 68000 bus advances one slot per
/// CCK; the CPU runs at a whole multiple of it (2x for a stock 68000).
pub const COLOR_CLOCK_HZ: f64 = 3_546_895.0;

/// The CPU clock expressed as a whole multiple of the colour clock, clamped
/// to at least 1. A stock 68000 is 2 (7.09 MHz / 3.55 MHz); 14 MHz -> 4;
/// 25 MHz -> 7. The user can ask for any MHz; the chipset advance and pacing
/// model in whole CCK multiples ("multiples of the bus"), so the effective
/// clock is `clocks_per_cck * COLOR_CLOCK_HZ`.
pub fn clocks_per_cck_for_mhz(clock_mhz: f64) -> u32 {
    ((clock_mhz * 1.0e6) / COLOR_CLOCK_HZ).round().max(1.0) as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Chipset {
    Ocs,
    Ecs,
    Aga,
}

/// `[machine] profile`: a validated bundle of chipset revisions, CPU model and
/// clock, memory sizes, RTC presence, and gate array. Explicit `[cpu]`/
/// `[chipset]`/`[memory]` sections override the profile defaults where
/// compatible; the profile owns what those sections cannot express (Gayle,
/// RTC presence). With no `[machine]` section the defaults match the `A500`
/// profile: the A500 Rev 6A (ECS 8372A Agnus, OCS 8362 Denise, 68000,
/// 512 KiB chip RAM, and 512 KiB trapdoor slow RAM).
///
/// Append new models at the end: savestates carry the discriminant, so
/// inserting one in the middle renames every model below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MachineModel {
    /// A500 Rev 6A: the ECS "Fatter" 8372A Agnus (1 MiB chip reach, software
    /// PAL/NTSC switch) with the original OCS 8362 Denise.
    A500,
    /// Early A500 (Rev 3/5), equivalent to an A2000: the 512 KiB OCS "Fat
    /// Agnus" (8370/8371) with OCS 8362 Denise.
    A500Ocs,
    A500Plus,
    A600,
    A1200,
    /// CDTV: an OCS A500-class machine with 1 MB chip RAM and the 256 KiB
    /// extended ROM at $F00000. Enables the DMAC/CD-ROM controller used by
    /// the CDTV drive.
    Cdtv,
    /// CD32: AGA, 68EC020, 2 MB chip RAM, Akiko at $B80000, and the
    /// 512 KiB extended ROM at $E00000. Enables Akiko and the CD32 CD-ROM
    /// path.
    Cd32,
    /// A1000: the original Amiga. OCS 8361/8367 Agnus + OCS 8362 Denise, and
    /// no Kickstart ROM -- the `rom` is the 64 KiB bootstrap ROM, which loads
    /// Kickstart from the Kickstart disk in DF0 into 256 KiB of writable
    /// control store (WCS) at $FC0000 and then write-protects it. 256 KiB
    /// stock chip RAM, no trapdoor slow RAM, no RTC.
    A1000,
    /// A3000: ECS, 68030 at 25 MHz, 2 MB chip RAM, a Ramsey-04 memory
    /// controller with the stock 4 MB of motherboard fast RAM
    /// (`[memory] motherboard` resizes it), and the battery-backed Ricoh
    /// RP5C01 clock. No Gayle -- the big-box machines carry Gary -- and no
    /// slow RAM.
    A3000,
    /// A4000: the same board a generation later -- AGA, a 25 MHz 68040, and
    /// Ramsey-07 with the same stock 4 MB of motherboard fast RAM. Same
    /// story on Gayle and slow RAM as the A3000.
    A4000,
}

/// Identity of a ROM image: its length and a CRC-32 of its bytes. Enough to
/// tell two Kickstarts apart (a different revision, or a CDTV/CD32 extended
/// ROM) without storing the image itself. The CRC is the standard IEEE
/// polynomial via `flate2::Crc`, so it is stable across builds and platforms.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RomId {
    pub len: usize,
    pub crc32: u32,
}

impl RomId {
    /// Fingerprint a ROM image. The empty slice gives `len 0`, which callers
    /// use to mean "no such ROM".
    pub fn of(bytes: &[u8]) -> Self {
        let mut crc = flate2::Crc::new();
        crc.update(bytes);
        Self {
            len: bytes.len(),
            crc32: crc.sum(),
        }
    }

    /// Compact label for logs/summaries, e.g. "512K:a1b2c3d4".
    pub fn label(&self) -> String {
        format!("{}K:{:08x}", self.len / 1024, self.crc32)
    }
}

/// The "shape" of a machine plus its ROM identity: the values that, taken
/// together, decide what kind of Amiga is running and which Kickstart it runs.
/// Embedded in the save-state header so a load can tell whether the state
/// belongs to a different machine than the running config and reconfigure the
/// host to match it.
///
/// The serialized `Bus`/`CpuCore` already carry the actual hardware (RAM
/// contents, ROM bytes, chip revisions, CPU type), so a state always rebuilds
/// its own machine on load; this descriptor is the compact, human-readable
/// identity used for the comparison and the log message, plus the machine
/// profile (`A500`/`A1200`/...), which is a config-level concept the Bus does
/// not record. The ROM fields fingerprint the boot/extended ROM bytes so a
/// swapped Kickstart of the same machine shape is still flagged.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MachineDescriptor {
    pub cpu: CpuModel,
    pub chip_ram_bytes: usize,
    pub fast_ram_bytes: usize,
    pub slow_ram_bytes: usize,
    /// Ramsey-controlled motherboard fast RAM (A3000/A4000).
    #[serde(default)]
    pub mb_ram_bytes: usize,
    /// CPU-slot (accelerator) fast RAM at $08000000.
    #[serde(default)]
    pub accel_ram_bytes: usize,
    pub chipset: Chipset,
    pub video_standard: VideoStandard,
    pub machine: Option<MachineModel>,
    /// Boot ROM identity (the normalized in-memory image).
    pub rom: RomId,
    /// Extended ROM identity (CDTV $F00000 / CD32 $E00000), `None` when none
    /// is fitted.
    pub extended_rom: Option<RomId>,
}

impl Default for MachineDescriptor {
    /// A stock OCS A500 with no ROM fingerprint yet: the shape of the minimal
    /// machine the headless test fixtures build. Real runs overwrite this from
    /// the loaded `Config` and the in-memory ROM.
    fn default() -> Self {
        Self {
            cpu: CpuModel::M68000,
            chip_ram_bytes: 512 * 1024,
            fast_ram_bytes: 0,
            slow_ram_bytes: 0,
            mb_ram_bytes: 0,
            accel_ram_bytes: 0,
            chipset: Chipset::Ocs,
            video_standard: VideoStandard::Pal,
            machine: None,
            rom: RomId::default(),
            extended_rom: None,
        }
    }
}

impl MachineDescriptor {
    /// Fill the ROM fields from the live in-memory images. `extended_rom` is an
    /// empty slice when no extended ROM is fitted. Called once the machine is
    /// built (the bytes live in the `Bus`, not the `Config`).
    pub fn set_rom_fingerprint(&mut self, rom: &[u8], extended_rom: &[u8]) {
        self.rom = RomId::of(rom);
        self.extended_rom = (!extended_rom.is_empty()).then(|| RomId::of(extended_rom));
    }

    /// One-line human summary, e.g.
    /// "A1200 / 68EC020 / AGA / PAL / chip 2048K fast 0K slow 0K / ROM 512K:a1b2c3d4".
    pub fn summary(&self) -> String {
        let profile = match self.machine {
            Some(m) => format!("{m:?}"),
            None => "custom".to_string(),
        };
        let ext = match &self.extended_rom {
            Some(id) => format!(" +ext {}", id.label()),
            None => String::new(),
        };
        format!(
            "{profile} / {:?} / {:?} / {:?} / chip {}K fast {}K slow {}K mb {}K accel {}K / ROM {}{ext}",
            self.cpu,
            self.chipset,
            self.video_standard,
            self.chip_ram_bytes / 1024,
            self.fast_ram_bytes / 1024,
            self.slow_ram_bytes / 1024,
            self.mb_ram_bytes / 1024,
            self.accel_ram_bytes / 1024,
            self.rom.label(),
        )
    }

    /// Human-readable, field-by-field differences between the running machine
    /// (`self`) and a state's machine (`other`), for the load-time log when
    /// they do not match. Empty when the shapes and ROMs are identical.
    pub fn differences(&self, other: &MachineDescriptor) -> Vec<String> {
        let mut diffs = Vec::new();
        if self.machine != other.machine {
            diffs.push(format!("profile {:?} -> {:?}", self.machine, other.machine));
        }
        if self.cpu != other.cpu {
            diffs.push(format!("cpu {:?} -> {:?}", self.cpu, other.cpu));
        }
        if self.chipset != other.chipset {
            diffs.push(format!("chipset {:?} -> {:?}", self.chipset, other.chipset));
        }
        if self.video_standard != other.video_standard {
            diffs.push(format!(
                "video {:?} -> {:?}",
                self.video_standard, other.video_standard
            ));
        }
        if self.chip_ram_bytes != other.chip_ram_bytes {
            diffs.push(format!(
                "chip RAM {}K -> {}K",
                self.chip_ram_bytes / 1024,
                other.chip_ram_bytes / 1024
            ));
        }
        if self.fast_ram_bytes != other.fast_ram_bytes {
            diffs.push(format!(
                "fast RAM {}K -> {}K",
                self.fast_ram_bytes / 1024,
                other.fast_ram_bytes / 1024
            ));
        }
        if self.slow_ram_bytes != other.slow_ram_bytes {
            diffs.push(format!(
                "slow RAM {}K -> {}K",
                self.slow_ram_bytes / 1024,
                other.slow_ram_bytes / 1024
            ));
        }
        if self.mb_ram_bytes != other.mb_ram_bytes {
            diffs.push(format!(
                "motherboard RAM {}K -> {}K",
                self.mb_ram_bytes / 1024,
                other.mb_ram_bytes / 1024
            ));
        }
        if self.accel_ram_bytes != other.accel_ram_bytes {
            diffs.push(format!(
                "accelerator RAM {}K -> {}K",
                self.accel_ram_bytes / 1024,
                other.accel_ram_bytes / 1024
            ));
        }
        if self.rom != other.rom {
            diffs.push(format!("ROM {} -> {}", self.rom.label(), other.rom.label()));
        }
        if self.extended_rom != other.extended_rom {
            let label = |id: &Option<RomId>| match id {
                Some(id) => id.label(),
                None => "none".to_string(),
            };
            diffs.push(format!(
                "extended ROM {} -> {}",
                label(&self.extended_rom),
                label(&other.extended_rom)
            ));
        }
        diffs
    }
}

/// Which bus gate array the machine carries. A machine has exactly one, and
/// they are not interchangeable parts so much as the same seat on the board:
/// both decode the $DE0000 page, so fitting two would make the decode
/// ambiguous.
///
/// Gayle (the wedge machines) is the bus controller plus IDE, PCMCIA, and the
/// interrupt plumbing at $DA8000-$DAA000, with an ID register at $DE1000. Fat
/// Gary (the big-box machines) is only a bus controller: three flag registers
/// on byte lanes 0-2 of the $DE0000 page, with Ramsey answering on lane 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GateArray {
    #[default]
    None,
    GayleA600,
    GayleA1200,
    /// Fat Gary, as fitted to the A3000 and A4000. Always accompanied by a
    /// Ramsey (see [`MemController`]): they share one address decode.
    FatGary,
}

impl GateArray {
    /// The 8-bit ID shifted out of $DE1000 (MSB first): $D0 on the A600,
    /// $D1 on the A1200. Only Gayle has one.
    pub fn gayle_id(self) -> Option<u8> {
        match self {
            Self::None | Self::FatGary => None,
            Self::GayleA600 => Some(0xD0),
            Self::GayleA1200 => Some(0xD1),
        }
    }

    /// Whether this machine's gate array is a Fat Gary.
    pub fn is_fat_gary(self) -> bool {
        self == Self::FatGary
    }
}

/// Which memory controller the machine carries. The big-box machines put a
/// Ramsey at $DE0000, where the wedge machines put Gayle; the two are mutually
/// exclusive, and everything else has neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemController {
    #[default]
    None,
    /// Ramsey-04, as fitted to the A3000.
    Ramsey4,
    /// Ramsey-07, as fitted to the A4000.
    Ramsey7,
}

impl MemController {
    pub fn ramsey_revision(self) -> Option<crate::ramsey::RamseyRevision> {
        match self {
            Self::None => None,
            Self::Ramsey4 => Some(crate::ramsey::RamseyRevision::Rev4),
            Self::Ramsey7 => Some(crate::ramsey::RamseyRevision::Rev7),
        }
    }
}

const A500_TRAPDOOR_RAM_BYTES: usize = 512 * 1024;

impl Default for Config {
    fn default() -> Self {
        Self {
            rom_path: PathBuf::from(BUNDLED_AROS_ROM),
            cpu: CpuModel::M68000,
            fpu: CpuModel::M68000.default_fpu(),
            cpu_clock_mhz: CpuModel::M68000.default_clock_mhz(),
            cpu_icache: false,
            cpu_dcache: false,
            cpu_unimplemented: UnimplementedPolicy::Trap,
            cpu_jit: false,
            emulation: Emulation {
                power_on: true,
                pacing_budget: PacingBudget::Cycles,
                realtime_priority: false,
                warp_speed: WarpSpeed::default(),
                rewind: false,
                rewind_budget_mb: REWIND_DEFAULT_BUDGET_MB,
                rewind_interval_frames: REWIND_DEFAULT_INTERVAL_FRAMES,
            },
            chip_ram_bytes: 512 * 1024,
            fast_ram_bytes: 0,
            slow_ram_bytes: A500_TRAPDOOR_RAM_BYTES,
            mb_ram_bytes: 0,
            accel_ram_bytes: 0,
            z3_ram_bytes: 0,
            zorro_boards: Vec::new(),
            wasm_boards: Vec::new(),
            identify_board: true,
            filesys: Vec::new(),
            // The no-[machine] default models the most common and most-
            // targeted Amiga: the A500 Rev 6A (the ECS "Fatter" 8372A Agnus
            // with the original OCS 8362 Denise). Selecting `[chipset]
            // revision` or a different `[machine] profile` opts out.
            chipset: Chipset::Ecs,
            agnus_revision: AgnusRevision::Ecs8372Rev4,
            denise_revision: DeniseRevision::Ocs,
            machine: None,
            gate_array: GateArray::None,
            mem_controller: MemController::None,
            rom_scsi_device_disable: false,
            log_unmapped: None,
            validate_chipset: false,
            detect_smc: false,
            ide_a4000: false,
            sdmac: false,
            akiko: false,
            cdtv_cd: false,
            extended_rom_path: None,
            cd_image_path: None,
            cd_insert_delay_secs: 0.0,
            cd32_nvram_path: None,
            // The default machine is the A500 Rev 6A, which had no battery
            // clock; only the A500+/CDTV profiles fit one (see
            // machine_profile_defaults).
            rtc_present: false,
            rtc_chip: crate::rtc::RtcChip::Msm6242,
            rtc_seed_unix: None,
            rtc_frozen: false,
            battmem_path: None,
            video_standard: VideoStandard::Pal,
            audio: AudioConfig::default(),
            ide: IdeConfig::default(),
            scsi: ScsiConfig::default(),
            a2065_net: None,
            hostsocket_net: None,
            rtg: RtgCard::None,
            rtg_vram_bytes: 2 * 1024 * 1024,
            floppy: FloppyConfig::default(),
            floppy_connected: [true, false, false, false],
            floppy_playlists: std::array::from_fn(|_| Vec::new()),
            overscan: Overscan::Tv,
            pixel_aspect: PixelAspect::Tv,
            scaling: DisplayScaling::Smooth,
            deinterlace: true,
            phosphor: 0.0,
            shader: ShaderMode::None,
            shader_strength: 1.0,
            bezel: false,
            perf_overlay: false,
            tint: Tint::None,
            menu_scale: MenuScale::Normal,
            full_screen: false,
            status_bar: true,
            joystick_input_mode: JoystickInputMode::Gamepad,
            mouse_sensitivity: 50,
            mouse_capture: MouseCapture::Click,
            autofire_hz: 0,
            port_devices: [PortDevice::Mouse, PortDevice::Joystick],
            serial: SerialConfig::default(),
            parallel: ParallelConfig::default(),
        }
    }
}

impl Config {
    /// Load a config, applying command-line overrides on top of whatever the
    /// file (or the built-in defaults, when `path` is `None`) provides. The
    /// overrides are injected into the raw TOML view before validation, so
    /// they go through exactly the same profile-defaulting, derivation, and
    /// range-checking as the equivalent config fields would.
    /// The raw TOML view a config is loaded from, with the CLI overrides
    /// already applied but before validation/derivation. `main` validates this
    /// into a [`Config`] to build the machine and also keeps the raw view, so
    /// the configuration screen can reopen showing the running machine's
    /// settings and re-emit them on Save.
    pub fn load_raw(path: Option<&Path>, overrides: &ConfigOverrides) -> Result<RawConfig> {
        let mut raw = match path {
            Some(p) => raw_from_path(p)?,
            None => RawConfig::default(),
        };
        overrides.apply_to(&mut raw);
        Ok(raw)
    }

    /// Apply a CLI ROM-path override on top of whatever the config
    /// produced. None leaves the config's value untouched.
    pub fn with_rom_override(mut self, rom: Option<PathBuf>) -> Self {
        if let Some(p) = rom {
            self.rom_path = p;
        }
        self
    }

    /// The machine "shape" this config describes, stamped into save states so
    /// a load can detect a different machine and reconfigure the host to match.
    /// The ROM fields are left empty here (the `Config` holds only a path); the
    /// caller fills them from the in-memory ROM via
    /// [`MachineDescriptor::set_rom_fingerprint`] once the machine is built.
    pub fn descriptor(&self) -> MachineDescriptor {
        MachineDescriptor {
            cpu: self.cpu,
            chip_ram_bytes: self.chip_ram_bytes,
            fast_ram_bytes: self.fast_ram_bytes,
            slow_ram_bytes: self.slow_ram_bytes,
            mb_ram_bytes: self.mb_ram_bytes,
            accel_ram_bytes: self.accel_ram_bytes,
            chipset: self.chipset,
            video_standard: self.video_standard,
            machine: self.machine,
            rom: RomId::default(),
            extended_rom: None,
        }
    }

    /// Build the Zorro autoconfig chain this config asks for: the built-in
    /// Zorro II fast RAM board, the built-in Zorro III RAM board, any
    /// `[[zorro]]` metadata boards in file order, and finally (unless
    /// `identify = false`) the Copperline identification board. The ID board
    /// comes last so the configured RAM boards keep the autoconfig base
    /// addresses they would get without it.
    pub fn build_zorro_chain(&self) -> Result<ZorroChain> {
        let mut chain = ZorroChain::default();
        if self.fast_ram_bytes > 0 {
            chain.add_board(BoardSpec::fast_ram(self.fast_ram_bytes))?;
        }
        if self.z3_ram_bytes > 0 {
            chain.add_board(BoardSpec::z3_ram(self.z3_ram_bytes))?;
        }
        for board in &self.zorro_boards {
            chain.add_board(board.clone())?;
        }
        if self.identify_board {
            chain.add_board(BoardSpec::copperline_id())?;
        }
        // The Copperline services board itself (`[[filesys]]`) is a
        // functional device, added in emulator.rs where its device slot is
        // assigned (like the A4091); only the config validation lives here.
        if !self.filesys.is_empty() || self.rom_scsi_device_disable {
            if self.filesys.len() > crate::filesys::MOUNT_MAX_COUNT {
                anyhow::bail!(
                    "[[filesys]]: at most {} mounts supported",
                    crate::filesys::MOUNT_MAX_COUNT
                );
            }
            for m in &self.filesys {
                if !m.path.is_dir() {
                    anyhow::bail!("[[filesys]] path {} is not a directory", m.path.display());
                }
                if let Some(err) = crate::filesys::volume_name_error(&m.volume) {
                    anyhow::bail!("[[filesys]] {err}");
                }
            }
        }
        Ok(chain)
    }
}

/// Read and parse a config file into its raw TOML view.
pub(crate) fn raw_from_path(path: &Path) -> Result<RawConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config {}", path.display()))?;
    toml::from_str(&text).map_err(|e| {
        let mut err = anyhow::Error::new(e);
        // A backslash in a double-quoted TOML string is an escape character,
        // so a Windows path like "C:\Kickstarts\KICK31.ROM" fails to parse on
        // "\K". The bare "invalid escape sequence" message rarely makes that
        // connection, so point at the fix.
        if err.to_string().contains("escape") {
            err = err.context(
                "a backslash in a double-quoted string is an escape character; \
                 for Windows paths use single quotes ('C:\\dir\\file'), double \
                 the backslashes, or use forward slashes",
            );
        }
        err.context(format!("parsing config {}", path.display()))
    })
}

/// Command-line overrides for the handful of machine knobs it is convenient
/// to set without writing a config file: the machine model, the chipset
/// preset, the CPU and its FPU/clock, and the chip/fast/slow RAM sizes. Each
/// field is `None` when the corresponding flag was not given, leaving the file
/// (or profile default) value untouched. The string fields carry the same
/// syntax the matching TOML fields accept and are validated by the same
/// parsers.
#[derive(Debug, Default, Clone)]
pub struct ConfigOverrides {
    pub model: Option<String>,
    pub chipset: Option<String>,
    pub cpu: Option<String>,
    pub fpu: Option<bool>,
    pub cpu_clock_mhz: Option<f64>,
    /// Fast CPU execution via the batch/trace-JIT path (`--jit`/`--no-jit`).
    /// Same semantics as `[cpu] jit`.
    pub cpu_jit: Option<bool>,
    pub chip: Option<String>,
    pub fast: Option<String>,
    pub slow: Option<String>,
    /// Ramsey motherboard fast RAM size (`--motherboard`). Same parser as
    /// `[memory] motherboard`.
    pub motherboard: Option<String>,
    /// CPU-slot accelerator fast RAM size (`--accelerator`). Same parser as
    /// `[memory] accelerator`.
    pub accelerator: Option<String>,
    pub floppy_drives: Option<u8>,
    /// Drive speed override (`--floppy-speed`): a percentage (100/200/400/
    /// 800) or 0 for turbo. Same values as `[floppy] speed`.
    pub floppy_speed: Option<u16>,
    /// Initial joystick input mode (`--joystick`): "gamepad" or "keyboard"
    /// ("auto" still accepted as a compatibility alias). Validated by the same
    /// parser as `[input] joystick`.
    pub joystick: Option<String>,
    /// Host mouse sensitivity (`--mouse-sensitivity`), 0-100. Same as
    /// `[input] mouse_sensitivity`.
    pub mouse_sensitivity: Option<u16>,
    /// When the host mouse is grabbed (`--mouse-capture`): "click",
    /// "auto", or "manual". Same parser as `[input] mouse_capture`.
    pub mouse_capture: Option<String>,
    /// Device in game port 1 (`--port1`): "mouse", "joystick", "cd32",
    /// "analogue", or "none". Same parser as `[input] port1`.
    pub port1: Option<String>,
    /// Device in game port 2 (`--port2`). Same parser as `[input] port2`.
    pub port2: Option<String>,
    /// Autofire rate in Hz (`--autofire`), 0 for off. Same validation as
    /// `[input] autofire_hz`.
    pub autofire_hz: Option<u8>,
    /// Serial port wiring (`--serial`): "off", "stdout", "midi", "tcp",
    /// "tcp-connect", or "pty" ("none" and "terminal" parse as
    /// compatibility aliases of the first two). Same parser as
    /// `[serial] mode`.
    pub serial: Option<String>,
    /// Remote host:port the serial port dials (`--serial-connect`),
    /// implying `--serial tcp-connect`.
    pub serial_connect: Option<String>,
    /// Host MIDI output endpoint (`--midi-out`), implying `--serial midi`.
    pub midi_out: Option<String>,
    /// Host MIDI input endpoint (`--midi-in`), implying `--serial midi`.
    pub midi_in: Option<String>,
    /// Parallel port device (`--parallel`): "none", "printer", or "sampler".
    /// Same parser as `[parallel] device`.
    pub parallel: Option<String>,
    /// Sampler host capture device (`--sampler-audio-input`), implying
    /// `--parallel sampler`. Substring match.
    pub sampler_input: Option<String>,
    /// Sampler input gain in decibels (`--sampler-input-gain`), implying
    /// `--parallel sampler`. Preamp; 0 dB = unity.
    pub sampler_gain: Option<f32>,
    /// Host audio output device (`--audio-device`), substring match.
    pub audio_device: Option<String>,
    /// Output channel mode (`--audio-channel-mode`): "stereo" or "mono".
    pub audio_channel_mode: Option<String>,
    /// Paula audio filter mode (`--audio-filter`): "auto", "on", or "off".
    pub audio_filter: Option<String>,
    /// Stereo separation percent (`--audio-stereo-separation`), 0-100.
    pub audio_stereo_separation: Option<u16>,
    /// Power-on RTC value (`--rtc-time`): Unix seconds or
    /// "YYYY-MM-DD HH:MM[:SS]". Same parser as `[machine] rtc_time`.
    pub rtc_time: Option<String>,
    /// Freeze the seeded RTC (`--rtc-frozen`). Same as
    /// `[machine] rtc_frozen`.
    pub rtc_frozen: Option<bool>,
    /// A2065 Ethernet backend (`--a2065-net`): "none", "loopback", "nat", or
    /// "bridge".
    /// Same parser as `[a2065] net`; setting it fits the board.
    pub a2065_net: Option<String>,
    /// Host adapter for bridged A2065 networking (`--a2065-interface`).
    pub a2065_interface: Option<String>,
    /// HostSocket bsdsocket.library backend (`--hostsocket-net`): "none",
    /// "loopback", "nat", "bridge", or "host" (the Amiberry-style
    /// host-socket backend -- see `[hostsocket] net`'s own doc comment).
    /// Same parser as `[hostsocket] net`; setting it fits the board.
    pub hostsocket_net: Option<String>,
    /// Host adapter for bridged HostSocket networking
    /// (`--hostsocket-interface`).
    pub hostsocket_interface: Option<String>,
    /// Open fullscreen at start (`--full-screen` / `--windowed`). Same as
    /// `[display] full_screen`.
    pub full_screen: Option<bool>,
    /// Show the status bar at start (`--show-status-bar` /
    /// `--hide-status-bar`). Same as `[display] status_bar`.
    pub status_bar: Option<bool>,
    /// Show the performance overlay at start (`--perf-overlay`). Same as
    /// `[display] perf_overlay`.
    pub perf_overlay: Option<bool>,
    /// How large the pop-up menu is drawn (`--menu-scale`). Same values as
    /// `[display] menu_scale`.
    pub menu_scale: Option<String>,
    /// MT-32 control and PCM ROM images (`--mt32-control-rom`,
    /// `--mt32-pcm-rom`). Same as `[serial] mt32_control_rom`/`mt32_pcm_rom`.
    pub mt32_control_rom: Option<String>,
    pub mt32_pcm_rom: Option<String>,
    /// Show the MT-32's front panel (`--mt32-panel`). Same as
    /// `[serial] mt32_panel`.
    pub mt32_panel: Option<bool>,
    /// A real floppy drive on a bay (`--floppy-bridge DFN INTERFACE`), by bay.
    /// Same values as `[floppy.dfN] bridge`.
    pub floppy_bridge: [Option<String>; 4],
    /// The interface's serial port (`--floppy-bridge-port DFN PORT`). Same as
    /// `[floppy.dfN] bridge_port`; unset auto-detects.
    pub floppy_bridge_port: [Option<String>; 4],
    /// Which drive on the cable to select (`--floppy-bridge-cable DFN CABLE`).
    /// Same as `[floppy.dfN] bridge_cable`.
    pub floppy_bridge_cable: [Option<String>; 4],
    /// Drop the emulator's own write protection on a bay
    /// (`--floppy-bridge-writable DFN`), leaving only the disk's tab between
    /// the guest and the platter. Same as `[floppy.dfN] write_protected =
    /// false`; there is deliberately no flag the other way, because protected
    /// is already the default.
    pub floppy_bridge_writable: [bool; 4],
    /// How the interface captures a track (`--floppy-bridge-mode DFN MODE`).
    /// Same as `[floppy.dfN] bridge_mode`.
    pub floppy_bridge_mode: [Option<String>; 4],
    /// Force a density rather than sensing it (`--floppy-bridge-density DFN
    /// DENSITY`). Same as `[floppy.dfN] bridge_density`.
    pub floppy_bridge_density: [Option<String>; 4],
    /// Serve captured tracks at a percentage of real speed
    /// (`--floppy-bridge-speed DFN PERCENT`). Same as `[floppy.dfN]
    /// bridge_speed`.
    pub floppy_bridge_speed: [Option<u16>; 4],
}

impl ConfigOverrides {
    /// Whether any override was set.
    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.chipset.is_none()
            && self.cpu.is_none()
            && self.fpu.is_none()
            && self.cpu_clock_mhz.is_none()
            && self.cpu_jit.is_none()
            && self.chip.is_none()
            && self.fast.is_none()
            && self.slow.is_none()
            && self.motherboard.is_none()
            && self.accelerator.is_none()
            && self.floppy_drives.is_none()
            && self.floppy_speed.is_none()
            && self.floppy_bridge.iter().all(Option::is_none)
            && self.floppy_bridge_port.iter().all(Option::is_none)
            && self.floppy_bridge_cable.iter().all(Option::is_none)
            && self.floppy_bridge_mode.iter().all(Option::is_none)
            && self.floppy_bridge_density.iter().all(Option::is_none)
            && self.floppy_bridge_speed.iter().all(Option::is_none)
            && !self.floppy_bridge_writable.iter().any(|w| *w)
            && self.joystick.is_none()
            && self.mouse_sensitivity.is_none()
            && self.mouse_capture.is_none()
            && self.port1.is_none()
            && self.port2.is_none()
            && self.autofire_hz.is_none()
            && self.serial.is_none()
            && self.serial_connect.is_none()
            && self.midi_out.is_none()
            && self.midi_in.is_none()
            && self.parallel.is_none()
            && self.sampler_input.is_none()
            && self.sampler_gain.is_none()
            && self.audio_device.is_none()
            && self.audio_channel_mode.is_none()
            && self.audio_filter.is_none()
            && self.audio_stereo_separation.is_none()
            && self.rtc_time.is_none()
            && self.rtc_frozen.is_none()
            && self.a2065_net.is_none()
            && self.a2065_interface.is_none()
            && self.hostsocket_net.is_none()
            && self.hostsocket_interface.is_none()
            && self.full_screen.is_none()
            && self.status_bar.is_none()
            && self.perf_overlay.is_none()
            && self.menu_scale.is_none()
            && self.mt32_control_rom.is_none()
            && self.mt32_pcm_rom.is_none()
            && self.mt32_panel.is_none()
    }

    /// Inject the set overrides into the raw config, replacing the values
    /// the file (or its absence) provided. Conversion validates the result.
    fn apply_to(&self, raw: &mut RawConfig) {
        if let Some(model) = &self.model {
            raw.machine.profile = Some(model.clone());
        }
        if let Some(chipset) = &self.chipset {
            raw.chipset.revision = Some(chipset.clone());
        }
        if let Some(cpu) = &self.cpu {
            raw.cpu.model = Some(cpu.clone());
        }
        if let Some(fpu) = self.fpu {
            raw.cpu.fpu = Some(fpu);
        }
        if let Some(mhz) = self.cpu_clock_mhz {
            raw.cpu.clock_mhz = Some(mhz);
        }
        if let Some(jit) = self.cpu_jit {
            raw.cpu.jit = Some(jit);
        }
        if let Some(chip) = &self.chip {
            raw.memory.chip = Some(chip.clone());
        }
        if let Some(fast) = &self.fast {
            raw.memory.fast = Some(fast.clone());
        }
        if let Some(slow) = &self.slow {
            raw.memory.slow = Some(slow.clone());
        }
        if let Some(motherboard) = &self.motherboard {
            raw.memory.motherboard = Some(motherboard.clone());
        }
        if let Some(accelerator) = &self.accelerator {
            raw.memory.accelerator = Some(accelerator.clone());
        }
        if let Some(drives) = self.floppy_drives {
            raw.floppy.drives = Some(drives);
        }
        if let Some(speed) = self.floppy_speed {
            raw.floppy.speed = Some(speed);
        }
        for idx in 0..4 {
            if self.floppy_bridge[idx].is_none()
                && self.floppy_bridge_port[idx].is_none()
                && self.floppy_bridge_cable[idx].is_none()
                && !self.floppy_bridge_writable[idx]
                && self.floppy_bridge_mode[idx].is_none()
                && self.floppy_bridge_density[idx].is_none()
                && self.floppy_bridge_speed[idx].is_none()
            {
                continue;
            }
            // A bay named on the command line gets a table if it had none, so
            // a bridge can be asked for with no config file at all.
            let drive = match idx {
                0 => &mut raw.floppy.df0,
                1 => &mut raw.floppy.df1,
                2 => &mut raw.floppy.df2,
                _ => &mut raw.floppy.df3,
            }
            .get_or_insert_with(RawFloppyDrive::default);
            if let Some(bridge) = &self.floppy_bridge[idx] {
                drive.bridge = Some(bridge.clone());
                // The flag says "this bay is a real drive", so an image the
                // config file left here would otherwise be a contradiction the
                // parser rejects. The command line wins, as it does elsewhere.
                // Except for `off`, whose whole point is to hand the bay back
                // to images: clearing the path there would take away the very
                // disk the config file asked for.
                if !bridge.trim().eq_ignore_ascii_case("off") {
                    drive.path = None;
                    drive.paths = None;
                }
            }
            if let Some(port) = &self.floppy_bridge_port[idx] {
                drive.bridge_port = Some(port.clone());
            }
            if let Some(cable) = &self.floppy_bridge_cable[idx] {
                drive.bridge_cable = Some(cable.clone());
            }
            if self.floppy_bridge_writable[idx] {
                drive.write_protected = Some(false);
            }
            if let Some(mode) = &self.floppy_bridge_mode[idx] {
                drive.bridge_mode = Some(mode.clone());
            }
            if let Some(density) = &self.floppy_bridge_density[idx] {
                drive.bridge_density = Some(density.clone());
            }
            if let Some(speed) = self.floppy_bridge_speed[idx] {
                drive.bridge_speed = Some(RawReplaySpeed::Percent(speed));
            }
        }
        if let Some(joystick) = &self.joystick {
            raw.input.joystick = Some(joystick.clone());
        }
        if let Some(sensitivity) = self.mouse_sensitivity {
            raw.input.mouse_sensitivity = Some(sensitivity);
        }
        if let Some(capture) = &self.mouse_capture {
            raw.input.mouse_capture = Some(capture.clone());
        }
        if let Some(port1) = &self.port1 {
            raw.input.port1 = Some(port1.clone());
        }
        if let Some(port2) = &self.port2 {
            raw.input.port2 = Some(port2.clone());
        }
        if let Some(hz) = self.autofire_hz {
            raw.input.autofire_hz = Some(hz);
        }
        if let Some(mode) = &self.serial {
            raw.serial.mode = Some(mode.clone());
        }
        if let Some(addr) = &self.serial_connect {
            raw.serial.connect = Some(addr.clone());
        }
        if let Some(out) = &self.midi_out {
            raw.serial.midi_out = Some(out.clone());
        }
        if let Some(input) = &self.midi_in {
            raw.serial.midi_in = Some(input.clone());
        }
        // Naming a MIDI endpoint or a dial-out address on the command line
        // selects the matching mode unless `--serial` said otherwise.
        if self.serial.is_none() && (self.midi_out.is_some() || self.midi_in.is_some()) {
            raw.serial.mode = Some(SerialMode::Midi.label().to_string());
        }
        if self.serial.is_none()
            && self.midi_out.is_none()
            && self.midi_in.is_none()
            && self.serial_connect.is_some()
        {
            raw.serial.mode = Some(SerialMode::TcpConnect.label().to_string());
        }
        if let Some(device) = &self.parallel {
            raw.parallel.device = Some(device.clone());
        }
        if let Some(input) = &self.sampler_input {
            raw.parallel.sampler_input = Some(input.clone());
        }
        if let Some(gain) = self.sampler_gain {
            raw.parallel.sampler_gain = Some(gain);
        }
        // Naming a sampler option selects the sampler unless `--parallel` said
        // otherwise (mirrors `--midi-out` implying `--serial midi`).
        if self.parallel.is_none() && (self.sampler_input.is_some() || self.sampler_gain.is_some())
        {
            raw.parallel.device = Some(ParallelDevice::Sampler.label().to_string());
        }
        if let Some(dev) = &self.audio_device {
            raw.audio.output_device = Some(dev.clone());
        }
        if let Some(mode) = &self.audio_channel_mode {
            raw.audio.channel_mode = Some(mode.clone());
        }
        if let Some(filter) = &self.audio_filter {
            raw.audio.audio_filter = Some(filter.clone());
        }
        if let Some(sep) = self.audio_stereo_separation {
            raw.audio.stereo_separation = Some(sep);
        }
        if let Some(time) = &self.rtc_time {
            // The text form parses bare digits as Unix seconds, so both
            // CLI notations funnel through one raw variant.
            raw.machine.rtc_time = Some(RawRtcTime::Text(time.clone()));
        }
        if let Some(frozen) = self.rtc_frozen {
            raw.machine.rtc_frozen = Some(frozen);
        }
        if let Some(net) = &self.a2065_net {
            raw.a2065.net = Some(net.clone());
            if !matches!(
                net.trim().to_ascii_lowercase().as_str(),
                "bridge" | "bridged"
            ) {
                raw.a2065.interface = None;
            }
        }
        if let Some(interface) = &self.a2065_interface {
            raw.a2065.interface = Some(interface.clone());
            if self.a2065_net.is_none() {
                raw.a2065.net = Some("bridge".to_string());
            }
        }
        if let Some(net) = &self.hostsocket_net {
            raw.hostsocket.net = Some(net.clone());
            if !matches!(
                net.trim().to_ascii_lowercase().as_str(),
                "bridge" | "bridged"
            ) {
                raw.hostsocket.interface = None;
            }
        }
        if let Some(interface) = &self.hostsocket_interface {
            raw.hostsocket.interface = Some(interface.clone());
            if self.hostsocket_net.is_none() {
                raw.hostsocket.net = Some("bridge".to_string());
            }
        }
        if let Some(full_screen) = self.full_screen {
            raw.display.full_screen = Some(full_screen);
        }
        if let Some(status_bar) = self.status_bar {
            raw.display.status_bar = Some(status_bar);
        }
        if let Some(perf_overlay) = self.perf_overlay {
            raw.display.perf_overlay = Some(perf_overlay);
        }
        if let Some(menu_scale) = &self.menu_scale {
            raw.display.menu_scale = Some(menu_scale.clone());
        }
        if let Some(rom) = &self.mt32_control_rom {
            raw.serial.mt32_control_rom = Some(rom.clone());
        }
        if let Some(rom) = &self.mt32_pcm_rom {
            raw.serial.mt32_pcm_rom = Some(rom.clone());
        }
        if let Some(panel) = self.mt32_panel {
            raw.serial.mt32_panel = Some(panel);
        }
    }
}

// --- raw deserialization (one nested struct per [section]) ---------------

// `Serialize` lets the launcher write a configured machine back out as TOML
// (the configuration screen's Save). The `skip_serializing_if` attributes keep
// the output minimal -- only fields and sections the user actually set are
// emitted, matching the style of the hand-written `*.example.toml`. The
// `toml` serializer requires every top-level scalar key to be emitted before
// any `[table]`, so the three top-level scalars (`rom`, `extended_rom`,
// `identify`) are declared first, ahead of the section tables and the `zorro`
// array of tables. Field declaration order otherwise mirrors deserialization,
// which is order-independent.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rom: Option<String>,
    /// Extended ROM image (CD32 512K at $E00000, CDTV 256K at $F00000).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) extended_rom: Option<String>,
    /// `identify = false` drops the Copperline identification board from the
    /// autoconfig chain (default: present).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) identify: Option<bool>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) cd: RawCd,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) debug: RawDebug,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) cpu: RawCpu,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) emulation: RawEmulation,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) memory: RawMemory,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) machine: RawMachine,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) chipset: RawChipset,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) audio: RawAudio,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) ide: RawIde,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) scsi: RawScsi,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) a2065: RawA2065,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) hostsocket: RawHostSocket,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) rtg: RawRtg,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) floppy: RawFloppy,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) display: RawDisplay,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) input: RawInput,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) serial: RawSerial,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) parallel: RawParallel,
    /// `[[filesys]]` host-directory mount entries, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) filesys: Vec<RawFilesysMount>,
    /// `[[zorro]]` board entries, configured in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) zorro: Vec<RawZorroBoard>,
}

impl RawConfig {
    /// Serialize this raw config back to TOML text for the configuration
    /// screen's Save. Only non-default fields are written (see the
    /// `skip_serializing_if` attributes), so the result reads like the
    /// hand-written example configs.
    #[cfg_attr(not(feature = "frontend"), allow(dead_code))]
    pub(crate) fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).context("serializing configuration to TOML")
    }

    /// The configured menu size, for the paths that put a window up before a
    /// whole [`Config`] has been built.
    pub fn menu_scale(&self) -> MenuScale {
        self.display
            .menu_scale
            .as_deref()
            .and_then(|s| parse_menu_scale(s).ok())
            .unwrap_or_default()
    }

    /// The configured live-audio state (`[audio] output_enabled`), defaulting to
    /// on when unset -- matching [`AudioConfig`]'s default. Lets the binary seed
    /// the config-screen session audio without reaching into private raw fields.
    pub fn audio_output_enabled(&self) -> bool {
        self.audio.output_enabled.unwrap_or(true)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawDisplay {
    /// "tv" (default, mask deep overscan like a CRT bezel) or "full".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) overscan: Option<String>,
    /// "tv" (default, 4:3 CRT pixel aspect) or "square" (1:1 host
    /// pixels; a lo-res display is an exact 2x2 of its bitmap).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pixel_aspect: Option<String>,
    /// "smooth" (default, aspect-fit with filtering) or "integer" (whole
    /// -number multiples of the canvas, centred, point-sampled).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scaling: Option<String>,
    /// Motion-adaptive deinterlacing of interlaced content (default
    /// true); false line-doubles every field as it arrives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) deinterlace: Option<bool>,
    /// CRT phosphor persistence fraction, 0.0 (off, default) to 0.95.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) phosphor: Option<f32>,
    /// Window shader pass: "none" (default), "scanlines", "mask", "crt",
    /// or the path of a `.wgsl` file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) shader: Option<String>,
    /// Shader mix, 0.0 (invisible) to 1.0 (full effect, the default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) shader_strength: Option<f32>,
    /// Monitor-style front bezel around the window picture (default false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bezel: Option<bool>,
    /// Performance overlay in the top-right of the display (default false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) perf_overlay: Option<bool>,
    /// Screen tint: "none" (default), "bw", "green", "amber", or "sepia".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tint: Option<String>,
    /// Size of the pop-up menu: "1x" (default) or "2x".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) menu_scale: Option<String>,
    /// Open fullscreen at start (default false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) full_screen: Option<bool>,
    /// Show the status bar at start (default true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status_bar: Option<bool>,
}

/// One `[[filesys]]` entry (experimental): a host directory exported to the
/// guest as the AmigaDOS device `HOSTFS<n>:` (n = position in the config).
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawFilesysMount {
    /// Host directory to export.
    pub(crate) path: String,
    /// AmigaDOS volume name; defaults to the directory's name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) volume: Option<String>,
    /// Boot priority (-128..=127); defaults to -128, which mounts the
    /// volume but never offers it as a boot candidate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bootpri: Option<i8>,
    /// Export the directory write-protected: the guest sees the volume as a
    /// read-only disk and every write fails. Defaults to false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) readonly: Option<bool>,
}

/// `[input]` host-input preferences: which controller device is plugged into
/// each game port, and the host source for the joystick port. The status-bar
/// toggle and `Cmd+J` / `Alt+J` flip the joystick source live.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawInput {
    /// Initial joystick input source: "gamepad" (default) or "keyboard".
    /// ("auto" is still accepted for backward compatibility and maps to
    /// "gamepad".)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) joystick: Option<String>,
    /// Device in game port 1: "mouse" (default), "joystick", "cd32",
    /// "analogue", or "none".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) port1: Option<String>,
    /// Device in game port 2: same values; defaults to "joystick"
    /// ("cd32" on the CD32 profile).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) port2: Option<String>,
    /// Host mouse sensitivity, 0-100 (default 50).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mouse_sensitivity: Option<u16>,
    /// When the host mouse is grabbed: "click" (default), "auto", or
    /// "manual".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mouse_capture: Option<String>,
    /// Autofire rate in Hz for the fire button, or 0 (the default) for off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) autofire_hz: Option<u8>,
}

/// `[serial]` host wiring for Paula's serial (a.k.a. MIDI) port.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawSerial {
    /// "stdout" (default), "off", "midi", "tcp", "tcp-connect", or "pty".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mode: Option<String>,
    /// Host MIDI output endpoint name (substring match); MIDI mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) midi_out: Option<String>,
    /// Host MIDI input endpoint name (substring match); MIDI mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) midi_in: Option<String>,
    /// MT-32 control ROM image; needed when midi_out = "mt32".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mt32_control_rom: Option<String>,
    /// MT-32 PCM ROM image; needed when midi_out = "mt32".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mt32_pcm_rom: Option<String>,
    /// Show the MT-32's front panel under the status bar (default false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mt32_panel: Option<bool>,
    /// Its display: "mt32" (default), "superjv", "sseries", or "oled".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mt32_lcd: Option<String>,
    /// TCP listen address; tcp mode only. Defaults to 127.0.0.1:1234.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) listen: Option<String>,
    /// Remote host:port to dial; tcp-connect mode only, and required there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) connect: Option<String>,
}

/// `[parallel]` peripheral selection for the Amiga Centronics parallel port.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawParallel {
    /// Which device is plugged in: `none`, `printer`, or `sampler`. When
    /// omitted, a bare `output` path implies `printer` (back-compat) and
    /// otherwise the port is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) device: Option<String>,
    /// Printer raw byte-stream output path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output: Option<String>,
    /// Sampler host capture device name (substring match); absent = default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sampler_input: Option<String>,
    /// Sampler input gain in decibels (preamp); absent = 0 dB (unity).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sampler_gain: Option<f32>,
}

/// A drive image entry in `[ide]`/`[scsi]`. Accepts either a bare path string
/// (`master = "disk.hdf"`) or a table carrying an explicit volume-name override
/// and/or boot priority (`master = { path = "games/", name = "Games",
/// bootpri = 5 }`). It serializes back to the bare string when neither
/// override is set, so existing minimal configs round-trip unchanged.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawDrive {
    pub(crate) path: String,
    pub(crate) name: Option<String>,
    /// Boot priority written into the synthesized RDB's `de_BootPri`
    /// (-128..=127); defaults to 0, the priority HDToolBox gives a hard-disk
    /// boot partition. -128 mounts the partition without offering it for boot.
    pub(crate) bootpri: Option<i8>,
}

impl RawDrive {
    pub(crate) fn from_path(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            name: None,
            bootpri: None,
        }
    }
}

impl Serialize for RawDrive {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.name.is_none() && self.bootpri.is_none() {
            // No overrides: a plain string keeps saved configs minimal.
            return serializer.serialize_str(&self.path);
        }
        use serde::ser::SerializeMap;
        let len = 1 + usize::from(self.name.is_some()) + usize::from(self.bootpri.is_some());
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("path", &self.path)?;
        if let Some(name) = &self.name {
            map.serialize_entry("name", name)?;
        }
        if let Some(bootpri) = &self.bootpri {
            map.serialize_entry("bootpri", bootpri)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for RawDrive {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct DriveVisitor;
        impl<'de> serde::de::Visitor<'de> for DriveVisitor {
            type Value = RawDrive;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(
                    "a drive image path, or a table with `path` and optional `name`/`bootpri`",
                )
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<RawDrive, E> {
                Ok(RawDrive::from_path(v))
            }
            fn visit_string<E: serde::de::Error>(
                self,
                v: String,
            ) -> std::result::Result<RawDrive, E> {
                Ok(RawDrive::from_path(v))
            }
            fn visit_map<A>(self, mut map: A) -> std::result::Result<RawDrive, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut path: Option<String> = None;
                let mut name: Option<String> = None;
                let mut bootpri: Option<i8> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "path" => {
                            if path.is_some() {
                                return Err(serde::de::Error::duplicate_field("path"));
                            }
                            path = Some(map.next_value()?);
                        }
                        "name" => {
                            if name.is_some() {
                                return Err(serde::de::Error::duplicate_field("name"));
                            }
                            name = Some(map.next_value()?);
                        }
                        "bootpri" => {
                            if bootpri.is_some() {
                                return Err(serde::de::Error::duplicate_field("bootpri"));
                            }
                            bootpri = Some(map.next_value()?);
                        }
                        other => {
                            return Err(serde::de::Error::unknown_field(
                                other,
                                &["path", "name", "bootpri"],
                            ));
                        }
                    }
                }
                let path = path.ok_or_else(|| serde::de::Error::missing_field("path"))?;
                Ok(RawDrive {
                    path,
                    name,
                    bootpri,
                })
            }
        }
        deserializer.deserialize_any(DriveVisitor)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawIde {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) master: Option<RawDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) slave: Option<RawDrive>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawScsi {
    /// Host adapter to fit: "a2091" (Zorro II, default), "a4091" (Zorro
    /// III), or "a3000" (the motherboard SDMAC, default on an A3000).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) controller: Option<String>,
    /// Boot ROM image. For split even/odd A2091 EPROM dumps, `rom` is the
    /// even half and `rom_odd` the odd half.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rom: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rom_odd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit0: Option<RawDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit1: Option<RawDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit2: Option<RawDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit3: Option<RawDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit4: Option<RawDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit5: Option<RawDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit6: Option<RawDrive>,
}

/// `[a2065]` Ethernet board. Fitting the board enables host networking, which
/// is non-deterministic.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawA2065 {
    /// Host network backend: "loopback", "nat", "bridge", or "none" for an
    /// isolated NIC. Absent means no A2065 board is fitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) net: Option<String>,
    /// Host adapter identifier used by `net = "bridge"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) interface: Option<String>,
}

/// `[hostsocket]` bundled bsdsocket.library board: a host-side TCP/IP stack
/// presented to the guest as `bsdsocket.library`, with no guest network
/// stack to boot (see `crate::hostsocket`). Fitting the board with a real
/// backend enables host networking, which is non-deterministic; the
/// "loopback" backend stays deterministic.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawHostSocket {
    /// Host network backend: "loopback", "nat", "bridge", "host" (the
    /// Amiberry-style host-socket backend -- see
    /// `crate::hostsocket::board_config`'s own `transport` parameter, and
    /// this crate's config-resolution code for what it implies:
    /// `interface`/`address`/`gateway` are rejected, and `resolver`
    /// defaults to `"host"`), or "none" for a board with a dead wire.
    /// Absent means no HostSocket board is fitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) net: Option<String>,
    /// Host adapter identifier used by `net = "bridge"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) interface: Option<String>,
    /// `gethostbyname()`'s DNS resolver. Defaults to Copperline NAT's own
    /// DNS forwarder (10.0.2.3); only meaningful to override under
    /// `net = "bridge"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dns_server: Option<String>,
    /// `gethostname()`'s return value. Purely cosmetic; defaults to "amiga".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hostname: Option<String>,
    /// Interface address ("a.b.c.d" or "a.b.c.d/prefix"), meaningful only
    /// under `net = "bridge"`: the default (Copperline NAT's own virtual
    /// address) is meaningless on a real LAN, where this must match one.
    /// Rejected under `net = "host"` (which has no interface address of
    /// its own at all).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) address: Option<String>,
    /// Default gateway, meaningful only under `net = "bridge"`; same
    /// reasoning as `address` above, including rejection under
    /// `net = "host"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) gateway: Option<String>,
    /// `gethostbyname()`'s resolver strategy: "host" asks Copperline's own
    /// process to resolve via the host OS resolver on a background
    /// thread, ignoring `dns_server` entirely; "dns" queries `dns_server`
    /// directly over the board's own network backend instead. Defaults to
    /// "host" under `net = "nat"`/`"bridge"`/`"host"` when left unset --
    /// the thing that works without a `dns_server` hand-matched to the
    /// backend (and `net = "host"`'s own underlying smoltcp interface is
    /// hardcoded to "loopback", which couldn't reach a real one anyway);
    /// set "dns" explicitly (with `dns_server`) to opt back into a
    /// specific resolver. Explicit "host" is rejected under
    /// `"loopback"`/`"none"`, where it would silently break the backend's
    /// own determinism guarantee (there is no sane default there either,
    /// so those backends simply get no resolver key at all).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resolver: Option<String>,
}

/// `[rtg]` graphics card: an RTG board on the Zorro chain.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawRtg {
    /// Card to fit: "z3660", "picasso2", "picasso2plus", or "none".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) card: Option<String>,
    /// Picasso II/II+ display memory: "1M" or "2M" (default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) vram: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawCd {
    /// Path to a cue sheet (BIN/CUE).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) image: Option<String>,
    /// Insert the disc this many emulated seconds after power-on
    /// instead of at boot (CDTV; some discs only boot when inserted
    /// after the boot screen).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) insert_delay: Option<f64>,
    /// CD32 NVRAM (save game EEPROM) backing file. Defaults to
    /// "cd32-nvram.bin" on CD32 machines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) nvram: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawCpu {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fpu: Option<bool>,
    /// Override the CPU clock in MHz. Defaults to the model's stock speed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) clock_mhz: Option<f64>,
    /// Model the on-chip instruction cache. Defaults on for the models
    /// that have one (all 020+).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) icache: Option<bool>,
    /// Model the on-chip data cache. Defaults on for the models that have
    /// one (030/040/060).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dcache: Option<bool>,
    /// 68060 only: what happens on the instructions the 68060 dropped from
    /// silicon (MOVEP, CHK2/CMP2, CAS2, misaligned CAS, 64-bit MUL/DIV, the
    /// unimplemented FPU subset). "trap" (default) is faithful - the OS
    /// needs 68060.library to emulate them; "native" executes them directly
    /// for systems without the library.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unimplemented: Option<String>,
    /// Fast CPU execution through the m68k core's batch/trace-JIT path.
    /// Not cycle-exact: the CPU behaves like an accelerator card. Defaults
    /// off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) jit: Option<bool>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawEmulation {
    /// Deprecated and ignored: "real" was the only remaining timing model,
    /// so the option carried no information. Still accepted (and warned
    /// about) so existing configs that name it keep parsing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) speed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) power_on: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pacing_budget: Option<String>,
    /// Best-effort realtime-like thread priority for the pacer and audio
    /// threads (default false). See `src/priority.rs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) realtime_priority: Option<bool>,
    /// UI warp/turbo speed: "2x", "4x", "8x", "16x", or "max" (default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) warp_speed: Option<String>,
    /// Record rewind history from power-on (default false), so the rewind
    /// hotkey works outside the debugger.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rewind: Option<bool>,
    /// Host memory (MiB) the rewind snapshot ring may hold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rewind_budget_mb: Option<usize>,
    /// Emulated frames between rewind snapshots (one rewind step).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rewind_interval_frames: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawMemory {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) chip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fast: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) slow: Option<String>,
    /// Ramsey-controlled motherboard fast RAM size (e.g. "16M"); needs a
    /// Ramsey (A3000/A4000 profiles) and a 32-bit CPU. Sizes beyond 16M
    /// (up to 64M) fill the motherboard RAM expansion space and need the
    /// A4000's Ramsey-07.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) motherboard: Option<String>,
    /// CPU-slot (accelerator) fast RAM size at $08000000 (e.g. "64M", up
    /// to 128M); 32-bit CPUs only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) accelerator: Option<String>,
    /// Zorro III autoconfig RAM size (e.g. "16M"); 32-bit CPUs only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) z3: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawZorroBoard {
    /// Path to a TOML board metadata file (see `src/zorro.rs` for the
    /// schema), resolved relative to the working directory.
    pub(crate) metadata: String,
    /// Per-board plugin setting overrides, layered over the manifest's
    /// `[config]` defaults (WASM plugin boards only). The launcher edits these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) config: Option<toml::Table>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawMachine {
    /// Machine profile name. Named `profile` (not `model`) so it never
    /// collides with `[cpu] model`: an uncommented profile line landing in
    /// the wrong table would otherwise be a confusing duplicate-key error.
    /// `model` stays accepted as a deprecated alias for old configs.
    #[serde(alias = "model", skip_serializing_if = "Option::is_none")]
    pub(crate) profile: Option<String>,
    /// Whether the $DC0000 RTC is fitted; defaults per profile (only the
    /// A500+ and CDTV ship with one, so the base A500/A600/A1200/etc. default
    /// to none). Set `rtc = true` to fit one, e.g. for an A600HD or a
    /// clock-equipped A1200.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rtc: Option<bool>,
    /// Which clock part fills the socket: `"MSM6242"` (OKI, most boards)
    /// or `"RP5C01"` (Ricoh, the A3000/A4000 part and the only protocol
    /// Linux/m68k drives on those models). Defaults per profile; setting
    /// it implies `rtc = true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rtc_chip: Option<String>,
    /// Power-on clock value: an integer (Unix seconds, UTC) or a string
    /// `"YYYY-MM-DD HH:MM[:SS]"` (the wall-clock time the guest reads).
    /// Seeds the battery clock and ticks it in emulated time so the
    /// guest-visible time is deterministic; implies `rtc = true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rtc_time: Option<RawRtcTime>,
    /// Stop the seeded clock so every read returns `rtc_time` exactly.
    /// Only meaningful together with `rtc_time`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rtc_frozen: Option<bool>,
    /// Backing file for the RP5C01's battery RAM (the storage behind
    /// AmigaOS `battmem.resource` on the A3000/A4000), in the
    /// WinUAE/Amiberry `.nvram` file layout so files interchange between
    /// emulators. Defaults to `battmem.nvram` whenever an RP5C01 is
    /// fitted; an empty string keeps the battery registers session-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) battmem: Option<String>,
    /// Memory controller fitted, defaulting per profile: `none`, `ramsey-04`
    /// (A3000) or `ramsey-07` (A4000). Ramsey answers at $DE0000, which no
    /// other chip decodes, so it can also be fitted to a wedge machine to
    /// exercise the diagnostic tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mem_controller: Option<String>,
    /// Skip the ROM's scsi.device. Defaults to true only when the machine's
    /// built-in disk controller (Gayle or A4000 IDE, A3000 SDMAC SCSI) has no
    /// drives configured, where the driver would only cost boot time probing
    /// an empty bus; false everywhere else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rom_scsi_device_disable: Option<bool>,
}

/// `[machine] rtc_time` accepts both TOML notations for one instant: a
/// bare integer (Unix seconds) or a calendar string. Both funnel through
/// `crate::rtc::parse_rtc_time` at validation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawRtcTime {
    Unix(i64),
    Text(String),
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawDebug {
    /// Arm the custom-register access validator: report software using the
    /// chipset in ways the hardware ignores (absent registers, undefined
    /// bits, wrong-direction and byte accesses, DMA pointers past Agnus's
    /// reach), each with the PC or Copper address that did it. Off by
    /// default; it also arms the per-register last-writer table.
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) validate_chipset: bool,
    /// Report writes that land on memory the CPU has already executed.
    /// Off by default; costs a 1 MiB execution map while armed.
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) detect_smc: bool,
    /// Log CPU accesses that no device decodes. Either `all`, or an address
    /// range like `"DD0000-DE0000"` (hex, end exclusive) to watch one window.
    /// Reads report the floating bus value they returned; writes report the
    /// value that went nowhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) log_unmapped: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawChipset {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) video: Option<String>,
    /// Fine-grained chip overrides on top of the `revision` preset, for the
    /// mixed machines that really shipped (e.g. late A500: ECS Agnus with an
    /// OCS Denise). `agnus` accepts OCS / 8370 / 8371 / 8372 / 8372A / 8372B /
    /// 8374 / 8375 / ALICE; `denise` accepts OCS / 8362 / ECS / 8373 / LISA /
    /// 4203.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agnus: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) denise: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAudio {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) floppy_sounds: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) floppy_sounds_volume: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) channel_mode: Option<String>,
    // `filter` is accepted as an alias so a config that followed the #278
    // request (which spelled it `[audio] filter`) still loads under
    // deny_unknown_fields; `audio_filter` is canonical and matches
    // `--audio-filter`.
    #[serde(alias = "filter", skip_serializing_if = "Option::is_none")]
    pub(crate) audio_filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stereo_separation: Option<u16>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawFloppy {
    /// Number of wired floppy drives, DF0..DFN-1. DF0 is the internal drive,
    /// so the valid range is 1-4.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) drives: Option<u8>,
    /// Drive speed percentage (100/200/400/800) or 0 for turbo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) speed: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) df0: Option<RawFloppyDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) df1: Option<RawFloppyDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) df2: Option<RawFloppyDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) df3: Option<RawFloppyDrive>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawFloppyDrive {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    /// A playlist of images for this drive, cycled with the disk-swap
    /// key. When given, the first entry is the boot disk. May be used
    /// instead of `path`; if both appear, `path` is treated as the first
    /// entry followed by `paths`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) write_protected: Option<bool>,
    /// Attach a real drive to this bay instead of an image, over a
    /// DrawBridge/Greaseweazle/Supercard Pro: `drawbridge`, `greaseweazle`,
    /// or `supercardpro` (aliases `arduino`, `gw`, `scp`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bridge: Option<String>,
    /// Serial port the interface is on. Omitted, the driver finds its own
    /// device; name one when two interfaces are plugged in at once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bridge_port: Option<String>,
    /// How each track is captured: `normal` (the default; `fast` is accepted
    /// as upstream's own name for it), `compatible`, or `stalling`. `turbo` is
    /// refused by name -- it answers AmigaDOS calls rather than reading the
    /// disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bridge_mode: Option<String>,
    /// Force a density instead of sensing it: `auto`, `dd`, or `hd`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bridge_density: Option<String>,
    /// Which drive on the cable to select: `a`/`b` (IBM PC cabling) or
    /// `0`..`3` (Shugart).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bridge_cable: Option<String>,
    /// Serve captured tracks at this percentage of real speed: 100,
    /// 125 (the default), 150, 175, or 200.
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "bridge_speed",
        rename = "replay_speed"
    )]
    pub(crate) bridge_speed: Option<RawReplaySpeed>,
}

/// The replay speed as a config file may spell it: a word, or one of the
/// percentages the setting used to take.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawReplaySpeed {
    Percent(u16),
    Word(String),
}

/// Convert a parsed `[ide]`/`[scsi]` drive entry into a `DriveImage`,
/// validating any volume-name override. An empty/whitespace name is treated as
/// no override; AmigaDOS volume names cannot contain ':' or '/' and the FFS
/// root block stores at most 30 characters.
fn drive_image(raw: RawDrive) -> Result<DriveImage> {
    let volume_name = match raw.name {
        None => None,
        Some(name) => {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                None
            } else if let Some(err) = crate::filesys::volume_name_error(trimmed) {
                bail!("drive name: {err}");
            } else {
                Some(trimmed.to_string())
            }
        }
    };
    Ok(DriveImage {
        path: PathBuf::from(raw.path),
        volume_name,
        boot_pri: raw.bootpri.unwrap_or(HARDFILE_DEFAULT_BOOT_PRI),
    })
}

impl TryFrom<RawConfig> for Config {
    type Error = anyhow::Error;

    fn try_from(raw: RawConfig) -> Result<Self> {
        let machine = match raw.machine.profile.as_deref() {
            None => None,
            Some(s) => Some(parse_machine_model(s)?),
        };
        let defaults = machine.map_or_else(Config::default, machine_profile_defaults);
        // Independent validation failures accumulate here so a single pass
        // reports them all; parse failures whose value later checks depend on
        // still fail fast. On any accumulated error the fallback values never
        // reach a running machine.
        let mut errors: Vec<anyhow::Error> = Vec::new();
        let cpu = match raw.cpu.model.as_deref() {
            None => defaults.cpu,
            Some(s) => parse_cpu(s)?,
        };
        let fpu = raw.cpu.fpu.unwrap_or_else(|| cpu.default_fpu());
        let cpu_clock_mhz = match raw.cpu.clock_mhz {
            Some(mhz) if mhz.is_finite() && mhz > 0.0 => mhz,
            Some(_) => {
                errors.push(anyhow!("[cpu] clock_mhz must be a positive number"));
                cpu.default_clock_mhz()
            }
            // With the whole [cpu] pair absent, the profile's clock stands:
            // the A1200/CD32 profiles pin the authentic 14.18 MHz (4x the
            // PAL colour clock), where the generic 020 default is 14.0. An
            // explicit [cpu] model is a different part in the socket, so it
            // takes its own stock speed instead.
            None if raw.cpu.model.is_none() => defaults.cpu_clock_mhz,
            None => cpu.default_clock_mhz(),
        };
        if fpu && matches!(cpu, CpuModel::M68000 | CpuModel::M68010) {
            errors.push(anyhow!(
                "[cpu] fpu = true needs the 68020+ coprocessor interface; \
                 a 68000/68010 cannot drive a 68881/68882"
            ));
        }
        // The on-chip caches are silicon: model them by default whenever the
        // CPU has them (AmigaOS turns them on via CACR), so a 020/030 matches
        // real hardware instead of contending with chip-bus DMA on every
        // instruction fetch. `[cpu] icache`/`dcache` still force either way.
        let cpu_icache = raw
            .cpu
            .icache
            .unwrap_or_else(|| cpu.has_instruction_cache());
        let cpu_dcache = raw.cpu.dcache.unwrap_or_else(|| cpu.has_data_cache());
        let cpu_unimplemented = match raw.cpu.unimplemented.as_deref() {
            None => UnimplementedPolicy::Trap,
            Some(s) => {
                let policy = match s.trim().to_ascii_lowercase().as_str() {
                    "trap" => UnimplementedPolicy::Trap,
                    "native" => UnimplementedPolicy::Native,
                    _ => {
                        errors.push(anyhow!(
                            "[cpu] unimplemented must be \"trap\" or \"native\", got {:?}",
                            s
                        ));
                        UnimplementedPolicy::Trap
                    }
                };
                if cpu != CpuModel::M68060 {
                    errors.push(anyhow!(
                        "[cpu] unimplemented applies only to the 68060 \
                         (other models implement their full instruction sets)"
                    ));
                }
                policy
            }
        };
        if cpu_icache && !cpu.has_instruction_cache() {
            errors.push(anyhow!(
                "[cpu] icache = true needs a 68020/68EC020/68030/68040 \
                 (the 68000 has no instruction cache)"
            ));
        }
        if cpu_dcache && !cpu.has_data_cache() {
            errors.push(anyhow!(
                "[cpu] dcache = true needs a 68030 or 68040 \
                 (the 68000/68020 have no data cache)"
            ));
        }
        let cpu_jit = raw.cpu.jit.unwrap_or(false);
        if let Some(speed) = raw.emulation.speed.as_deref() {
            log::warn!(
                "[emulation] speed = {speed:?} is deprecated and ignored: the \
                 deterministic cycle-driven core is the only timing model"
            );
        }
        let emulation = Emulation {
            power_on: raw
                .emulation
                .power_on
                .unwrap_or(defaults.emulation.power_on),
            pacing_budget: match raw.emulation.pacing_budget.as_deref() {
                None => defaults.emulation.pacing_budget,
                Some(s) => parse_pacing_budget(s)?,
            },
            realtime_priority: raw
                .emulation
                .realtime_priority
                .unwrap_or(defaults.emulation.realtime_priority),
            warp_speed: match raw.emulation.warp_speed.as_deref() {
                None => defaults.emulation.warp_speed,
                Some(s) => parse_warp_speed(s)?,
            },
            rewind: raw.emulation.rewind.unwrap_or(defaults.emulation.rewind),
            rewind_budget_mb: match raw.emulation.rewind_budget_mb {
                None => defaults.emulation.rewind_budget_mb,
                // A ring that cannot hold a single snapshot has no anchor to
                // rewind to, so reject the degenerate value rather than
                // silently recording nothing.
                Some(0) => bail!("[emulation] rewind_budget_mb must be at least 1 MiB"),
                Some(mb) => mb,
            },
            rewind_interval_frames: match raw.emulation.rewind_interval_frames {
                None => defaults.emulation.rewind_interval_frames,
                Some(0) => bail!("[emulation] rewind_interval_frames must be at least 1"),
                Some(n) => n,
            },
        };
        let chip_ram_bytes = match raw.memory.chip.as_deref() {
            None => defaults.chip_ram_bytes,
            Some(s) => parse_size(s, "chip RAM")?,
        };
        let fast_ram_bytes = match raw.memory.fast.as_deref() {
            None => defaults.fast_ram_bytes,
            Some(s) => parse_size(s, "fast RAM")?,
        };
        let slow_ram_bytes = match raw.memory.slow.as_deref() {
            None => defaults.slow_ram_bytes,
            Some(s) => parse_size(s, "slow RAM")?,
        };
        let mb_ram_bytes = match raw.memory.motherboard.as_deref() {
            None => defaults.mb_ram_bytes,
            Some(s) => parse_size(s, "motherboard RAM")?,
        };
        let accel_ram_bytes = match raw.memory.accelerator.as_deref() {
            None => defaults.accel_ram_bytes,
            Some(s) => parse_size(s, "accelerator RAM")?,
        };
        let z3_ram_bytes = match raw.memory.z3.as_deref() {
            None => defaults.z3_ram_bytes,
            Some(s) => parse_size(s, "Zorro III RAM")?,
        };
        let mut zorro_boards = Vec::new();
        let mut wasm_boards = Vec::new();
        for entry in &raw.zorro {
            match crate::zorro::load_board_metadata(Path::new(&entry.metadata))? {
                crate::zorro::LoadedZorroBoard::Ram(spec) => zorro_boards.push(spec),
                crate::zorro::LoadedZorroBoard::Wasm {
                    spec,
                    wasm_path,
                    mut manifest,
                    default_config,
                    options: _,
                } => {
                    // Effective config = manifest defaults, with the user's
                    // per-board overrides layered on top.
                    let mut config = default_config;
                    if let Some(overrides) = &entry.config {
                        for (key, value) in overrides {
                            config.insert(key.clone(), crate::zorro::toml_value_to_string(value));
                        }
                    }
                    manifest.config = config;
                    // The bundled-ROM sentinel is reserved for the board
                    // [hostsocket] expands to: a metadata board's file-typed
                    // config value naming it must fail fast here (the module
                    // path sentinel is rejected in load_board_metadata), or
                    // the plugin host would silently hand it the embedded
                    // HostSocket stub ROM.
                    for key in &manifest.file_keys {
                        if manifest.config.get(key).map(String::as_str)
                            == Some(crate::hostsocket::BUNDLED_HOSTSOCKET_ROM)
                        {
                            return Err(anyhow!(
                                "{}: config {key:?} value {:?} is reserved for the bundled \
                                 [hostsocket] board",
                                entry.metadata,
                                crate::hostsocket::BUNDLED_HOSTSOCKET_ROM,
                            ));
                        }
                    }
                    wasm_boards.push(WasmBoardConfig {
                        spec,
                        wasm_path,
                        manifest,
                    });
                }
            }
        }
        let chipset = match raw.chipset.revision.as_deref() {
            None => defaults.chipset,
            Some(s) => parse_chipset(s)?,
        };
        let video_standard = match raw.chipset.video.as_deref() {
            None => defaults.video_standard,
            Some(s) => parse_video_standard(s)?,
        };
        let audio = AudioConfig {
            floppy_sounds: raw
                .audio
                .floppy_sounds
                .unwrap_or(defaults.audio.floppy_sounds),
            floppy_sounds_volume: match raw.audio.floppy_sounds_volume {
                None => defaults.audio.floppy_sounds_volume,
                Some(v) if v <= 100 => v as u8,
                Some(v) => {
                    errors.push(anyhow!(
                        "[audio] floppy_sounds_volume must be 0-100, got {v}"
                    ));
                    defaults.audio.floppy_sounds_volume
                }
            },
            output_device: raw
                .audio
                .output_device
                .clone()
                .filter(|name| !name.trim().is_empty()),
            output_enabled: raw
                .audio
                .output_enabled
                .unwrap_or(defaults.audio.output_enabled),
            channel_mode: match raw.audio.channel_mode.as_deref() {
                None => defaults.audio.channel_mode,
                Some(s) => match parse_channel_mode(s) {
                    Ok(mode) => mode,
                    Err(e) => {
                        errors.push(e);
                        defaults.audio.channel_mode
                    }
                },
            },
            stereo_separation: match raw.audio.stereo_separation {
                None => defaults.audio.stereo_separation,
                Some(v) if v <= 100 => v as u8,
                Some(v) => {
                    errors.push(anyhow!("[audio] stereo_separation must be 0-100, got {v}"));
                    defaults.audio.stereo_separation
                }
            },
            filter: match raw.audio.audio_filter.as_deref() {
                None => defaults.audio.filter,
                Some(s) => match parse_audio_filter_mode(s) {
                    Ok(mode) => mode,
                    Err(e) => {
                        errors.push(e);
                        defaults.audio.filter
                    }
                },
            },
        };
        let (floppy, floppy_connected, floppy_playlists) = parse_floppy(raw.floppy)?;
        let overscan = match raw.display.overscan.as_deref() {
            None => defaults.overscan,
            Some(s) => parse_overscan(s)?,
        };
        let pixel_aspect = match raw.display.pixel_aspect.as_deref() {
            None => defaults.pixel_aspect,
            Some(s) => parse_pixel_aspect(s)?,
        };
        let scaling = match raw.display.scaling.as_deref() {
            None => defaults.scaling,
            Some(s) => parse_display_scaling(s)?,
        };
        let deinterlace = raw.display.deinterlace.unwrap_or(defaults.deinterlace);
        let phosphor = match raw.display.phosphor {
            None => defaults.phosphor,
            Some(p) if (0.0..=0.95).contains(&p) => p,
            Some(p) => {
                errors.push(anyhow!(
                    "[display] phosphor must be between 0.0 and 0.95, got {p}"
                ));
                defaults.phosphor
            }
        };
        let shader = match raw.display.shader.as_deref() {
            None => defaults.shader.clone(),
            Some(s) => parse_shader(s)?,
        };
        let shader_strength = match raw.display.shader_strength {
            None => defaults.shader_strength,
            Some(p) if (0.0..=1.0).contains(&p) => p,
            Some(p) => {
                errors.push(anyhow!(
                    "[display] shader_strength must be between 0.0 and 1.0, got {p}"
                ));
                defaults.shader_strength
            }
        };
        let bezel = raw.display.bezel.unwrap_or(defaults.bezel);
        let perf_overlay = raw.display.perf_overlay.unwrap_or(defaults.perf_overlay);
        let tint = match raw.display.tint.as_deref() {
            None => defaults.tint,
            Some(s) => parse_tint(s)?,
        };
        let menu_scale = match raw.display.menu_scale.as_deref() {
            None => defaults.menu_scale,
            Some(s) => parse_menu_scale(s)?,
        };
        let full_screen = raw.display.full_screen.unwrap_or(defaults.full_screen);
        let status_bar = raw.display.status_bar.unwrap_or(defaults.status_bar);
        let joystick_input_mode = match raw.input.joystick.as_deref() {
            None => defaults.joystick_input_mode,
            Some(s) => parse_joystick_input_mode(s)?,
        };
        let mouse_sensitivity = match raw.input.mouse_sensitivity {
            None => defaults.mouse_sensitivity,
            Some(v) if v <= 100 => v as u8,
            Some(v) => {
                errors.push(anyhow!("[input] mouse_sensitivity must be 0-100, got {v}"));
                defaults.mouse_sensitivity
            }
        };
        let mouse_capture = match raw.input.mouse_capture.as_deref() {
            None => defaults.mouse_capture,
            Some(s) => parse_mouse_capture(s)?,
        };
        // An implausibly fast autofire is a typo, not a preference: at more
        // than ~30 Hz the pulse is shorter than the frame the guest samples
        // it on, so the button would read as noise or as never pressed.
        let autofire_hz = match raw.input.autofire_hz {
            None => defaults.autofire_hz,
            Some(hz) if hz <= AUTOFIRE_MAX_HZ => hz,
            Some(hz) => {
                errors.push(anyhow!(
                    "[input] autofire_hz must be 0 (off) to {AUTOFIRE_MAX_HZ}, got {hz}"
                ));
                defaults.autofire_hz
            }
        };
        // The profile carries the default wiring (mouse + joystick, with a
        // CD32 pad on the CD32 profile); an explicit key beats it either
        // way -- a real CD32 accepts any controller too.
        let port_devices = [
            match raw.input.port1.as_deref() {
                None => defaults.port_devices[0],
                Some(s) => parse_port_device(s, "port1")?,
            },
            match raw.input.port2.as_deref() {
                None => defaults.port_devices[1],
                Some(s) => parse_port_device(s, "port2")?,
            },
        ];
        let serial = SerialConfig {
            mode: match raw.serial.mode.as_deref() {
                None => defaults.serial.mode,
                Some(s) => parse_serial_mode(s)?,
            },
            midi_out: raw.serial.midi_out.clone(),
            midi_in: raw.serial.midi_in.clone(),
            mt32_control_rom: raw.serial.mt32_control_rom.as_ref().map(PathBuf::from),
            mt32_lcd: match raw.serial.mt32_lcd.as_deref() {
                None => defaults.serial.mt32_lcd,
                Some(s) => parse_mt32_lcd(s)?,
            },
            mt32_pcm_rom: raw.serial.mt32_pcm_rom.as_ref().map(PathBuf::from),
            mt32_panel: raw.serial.mt32_panel.unwrap_or(defaults.serial.mt32_panel),
            listen: raw.serial.listen.clone(),
            connect: raw.serial.connect.clone(),
        };

        let ide = IdeConfig {
            master: raw.ide.master.map(drive_image).transpose()?,
            slave: raw.ide.slave.map(drive_image).transpose()?,
        };
        // Two machines have an IDE port: a Gayle one (A600/A1200) and the
        // A4000's, which is the same ATA cable off the Fat Gary bus. `[ide]`
        // fits either; nothing else has anywhere to put the drives.
        let has_ide_port = defaults.gate_array.gayle_id().is_some() || defaults.ide_a4000;
        if (ide.master.is_some() || ide.slave.is_some()) && !has_ide_port {
            errors.push(anyhow!(
                "[ide] images need a machine with an IDE port: set [machine] profile = \"A600\" \
                 (or A1200, or A4000)"
            ));
        }
        // The IDE interfaces speak plain ATA, not ATAPI: a CD image on the
        // cable would be served as a garbage hard disk.
        for drive in [&ide.master, &ide.slave].into_iter().flatten() {
            if is_cd_image_path(&drive.path) {
                errors.push(anyhow!(
                    "[ide] {}: CD images attach a CD-ROM drive on the SCSI bus \
                     ([scsi] unit0..unit6); the IDE port has no ATAPI support",
                    drive.path.display()
                ));
            }
        }

        let scsi_controller = match raw.scsi.controller.as_deref() {
            // A machine with a Super DMAC already has a SCSI bus, so drives go
            // on it unless the config asks for a Zorro board instead.
            None if defaults.sdmac => ScsiController::A3000,
            None => ScsiController::A2091,
            Some(raw_ctrl) => match raw_ctrl.trim().to_ascii_lowercase().as_str() {
                "a2091" => ScsiController::A2091,
                "a4091" => ScsiController::A4091,
                "a3000" => ScsiController::A3000,
                _ => {
                    errors.push(anyhow!(
                        "[scsi] controller = {raw_ctrl:?} is not known \
                         (expected \"a2091\", \"a4091\", or \"a3000\")"
                    ));
                    ScsiController::A2091
                }
            },
        };
        let rtg = match raw.rtg.card.as_deref() {
            None => defaults.rtg,
            Some(raw_card) => match raw_card.trim().to_ascii_lowercase().as_str() {
                "none" => RtgCard::None,
                "z3660" => RtgCard::Z3660,
                "picasso2" => RtgCard::Picasso2,
                "picasso2plus" | "picasso2+" => RtgCard::Picasso2Plus,
                _ => {
                    errors.push(anyhow!(
                        "[rtg] card = {raw_card:?} is not known \
                         (expected \"z3660\", \"picasso2\", \"picasso2plus\", or \"none\")"
                    ));
                    RtgCard::None
                }
            },
        };
        // Only the Picasso II cards have configurable display memory; other
        // cards ignore [rtg] vram entirely, so a leftover value must not
        // fail an unrelated configuration.
        let rtg_vram_bytes = if matches!(rtg, RtgCard::Picasso2 | RtgCard::Picasso2Plus) {
            match raw.rtg.vram.as_deref() {
                None => defaults.rtg_vram_bytes,
                Some(value) => parse_size(value, "RTG VRAM")?,
            }
        } else {
            defaults.rtg_vram_bytes
        };

        let mut scsi = ScsiConfig {
            controller: scsi_controller,
            rom: raw.scsi.rom.map(PathBuf::from),
            rom_odd: raw.scsi.rom_odd.map(PathBuf::from),
            units: [
                raw.scsi.unit0.map(drive_image).transpose()?,
                raw.scsi.unit1.map(drive_image).transpose()?,
                raw.scsi.unit2.map(drive_image).transpose()?,
                raw.scsi.unit3.map(drive_image).transpose()?,
                raw.scsi.unit4.map(drive_image).transpose()?,
                raw.scsi.unit5.map(drive_image).transpose()?,
                raw.scsi.unit6.map(drive_image).transpose()?,
            ],
        };
        // An explicitly-fitted A4091 with no ROM named defaults to the bundled
        // one (resolved to a real path later). This also fits the board with no
        // drives, exactly as naming a ROM always has -- the setup for booting a
        // CD inserted at runtime.
        if scsi.controller == ScsiController::A4091 && scsi.rom.is_none() {
            scsi.rom = Some(PathBuf::from(BUNDLED_A4091_ROM));
        }
        if scsi.enabled() && scsi.rom.is_none() && scsi.controller.is_zorro_board() {
            errors.push(anyhow!(
                "[scsi] drives need the boot ROM: set [scsi] rom = \"...\" \
                 (an A590/A2091 6.x ROM image; its scsi.device drives the disks)"
            ));
        }
        // The motherboard SCSI is silicon, not a card: it has no boot ROM (the
        // Kickstart carries its driver), and it only exists where the Super
        // DMAC does.
        if scsi.controller == ScsiController::A3000 {
            if !defaults.sdmac {
                errors.push(anyhow!(
                    "[scsi] controller = \"a3000\" is the motherboard SCSI: set \
                     [machine] profile = \"A3000\", or fit a Zorro board with \
                     controller = \"a2091\" (or \"a4091\")"
                ));
            }
            if scsi.rom.is_some() {
                errors.push(anyhow!(
                    "[scsi] rom does not apply to the A3000 motherboard SCSI: it has no \
                     boot ROM, Kickstart's own scsi.device drives it"
                ));
            }
        }
        if scsi.rom_odd.is_some() && scsi.controller != ScsiController::A2091 {
            errors.push(anyhow!(
                "[scsi] rom_odd is an A2091 split-EPROM option; the A4091 has a single rom"
            ));
        }
        if scsi.rom_odd.is_some() && scsi.rom.is_none() {
            errors.push(anyhow!("[scsi] rom_odd needs rom (the even EPROM half)"));
        }

        let a2065_net = match (&raw.a2065.net, &raw.a2065.interface) {
            (None, None) => None,
            (None, Some(_)) => {
                return Err(anyhow!(
                    "[a2065] interface needs net = \"bridge\" (or use --a2065-interface)"
                ));
            }
            (Some(s), interface) => {
                let config = crate::net::parse_net_config(s, interface.as_deref())
                    .map_err(|error| anyhow::anyhow!("[a2065] {error}"))?;
                if interface.is_some() && !matches!(&config, crate::net::NetConfig::Bridge { .. }) {
                    return Err(anyhow!(
                        "[a2065] interface applies only to net = \"bridge\""
                    ));
                }
                Some(config)
            }
        };

        // `[hostsocket]` expands to the bundled WASM plugin board (see
        // crate::hostsocket), appended after any [[zorro]] metadata boards.
        // `net = "host"` is not one of `crate::net::NetConfig`'s own
        // backends at all -- it selects the Amiberry-style host-socket
        // backend (`crate::hostsocket::board_config`'s own `transport`
        // parameter) instead of routing TCP/UDP through this board's
        // smoltcp stack. The underlying smoltcp interface still needs a
        // real (if now mostly unused -- ICMP and DNS-over-`net` are the
        // only things still on it, see `resolver` below) `net` backend of
        // its own, hardcoded to `"loopback"` under `"host"`: keeping this
        // mode's whole zero-config premise (no interface address, no
        // gateway, no pcap/TAP privileges) means it can't be user-picked
        // here the way `"bridge"`'s own address/gateway are.
        let (hostsocket_net, hostsocket_transport) =
            match (&raw.hostsocket.net, &raw.hostsocket.interface) {
                (None, None) => (None, None),
                (None, Some(_)) => {
                    return Err(anyhow!(
                        "[hostsocket] interface needs net = \"bridge\" (or use \
                         --hostsocket-interface)"
                    ));
                }
                (Some(s), interface) if s.trim().eq_ignore_ascii_case("host") => {
                    if interface.is_some() {
                        return Err(anyhow!(
                            "[hostsocket] interface applies only to net = \"bridge\""
                        ));
                    }
                    if raw.hostsocket.address.is_some() || raw.hostsocket.gateway.is_some() {
                        return Err(anyhow!(
                            "[hostsocket] address/gateway don't apply to net = \"host\" (it \
                             has no interface address of its own -- sockets go out on the \
                             host's own network identity, see docs/guide/configuration.md)"
                        ));
                    }
                    (
                        Some(crate::net::NetConfig::Loopback),
                        Some("host".to_string()),
                    )
                }
                (Some(s), interface) => {
                    let config = crate::net::parse_net_config(s, interface.as_deref())
                        .map_err(|error| anyhow::anyhow!("[hostsocket] {error}"))?;
                    if interface.is_some()
                        && !matches!(&config, crate::net::NetConfig::Bridge { .. })
                    {
                        return Err(anyhow!(
                            "[hostsocket] interface applies only to net = \"bridge\""
                        ));
                    }
                    (Some(config), None)
                }
            };
        let hostsocket_resolver = match raw.hostsocket.resolver.as_deref() {
            // No explicit choice: default to the host OS resolver under
            // net = "nat"/"bridge"/"host" -- the thing that works without
            // any dns_server hand-configured to match the backend
            // (bridge's own default dns_server is NAT's virtual forwarder
            // address, unreachable on a real LAN; "host" mode's own
            // smoltcp interface is hardcoded to "loopback", which
            // wouldn't reach a real dns_server at all). Set
            // resolver = "dns" explicitly (with dns_server, if a specific
            // one is wanted) to opt back into the board's own
            // DNS-over-net query. Plain loopback/none have no sane
            // default here ("host" is rejected there below), so they get
            // no resolver key at all -- gethostbyname() stays whatever it
            // already was for those backends.
            None if hostsocket_transport.as_deref() == Some("host") => Some("host".to_string()),
            None => match &hostsocket_net {
                Some(crate::net::NetConfig::Nat) | Some(crate::net::NetConfig::Bridge { .. }) => {
                    Some("host".to_string())
                }
                _ => None,
            },
            Some(s) => {
                let normalized = s.trim().to_ascii_lowercase();
                if normalized != "dns" && normalized != "host" {
                    return Err(anyhow!(
                        "[hostsocket] resolver {s:?} is not one of \"dns\", \"host\""
                    ));
                }
                if normalized == "host"
                    && hostsocket_transport.as_deref() != Some("host")
                    && !matches!(
                        &hostsocket_net,
                        Some(crate::net::NetConfig::Nat)
                            | Some(crate::net::NetConfig::Bridge { .. })
                    )
                {
                    return Err(anyhow!(
                        "[hostsocket] resolver = \"host\" needs net = \"nat\", \"bridge\", or \
                         \"host\" (it would silently defeat \"loopback\"'s own determinism \
                         guarantee)"
                    ));
                }
                Some(normalized)
            }
        };
        if let Some(net) = &hostsocket_net {
            wasm_boards.push(crate::hostsocket::board_config(
                net.clone(),
                raw.hostsocket.dns_server.as_deref(),
                raw.hostsocket.hostname.as_deref(),
                raw.hostsocket.address.as_deref(),
                raw.hostsocket.gateway.as_deref(),
                hostsocket_resolver.as_deref(),
                hostsocket_transport.as_deref(),
            ));
        }

        // The A500 Rev 6A is both the "A500" profile and the no-profile
        // default machine (the most common, most-targeted Amiga): the Fatter
        // 8372A Agnus with the original OCS 8362 Denise. An explicit [chipset]
        // revision is an intentional override, so it re-derives the chips from
        // the generic preset instead -- e.g. revision = "OCS" forces a plain
        // 8371/8362 machine even under profile = "A500". cfg.machine, the gate
        // array, and the descriptor machine id are unaffected.
        let a500_default =
            raw.chipset.revision.is_none() && matches!(machine, None | Some(MachineModel::A500));
        let agnus_revision = match raw.chipset.agnus.as_deref() {
            None => match machine {
                // The A500+ (Rev 8A) and A600 boards have the 2 MB "Super Fat"
                // 8375 soldered on, regardless of preset or fitted chip RAM.
                Some(MachineModel::A500Plus | MachineModel::A600) => AgnusRevision::Ecs8375,
                // The A500 Rev 6A / default machine has the 1 MB 8372A. Pinning
                // it keeps the authentic 1 MiB chip-RAM ceiling, so fitting
                // more is rejected by validate_chip_ram rather than silently
                // promoted to an 8375.
                _ if a500_default => AgnusRevision::Ecs8372Rev4,
                // Everything else -- the A1200's AGA preset (Alice) or an
                // explicit revision preset -- picks by preset + fitted chip RAM.
                _ => default_agnus_revision(chipset, chip_ram_bytes),
            },
            Some(s) => parse_agnus_revision(s)?,
        };
        let denise_revision = match raw.chipset.denise.as_deref() {
            // The A500 Rev 6A / default machine pairs its ECS Agnus with the
            // original 8362 OCS Denise (no superhires/BRDRBLNK). Every other
            // machine, and any explicit revision preset, takes the Denise that
            // matches its preset.
            None if a500_default => DeniseRevision::Ocs,
            None => default_denise_revision(chipset),
            Some(s) => parse_denise_revision(s)?,
        };

        let mem_controller = match raw.machine.mem_controller.as_deref() {
            None => defaults.mem_controller,
            Some("none") => MemController::None,
            Some("ramsey-04") => MemController::Ramsey4,
            Some("ramsey-07") => MemController::Ramsey7,
            Some(other) => anyhow::bail!(
                "[machine] mem_controller {other:?} is not one of \
                 none, ramsey-04, ramsey-07"
            ),
        };

        errors.extend(validate_chip_ram(chip_ram_bytes, chipset, agnus_revision).err());
        errors.extend(validate_fast_ram(fast_ram_bytes, chip_ram_bytes).err());
        errors.extend(validate_slow_ram(slow_ram_bytes).err());
        errors.extend(validate_mb_ram(mb_ram_bytes, mem_controller, cpu).err());
        errors.extend(validate_accel_ram(accel_ram_bytes, cpu).err());
        errors.extend(validate_z3_ram(z3_ram_bytes, cpu).err());
        errors.extend(validate_rtg_card(rtg, rtg_vram_bytes, cpu).err());
        let board_specs = zorro_boards
            .iter()
            .chain(wasm_boards.iter().map(|w| &w.spec));
        for board in board_specs {
            if board.version == ZorroVersion::III && !cpu_has_32bit_bus(cpu) {
                errors.push(anyhow!(
                    "zorro board {:?} is Zorro III, which needs a 32-bit CPU \
                     (68020/68030/68040); {:?} has a 24-bit address bus",
                    board.name,
                    cpu
                ));
            }
        }
        let cd_insert_delay_secs = match raw.cd.insert_delay {
            Some(secs) if secs.is_finite() && secs >= 0.0 => secs,
            Some(_) => {
                errors.push(anyhow!("[cd] insert_delay must be a non-negative number"));
                0.0
            }
            None => 0.0,
        };
        let rtc_seed_unix = match &raw.machine.rtc_time {
            Some(RawRtcTime::Unix(n)) => match u64::try_from(*n) {
                Ok(secs) => Some(secs),
                Err(_) => {
                    errors.push(anyhow!(
                        "[machine] rtc_time must be non-negative Unix seconds \
                         (1970 or later)"
                    ));
                    None
                }
            },
            Some(RawRtcTime::Text(s)) => match crate::rtc::parse_rtc_time(s) {
                Ok(secs) => Some(secs),
                Err(e) => {
                    errors.push(anyhow!("[machine] rtc_time: {e}"));
                    None
                }
            },
            None => None,
        };
        let rtc_frozen = raw.machine.rtc_frozen.unwrap_or(false);
        if rtc_frozen && raw.machine.rtc_time.is_none() {
            errors.push(anyhow!(
                "[machine] rtc_frozen = true needs an rtc_time to freeze at"
            ));
        }
        let rtc_chip = match raw.machine.rtc_chip.as_deref().map(parse_rtc_chip) {
            Some(Ok(chip)) => Some(chip),
            Some(Err(e)) => {
                errors.push(e);
                None
            }
            None => None,
        };

        match errors.len() {
            0 => {}
            1 => return Err(errors.remove(0)),
            _ => {
                let mut msg = String::from("configuration has multiple errors:");
                for e in &errors {
                    msg.push_str(&format!("\n  - {e:#}"));
                }
                bail!("{msg}");
            }
        }

        let rtc_present = match raw.machine.rtc {
            // A configured time on an explicitly unfitted clock would
            // silently do nothing; make the contradiction loud.
            Some(false) if rtc_seed_unix.is_some() => anyhow::bail!(
                "[machine] rtc_time is set but rtc = false leaves the \
                 clock unfitted; drop one of them"
            ),
            // Naming a chip for a socket declared empty is the same
            // contradiction.
            Some(false) if rtc_chip.is_some() => anyhow::bail!(
                "[machine] rtc_chip is set but rtc = false leaves the \
                 clock unfitted; drop one of them"
            ),
            Some(fitted) => fitted,
            None => defaults.rtc_present || rtc_seed_unix.is_some() || rtc_chip.is_some(),
        };
        let rtc_chip = rtc_chip.unwrap_or(defaults.rtc_chip);
        let rp5c01_fitted = rtc_present && rtc_chip == crate::rtc::RtcChip::Rp5c01;
        let battmem_path = match raw.machine.battmem.as_deref() {
            // An empty path keeps the battery registers session-only.
            Some("") => None,
            // A backing file for battery RAM the machine does not have
            // would silently never fill; make the contradiction loud.
            Some(path) if !rp5c01_fitted => anyhow::bail!(
                "[machine] battmem ({path}) backs the RP5C01's battery RAM, \
                 but this machine has no RP5C01 fitted; set \
                 rtc_chip = \"RP5C01\" or drop battmem"
            ),
            Some(path) => Some(PathBuf::from(path)),
            None => rp5c01_fitted.then(|| PathBuf::from("battmem.nvram")),
        };

        Ok(Config {
            rom_path: raw.rom.map(PathBuf::from).unwrap_or(defaults.rom_path),
            cpu,
            fpu,
            cpu_clock_mhz,
            cpu_icache,
            cpu_dcache,
            cpu_unimplemented,
            cpu_jit,
            emulation,
            chip_ram_bytes,
            fast_ram_bytes,
            slow_ram_bytes,
            mb_ram_bytes,
            accel_ram_bytes,
            z3_ram_bytes,
            zorro_boards,
            wasm_boards,
            identify_board: raw.identify.unwrap_or(defaults.identify_board),
            filesys: raw
                .filesys
                .iter()
                .map(|m| crate::filesys::MountSpec {
                    path: std::path::PathBuf::from(&m.path),
                    volume: m.volume.clone().unwrap_or_else(|| {
                        std::path::Path::new(&m.path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "HostFS".into())
                    }),
                    boot_pri: m.bootpri.unwrap_or(-128),
                    readonly: m.readonly.unwrap_or(false),
                })
                .collect(),
            chipset,
            agnus_revision,
            denise_revision,
            machine,
            gate_array: defaults.gate_array,
            ide_a4000: defaults.ide_a4000,
            sdmac: defaults.sdmac,
            // The ROM's scsi.device is pure probe time when the machine's
            // built-in disk controller (Gayle or A4000 IDE, A3000 SDMAC SCSI)
            // has no drives on it: disable it then. With drives it is their
            // boot path and runs; machines with no built-in controller carry
            // no scsi.device in ROM, so there is nothing to disable.
            rom_scsi_device_disable: raw.machine.rom_scsi_device_disable.unwrap_or({
                let builtin_drives = (has_ide_port
                    && (ide.master.is_some() || ide.slave.is_some()))
                    || (defaults.sdmac
                        && scsi.controller == ScsiController::A3000
                        && scsi.units.iter().any(Option::is_some));
                (has_ide_port || defaults.sdmac) && !builtin_drives
            }),
            akiko: defaults.akiko,
            cdtv_cd: defaults.cdtv_cd,
            extended_rom_path: raw
                .extended_rom
                .map(PathBuf::from)
                .or(defaults.extended_rom_path),
            cd_image_path: raw.cd.image.map(PathBuf::from),
            cd_insert_delay_secs,
            cd32_nvram_path: raw
                .cd
                .nvram
                .map(PathBuf::from)
                .or_else(|| defaults.akiko.then(|| PathBuf::from("cd32-nvram.bin"))),
            rtc_present,
            rtc_chip,
            rtc_seed_unix,
            rtc_frozen,
            battmem_path,
            log_unmapped: raw
                .debug
                .log_unmapped
                .as_deref()
                .map(parse_log_unmapped)
                .transpose()?,
            validate_chipset: raw.debug.validate_chipset,
            detect_smc: raw.debug.detect_smc,
            mem_controller,
            video_standard,
            audio,
            ide,
            scsi,
            a2065_net,
            hostsocket_net,
            rtg,
            rtg_vram_bytes,
            floppy,
            floppy_connected,
            floppy_playlists,
            overscan,
            pixel_aspect,
            scaling,
            deinterlace,
            phosphor,
            shader,
            shader_strength,
            bezel,
            perf_overlay,
            tint,
            menu_scale,
            full_screen,
            status_bar,
            joystick_input_mode,
            mouse_sensitivity,
            mouse_capture,
            autofire_hz,
            port_devices,
            serial,
            parallel: resolve_parallel(raw.parallel)?,
        })
    }
}

/// Resolve `[parallel]` into a [`ParallelConfig`]. An explicit `device` selects
/// the peripheral; with none set, a bare `output` path implies a printer
/// (back-compat with the original `[parallel] output = "..."`) and otherwise the
/// port is empty. Rejects a printer with no capture path and an out-of-range
/// sampler gain.
fn resolve_parallel(raw: RawParallel) -> Result<ParallelConfig> {
    let device = match raw.device.as_deref() {
        Some(s) => parse_parallel_device(s)?,
        None if raw.output.is_some() => ParallelDevice::Printer,
        None => ParallelDevice::None,
    };
    if device == ParallelDevice::Printer && raw.output.is_none() {
        bail!("[parallel] device = \"printer\" needs an output path (output = \"...\")");
    }
    let sampler_gain_db = raw.sampler_gain.unwrap_or(0.0);
    let gain_range = crate::sampler::MIN_SAMPLER_GAIN_DB..=crate::sampler::MAX_SAMPLER_GAIN_DB;
    if device == ParallelDevice::Sampler
        && (!sampler_gain_db.is_finite() || !gain_range.contains(&sampler_gain_db))
    {
        bail!(
            "[parallel] sampler_gain must be between {} and {} dB, got {sampler_gain_db}",
            crate::sampler::MIN_SAMPLER_GAIN_DB,
            crate::sampler::MAX_SAMPLER_GAIN_DB
        );
    }
    Ok(ParallelConfig {
        device,
        printer_output: raw.output.map(PathBuf::from),
        sampler_input: raw.sampler_input,
        sampler_gain_db,
    })
}

pub(crate) fn parse_parallel_device(s: &str) -> Result<ParallelDevice> {
    match s.trim().to_ascii_lowercase().as_str() {
        "none" | "off" => Ok(ParallelDevice::None),
        "printer" => Ok(ParallelDevice::Printer),
        "sampler" => Ok(ParallelDevice::Sampler),
        other => bail!(
            "[parallel] device must be \"none\", \"printer\", or \"sampler\", got \"{other}\""
        ),
    }
}

pub(crate) fn parse_overscan(s: &str) -> Result<Overscan> {
    match s.trim().to_ascii_lowercase().as_str() {
        "full" => Ok(Overscan::Full),
        "tv" => Ok(Overscan::Tv),
        other => bail!("[display] overscan must be \"full\" or \"tv\", got \"{other}\""),
    }
}

pub(crate) fn parse_pixel_aspect(s: &str) -> Result<PixelAspect> {
    match s.trim().to_ascii_lowercase().as_str() {
        "tv" => Ok(PixelAspect::Tv),
        "square" => Ok(PixelAspect::Square),
        other => bail!("[display] pixel_aspect must be \"tv\" or \"square\", got \"{other}\""),
    }
}

pub(crate) fn parse_display_scaling(s: &str) -> Result<DisplayScaling> {
    match s.trim().to_ascii_lowercase().as_str() {
        "smooth" => Ok(DisplayScaling::Smooth),
        "integer" => Ok(DisplayScaling::Integer),
        other => bail!("[display] scaling must be \"smooth\" or \"integer\", got \"{other}\""),
    }
}

/// Parse a `[display] shader` value: a preset name ("off" is accepted for
/// "none", so [`ShaderKind::label`] round-trips), or the path of a `.wgsl`
/// file, which is kept verbatim since host paths are case-sensitive.
/// Whether the file exists is the loader's business, not the parser's:
/// a missing custom shader falls back to no shader rather than failing
/// the whole config.
pub(crate) fn parse_shader(s: &str) -> Result<ShaderMode> {
    let s = s.trim();
    match s.to_ascii_lowercase().as_str() {
        "none" | "off" => Ok(ShaderMode::None),
        "scanlines" => Ok(ShaderMode::Scanlines),
        "mask" => Ok(ShaderMode::Mask),
        "crt" => Ok(ShaderMode::Crt),
        lower if lower.ends_with(".wgsl") => Ok(ShaderMode::Custom(PathBuf::from(s))),
        _ => Err(anyhow!(
            "[display] shader must be \"none\", \"scanlines\", \"mask\", \"crt\", \
             or a \".wgsl\" file path, got {:?}",
            s
        )),
    }
}

/// Parse a `[display] tint` value ("off" is accepted for "none", so
/// [`Tint::label`] round-trips).
pub(crate) fn parse_tint(s: &str) -> Result<Tint> {
    match s.trim().to_ascii_lowercase().as_str() {
        "none" | "off" => Ok(Tint::None),
        "bw" => Ok(Tint::Bw),
        "green" => Ok(Tint::Green),
        "amber" => Ok(Tint::Amber),
        "sepia" => Ok(Tint::Sepia),
        other => bail!(
            "[display] tint must be \"none\", \"bw\", \"green\", \"amber\", \
             or \"sepia\", got \"{other}\""
        ),
    }
}

/// Parse a `[display] menu_scale` value.
pub(crate) fn parse_menu_scale(s: &str) -> Result<MenuScale> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1x" | "1" | "normal" => Ok(MenuScale::Normal),
        "2x" | "2" | "large" => Ok(MenuScale::Large),
        other => bail!("[display] menu_scale must be \"1x\" or \"2x\", got \"{other}\""),
    }
}

pub(crate) fn parse_port_device(s: &str, key: &str) -> Result<PortDevice> {
    PortDevice::parse(s).ok_or_else(|| {
        anyhow!(
            "[input] {key} must be \"mouse\", \"joystick\", \"cd32\", \
             \"analogue\", or \"none\", got {s:?}"
        )
    })
}

/// Display label for a mouse sensitivity value: the neutral midpoint shows as
/// "Default" in the GUI and OSD, every other value as its number. The config
/// and CLI still use the number 50.
pub(crate) fn mouse_sensitivity_label(sensitivity: u8) -> String {
    if sensitivity == 50 {
        "Default".to_string()
    } else {
        sensitivity.to_string()
    }
}

pub(crate) fn parse_joystick_input_mode(s: &str) -> Result<JoystickInputMode> {
    match s.trim().to_ascii_lowercase().as_str() {
        // "auto" is retained as a backward-compatibility alias for older configs
        // and `--joystick auto`; the auto-detect mode was removed in favour of
        // the two explicit, always-visible modes, so it now maps to the default.
        "auto" | "gamepad" | "pad" | "joystick" | "joy" => Ok(JoystickInputMode::Gamepad),
        "keyboard" | "kbd" | "key" => Ok(JoystickInputMode::Keyboard),
        _ => Err(anyhow!(
            "unknown [input] joystick {:?}: expected \"gamepad\" or \"keyboard\"",
            s
        )),
    }
}

pub(crate) fn parse_mouse_capture(s: &str) -> Result<MouseCapture> {
    match s.trim().to_ascii_lowercase().as_str() {
        "click" | "on-click" => Ok(MouseCapture::Click),
        "auto" | "focus" => Ok(MouseCapture::Auto),
        "manual" | "off" | "none" => Ok(MouseCapture::Manual),
        _ => Err(anyhow!(
            "unknown [input] mouse_capture {:?}: expected \"click\", \"auto\", or \"manual\"",
            s
        )),
    }
}

pub(crate) fn parse_serial_mode(s: &str) -> Result<SerialMode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => Ok(SerialMode::Off),
        "stdout" | "terminal" => Ok(SerialMode::Stdout),
        "midi" => Ok(SerialMode::Midi),
        "tcp" => Ok(SerialMode::Tcp),
        "tcp-connect" => Ok(SerialMode::TcpConnect),
        "pty" => Ok(SerialMode::Pty),
        _ => Err(anyhow!(
            "unknown [serial] mode {:?}: expected \"off\", \"stdout\", \"midi\", \"tcp\", \
             \"tcp-connect\", or \"pty\"",
            s
        )),
    }
}

fn parse_pacing_budget(s: &str) -> Result<PacingBudget> {
    match s.trim().to_ascii_lowercase().as_str() {
        "cycles" | "m68k-cycles" => Ok(PacingBudget::Cycles),
        "instructions" | "retired-instructions" => Ok(PacingBudget::Instructions),
        _ => Err(anyhow!(
            "unknown emulation pacing_budget {:?}: expected \"cycles\" or \"instructions\"",
            s
        )),
    }
}

fn parse_warp_speed(s: &str) -> Result<WarpSpeed> {
    match s.trim().to_ascii_lowercase().as_str() {
        "2x" | "2" => Ok(WarpSpeed::X2),
        "4x" | "4" => Ok(WarpSpeed::X4),
        "8x" | "8" => Ok(WarpSpeed::X8),
        "16x" | "16" => Ok(WarpSpeed::X16),
        "max" | "unlimited" => Ok(WarpSpeed::Max),
        _ => Err(anyhow!(
            "unknown emulation warp_speed {:?}: expected \"2x\", \"4x\", \"8x\", \"16x\", or \"max\"",
            s
        )),
    }
}

fn parse_cpu(s: &str) -> Result<CpuModel> {
    let norm = s.trim().to_ascii_lowercase().replace(['m', '_', '-'], "");
    match norm.as_str() {
        "68000" | "000" => Ok(CpuModel::M68000),
        "68010" | "010" => Ok(CpuModel::M68010),
        "68ec020" | "ec020" => Ok(CpuModel::M68EC020),
        "68020" | "020" => Ok(CpuModel::M68020),
        "68030" | "030" => Ok(CpuModel::M68030),
        "68040" | "040" => Ok(CpuModel::M68040),
        "68060" | "060" => Ok(CpuModel::M68060),
        _ => Err(anyhow!(
            "unknown cpu model {:?}: expected 68000 / 68010 / 68EC020 / 68020 / 68030 / 68040 / 68060",
            s
        )),
    }
}

fn parse_chipset(s: &str) -> Result<Chipset> {
    match s.trim().to_ascii_uppercase().as_str() {
        "OCS" => Ok(Chipset::Ocs),
        "ECS" => Ok(Chipset::Ecs),
        "AGA" => Ok(Chipset::Aga),
        _ => Err(anyhow!("unknown chipset {:?}: expected OCS / ECS / AGA", s)),
    }
}

/// Parse `[debug] log_unmapped`: `all`, or a hex `START-END` range with an
/// inclusive end (e.g. `"DD0000-DEFFFF"`, or `"0x00DD0000-0x00DEFFFF"`).
pub(crate) fn parse_log_unmapped(s: &str) -> Result<std::ops::RangeInclusive<u32>> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("all") {
        return Ok(0..=u32::MAX);
    }
    let hex = |v: &str| -> Result<u32> {
        let v = v.trim();
        let digits = v
            .strip_prefix("0x")
            .or_else(|| v.strip_prefix("0X"))
            .unwrap_or(v);
        u32::from_str_radix(digits, 16)
            .with_context(|| format!("[debug] log_unmapped: {v:?} is not a hex address"))
    };
    let (start, end) = s
        .split_once('-')
        .ok_or_else(|| anyhow!("[debug] log_unmapped {s:?}: expected \"all\" or \"START-END\""))?;
    let (start, end) = (hex(start)?, hex(end)?);
    if start > end {
        bail!("[debug] log_unmapped {s:?}: start must not be above end");
    }
    Ok(start..=end)
}

/// Parse a machine model name (`"A500"`, `"A1200"`, ...) as the `--model`
/// flag and `[machine] profile` accept it: case-insensitive, with `_`/`-`/
/// spaces ignored. Public for alternative frontends (the browser build) that
/// take a model name from their own UI.
pub fn parse_machine_model(s: &str) -> Result<MachineModel> {
    let norm = s.trim().to_ascii_uppercase().replace(['_', '-', ' '], "");
    match norm.as_str() {
        "A1000" => Ok(MachineModel::A1000),
        "A500" => Ok(MachineModel::A500),
        "A500OCS" => Ok(MachineModel::A500Ocs),
        "A500PLUS" | "A500+" => Ok(MachineModel::A500Plus),
        "A600" => Ok(MachineModel::A600),
        "A1200" => Ok(MachineModel::A1200),
        "A3000" => Ok(MachineModel::A3000),
        "A4000" => Ok(MachineModel::A4000),
        "CDTV" => Ok(MachineModel::Cdtv),
        "CD32" => Ok(MachineModel::Cd32),
        _ => Err(anyhow!(
            "unknown machine model {:?}: expected A1000 / A500 / A500OCS / A500Plus / A600 / A1200 / A3000 / A4000 / CDTV / CD32",
            s
        )),
    }
}

/// The defaults a `[machine] profile` supplies before the explicit
/// `[cpu]`/`[chipset]`/`[memory]` sections override them. Also the way an
/// alternative frontend builds a stock machine of a given model without a
/// config file, as the desktop launcher and the browser build do.
pub fn machine_profile_defaults(model: MachineModel) -> Config {
    let mut d = Config {
        machine: Some(model),
        ..Config::default()
    };
    match model {
        // The A500 Rev 6A board: the ECS "Fatter" 8372A Agnus (1 MiB chip
        // reach plus the software PAL/NTSC switch) paired with the original
        // OCS 8362 Denise, and the common 512 KiB chip + 512 KiB trapdoor
        // slow RAM. The 8372A makes up to 1 MiB chip RAM possible (chip =
        // "1M" / --chip 1M); the Denise stays OCS, so this is an Agnus-only
        // ECS machine, not a full-ECS A500+. The 8372A/8362 pairing is pinned
        // in the agnus/denise derivation below. A bare 512 KiB machine is
        // still available with `[memory] slow = "0"` or `--slow 0`.
        MachineModel::A500 => {
            d.chipset = Chipset::Ecs;
        }
        // The original Amiga: OCS 8361/8367 Agnus + OCS 8362 Denise, 256 KiB
        // stock chip RAM, no trapdoor slow RAM, no RTC. The `rom` is the
        // 64 KiB bootstrap ROM and the Kickstart disk goes in DF0; the boot
        // ROM loads it into the WCS at $FC0000 (see Memory::load_a1000).
        MachineModel::A1000 => {
            d.chipset = Chipset::Ocs;
            d.chip_ram_bytes = 256 * 1024;
            d.slow_ram_bytes = 0;
            // No RTC (inherits the default-off).
        }
        // The early A500 (Rev 3/5) / A2000: the 512 KiB OCS "Fat Agnus"
        // (8370/8371) and OCS 8362 Denise, with the same 512 KiB chip +
        // 512 KiB trapdoor slow RAM. This is the pre-Rev-6A machine the
        // default used to be; `revision = "OCS"` gives the same chips.
        MachineModel::A500Ocs => {
            d.chipset = Chipset::Ocs;
        }
        // The A500+ (Rev 8A) has a battery-backed OKI RTC soldered to the
        // motherboard -- one of the few models that ships with a clock.
        MachineModel::A500Plus => {
            d.chipset = Chipset::Ecs;
            d.chip_ram_bytes = 1024 * 1024;
            d.slow_ram_bytes = 0;
            d.rtc_present = true;
        }
        // The base A600 shipped without an RTC (only the A600HD added one);
        // it inherits the default-off, so `[machine] rtc = true` re-fits it.
        MachineModel::A600 => {
            d.chipset = Chipset::Ecs;
            d.chip_ram_bytes = 1024 * 1024;
            d.slow_ram_bytes = 0;
            d.gate_array = GateArray::GayleA600;
        }
        MachineModel::A1200 => {
            d.chipset = Chipset::Aga;
            d.chip_ram_bytes = 2 * 1024 * 1024;
            d.slow_ram_bytes = 0;
            d.cpu = CpuModel::M68EC020;
            d.cpu_clock_mhz = 14.18;
            d.gate_array = GateArray::GayleA1200;
        }
        // The A3000: ECS on a big-box board, a 25 MHz 68030 with a real MMU,
        // and Ramsey-04 in front of the motherboard DRAM. Gary, not Gayle, so
        // no PCMCIA and no Gayle IDE. Its SCSI is a Super DMAC at $DD0000
        // driving a WD33C93; `[scsi]` fits drives to it.
        MachineModel::A3000 => {
            d.chipset = Chipset::Ecs;
            d.chip_ram_bytes = 2 * 1024 * 1024;
            d.slow_ram_bytes = 0;
            // Stock motherboard fast RAM: four banks of 256Kx4 ZIPs.
            d.mb_ram_bytes = 4 * 1024 * 1024;
            d.cpu = CpuModel::M68030;
            d.cpu_clock_mhz = 25.0;
            d.mem_controller = MemController::Ramsey4;
            d.gate_array = GateArray::FatGary;
            d.rtc_present = true;
            // The big boxes carry the Ricoh clock part, not the OKI one --
            // and Linux/m68k hard-assumes RP5C01 on these models.
            d.rtc_chip = crate::rtc::RtcChip::Rp5c01;
            d.sdmac = true;
        }
        // The A4000: the same board a generation later -- AGA, a 25 MHz 68040,
        // and Ramsey-07. Its IDE at $DD2020 is Gayle's ATA cable without the
        // gate array; `[ide]` fits drives to it.
        MachineModel::A4000 => {
            d.chipset = Chipset::Aga;
            d.chip_ram_bytes = 2 * 1024 * 1024;
            d.slow_ram_bytes = 0;
            // Stock motherboard fast RAM: one 4 MiB bank of 1Mx4 SIMMs.
            d.mb_ram_bytes = 4 * 1024 * 1024;
            d.cpu = CpuModel::M68040;
            d.cpu_clock_mhz = 25.0;
            d.mem_controller = MemController::Ramsey7;
            d.gate_array = GateArray::FatGary;
            d.rtc_present = true;
            d.rtc_chip = crate::rtc::RtcChip::Rp5c01;
            d.ide_a4000 = true;
        }
        // CDTV: A500-class board with the 1 MB ECS Agnus and 1 MB chip
        // RAM, plus the 256 KiB extended ROM at $F00000 (configure it via
        // extended_rom = "..."). No Gayle. It carries a battery-backed clock.
        MachineModel::Cdtv => {
            d.chipset = Chipset::Ecs;
            d.chip_ram_bytes = 1024 * 1024;
            d.slow_ram_bytes = 0;
            d.cdtv_cd = true;
            d.rtc_present = true;
        }
        // CD32: AGA, 68EC020 at 14 MHz, 2 MB chip RAM, Akiko, and the
        // 512 KiB extended ROM at $E00000. No Gayle, no RTC (default-off).
        MachineModel::Cd32 => {
            d.chipset = Chipset::Aga;
            d.chip_ram_bytes = 2 * 1024 * 1024;
            d.slow_ram_bytes = 0;
            d.cpu = CpuModel::M68EC020;
            d.cpu_clock_mhz = 14.18;
            d.akiko = true;
            // The bundled controller: lowlevel.library expects the pad's
            // serial button protocol on port 2.
            d.port_devices[1] = PortDevice::Cd32Pad;
        }
    }
    // Hardware that follows from the parts picked above, derived exactly as
    // the raw-config pipeline derives it when the matching [chipset]/[cpu]
    // keys are absent, so a profile built directly (the browser build, the
    // launcher's fallback) is the same machine as a config file naming the
    // profile. Skipping this is how the browser's first A1200 ended up an
    // AGA machine with a 1 MiB-reach ECS Agnus: the chip window mirrors by
    // Agnus reach, so the guest sized 1 MiB of its 2 MiB chip RAM.
    // machine_profile_defaults_match_bare_profile_configs pins the parity.
    d.agnus_revision = match model {
        // The A500+/A600 boards have the 2 MB "Super Fat" 8375 soldered on,
        // regardless of fitted chip RAM.
        MachineModel::A500Plus | MachineModel::A600 => AgnusRevision::Ecs8375,
        // The A500 Rev 6A keeps its pinned 8372A/OCS-Denise pairing (also
        // the no-profile default machine's chips).
        MachineModel::A500 => AgnusRevision::Ecs8372Rev4,
        _ => default_agnus_revision(d.chipset, d.chip_ram_bytes),
    };
    d.denise_revision = match model {
        MachineModel::A500 => DeniseRevision::Ocs,
        _ => default_denise_revision(d.chipset),
    };
    // The FPU and on-chip caches are silicon: present whenever the CPU has
    // them, exactly like the pipeline's [cpu] defaults.
    d.fpu = d.cpu.default_fpu();
    d.cpu_icache = d.cpu.has_instruction_cache();
    d.cpu_dcache = d.cpu.has_data_cache();
    // An RTG card comes fitted wherever the machine can host one, so RTG
    // needs no config step beyond installing the guest driver. The Z3660 is
    // a Zorro III board, so the gate is the same one Zorro III RAM uses: a
    // CPU with a 32-bit address bus. That is the A3000 and A4000 today, and
    // any future profile that qualifies, without a model list to maintain.
    if cpu_has_32bit_bus(d.cpu) {
        d.rtg = RtgCard::Z3660;
    }
    d
}

/// Preset to Agnus mapping: the ECS preset picks the 2 MB 8375 only when
/// more than 1 MB of chip RAM is fitted, so identification and DMA pointer
/// gating match what such a machine would really carry. AGA selects Alice.
fn default_agnus_revision(chipset: Chipset, chip_ram_bytes: usize) -> AgnusRevision {
    match chipset {
        Chipset::Ocs => AgnusRevision::Ocs,
        Chipset::Ecs => {
            if chip_ram_bytes > 1024 * 1024 {
                AgnusRevision::Ecs8375
            } else {
                AgnusRevision::Ecs8372Rev4
            }
        }
        Chipset::Aga => AgnusRevision::AgaAlice,
    }
}

fn default_denise_revision(chipset: Chipset) -> DeniseRevision {
    match chipset {
        Chipset::Ocs => DeniseRevision::Ocs,
        Chipset::Ecs => DeniseRevision::Ecs8373,
        Chipset::Aga => DeniseRevision::AgaLisa,
    }
}

/// Parse `[machine] rtc_chip`. "RF5C01A" is accepted as an alias for the
/// Ricoh part because that is what AmigaOS-lineage sources call it.
fn parse_rtc_chip(s: &str) -> Result<crate::rtc::RtcChip> {
    match s.trim().to_ascii_uppercase().as_str() {
        "MSM6242" | "MSM6242B" | "OKI" => Ok(crate::rtc::RtcChip::Msm6242),
        "RP5C01" | "RP5C01A" | "RF5C01A" | "RICOH" => Ok(crate::rtc::RtcChip::Rp5c01),
        _ => Err(anyhow!(
            "unknown machine rtc_chip {:?}: expected MSM6242 / RP5C01",
            s
        )),
    }
}

fn parse_agnus_revision(s: &str) -> Result<AgnusRevision> {
    match s.trim().to_ascii_uppercase().as_str() {
        "OCS" | "8370" | "8371" => Ok(AgnusRevision::Ocs),
        "8372" | "8372A" => Ok(AgnusRevision::Ecs8372Rev4),
        "8375" | "8372B" => Ok(AgnusRevision::Ecs8375),
        "8374" | "ALICE" => Ok(AgnusRevision::AgaAlice),
        _ => Err(anyhow!(
            "unknown chipset agnus {:?}: expected OCS / 8370 / 8371 / 8372 / 8372A / 8375 / 8374 / ALICE",
            s
        )),
    }
}

fn parse_denise_revision(s: &str) -> Result<DeniseRevision> {
    match s.trim().to_ascii_uppercase().as_str() {
        "OCS" | "8362" => Ok(DeniseRevision::Ocs),
        "ECS" | "8373" => Ok(DeniseRevision::Ecs8373),
        "LISA" | "4203" => Ok(DeniseRevision::AgaLisa),
        _ => Err(anyhow!(
            "unknown chipset denise {:?}: expected OCS / 8362 / ECS / 8373 / LISA / 4203",
            s
        )),
    }
}

/// Public for the browser frontend (crates/copperline-web), whose `WebEmu`
/// constructor takes the same PAL/NTSC names as the `[chipset] video` key,
/// like `parse_machine_model`.
pub fn parse_video_standard(s: &str) -> Result<VideoStandard> {
    match s.trim().to_ascii_uppercase().as_str() {
        "PAL" => Ok(VideoStandard::Pal),
        "NTSC" => Ok(VideoStandard::Ntsc),
        _ => Err(anyhow!(
            "unknown chipset video {:?}: expected PAL / NTSC",
            s
        )),
    }
}

/// Format a byte count back into the compact human size the config screen
/// writes into `[memory]` (the inverse of [`parse_size`] for the multiples it
/// produces): exact GiB/MiB/KiB get a `G`/`M`/`K` suffix, anything else falls
/// back to a raw byte count. Always emits a 4 KiB-aligned value the parser
/// round-trips.
#[cfg_attr(not(feature = "frontend"), allow(dead_code))]
pub(crate) fn format_size(bytes: usize) -> String {
    const K: usize = 1024;
    const M: usize = 1024 * 1024;
    const G: usize = 1024 * 1024 * 1024;
    if bytes == 0 {
        "0".to_string()
    } else if bytes.is_multiple_of(G) {
        format!("{}G", bytes / G)
    } else if bytes.is_multiple_of(M) {
        format!("{}M", bytes / M)
    } else if bytes.is_multiple_of(K) {
        format!("{}K", bytes / K)
    } else {
        bytes.to_string()
    }
}

/// Parse a human size like "512K", "1M", "2 MiB" or a raw byte count.
pub(crate) fn parse_size(s: &str, what: &str) -> Result<usize> {
    let raw = s.trim();
    if raw.is_empty() {
        bail!("{} size is empty", what);
    }
    // Split into numeric prefix + unit suffix.
    let split = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    let (num_str, unit_str) = raw.split_at(split);
    let n: u64 = num_str
        .parse()
        .with_context(|| format!("{} size {:?}: bad number", what, s))?;
    let unit = unit_str.trim().to_ascii_uppercase().replace("IB", "B");
    let bytes = match unit.as_str() {
        "" | "B" => n,
        "K" | "KB" => n * 1024,
        "M" | "MB" => n * 1024 * 1024,
        "G" | "GB" => n * 1024 * 1024 * 1024,
        _ => bail!("{} size {:?}: unknown unit {:?}", what, s, unit_str),
    };
    if bytes % 4096 != 0 {
        bail!("{} size {} bytes must be a multiple of 4 KiB", what, bytes);
    }
    Ok(bytes as usize)
}

fn validate_chip_ram(bytes: usize, chipset: Chipset, agnus: AgnusRevision) -> Result<()> {
    let max = match chipset {
        Chipset::Ocs => 512 * 1024,
        Chipset::Ecs => 2 * 1024 * 1024,
        Chipset::Aga => 2 * 1024 * 1024,
    };
    if bytes == 0 {
        bail!("chip RAM must be > 0");
    }
    if bytes > max {
        bail!(
            "chip RAM {} bytes exceeds {:?} chipset maximum of {} bytes",
            bytes,
            chipset,
            max
        );
    }
    let agnus_max = agnus.dma_addr_capability_mask() as usize + 1;
    if bytes > agnus_max {
        bail!(
            "chip RAM {} bytes exceeds the {:?} Agnus address reach of {} bytes",
            bytes,
            agnus,
            agnus_max
        );
    }
    Ok(())
}

fn validate_fast_ram(fast: usize, chip: usize) -> Result<()> {
    // Standard Zorro II auto-configured fast RAM sits at $00200000,
    // limited to 8 MiB. If chip RAM occupies that space (only happens
    // with 2 MiB chip RAM on ECS/AGA), there's nowhere to put it.
    const FAST_BASE: usize = 0x0020_0000;
    const FAST_LIMIT: usize = 8 * 1024 * 1024;
    if fast == 0 {
        return Ok(());
    }
    if chip > FAST_BASE {
        bail!("fast RAM > 0 incompatible with chip RAM > 2 MiB (no room at $00200000)");
    }
    if fast > FAST_LIMIT {
        bail!(
            "fast RAM {} bytes exceeds Zorro II maximum of {} bytes",
            fast,
            FAST_LIMIT
        );
    }
    if zorro_ii_size_code(fast).is_none() {
        bail!(
            "fast RAM {} bytes is not an autoconfigurable Zorro II size (64K, 128K, 256K, 512K, 1M, 2M, 4M, or 8M)",
            fast
        );
    }
    Ok(())
}

/// Motherboard fast RAM must land on Ramsey's bank layout: four banks of
/// either 256Kx4 parts (1 MiB per bank) or 1Mx4 parts (4 MiB per bank), so
/// 1M-4M in 1M steps or 4M/8M/12M/16M. Beyond the four banks the big-box
/// memory map reserves $04000000-$06FFFFFF for motherboard RAM expansion;
/// filling it (whole 4M steps up to 64M) is an A4000/Ramsey-07 option,
/// sized by the same top-down Kickstart probe. It also needs the Ramsey
/// itself and a CPU whose address bus reaches $07000000 at all.
fn validate_mb_ram(mb: usize, mem_controller: MemController, cpu: CpuModel) -> Result<()> {
    const BANK_1M: usize = 1024 * 1024;
    const BANK_4M: usize = 4 * 1024 * 1024;
    if mb == 0 {
        return Ok(());
    }
    if mem_controller.ramsey_revision().is_none() {
        bail!(
            "motherboard RAM needs a Ramsey memory controller \
             ([machine] mem_controller = \"ramsey-04\" or \"ramsey-07\", \
             fitted by the A3000/A4000 profiles)"
        );
    }
    if !cpu_has_32bit_bus(cpu) {
        bail!(
            "motherboard RAM ends at $08000000, beyond a 24-bit address bus: \
             {:?} cannot reach it (needs a 68020/68030/68040/68060)",
            cpu
        );
    }
    if mb > 4 * BANK_4M {
        if mem_controller.ramsey_revision() != Some(crate::ramsey::RamseyRevision::Rev7) {
            bail!(
                "motherboard RAM beyond 16M fills the $04000000-$06FFFFFF \
                 expansion space, an A4000 option (needs \
                 [machine] mem_controller = \"ramsey-07\")"
            );
        }
        if !mb.is_multiple_of(BANK_4M) || mb > crate::memory::MB_RAM_MAX {
            bail!(
                "motherboard RAM {} bytes does not fill the expansion space \
                 in whole 4M banks (20M-64M in 4M steps)",
                mb
            );
        }
        return Ok(());
    }
    let on_1m_banks = mb.is_multiple_of(BANK_1M) && mb <= 4 * BANK_1M;
    let on_4m_banks = mb.is_multiple_of(BANK_4M) && mb <= 4 * BANK_4M;
    if !(on_1m_banks || on_4m_banks) {
        bail!(
            "motherboard RAM {} bytes does not fill Ramsey banks \
             (1M-4M in 1M steps, or 8M, 12M, 16M; the A4000 extends \
             in 4M steps to 64M)",
            mb
        );
    }
    Ok(())
}

/// CPU-slot (accelerator) fast RAM occupies $08000000-$0FFFFFFF, which only
/// a 32-bit address bus reaches. The bank is whatever DRAM the CPU board
/// carries, so any whole number of megabytes up to the 128M slot space fits.
fn validate_accel_ram(accel: usize, cpu: CpuModel) -> Result<()> {
    const MB: usize = 1024 * 1024;
    if accel == 0 {
        return Ok(());
    }
    if !cpu_has_32bit_bus(cpu) {
        bail!(
            "accelerator RAM sits at $08000000-$0FFFFFFF, beyond a 24-bit \
             address bus: {:?} cannot reach it (needs a 68020/68030/68040/68060)",
            cpu
        );
    }
    if !accel.is_multiple_of(MB) || accel > crate::memory::ACCEL_RAM_MAX {
        bail!(
            "accelerator RAM {} bytes is not a whole number of megabytes \
             up to the 128M CPU-slot space",
            accel
        );
    }
    Ok(())
}

fn cpu_has_32bit_bus(cpu: CpuModel) -> bool {
    matches!(
        cpu,
        CpuModel::M68020 | CpuModel::M68030 | CpuModel::M68040 | CpuModel::M68060
    )
}

fn validate_rtg_card(rtg: RtgCard, vram_bytes: usize, cpu: CpuModel) -> Result<()> {
    if rtg == RtgCard::Z3660 && !cpu_has_32bit_bus(cpu) {
        bail!(
            "[rtg] card = \"z3660\" is a Zorro III board and needs a CPU \
             with a 32-bit address bus (68020/68030/68040/68060); {:?} has \
             a 24-bit bus",
            cpu
        );
    }
    if matches!(rtg, RtgCard::Picasso2 | RtgCard::Picasso2Plus)
        && !matches!(vram_bytes, 0x10_0000 | 0x20_0000)
    {
        bail!(
            "[rtg] vram for Picasso II cards must be \"1M\" or \"2M\", got {} bytes",
            vram_bytes
        );
    }
    Ok(())
}

fn validate_z3_ram(z3: usize, cpu: CpuModel) -> Result<()> {
    if z3 == 0 {
        return Ok(());
    }
    if !cpu_has_32bit_bus(cpu) {
        bail!(
            "Zorro III RAM needs a CPU with a 32-bit address bus \
             (68020/68030/68040); {:?} has a 24-bit bus",
            cpu
        );
    }
    if zorro_iii_size_bits(z3).is_none() {
        bail!(
            "Zorro III RAM {} bytes is not an autoconfigurable size \
             (a power of two from 64K to 1G)",
            z3
        );
    }
    Ok(())
}

fn validate_slow_ram(slow: usize) -> Result<()> {
    const SLOW_LIMIT: usize = 512 * 1024;
    if slow > SLOW_LIMIT {
        bail!(
            "slow RAM {} bytes exceeds A500 trapdoor/fake-fast maximum of {} bytes",
            slow,
            SLOW_LIMIT
        );
    }
    Ok(())
}

fn parse_floppy(raw: RawFloppy) -> Result<(FloppyConfig, [bool; 4], [Vec<PathBuf>; 4])> {
    let connected_count = match raw.drives {
        None => None,
        Some(n @ 1..=4) => Some(usize::from(n)),
        Some(n) => bail!("[floppy] drives must be between 1 and 4, got {n}"),
    };
    let speed = match raw.speed {
        None => 100,
        Some(s)
            if s == crate::floppy::SPEED_TURBO
                || crate::floppy::SUPPORTED_SPEED_PERCENTS.contains(&s) =>
        {
            s
        }
        Some(s) => bail!("[floppy] speed must be 100, 200, 400, 800, or 0 (turbo), got {s}"),
    };
    let raws = [raw.df0, raw.df1, raw.df2, raw.df3];
    let mut drives: [Option<FloppyDriveConfig>; 4] = std::array::from_fn(|_| None);
    let mut connected = match connected_count {
        Some(count) => std::array::from_fn(|idx| idx < count),
        None => [true, false, false, false],
    };
    let mut playlists: [Vec<PathBuf>; 4] = std::array::from_fn(|_| Vec::new());
    #[cfg_attr(not(feature = "fluxbridge"), allow(unused_mut))]
    let mut bridges: [Option<FluxBridgeConfig>; 4] = std::array::from_fn(|_| None);
    for (idx, raw_drive) in raws.into_iter().enumerate() {
        let Some(raw_drive) = raw_drive else {
            continue;
        };

        // A physical drive takes the bay instead of an image. A build without
        // the feature has no way to drive one, so the keys are read and
        // ignored rather than rejected: a config file shared between builds
        // stays valid, it just does nothing here.
        #[cfg(not(feature = "fluxbridge"))]
        let _ = &raw_drive.bridge;
        #[cfg(feature = "fluxbridge")]
        if let Some(spec) = raw_drive.bridge.as_deref() {
            let spec = spec.trim();
            if !spec.eq_ignore_ascii_case("off") && !spec.is_empty() {
                if raw_drive.path.is_some() || raw_drive.paths.is_some() {
                    bail!(
                        "floppy.df{idx} has both a bridge and a disk image; a physical drive \
                         supplies its own media, so give one or the other"
                    );
                }
                bridges[idx] = Some(parse_floppy_bridge(idx, spec, &raw_drive)?);
                if let Some(count) = connected_count {
                    if !connected[idx] {
                        bail!(
                            "[floppy] drives = {count} leaves floppy.df{idx} disconnected, \
                             but floppy.df{idx} has a bridge configured"
                        );
                    }
                } else {
                    connected[idx] = true;
                }
                continue;
            }
        }
        // Combine `path` (single) and `paths` (playlist) into one ordered
        // list, with `path` first when both are present.
        let mut raw_images: Vec<String> = Vec::new();
        if let Some(path) = raw_drive.path {
            raw_images.push(path);
        }
        if let Some(paths) = raw_drive.paths {
            raw_images.extend(paths);
        }
        let has_images = !raw_images.is_empty();
        let enabled = raw_drive.enabled.unwrap_or(has_images);
        if !enabled {
            continue;
        }
        if let Some(count) = connected_count {
            if !connected[idx] {
                bail!(
                    "[floppy] drives = {} leaves floppy.df{} disconnected, \
                     but floppy.df{} has media configured",
                    count,
                    idx,
                    idx
                );
            }
        } else {
            connected[idx] = true;
        }
        if !has_images {
            bail!("floppy.df{} is enabled but has no path", idx);
        }
        let mut images = Vec::with_capacity(raw_images.len());
        for image in raw_images {
            if image.trim().is_empty() {
                bail!("floppy.df{} path is empty", idx);
            }
            let image = PathBuf::from(image);
            validate_floppy_image_path(idx, &image)?;
            images.push(image);
        }
        drives[idx] = Some(FloppyDriveConfig {
            path: images[0].clone(),
            write_protected: raw_drive.write_protected.unwrap_or(true),
        });
        playlists[idx] = images;
    }
    Ok((
        FloppyConfig {
            drives,
            bridges,
            speed,
        },
        connected,
        playlists,
    ))
}

/// The interface names this build actually accepts, from the library's own
/// driver table -- so help text, error messages and validation cannot drift
/// from what can really be opened.
#[cfg(feature = "fluxbridge")]
pub fn supported_bridge_drivers() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = crate::fluxbridge::drivers()
        .into_iter()
        .map(|driver| driver.token)
        .collect();
    names.push("off");
    names
}

/// Parse one bay's `bridge = ...` plus its `bridge_*` settings.
#[cfg(feature = "fluxbridge")]
fn parse_floppy_bridge(idx: usize, spec: &str, raw: &RawFloppyDrive) -> Result<FluxBridgeConfig> {
    let driver = match spec
        .to_ascii_lowercase()
        .replace([' ', '-', '_'], "")
        .as_str()
    {
        "drawbridge" | "arduino" => BridgeDriver::DrawBridge,
        "greaseweazle" | "gw" => BridgeDriver::Greaseweazle,
        "supercardpro" | "scp" => BridgeDriver::SupercardPro,
        _ => bail!(
            "floppy.df{idx} bridge = \"{spec}\" is not a known interface ({})",
            supported_bridge_drivers().join(", ")
        ),
    };
    // Recognising a name is not the same as having its driver. FluxBridge is
    // compiled with the drivers Copperline supports, so a configuration
    // naming one this build does not carry is refused here, where the name is
    // read, rather than surviving validation only to fail when the drive is
    // opened.
    if crate::fluxbridge::driver_named(driver.match_token()).is_none() {
        bail!(
            "floppy.df{idx} bridge = \"{spec}\": this build has no {} driver (it has {})",
            driver.label(),
            supported_bridge_drivers().join(", ")
        );
    }

    let mode = match raw.bridge_mode.as_deref().map(str::trim) {
        None => BridgeReadMode::default(),
        Some(s) if s.eq_ignore_ascii_case("normal") => BridgeReadMode::Normal,
        Some(s) if s.eq_ignore_ascii_case("compatible") => BridgeReadMode::Compatible,
        Some(s) if s.eq_ignore_ascii_case("stalling") => BridgeReadMode::Stalling,
        // "turbo" is not a read mode: it intercepts AmigaDOS calls rather
        // than reading the disk, which is not something an emulator modelling
        // the hardware can use. Refused by name rather than quietly
        // substituted, so a config carried over from another emulator says
        // what happened.
        Some(s) if s.eq_ignore_ascii_case("turbo") => bail!(
            "floppy.df{idx} bridge_mode = \"{s}\" answers AmigaDOS calls instead of \
             reading the disk, which Copperline has no use for: it models the drive. \
             Use normal, compatible or stalling."
        ),
        Some(s) => bail!("floppy.df{idx} unknown bridge_mode {s:?}"),
    };
    let density = match raw.bridge_density.as_deref().map(str::trim) {
        None => BridgeDensity::Auto,
        Some(s) if s.eq_ignore_ascii_case("auto") => BridgeDensity::Auto,
        Some(s) if s.eq_ignore_ascii_case("dd") => BridgeDensity::Dd,
        Some(s) if s.eq_ignore_ascii_case("hd") => BridgeDensity::Hd,
        Some(s) => bail!("floppy.df{idx} bridge_density = \"{s}\" is not one of auto, dd, hd"),
    };
    let cable = match raw.bridge_cable.as_deref().map(str::trim) {
        None => BridgeCable::default(),
        Some(s) if s.eq_ignore_ascii_case("a") => BridgeCable::DriveA,
        Some(s) if s.eq_ignore_ascii_case("b") => BridgeCable::DriveB,
        Some("0") => BridgeCable::Shugart0,
        Some("1") => BridgeCable::Shugart1,
        Some("2") => BridgeCable::Shugart2,
        Some("3") => BridgeCable::Shugart3,
        Some(s) => bail!("floppy.df{idx} bridge_cable = \"{s}\" is not one of a, b, 0, 1, 2, 3"),
    };
    let port = raw
        .bridge_port
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // Replays of captured revolutions run at real speed or at double it; a
    // track's first read always streams at the platter's own pace, so finer
    // steps between the two had nothing distinct to mean.
    let speed = match &raw.bridge_speed {
        None => DEFAULT_BRIDGE_SPEED_PERCENT,
        Some(RawReplaySpeed::Word(word)) if word.eq_ignore_ascii_case("normal") => 100,
        Some(RawReplaySpeed::Word(word)) if word.eq_ignore_ascii_case("fast") => 200,
        Some(RawReplaySpeed::Percent(100)) => 100,
        Some(RawReplaySpeed::Percent(200)) => 200,
        Some(other) => bail!(
            "floppy.df{idx} replay_speed = {other:?} is not a replay speed: \
             \"normal\" (real speed) or \"fast\" (double)"
        ),
    };

    Ok(FluxBridgeConfig {
        driver,
        write_protected: raw.write_protected.unwrap_or(true),
        port,
        mode,
        density,
        cable,
        speed,
    })
}

fn validate_floppy_image_path(idx: usize, path: &Path) -> Result<()> {
    const ADF_SIZE: u64 = 80 * 2 * 11 * 512;
    let meta = std::fs::metadata(path)
        .with_context(|| format!("reading floppy.df{} image {}", idx, path.display()))?;
    if !meta.is_file() {
        bail!("floppy.df{} image {} is not a file", idx, path.display());
    }
    if meta.len() == ADF_SIZE {
        return Ok(());
    }

    let mut sig = [0u8; 8];
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("opening floppy.df{} image {}", idx, path.display()))?;
    file.read_exact(&mut sig).with_context(|| {
        format!(
            "reading floppy.df{} image signature {}",
            idx,
            path.display()
        )
    })?;
    if sig[..2] == [0x1F, 0x8B]
        || sig[..4] == [0x50, 0x4b, 0x03, 0x04]
        || &sig[..3] == b"SCP"
        || &sig[..4] == b"DMS!"
        || &sig[..4] == b"CAPS"
        || &sig == b"UAE-1ADF"
        || &sig == b"UAE--ADF"
    {
        return Ok(());
    }

    bail!(
        "floppy.df{} image {} is {} bytes, expected {} bytes (standard DD ADF),
        gzip-compressed supported image, UAE extended ADF, IPF, SCP, DMS or single file ZIP",
        idx,
        path.display(),
        meta.len(),
        ADF_SIZE
    );
}

/// Emulated-machine summary lines for the About window.
pub fn about_machine_lines(cfg: &Config) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(machine) = cfg.machine {
        lines.push(format!("Machine: {machine:?}"));
    }
    lines.push(format!("CPU: {:?} @ {} MHz", cfg.cpu, cfg.cpu_clock_mhz));
    lines.push(format!(
        "Chipset: {:?} ({:?}/{:?}, {:?})",
        cfg.chipset, cfg.agnus_revision, cfg.denise_revision, cfg.video_standard
    ));
    let mut ram = format!("RAM: {}K chip", cfg.chip_ram_bytes / 1024);
    if cfg.slow_ram_bytes > 0 {
        ram.push_str(&format!(", {}K slow", cfg.slow_ram_bytes / 1024));
    }
    if cfg.fast_ram_bytes > 0 {
        ram.push_str(&format!(", {}K fast", cfg.fast_ram_bytes / 1024));
    }
    if cfg.mb_ram_bytes > 0 {
        ram.push_str(&format!(", {}K motherboard", cfg.mb_ram_bytes / 1024));
    }
    if cfg.accel_ram_bytes > 0 {
        ram.push_str(&format!(", {}K accelerator", cfg.accel_ram_bytes / 1024));
    }
    if cfg.z3_ram_bytes > 0 {
        ram.push_str(&format!(", {}K Z3", cfg.z3_ram_bytes / 1024));
    }
    lines.push(ram);
    if let Some(name) = cfg.rom_path.file_name() {
        lines.push(format!("ROM: {}", name.to_string_lossy()));
    }
    let drives = cfg
        .floppy_connected
        .iter()
        .filter(|&&connected| connected)
        .count();
    lines.push(format!("Floppy drives: {drives}"));
    lines
}

/// Resolve the phosphor persistence fraction: the `COPPERLINE_PHOSPHOR`
/// env var (0.0..=0.95) overrides the `[display] phosphor` config for one
/// run.
pub fn resolve_phosphor(from_config: f32) -> f32 {
    match crate::envcfg::var("COPPERLINE_PHOSPHOR") {
        Some(v) => match v.trim().parse::<f32>() {
            Ok(p) if (0.0..=0.95).contains(&p) => p,
            _ => {
                log::warn!(
                    "COPPERLINE_PHOSPHOR must be between 0.0 and 0.95, got {v:?}; using config value"
                );
                from_config
            }
        },
        None => from_config,
    }
}

/// Resolve deinterlacing: the `COPPERLINE_DEINTERLACE` env var overrides
/// the `[display] deinterlace` config for one run (any value other than
/// 0/false/off/no enables it).
pub fn resolve_deinterlace(from_config: bool) -> bool {
    match crate::envcfg::var("COPPERLINE_DEINTERLACE") {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => from_config,
    }
}

/// Resolve the presented overscan mode: the `COPPERLINE_OVERSCAN` env var
/// (full/tv) overrides the `[display] overscan` config for one run. The
/// image-regression harness pins "full" so its baselines always carry the
/// whole overscan field regardless of the config default.
pub fn resolve_overscan(from_config: Overscan) -> Overscan {
    match crate::envcfg::var("COPPERLINE_OVERSCAN") {
        Some(v) => match parse_overscan(&v) {
            Ok(o) => o,
            Err(e) => {
                log::warn!("ignoring COPPERLINE_OVERSCAN: {e}");
                from_config
            }
        },
        None => from_config,
    }
}

/// Resolve the presentation pixel aspect: the `COPPERLINE_PIXEL_ASPECT`
/// env var (tv/square) overrides the `[display] pixel_aspect` config for
/// one run, so headless A/B captures can pin a mode without editing the
/// config.
pub fn resolve_pixel_aspect(from_config: PixelAspect) -> PixelAspect {
    match crate::envcfg::var("COPPERLINE_PIXEL_ASPECT") {
        Some(v) => match parse_pixel_aspect(&v) {
            Ok(a) => a,
            Err(e) => {
                log::warn!("ignoring COPPERLINE_PIXEL_ASPECT: {e}");
                from_config
            }
        },
        None => from_config,
    }
}

/// Resolve the window shader pass: the `COPPERLINE_SHADER` env var (a
/// preset name or a `.wgsl` path) overrides the `[display] shader` config
/// for one run.
pub fn resolve_shader(from_config: ShaderMode) -> ShaderMode {
    match crate::envcfg::var("COPPERLINE_SHADER") {
        Some(v) => match parse_shader(&v) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("ignoring COPPERLINE_SHADER: {e}");
                from_config
            }
        },
        None => from_config,
    }
}

/// Resolve the shader mix: the `COPPERLINE_SHADER_STRENGTH` env var
/// (0.0..=1.0) overrides the `[display] shader_strength` config for one
/// run.
pub fn resolve_shader_strength(from_config: f32) -> f32 {
    match crate::envcfg::var("COPPERLINE_SHADER_STRENGTH") {
        Some(v) => match v.trim().parse::<f32>() {
            Ok(p) if (0.0..=1.0).contains(&p) => p,
            _ => {
                log::warn!(
                    "COPPERLINE_SHADER_STRENGTH must be between 0.0 and 1.0, got {v:?}; using config value"
                );
                from_config
            }
        },
        None => from_config,
    }
}

/// Resolve the monitor bezel: the `COPPERLINE_BEZEL` env var (0/false/off/no
/// disables, anything else enables) overrides the `[display] bezel` config
/// for one run.
pub fn resolve_bezel(from_config: bool) -> bool {
    match crate::envcfg::var("COPPERLINE_BEZEL") {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => from_config,
    }
}

/// Resolve the performance overlay: the `COPPERLINE_PERF_OVERLAY` env var
/// (0/false/off/no disables, anything else enables) overrides the
/// `[display] perf_overlay` config for one run.
pub fn resolve_perf_overlay(from_config: bool) -> bool {
    match crate::envcfg::var("COPPERLINE_PERF_OVERLAY") {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => from_config,
    }
}

/// Resolve the screen tint: the `COPPERLINE_TINT` env var (a tint name)
/// overrides the `[display] tint` config for one run.
pub fn resolve_tint(from_config: Tint) -> Tint {
    match crate::envcfg::var("COPPERLINE_TINT") {
        Some(v) => match parse_tint(&v) {
            Ok(t) => t,
            Err(e) => {
                log::warn!("ignoring COPPERLINE_TINT: {e}");
                from_config
            }
        },
        None => from_config,
    }
}

/// Substitute the bundled AROS ROM when the user named no ROM. The default
/// `rom_path` is a sentinel ([`BUNDLED_AROS_ROM`]); any real path from
/// `rom = "..."` or the CLI argument replaces it before this runs and is left
/// untouched. When the sentinel survives, locate the bundled AROS main +
/// extended ROM pair and rewrite the config to point at them, so every
/// downstream consumer (start-up banner, window title, save states) sees the
/// real paths. An explicit `extended_rom` still wins over the AROS one.
pub fn resolve_bundled_rom(cfg: &mut Config) -> Result<()> {
    // An A4091 may want its bundled ROM regardless of what Kickstart is in use.
    resolve_bundled_a4091_rom(cfg)?;
    if cfg.rom_path != Path::new(BUNDLED_AROS_ROM) {
        return Ok(());
    }
    let aros = crate::romsearch::find_bundled_aros().ok_or_else(|| {
        anyhow!(
            "no ROM specified and the bundled AROS ROM was not found. Pass a \
             Kickstart ROM (as the first argument or rom = \"...\" in a config), \
             or install the AROS files ({} and {}) next to the binary or under \
             share/copperline/aros/.",
            crate::romsearch::AROS_MAIN_FILE,
            crate::romsearch::AROS_EXT_FILE
        )
    })?;
    log::info!(
        "no ROM specified; booting bundled AROS ({})",
        aros.main.display()
    );
    cfg.rom_path = aros.main;
    cfg.extended_rom_path.get_or_insert(aros.extended);
    Ok(())
}

/// Resolve a [`BUNDLED_A4091_ROM`] sentinel in `[scsi] rom` to the located
/// bundled ROM, or fail telling the user where to install one.
fn resolve_bundled_a4091_rom(cfg: &mut Config) -> Result<()> {
    if cfg.scsi.rom.as_deref() != Some(Path::new(BUNDLED_A4091_ROM)) {
        return Ok(());
    }
    let rom = crate::romsearch::find_bundled_a4091().ok_or_else(|| {
        anyhow!(
            "[scsi] controller = \"a4091\" but no ROM was named and the bundled \
             A4091 ROM was not found. Set [scsi] rom = \"...\" (a raw A4091 EPROM \
             image), or install {} next to the binary or under \
             share/copperline/a4091/.",
            crate::romsearch::A4091_ROM_FILE
        )
    })?;
    log::info!(
        "no A4091 ROM specified; using bundled ROM ({})",
        rom.display()
    );
    cfg.scsi.rom = Some(rom);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn parse_config(text: &str) -> Result<Config> {
        let raw: RawConfig = toml::from_str(text)?;
        raw.try_into()
    }

    /// Build a config from CLI overrides only (no file), exercising the same
    /// raw-load + validation path `main` uses.
    fn load_overrides(overrides: &ConfigOverrides) -> Result<Config> {
        Config::load_raw(None, overrides)?.try_into()
    }

    #[test]
    fn rtc_time_accepts_both_notations_and_implies_a_fitted_clock() -> Result<()> {
        // Integer form: Unix seconds (an RFC 6238 test-vector instant).
        let cfg = parse_config(
            r#"
            [machine]
            rtc_time = 1111111109
            "#,
        )?;
        assert_eq!(cfg.rtc_seed_unix, Some(1_111_111_109));
        assert!(!cfg.rtc_frozen);
        // The default A500 has no clock, but seeding one fits it.
        assert!(cfg.rtc_present);

        // Calendar form: the same instant as the guest reads it.
        let cfg = parse_config(
            r#"
            [machine]
            rtc_time = "2005-03-18 01:58:29"
            rtc_frozen = true
            "#,
        )?;
        assert_eq!(cfg.rtc_seed_unix, Some(1_111_111_109));
        assert!(cfg.rtc_frozen);
        assert!(cfg.rtc_present);
        Ok(())
    }

    #[test]
    fn rtc_time_misconfigurations_are_rejected() {
        // An explicitly unfitted clock contradicts a configured time.
        let err = parse_config(
            r#"
            [machine]
            rtc = false
            rtc_time = 1111111109
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("rtc = false"), "{err:#}");

        // Freezing needs a time to freeze at.
        let err = parse_config(
            r#"
            [machine]
            rtc_frozen = true
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("rtc_frozen"), "{err:#}");

        // Negative Unix seconds and malformed strings fail validation.
        for bad in ["rtc_time = -1", "rtc_time = \"tomorrow\""] {
            let err = parse_config(&format!("[machine]\n{bad}\n")).unwrap_err();
            assert!(err.to_string().contains("rtc_time"), "{bad}: {err:#}");
        }
    }

    #[test]
    fn rtc_chip_defaults_per_profile_and_implies_a_fitted_clock() -> Result<()> {
        use crate::rtc::RtcChip;

        // The big boxes carry the Ricoh part by default.
        for profile in ["A3000", "A4000"] {
            let cfg = parse_config(&format!("[machine]\nprofile = \"{profile}\"\n"))?;
            assert!(cfg.rtc_present, "{profile}");
            assert_eq!(cfg.rtc_chip, RtcChip::Rp5c01, "{profile}");
        }
        // The clock-equipped small boxes keep the OKI one.
        let cfg = parse_config("[machine]\nprofile = \"A500+\"\n")?;
        assert!(cfg.rtc_present);
        assert_eq!(cfg.rtc_chip, RtcChip::Msm6242);

        // Naming a chip fits the clock on a machine that has none...
        let cfg = parse_config("[machine]\nrtc_chip = \"RP5C01\"\n")?;
        assert!(cfg.rtc_present);
        assert_eq!(cfg.rtc_chip, RtcChip::Rp5c01);
        // ...and the aliases and the big-box override both parse.
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A3000"
            rtc_chip = "MSM6242B"
            "#,
        )?;
        assert_eq!(cfg.rtc_chip, RtcChip::Msm6242);
        let cfg = parse_config("[machine]\nrtc_chip = \"rf5c01a\"\n")?;
        assert_eq!(cfg.rtc_chip, RtcChip::Rp5c01);
        Ok(())
    }

    #[test]
    fn rtc_chip_misconfigurations_are_rejected() {
        // A chip named for a socket declared empty is a contradiction.
        let err = parse_config(
            r#"
            [machine]
            rtc = false
            rtc_chip = "RP5C01"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("rtc = false"), "{err:#}");

        let err = parse_config("[machine]\nrtc_chip = \"DS1307\"\n").unwrap_err();
        assert!(err.to_string().contains("rtc_chip"), "{err:#}");
    }

    #[test]
    fn battmem_defaults_to_a_backing_file_only_where_an_rp5c01_sits() -> Result<()> {
        // The big boxes get the default backing file with their Ricoh part.
        for profile in ["A3000", "A4000"] {
            let cfg = parse_config(&format!("[machine]\nprofile = \"{profile}\"\n"))?;
            assert_eq!(
                cfg.battmem_path.as_deref(),
                Some(std::path::Path::new("battmem.nvram")),
                "{profile}"
            );
        }
        // The OKI part has no battery RAM: no file, even with a clock.
        let cfg = parse_config("[machine]\nprofile = \"A500+\"\n")?;
        assert_eq!(cfg.battmem_path, None);
        // Fitting an RP5C01 anywhere brings the default with it.
        let cfg = parse_config("[machine]\nrtc_chip = \"RP5C01\"\n")?;
        assert!(cfg.battmem_path.is_some());

        // An explicit path wins; an empty one keeps RAM session-only.
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A4000"
            battmem = "shared/A4000.nvram"
            "#,
        )?;
        assert_eq!(
            cfg.battmem_path.as_deref(),
            Some(std::path::Path::new("shared/A4000.nvram"))
        );
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A4000"
            battmem = ""
            "#,
        )?;
        assert_eq!(cfg.battmem_path, None);
        Ok(())
    }

    #[test]
    fn battmem_without_an_rp5c01_is_rejected() {
        // The default A500 has no RP5C01 for the file to back.
        let err = parse_config("[machine]\nbattmem = \"battmem.nvram\"\n").unwrap_err();
        assert!(err.to_string().contains("RP5C01"), "{err:#}");

        // An MSM6242 clock is not enough: it carries no battery RAM.
        let err = parse_config(
            r#"
            [machine]
            rtc = true
            battmem = "battmem.nvram"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("RP5C01"), "{err:#}");
    }

    #[test]
    fn rtc_time_cli_override_matches_the_config_field() -> Result<()> {
        let cfg = load_overrides(&ConfigOverrides {
            rtc_time: Some("1111111109".to_string()),
            rtc_frozen: Some(true),
            ..ConfigOverrides::default()
        })?;
        assert_eq!(cfg.rtc_seed_unix, Some(1_111_111_109));
        assert!(cfg.rtc_frozen);
        assert!(cfg.rtc_present);
        Ok(())
    }

    #[test]
    fn format_size_inverts_parse_size() {
        for (bytes, text) in [
            (0usize, "0"),
            (512 * 1024, "512K"),
            (1024 * 1024, "1M"),
            (2 * 1024 * 1024, "2M"),
            (16 * 1024 * 1024, "16M"),
            (1024 * 1024 * 1024, "1G"),
        ] {
            assert_eq!(format_size(bytes), text, "format_size({bytes})");
            assert_eq!(
                parse_size(&format_size(bytes), "test").unwrap(),
                bytes,
                "round-trip {bytes}"
            );
        }
    }

    #[test]
    fn filesys_volume_name_is_validated() {
        use crate::filesys::MountSpec;
        let with_volume = |volume: &str| Config {
            filesys: vec![MountSpec {
                path: std::path::PathBuf::from("."),
                volume: volume.to_string(),
                boot_pri: -128,
                readonly: false,
            }],
            ..Config::default()
        };
        // A sane label mounts (the services board is added).
        assert!(with_volume("Work").build_zorro_chain().is_ok());
        // The three failure modes each report their own error.
        let err = |v: &str| format!("{:#}", with_volume(v).build_zorro_chain().unwrap_err());
        assert!(err("").contains("must not be empty"));
        assert!(err("this-volume-name-is-far-too-long-to-fit").contains("too long"));
        for bad in ["a:b", "a/b", "a\0b"] {
            assert!(err(bad).contains("invalid character"), "volume {bad:?}");
        }
    }

    #[test]
    fn raw_config_serialize_round_trips() {
        // A populated raw config (the kind the configuration screen builds)
        // serialized to TOML and parsed back must be identical -- this guards
        // the Serialize field names/ordering against the deny_unknown_fields
        // deserialize schema.
        let raw = RawConfig {
            rom: Some("kick.rom".to_string()),
            extended_rom: Some("ext.rom".to_string()),
            identify: Some(false),
            machine: RawMachine {
                profile: Some("A1200".to_string()),
                rtc: Some(true),
                rtc_chip: Some("RP5C01".to_string()),
                rtc_time: Some(RawRtcTime::Text("2005-03-18 01:58:29".to_string())),
                rtc_frozen: Some(true),
                battmem: Some("battmem.nvram".to_string()),
                mem_controller: Some("ramsey-07".to_string()),
                rom_scsi_device_disable: Some(true),
            },
            cpu: RawCpu {
                model: Some("68EC020".to_string()),
                fpu: Some(true),
                clock_mhz: Some(14.18),
                icache: Some(true),
                dcache: None,
                unimplemented: None,
                jit: None,
            },
            memory: RawMemory {
                chip: Some("2M".to_string()),
                fast: Some("8M".to_string()),
                slow: None,
                motherboard: None,
                accelerator: None,
                z3: None,
            },
            chipset: RawChipset {
                revision: Some("AGA".to_string()),
                video: Some("PAL".to_string()),
                agnus: None,
                denise: None,
            },
            ide: RawIde {
                master: Some(RawDrive::from_path("hd0.hdf")),
                slave: None,
            },
            floppy: RawFloppy {
                drives: Some(2),
                df0: Some(RawFloppyDrive {
                    enabled: Some(true),
                    path: Some("game.adf".to_string()),
                    paths: None,
                    write_protected: Some(true),
                    ..RawFloppyDrive::default()
                }),
                ..RawFloppy::default()
            },
            zorro: vec![RawZorroBoard {
                metadata: "board.toml".to_string(),
                config: None,
            }],
            ..RawConfig::default()
        };
        let text = raw.to_toml_string().unwrap();
        let back: RawConfig = toml::from_str(&text).unwrap();
        assert_eq!(raw, back, "round-trip mismatch; TOML was:\n{text}");
    }

    #[test]
    fn default_raw_config_serializes_to_empty() {
        // Nothing set means nothing written: a default machine saves as an
        // empty file (which re-parses to the defaults).
        let text = RawConfig::default().to_toml_string().unwrap();
        assert!(text.trim().is_empty(), "expected empty TOML, got:\n{text}");
    }

    #[test]
    fn rom_fingerprint_distinguishes_same_shape_kickstarts() {
        // Two machines of identical shape but different boot ROMs must compare
        // as a mismatch (the whole point of fingerprinting the ROM rather than
        // only the machine shape).
        let mut a = MachineDescriptor::default();
        a.set_rom_fingerprint(b"kickstart 3.1 r40.068", b"");
        let mut b = MachineDescriptor::default();
        b.set_rom_fingerprint(b"kickstart 3.1.4 r46.143", b"");
        assert_ne!(a.rom, b.rom);
        let diffs = a.differences(&b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].starts_with("ROM "), "{diffs:?}");

        // The same image fingerprints identically, and an added extended ROM is
        // flagged on its own (same boot ROM, gained an extended ROM).
        let mut c = MachineDescriptor::default();
        c.set_rom_fingerprint(b"kickstart 3.1 r40.068", b"");
        assert_eq!(a.rom, c.rom);
        assert!(a.differences(&c).is_empty());
        let mut d = a.clone();
        d.set_rom_fingerprint(b"kickstart 3.1 r40.068", b"cd32 extended rom");
        let ext_diffs = a.differences(&d);
        assert_eq!(ext_diffs.len(), 1);
        assert!(
            ext_diffs[0].starts_with("extended ROM none -> "),
            "{ext_diffs:?}"
        );
    }

    #[test]
    fn windows_path_escape_error_explains_fix() {
        // A backslash in a double-quoted TOML string is an escape character,
        // so an unescaped Windows path fails on "\K". The error must point at
        // the remedy rather than leaving a bare "invalid escape sequence".
        let path = temp_path("badescape.toml");
        fs::write(&path, "rom = \"C:\\Kickstarts\\KICK31.ROM\"\n").unwrap();
        let err = raw_from_path(&path).unwrap_err();
        let _ = fs::remove_file(&path);
        let msg = format!("{err:#}");
        assert!(
            msg.contains("single quotes") && msg.contains("forward slashes"),
            "error should explain the Windows-path fix, got: {msg}"
        );
    }

    #[test]
    fn missing_emulation_uses_defaults() -> Result<()> {
        let cfg = parse_config("")?;
        assert!(cfg.emulation.power_on);
        assert_eq!(cfg.emulation.pacing_budget, PacingBudget::Cycles);
        assert_eq!(cfg.emulation.warp_speed, WarpSpeed::Max);
        Ok(())
    }

    #[test]
    fn warp_speed_parses_levels_and_rejects_garbage() -> Result<()> {
        for (text, expected) in [
            ("2x", WarpSpeed::X2),
            ("4x", WarpSpeed::X4),
            ("8x", WarpSpeed::X8),
            ("16x", WarpSpeed::X16),
            ("max", WarpSpeed::Max),
            ("MAX", WarpSpeed::Max),
        ] {
            let cfg = parse_config(&format!("[emulation]\nwarp_speed = {text:?}\n"))?;
            assert_eq!(cfg.emulation.warp_speed, expected, "for {text:?}");
        }
        assert!(parse_config("[emulation]\nwarp_speed = \"32x\"\n").is_err());
        Ok(())
    }

    #[test]
    fn joystick_input_mode_defaults_to_gamepad() -> Result<()> {
        // No [input] section: the port-2 source starts in Gamepad, regardless
        // of the machine profile (it is a host-input preference, not hardware).
        // Gamepad is the no-surprise default: with no pad the keyboard reaches
        // the Amiga normally instead of being captured as joystick input.
        assert_eq!(
            parse_config("")?.joystick_input_mode,
            JoystickInputMode::Gamepad
        );
        assert_eq!(
            parse_config("[machine]\nprofile = \"A1200\"\n")?.joystick_input_mode,
            JoystickInputMode::Gamepad
        );
        Ok(())
    }

    #[test]
    fn mouse_sensitivity_defaults_to_50_and_validates() -> Result<()> {
        assert_eq!(parse_config("")?.mouse_sensitivity, 50);
        assert_eq!(
            parse_config("[input]\nmouse_sensitivity = 0\n")?.mouse_sensitivity,
            0
        );
        assert_eq!(
            parse_config("[input]\nmouse_sensitivity = 100\n")?.mouse_sensitivity,
            100
        );
        assert!(parse_config("[input]\nmouse_sensitivity = 101\n").is_err());

        // CLI override.
        let overrides = ConfigOverrides {
            mouse_sensitivity: Some(75),
            ..Default::default()
        };
        assert_eq!(load_overrides(&overrides)?.mouse_sensitivity, 75);
        Ok(())
    }

    #[test]
    fn joystick_input_mode_parses_and_rejects_garbage() -> Result<()> {
        for (text, expected) in [
            // "auto" is a backward-compatibility alias mapping to the default.
            ("auto", JoystickInputMode::Gamepad),
            ("keyboard", JoystickInputMode::Keyboard),
            ("gamepad", JoystickInputMode::Gamepad),
            ("GAMEPAD", JoystickInputMode::Gamepad),
        ] {
            let cfg = parse_config(&format!("[input]\njoystick = {text:?}\n"))?;
            assert_eq!(cfg.joystick_input_mode, expected, "for {text:?}");
        }
        assert!(parse_config("[input]\njoystick = \"mouse\"\n").is_err());
        Ok(())
    }

    #[test]
    fn joystick_cli_override_sets_initial_mode() -> Result<()> {
        let overrides = ConfigOverrides {
            joystick: Some("gamepad".to_string()),
            ..ConfigOverrides::default()
        };
        assert_eq!(
            load_overrides(&overrides)?.joystick_input_mode,
            JoystickInputMode::Gamepad
        );
        Ok(())
    }

    #[test]
    fn mouse_capture_defaults_to_click_and_parses_its_modes() -> Result<()> {
        // The default is the historical behaviour: no config, no change.
        assert_eq!(parse_config("")?.mouse_capture, MouseCapture::Click);

        for (text, expected) in [
            ("click", MouseCapture::Click),
            ("on-click", MouseCapture::Click),
            ("auto", MouseCapture::Auto),
            ("focus", MouseCapture::Auto),
            ("manual", MouseCapture::Manual),
            ("off", MouseCapture::Manual),
            ("none", MouseCapture::Manual),
            ("AUTO", MouseCapture::Auto),
        ] {
            let cfg = parse_config(&format!("[input]\nmouse_capture = {text:?}\n"))?;
            assert_eq!(cfg.mouse_capture, expected, "for {text:?}");
        }

        // A typo is an error rather than a silent fallback to the default.
        assert!(parse_config("[input]\nmouse_capture = \"grab\"\n").is_err());
        Ok(())
    }

    #[test]
    fn mouse_capture_cli_override_sets_the_mode() -> Result<()> {
        let overrides = ConfigOverrides {
            mouse_capture: Some("auto".to_string()),
            ..ConfigOverrides::default()
        };
        assert_eq!(
            load_overrides(&overrides)?.mouse_capture,
            MouseCapture::Auto
        );
        Ok(())
    }

    /// Every mode's label has to parse back to the same mode: the launcher
    /// writes the label into the config file it saves.
    #[test]
    fn mouse_capture_labels_round_trip_through_the_parser() -> Result<()> {
        for mode in [
            MouseCapture::Click,
            MouseCapture::Auto,
            MouseCapture::Manual,
        ] {
            assert_eq!(parse_mouse_capture(mode.label())?, mode);
        }
        Ok(())
    }

    #[test]
    fn port_devices_default_to_mouse_and_joystick() -> Result<()> {
        // No [input] port keys: the stock wiring, on every non-CD32 profile.
        assert_eq!(
            parse_config("")?.port_devices,
            [PortDevice::Mouse, PortDevice::Joystick]
        );
        assert_eq!(
            parse_config("[machine]\nprofile = \"A1200\"\n")?.port_devices,
            [PortDevice::Mouse, PortDevice::Joystick]
        );
        Ok(())
    }

    #[test]
    fn cd32_profile_defaults_port_2_to_the_bundled_pad() -> Result<()> {
        let cfg = parse_config("[machine]\nprofile = \"CD32\"\n")?;
        assert_eq!(cfg.port_devices, [PortDevice::Mouse, PortDevice::Cd32Pad]);
        // An explicit key beats the profile: a real CD32 accepts any
        // controller.
        let cfg = parse_config("[machine]\nprofile = \"CD32\"\n[input]\nport2 = \"joystick\"\n")?;
        assert_eq!(cfg.port_devices[1], PortDevice::Joystick);
        Ok(())
    }

    #[test]
    fn autofire_off_holds_the_button_and_a_rate_pulses_it() {
        // Off: always asserted, so a held button is simply held.
        for t in [0.0, 0.01, 12.7] {
            assert!(autofire_asserted(0, t));
        }
        // 5 Hz: 100 ms asserted, 100 ms released, from t=0.
        assert!(autofire_asserted(5, 0.0));
        assert!(autofire_asserted(5, 0.099));
        assert!(!autofire_asserted(5, 0.101));
        assert!(!autofire_asserted(5, 0.199));
        assert!(autofire_asserted(5, 0.201));

        // One full cycle per 1/hz second, at every offered rate.
        for hz in AUTOFIRE_RATES.into_iter().filter(|&r| r != 0) {
            let period = 1.0 / f64::from(hz);
            let samples = 400;
            let asserted = (0..samples)
                .filter(|i| autofire_asserted(hz, period * f64::from(*i) / f64::from(samples)))
                .count();
            assert!(
                (asserted as i32 - samples / 2).abs() <= 1,
                "{hz} Hz should be asserted for half of each period, was {asserted}/{samples}"
            );
        }
    }

    #[test]
    fn autofire_rates_are_labelled_and_stay_within_the_usable_range() {
        assert_eq!(AUTOFIRE_RATES[0], 0, "the list opens with off");
        assert_eq!(autofire_label(0), "off");
        assert_eq!(autofire_label(8), "8 Hz");
        // Above the cap the assert window is shorter than the frame the guest
        // samples on, so no rate the menu offers may exceed it.
        for hz in AUTOFIRE_RATES {
            assert!(hz <= AUTOFIRE_MAX_HZ, "{hz} Hz is past the usable range");
        }
    }

    #[test]
    fn autofire_defaults_off_and_rejects_implausible_rates() -> Result<()> {
        assert_eq!(parse_config("")?.autofire_hz, 0);
        assert_eq!(parse_config("[input]\nautofire_hz = 8\n")?.autofire_hz, 8);
        assert_eq!(
            parse_config(&format!("[input]\nautofire_hz = {}\n", AUTOFIRE_MAX_HZ))?.autofire_hz,
            AUTOFIRE_MAX_HZ
        );
        // Faster than the guest can sample the port is a typo, not a setting.
        assert!(
            parse_config(&format!("[input]\nautofire_hz = {}\n", AUTOFIRE_MAX_HZ + 1)).is_err()
        );

        // The CLI flag layers over the config file, as the other input keys do.
        let overrides = ConfigOverrides {
            autofire_hz: Some(12),
            ..Default::default()
        };
        assert_eq!(load_overrides(&overrides)?.autofire_hz, 12);
        Ok(())
    }

    #[test]
    fn port_device_keys_parse_aliases_and_reject_garbage() -> Result<()> {
        for (text, expected) in [
            ("mouse", PortDevice::Mouse),
            ("joystick", PortDevice::Joystick),
            ("JOY", PortDevice::Joystick),
            ("cd32", PortDevice::Cd32Pad),
            ("cd32pad", PortDevice::Cd32Pad),
            ("pad", PortDevice::Cd32Pad),
            ("analogue", PortDevice::Analogue),
            ("analog", PortDevice::Analogue),
            ("paddle", PortDevice::Analogue),
            ("none", PortDevice::None),
            ("off", PortDevice::None),
        ] {
            let cfg = parse_config(&format!("[input]\nport1 = {text:?}\n"))?;
            assert_eq!(cfg.port_devices[0], expected, "for {text:?}");
        }
        let err = parse_config("[input]\nport2 = \"trackball\"\n").unwrap_err();
        assert!(err.to_string().contains("port2"), "{err}");
        Ok(())
    }

    #[test]
    fn port_device_cli_overrides_swap_the_wiring() -> Result<()> {
        let overrides = ConfigOverrides {
            port1: Some("joystick".to_string()),
            port2: Some("mouse".to_string()),
            ..ConfigOverrides::default()
        };
        assert!(!overrides.is_empty());
        assert_eq!(
            load_overrides(&overrides)?.port_devices,
            [PortDevice::Joystick, PortDevice::Mouse]
        );
        Ok(())
    }

    #[test]
    fn warp_speed_cycle_wraps_through_levels() {
        // The menu/keyboard "cycle" control walks 2x -> 4x -> 8x -> 16x ->
        // Max and back to 2x.
        let order = [
            WarpSpeed::X2,
            WarpSpeed::X4,
            WarpSpeed::X8,
            WarpSpeed::X16,
            WarpSpeed::Max,
        ];
        for window in order.windows(2) {
            assert_eq!(window[0].next(), window[1]);
        }
        assert_eq!(WarpSpeed::Max.next(), WarpSpeed::X2);
        // Fixed levels retire exactly their multiplier in frames; Max is
        // bounded by a wall-clock budget rather than a small fixed count.
        assert_eq!(WarpSpeed::X8.frame_cap(), 8);
        assert!(WarpSpeed::X8.time_budget_ms().is_none());
        assert_eq!(WarpSpeed::Max.time_budget_ms(), Some(WARP_MAX_BUDGET_MS));
    }

    #[test]
    fn deprecated_speed_option_is_accepted_and_ignored() -> Result<()> {
        // `[emulation] speed` was removed once "real" became the only timing
        // model. Any value is now tolerated (and warned about) so old configs
        // still parse, but it has no effect.
        for value in ["real", "turbo", "warp"] {
            parse_config(&format!("[emulation]\nspeed = {value:?}\n"))?;
        }
        Ok(())
    }

    #[test]
    fn power_on_defaults_to_true() -> Result<()> {
        let cfg = parse_config("")?;
        assert!(cfg.emulation.power_on);
        Ok(())
    }

    #[test]
    fn power_on_false_parses() -> Result<()> {
        let cfg = parse_config(
            r#"
            [emulation]
            power_on = false
            "#,
        )?;
        assert!(!cfg.emulation.power_on);
        Ok(())
    }

    #[test]
    fn display_fullscreen_and_status_bar_default_and_parse() -> Result<()> {
        // Defaults: windowed, status bar shown.
        let cfg = parse_config("")?;
        assert!(!cfg.full_screen);
        assert!(cfg.status_bar);

        let cfg = parse_config("[display]\nfull_screen = true\nstatus_bar = false\n")?;
        assert!(cfg.full_screen);
        assert!(!cfg.status_bar);

        // CLI overrides.
        let overrides = ConfigOverrides {
            full_screen: Some(true),
            status_bar: Some(false),
            ..Default::default()
        };
        let cfg = load_overrides(&overrides)?;
        assert!(cfg.full_screen);
        assert!(!cfg.status_bar);
        Ok(())
    }

    #[test]
    fn display_overscan_parses_and_defaults_to_tv() -> Result<()> {
        assert_eq!(parse_config("")?.overscan, Overscan::Tv);
        let cfg = parse_config(
            r#"
            [display]
            overscan = "Full"
            "#,
        )?;
        assert_eq!(cfg.overscan, Overscan::Full);
        assert!(parse_config("[display]\noverscan = \"crop\"").is_err());
        Ok(())
    }

    #[test]
    fn display_pixel_aspect_parses_and_defaults_to_tv() -> Result<()> {
        assert_eq!(parse_config("")?.pixel_aspect, PixelAspect::Tv);
        let cfg = parse_config(
            r#"
            [display]
            pixel_aspect = "Square"
            "#,
        )?;
        assert_eq!(cfg.pixel_aspect, PixelAspect::Square);
        assert!(parse_config("[display]\npixel_aspect = \"1:1\"").is_err());
        Ok(())
    }

    #[test]
    fn display_scaling_parses_and_defaults_to_smooth() -> Result<()> {
        assert_eq!(parse_config("")?.scaling, DisplayScaling::Smooth);
        let cfg = parse_config(
            r#"
            [display]
            scaling = "Integer"
            "#,
        )?;
        assert_eq!(cfg.scaling, DisplayScaling::Integer);
        assert!(parse_config("[display]\nscaling = \"2x\"").is_err());
        Ok(())
    }

    #[test]
    fn display_phosphor_parses_and_rejects_out_of_range() -> Result<()> {
        assert_eq!(parse_config("")?.phosphor, 0.0);
        let cfg = parse_config(
            r#"
            [display]
            phosphor = 0.4
            "#,
        )?;
        assert_eq!(cfg.phosphor, 0.4);
        assert!(parse_config("[display]\nphosphor = 1.5").is_err());
        assert!(parse_config("[display]\nphosphor = -0.1").is_err());
        Ok(())
    }

    #[test]
    fn display_deinterlace_parses_and_defaults_on() -> Result<()> {
        assert!(parse_config("")?.deinterlace);
        assert!(!parse_config("[display]\ndeinterlace = false")?.deinterlace);
        assert!(parse_config("[display]\ndeinterlace = true")?.deinterlace);
        Ok(())
    }

    #[test]
    fn display_shader_parses_presets_and_defaults_to_none() -> Result<()> {
        assert_eq!(parse_config("")?.shader, ShaderMode::None);
        assert_eq!(parse_shader(" None ")?, ShaderMode::None);
        // "off" is the label spelling, and must parse back to the same mode.
        assert_eq!(parse_shader("off")?, ShaderMode::None);
        assert_eq!(parse_shader(" OFF ")?, ShaderMode::None);
        assert_eq!(parse_shader(ShaderKind::None.label())?, ShaderMode::None);
        assert_eq!(parse_shader("SCANLINES")?, ShaderMode::Scanlines);
        assert_eq!(parse_shader("Mask")?, ShaderMode::Mask);
        assert_eq!(parse_shader("\tcrt\n")?, ShaderMode::Crt);
        let cfg = parse_config(
            r#"
            [display]
            shader = "CRT"
            "#,
        )?;
        assert_eq!(cfg.shader, ShaderMode::Crt);
        Ok(())
    }

    #[test]
    fn display_bezel_parses_and_defaults_to_off() -> Result<()> {
        assert!(!parse_config("")?.bezel);
        let cfg = parse_config(
            r#"
            [display]
            bezel = true
            "#,
        )?;
        assert!(cfg.bezel);
        Ok(())
    }

    #[test]
    fn display_perf_overlay_parses_and_defaults_to_off() -> Result<()> {
        assert!(!parse_config("")?.perf_overlay);
        let cfg = parse_config(
            r#"
            [display]
            perf_overlay = true
            "#,
        )?;
        assert!(cfg.perf_overlay);

        let mut raw = RawConfig::default();
        ConfigOverrides {
            perf_overlay: Some(true),
            ..Default::default()
        }
        .apply_to(&mut raw);
        assert_eq!(raw.display.perf_overlay, Some(true));
        Ok(())
    }

    #[test]
    fn display_shader_takes_a_wgsl_path_verbatim() -> Result<()> {
        // Host paths are case-sensitive, so only the extension match is
        // case-insensitive: the path itself must survive unchanged.
        assert_eq!(
            parse_shader("shaders/my.wgsl")?,
            ShaderMode::Custom(PathBuf::from("shaders/my.wgsl"))
        );
        assert_eq!(
            parse_shader(" /abs/path/My.WGSL ")?,
            ShaderMode::Custom(PathBuf::from("/abs/path/My.WGSL"))
        );
        assert_eq!(parse_shader("shaders/my.wgsl")?.kind(), ShaderKind::Custom);
        assert_eq!(ShaderKind::Custom.label(), "custom");

        // The same through a whole config: a missing file is the loader's
        // problem, so parsing keeps the path as written.
        let cfg = parse_config(
            r#"
            [display]
            shader = "shaders/Aperture.wgsl"
            "#,
        )?;
        assert_eq!(
            cfg.shader,
            ShaderMode::Custom(PathBuf::from("shaders/Aperture.wgsl"))
        );
        Ok(())
    }

    #[test]
    fn display_shader_rejects_an_unknown_name() {
        let e = parse_shader("bloom").unwrap_err().to_string();
        assert!(
            e.contains("scanlines") && e.contains("crt") && e.contains(".wgsl"),
            "{e}"
        );
        // Quoted as written, since a rejected value is usually a mistyped
        // path and lowercasing it would hide the typo.
        let e = parse_shader(" Shaders/Bloom.wsgl ")
            .unwrap_err()
            .to_string();
        assert!(e.contains(r#""Shaders/Bloom.wsgl""#), "{e}");
        assert!(parse_config("[display]\nshader = \"bloom\"").is_err());
    }

    #[test]
    fn display_shader_strength_parses_and_rejects_out_of_range() -> Result<()> {
        assert_eq!(parse_config("")?.shader_strength, 1.0);
        let cfg = parse_config(
            r#"
            [display]
            shader_strength = 0.5
            "#,
        )?;
        assert_eq!(cfg.shader_strength, 0.5);
        assert!(parse_config("[display]\nshader_strength = 1.5").is_err());
        assert!(parse_config("[display]\nshader_strength = -0.1").is_err());
        Ok(())
    }

    #[test]
    fn display_menu_scale_parses_and_defaults_to_1x() -> Result<()> {
        assert_eq!(parse_config("")?.menu_scale, MenuScale::Normal);
        let cfg = parse_config(
            r#"
            [display]
            menu_scale = "2x"
        "#,
        )?;
        assert_eq!(cfg.menu_scale, MenuScale::Large);
        // Every label round-trips through the parser.
        for scale in MenuScale::MENU_ORDER {
            assert_eq!(
                parse_menu_scale(scale.label()).expect("label parses"),
                scale
            );
        }
        Ok(())
    }

    #[test]
    fn display_menu_scale_rejects_an_unknown_size() {
        let err = parse_config(
            r#"
            [display]
            menu_scale = "3x"
        "#,
        )
        .expect_err("unknown size");
        assert!(err.to_string().contains("menu_scale"), "{err}");
    }

    #[test]
    fn display_tint_parses_names_and_defaults_to_none() -> Result<()> {
        assert_eq!(parse_config("")?.tint, Tint::None);
        assert_eq!(parse_tint(" None ")?, Tint::None);
        // "off" is the label spelling, and must parse back to the same tint.
        assert_eq!(parse_tint("off")?, Tint::None);
        assert_eq!(parse_tint(Tint::None.label())?, Tint::None);
        assert_eq!(parse_tint("BW")?, Tint::Bw);
        assert_eq!(parse_tint("Green")?, Tint::Green);
        assert_eq!(parse_tint("\tamber\n")?, Tint::Amber);
        assert_eq!(parse_tint("sepia")?, Tint::Sepia);
        // Every label round-trips through the parser.
        for tint in [Tint::Bw, Tint::Green, Tint::Amber, Tint::Sepia] {
            assert_eq!(parse_tint(tint.label())?, tint);
        }
        let cfg = parse_config(
            r#"
            [display]
            tint = "green"
            "#,
        )?;
        assert_eq!(cfg.tint, Tint::Green);
        Ok(())
    }

    #[test]
    fn display_tint_rejects_an_unknown_name() {
        let e = parse_tint("purple").unwrap_err().to_string();
        assert!(
            e.contains("green") && e.contains("sepia") && e.contains(r#""purple""#),
            "{e}"
        );
        assert!(parse_config("[display]\ntint = \"purple\"").is_err());
    }

    #[test]
    fn display_shader_keys_round_trip_through_saved_toml() {
        let raw = RawConfig {
            display: RawDisplay {
                shader: Some("crt".to_string()),
                shader_strength: Some(0.75),
                tint: Some("amber".to_string()),
                ..RawDisplay::default()
            },
            ..RawConfig::default()
        };
        let text = raw.to_toml_string().unwrap();
        let back: RawConfig = toml::from_str(&text).unwrap();
        assert_eq!(raw, back, "round-trip mismatch; TOML was:\n{text}");
    }

    #[test]
    fn display_custom_shader_path_round_trips_through_saved_toml() {
        // A Windows path is all backslashes, which TOML escapes: the saved
        // file must parse back to the identical path, not a mangled one.
        let path = r"C:\Amiga\shaders\crt.wgsl";
        let raw = RawConfig {
            display: RawDisplay {
                shader: Some(path.to_string()),
                ..RawDisplay::default()
            },
            ..RawConfig::default()
        };
        let text = raw.to_toml_string().unwrap();
        let back: RawConfig = toml::from_str(&text).unwrap();
        assert_eq!(raw, back, "round-trip mismatch; TOML was:\n{text}");

        let cfg: Config = back.try_into().unwrap();
        assert_eq!(cfg.shader, ShaderMode::Custom(PathBuf::from(path)));
    }

    #[test]
    fn chipset_video_standard_parses() -> Result<()> {
        let cfg = parse_config(
            r#"
            [chipset]
            video = "NTSC"
            "#,
        )?;
        assert_eq!(cfg.video_standard, VideoStandard::Ntsc);
        Ok(())
    }

    #[test]
    fn machine_profile_defaults_match_bare_profile_configs() -> Result<()> {
        // machine_profile_defaults is also consumed directly, outside the
        // raw-config pipeline (the browser build, the launcher fallback), so
        // the machine it returns must be the machine a config file naming
        // just the profile produces -- including everything the pipeline
        // derives for absent [chipset]/[cpu] keys. The browser's first
        // A1200 shipped with this broken: an AGA machine carrying the
        // default 1 MiB-reach ECS Agnus, whose chip-window mirroring made
        // the guest size 1 MiB of the 2 MiB fitted chip RAM.
        use MachineModel::*;
        for model in [
            A1000, A500, A500Ocs, A500Plus, A600, A1200, A3000, A4000, Cdtv, Cd32,
        ] {
            let direct = machine_profile_defaults(model);
            let piped = parse_config(&format!("[machine]\nprofile = \"{model:?}\"\n"))?;
            assert_eq!(piped.chipset, direct.chipset, "{model:?} chipset");
            assert_eq!(
                piped.agnus_revision, direct.agnus_revision,
                "{model:?} agnus"
            );
            assert_eq!(
                piped.denise_revision, direct.denise_revision,
                "{model:?} denise"
            );
            assert_eq!(piped.cpu, direct.cpu, "{model:?} cpu");
            assert!(
                (piped.cpu_clock_mhz - direct.cpu_clock_mhz).abs() < 1e-9,
                "{model:?} cpu clock: piped {} vs direct {}",
                piped.cpu_clock_mhz,
                direct.cpu_clock_mhz
            );
            assert_eq!(piped.fpu, direct.fpu, "{model:?} fpu");
            assert_eq!(piped.cpu_icache, direct.cpu_icache, "{model:?} icache");
            assert_eq!(piped.cpu_dcache, direct.cpu_dcache, "{model:?} dcache");
            assert_eq!(
                piped.chip_ram_bytes, direct.chip_ram_bytes,
                "{model:?} chip RAM"
            );
            assert_eq!(
                piped.slow_ram_bytes, direct.slow_ram_bytes,
                "{model:?} slow RAM"
            );
            assert_eq!(piped.mb_ram_bytes, direct.mb_ram_bytes, "{model:?} mb RAM");
            assert_eq!(piped.gate_array, direct.gate_array, "{model:?} gate array");
            assert_eq!(
                piped.mem_controller, direct.mem_controller,
                "{model:?} mem controller"
            );
            assert_eq!(piped.rtc_present, direct.rtc_present, "{model:?} rtc");
            assert_eq!(piped.rtc_chip, direct.rtc_chip, "{model:?} rtc chip");
            assert_eq!(piped.rtg, direct.rtg, "{model:?} rtg");
            assert_eq!(
                piped.rtg_vram_bytes, direct.rtg_vram_bytes,
                "{model:?} RTG VRAM"
            );
        }
        Ok(())
    }

    #[test]
    fn machine_profiles_supply_defaults_and_keep_overrides() -> Result<()> {
        // No [machine] section: the default machine is the A500 Rev 6A
        // (ECS 8372A Agnus + OCS 8362 Denise), no gate array, no RTC (the base
        // A500 had no battery clock), stock 512K chip + 512K trapdoor slow RAM.
        // cfg.machine stays None -- the profile id only changes with an
        // explicit [machine] profile.
        let cfg = parse_config("")?;
        assert_eq!(cfg.machine, None);
        assert_eq!(cfg.chipset, Chipset::Ecs);
        assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8372Rev4);
        assert_eq!(cfg.denise_revision, DeniseRevision::Ocs);
        assert_eq!(cfg.gate_array, GateArray::None);
        assert_eq!(cfg.chip_ram_bytes, 512 * 1024);
        assert_eq!(cfg.slow_ram_bytes, 512 * 1024);
        assert!(!cfg.rtc_present);

        let cfg = parse_config(
            r#"
            [machine]
            profile = "A500"
            "#,
        )?;
        assert_eq!(cfg.machine, Some(MachineModel::A500));
        // Rev 6A board: ECS Agnus (the 1 MiB 8372A) with the original OCS
        // Denise, stock 512 KiB chip + 512 KiB trapdoor slow RAM.
        assert_eq!(cfg.chipset, Chipset::Ecs);
        assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8372Rev4);
        assert_eq!(cfg.denise_revision, DeniseRevision::Ocs);
        assert_eq!(cfg.chip_ram_bytes, 512 * 1024);
        assert_eq!(cfg.slow_ram_bytes, 512 * 1024);

        let cfg = parse_config(
            r#"
            [machine]
            profile = "A500"
            [memory]
            slow = "0"
            "#,
        )?;
        assert_eq!(cfg.slow_ram_bytes, 0);

        let cfg = parse_config(
            r#"
            [machine]
            profile = "A600"
            "#,
        )?;
        assert_eq!(cfg.machine, Some(MachineModel::A600));
        assert_eq!(cfg.gate_array, GateArray::GayleA600);
        assert_eq!(cfg.chipset, Chipset::Ecs);
        assert_eq!(cfg.chip_ram_bytes, 1024 * 1024);
        assert_eq!(cfg.slow_ram_bytes, 0);
        // The A600 board carries the 2 MB-capable 8375 even with 1 MB fitted.
        assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8375);
        assert_eq!(cfg.denise_revision, DeniseRevision::Ecs8373);
        assert_eq!(cfg.cpu, CpuModel::M68000);
        // The base A600 shipped without a battery clock.
        assert!(!cfg.rtc_present);

        // Explicit sections override profile defaults: an A600HD re-fits the
        // RTC the base A600 lacks.
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A600"
            rtc = true
            [memory]
            chip = "2M"
            "#,
        )?;
        assert_eq!(cfg.chip_ram_bytes, 2 * 1024 * 1024);
        assert!(cfg.rtc_present);

        let cfg = parse_config(
            r#"
            [machine]
            profile = "A500Plus"
            "#,
        )?;
        assert_eq!(cfg.mem_controller, MemController::None);
        assert_eq!(cfg.chipset, Chipset::Ecs);
        assert_eq!(cfg.chip_ram_bytes, 1024 * 1024);
        assert_eq!(cfg.slow_ram_bytes, 0);
        // The A500+ (Rev 8A) board carries the 2 MB-capable 8375, like the
        // A600, even though it ships with 1 MB chip RAM fitted.
        assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8375);
        assert_eq!(cfg.denise_revision, DeniseRevision::Ecs8373);
        assert_eq!(cfg.gate_array, GateArray::None);
        // The A500+ has an OKI RTC soldered to the motherboard.
        assert!(cfg.rtc_present);

        let cfg = parse_config(
            r#"
            [machine]
            profile = "A1200"
            "#,
        )?;
        assert_eq!(cfg.cpu, CpuModel::M68EC020);
        assert_eq!(cfg.slow_ram_bytes, 0);
        assert_eq!(cfg.gate_array, GateArray::GayleA1200);
        assert_eq!(cfg.agnus_revision, AgnusRevision::AgaAlice);
        assert_eq!(cfg.denise_revision, DeniseRevision::AgaLisa);

        // The big-box machines: Ramsey instead of Gayle, and a real CPU.
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A4000"
            "#,
        )?;
        assert_eq!(cfg.chipset, Chipset::Aga);
        assert_eq!(cfg.cpu, CpuModel::M68040);
        assert_eq!(cfg.chip_ram_bytes, 2 * 1024 * 1024);
        assert_eq!(cfg.slow_ram_bytes, 0);
        // Fat Gary, not Gayle: the big-box machines fill the same seat with the
        // other chip, so no PCMCIA and no Gayle IDE.
        assert_eq!(cfg.gate_array, GateArray::FatGary);
        assert_eq!(cfg.gate_array.gayle_id(), None);
        assert_eq!(cfg.mem_controller, MemController::Ramsey7);
        assert!(cfg.rtc_present);
        // With no [ide] drives the ROM's scsi.device would only stall the
        // boot probing the empty cable, so it is disabled by default...
        assert!(cfg.ide_a4000);
        assert!(cfg.rom_scsi_device_disable);

        // ...but drives on the cable need it: scsi.device is their boot path.
        let img = std::env::temp_dir().join(format!("clfs-ide-{}.img", std::process::id()));
        std::fs::write(&img, vec![0u8; 512 * 16]).unwrap();
        // TOML literal (single-quoted) strings so a Windows temp path's
        // backslashes are not parsed as escape sequences.
        let cfg = parse_config(&format!(
            r#"
            [machine]
            profile = "A4000"
            [ide]
            master = '{}'
            "#,
            img.display()
        ))?;
        assert!(!cfg.rom_scsi_device_disable);

        // Same rule on a Gayle machine: an A1200 with no drives skips the
        // driver, one with drives keeps it.
        let cfg = parse_config("[machine]\nprofile = \"A1200\"")?;
        assert!(cfg.rom_scsi_device_disable);
        let cfg = parse_config(&format!(
            "[machine]\nprofile = \"A1200\"\n[ide]\nmaster = '{}'",
            img.display()
        ))?;
        assert!(!cfg.rom_scsi_device_disable);

        let cfg = parse_config(
            r#"
            [machine]
            profile = "A3000"
            "#,
        )?;
        assert_eq!(cfg.chipset, Chipset::Ecs);
        assert_eq!(cfg.cpu, CpuModel::M68030);
        assert_eq!(cfg.gate_array, GateArray::FatGary);
        assert_eq!(cfg.mem_controller, MemController::Ramsey4);
        assert!(cfg.sdmac);
        // An empty SDMAC SCSI bus is probe time too, and a drive on it brings
        // the driver back, exactly like the IDE machines.
        assert!(cfg.rom_scsi_device_disable);
        let cfg = parse_config(&format!(
            "[machine]\nprofile = \"A3000\"\n[scsi]\nunit0 = '{}'",
            img.display()
        ))?;
        assert!(!cfg.rom_scsi_device_disable);
        std::fs::remove_file(&img).unwrap();

        // The default is an opt-out, not a lock-out: setting the flag wins
        // over the empty-bus heuristic in both directions.
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A3000"
            rom_scsi_device_disable = false
            "#,
        )?;
        assert!(!cfg.rom_scsi_device_disable);
        // A machine with no built-in controller has no scsi.device in ROM;
        // there is nothing to disable.
        assert!(!parse_config("")?.rom_scsi_device_disable);

        let err = parse_config(
            r#"
            [machine]
            profile = "A5000"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown machine model"), "{err:#}");
        Ok(())
    }

    #[test]
    fn a500_rev6a_agnus_allows_up_to_1mb_chip() -> Result<()> {
        // The Fatter 8372A reaches 1 MiB, so the 1 MiB chip-RAM mod is a
        // valid A500 configuration and still carries the 8372A (not the
        // 2 MiB 8375) alongside the OCS Denise.
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A500"
            [memory]
            chip = "1M"
            slow = "0"
            "#,
        )?;
        assert_eq!(cfg.chip_ram_bytes, 1024 * 1024);
        assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8372Rev4);
        assert_eq!(cfg.denise_revision, DeniseRevision::Ocs);

        // 2 MiB chip exceeds the 8372A's 1 MiB address reach: rejected
        // rather than silently promoted to a 2 MiB 8375.
        let err = parse_config(
            r#"
            [machine]
            profile = "A500"
            [memory]
            chip = "2M"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Agnus address reach"), "{err:#}");

        // An explicit [chipset] revision overrides the profile's board chips:
        // profile = "A500" + revision = "OCS" is a plain 8371/8362 OCS
        // machine, not the Fatter-Agnus Rev 6A.
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A500"
            [chipset]
            revision = "OCS"
            "#,
        )?;
        assert_eq!(cfg.chipset, Chipset::Ocs);
        assert_eq!(cfg.agnus_revision, AgnusRevision::Ocs);
        assert_eq!(cfg.denise_revision, DeniseRevision::Ocs);
        Ok(())
    }

    #[test]
    fn a500ocs_profile_is_a_plain_512k_ocs_machine() -> Result<()> {
        // The early A500 (Rev 3/5) / A2000: 8370/8371 Fat Agnus + OCS Denise,
        // 512 KiB chip + 512 KiB trapdoor slow RAM, no gate array.
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A500OCS"
            "#,
        )?;
        assert_eq!(cfg.machine, Some(MachineModel::A500Ocs));
        assert_eq!(cfg.chipset, Chipset::Ocs);
        assert_eq!(cfg.agnus_revision, AgnusRevision::Ocs);
        assert_eq!(cfg.denise_revision, DeniseRevision::Ocs);
        assert_eq!(cfg.chip_ram_bytes, 512 * 1024);
        assert_eq!(cfg.slow_ram_bytes, 512 * 1024);
        assert_eq!(cfg.gate_array, GateArray::None);

        // The OCS Fat Agnus tops out at 512 KiB chip RAM.
        let err = parse_config(
            r#"
            [machine]
            profile = "A500OCS"
            [memory]
            chip = "1M"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("chipset maximum"), "{err:#}");
        Ok(())
    }

    #[test]
    fn a1000_profile_is_an_ocs_machine_with_wcs_defaults() -> Result<()> {
        // The original Amiga: OCS 8361/8367 Agnus + OCS 8362 Denise, 256 KiB
        // stock chip RAM, no slow RAM, no RTC, no gate array. The `rom` is the
        // 64 KiB bootstrap ROM (loaded by Memory::load_a1000, not here).
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A1000"
            "#,
        )?;
        assert_eq!(cfg.machine, Some(MachineModel::A1000));
        assert_eq!(cfg.chipset, Chipset::Ocs);
        assert_eq!(cfg.agnus_revision, AgnusRevision::Ocs);
        assert_eq!(cfg.denise_revision, DeniseRevision::Ocs);
        assert_eq!(cfg.chip_ram_bytes, 256 * 1024);
        assert_eq!(cfg.slow_ram_bytes, 0);
        assert!(!cfg.rtc_present);
        assert_eq!(cfg.gate_array, GateArray::None);
        Ok(())
    }

    #[test]
    fn machine_profile_accepts_deprecated_model_alias() -> Result<()> {
        // `[machine] model` was the original key name; it now collides
        // visually with `[cpu] model`, so the canonical key is `profile`.
        // The old name stays accepted so existing configs keep working.
        let by_alias = parse_config(
            r#"
            [machine]
            model = "A1200"
            "#,
        )?;
        let by_profile = parse_config(
            r#"
            [machine]
            profile = "A1200"
            "#,
        )?;
        assert_eq!(by_alias.machine, Some(MachineModel::A1200));
        assert_eq!(by_alias.machine, by_profile.machine);
        Ok(())
    }

    #[test]
    fn ide_images_require_a_machine_with_an_ide_port() {
        // The default A500 has nowhere to put them.
        let err = parse_config(
            r#"
            [ide]
            master = "disk.hdf"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("IDE port"), "{err:#}");

        let cfg = parse_config(
            r#"
            [machine]
            profile = "A600"
            [ide]
            master = "disk.hdf"
            "#,
        )
        .unwrap();
        assert_eq!(
            cfg.ide.master.as_ref().map(|d| d.path.as_path()),
            Some(Path::new("disk.hdf"))
        );
        assert_eq!(cfg.ide.slave, None);

        // The A4000's port is not Gayle's, but it takes the same drives.
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A4000"
            [ide]
            master = "disk.hdf"
            "#,
        )
        .unwrap();
        assert!(cfg.ide_a4000);
        assert!(cfg.gate_array.gayle_id().is_none());
        assert_eq!(
            cfg.ide.master.as_ref().map(|d| d.path.as_path()),
            Some(Path::new("disk.hdf"))
        );
    }

    #[test]
    fn ecs_preset_picks_agnus_variant_from_chip_ram() -> Result<()> {
        let cfg = parse_config(
            r#"
            [chipset]
            revision = "ECS"
            [memory]
            chip = "512K"
            "#,
        )?;
        assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8372Rev4);
        assert_eq!(cfg.denise_revision, DeniseRevision::Ecs8373);

        let cfg = parse_config(
            r#"
            [chipset]
            revision = "ECS"
            [memory]
            chip = "2M"
            "#,
        )?;
        assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8375);
        Ok(())
    }

    #[test]
    fn chipset_agnus_denise_overrides_parse() -> Result<()> {
        // Late-A500 mix: ECS Agnus with the original OCS Denise.
        let cfg = parse_config(
            r#"
            [chipset]
            revision = "ECS"
            denise = "OCS"
            "#,
        )?;
        assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8372Rev4);
        assert_eq!(cfg.denise_revision, DeniseRevision::Ocs);

        let cfg = parse_config(
            r#"
            [chipset]
            revision = "ECS"
            agnus = "8375"
            "#,
        )?;
        assert_eq!(cfg.agnus_revision, AgnusRevision::Ecs8375);

        let err = parse_config(
            r#"
            [chipset]
            agnus = "8378"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown chipset agnus"), "{err:#}");
        Ok(())
    }

    #[test]
    fn chip_ram_beyond_agnus_reach_is_rejected() {
        let err = parse_config(
            r#"
            [chipset]
            revision = "ECS"
            agnus = "8372A"
            [memory]
            chip = "2M"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Agnus address reach"), "{err:#}");
    }

    #[test]
    fn invalid_video_standard_fails_cleanly() {
        let err = parse_config(
            r#"
            [chipset]
            video = "SECAM"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown chipset video"), "{err:#}");
    }

    #[test]
    fn cpu_68ec020_parses_as_24_bit_020() -> Result<()> {
        let cfg = parse_config(
            r#"
            [cpu]
            model = "68EC020"
            "#,
        )?;
        assert_eq!(cfg.cpu, CpuModel::M68EC020);
        Ok(())
    }

    #[test]
    fn fpu_defaults_from_cpu_model() -> Result<()> {
        // 68881/68882 boards are opt-in on the 020/030...
        let cfg = parse_config(
            r#"
            [cpu]
            model = "68020"
            "#,
        )?;
        assert!(!cfg.fpu);

        // ...but the full 68040 has its FPU on-die.
        let cfg = parse_config(
            r#"
            [cpu]
            model = "68040"
            "#,
        )?;
        assert!(cfg.fpu);
        Ok(())
    }

    #[test]
    fn fpu_needs_the_coprocessor_interface() -> Result<()> {
        // A 68000 cannot drive a 68881/68882.
        let err = parse_config(
            r#"
            [cpu]
            model = "68000"
            fpu = true
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("coprocessor interface"));

        // Any 020+ can.
        let cfg = parse_config(
            r#"
            [cpu]
            model = "68EC020"
            fpu = true
            "#,
        )?;
        assert!(cfg.fpu);
        Ok(())
    }

    #[test]
    fn cpu_68060_without_fpu_is_an_lc060() -> Result<()> {
        // fpu = false on the 060 models the LC/EC parts: accepted, and the
        // core presents it as PCR.DFP (FP instructions take the disabled
        // trap) rather than a config error.
        let cfg = parse_config(
            r#"
            [cpu]
            model = "68060"
            fpu = false
            "#,
        )?;
        assert_eq!(cfg.cpu, CpuModel::M68060);
        assert!(!cfg.fpu);
        Ok(())
    }

    #[test]
    fn fast_ram_must_use_zorro_ii_autoconfig_size() {
        let err = parse_config(
            r#"
            [memory]
            fast = "768K"
            "#,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("not an autoconfigurable"),
            "{err:#}"
        );
    }

    /// The big-box profiles fit their stock 4 MB of Ramsey motherboard RAM;
    /// `[memory] motherboard` resizes it within Ramsey's bank layout, and it
    /// is refused where no Ramsey (or no 32-bit CPU) could drive it.
    #[test]
    fn motherboard_ram_defaults_and_constraints() -> Result<()> {
        let cfg = parse_config("[machine]\nprofile = \"A3000\"")?;
        assert_eq!(cfg.mb_ram_bytes, 4 * 1024 * 1024);
        let cfg = parse_config("[machine]\nprofile = \"A4000\"")?;
        assert_eq!(cfg.mb_ram_bytes, 4 * 1024 * 1024);

        // Resizable up to the full 16 MB, and removable.
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A4000"
            [memory]
            motherboard = "16M"
            "#,
        )?;
        assert_eq!(cfg.mb_ram_bytes, 16 * 1024 * 1024);
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A4000"
            [memory]
            motherboard = "0"
            "#,
        )?;
        assert_eq!(cfg.mb_ram_bytes, 0);

        // A total that fills no whole bank layout is refused.
        let err = parse_config(
            r#"
            [machine]
            profile = "A4000"
            [memory]
            motherboard = "5M"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Ramsey banks"), "{err:#}");

        // No Ramsey to drive it on the default A500-class machine.
        let err = parse_config("[memory]\nmotherboard = \"4M\"").unwrap_err();
        assert!(
            err.to_string().contains("Ramsey memory controller"),
            "{err:#}"
        );

        // A 24-bit CPU cannot reach $08000000 at all.
        let err = parse_config(
            r#"
            [machine]
            profile = "A3000"
            [cpu]
            model = "68000"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("24-bit"), "{err:#}");
        Ok(())
    }

    /// Beyond Ramsey's four banks the A4000 fills the $04000000-$06FFFFFF
    /// motherboard RAM expansion space in whole 4M banks up to 64M; the
    /// A3000's Ramsey-04 does not, and partial banks are refused.
    #[test]
    fn motherboard_ram_expansion_space_is_an_a4000_option() -> Result<()> {
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A4000"
            [memory]
            motherboard = "64M"
            "#,
        )?;
        assert_eq!(cfg.mb_ram_bytes, 64 * 1024 * 1024);
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A4000"
            [memory]
            motherboard = "20M"
            "#,
        )?;
        assert_eq!(cfg.mb_ram_bytes, 20 * 1024 * 1024);

        // The A3000 stops at Ramsey's own 16M.
        let err = parse_config(
            r#"
            [machine]
            profile = "A3000"
            [memory]
            motherboard = "32M"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("A4000 option"), "{err:#}");

        // Partial expansion banks and totals past the window are refused.
        for size in ["18M", "65M", "128M"] {
            let err = parse_config(&format!(
                "[machine]\nprofile = \"A4000\"\n[memory]\nmotherboard = \"{size}\""
            ))
            .unwrap_err();
            assert!(err.to_string().contains("expansion space"), "{err:#}");
        }
        Ok(())
    }

    /// Accelerator (CPU-slot) RAM at $08000000 needs only a 32-bit address
    /// bus: any megabyte total up to the 128M slot space, on any machine.
    #[test]
    fn accelerator_ram_gates_on_the_cpu_bus() -> Result<()> {
        let cfg = parse_config(
            r#"
            [cpu]
            model = "68030"
            [memory]
            accelerator = "128M"
            "#,
        )?;
        assert_eq!(cfg.accel_ram_bytes, 128 * 1024 * 1024);
        // Not tied to the big-box profiles: an accelerated A1200 counts.
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A1200"
            [cpu]
            model = "68030"
            [memory]
            accelerator = "64M"
            "#,
        )?;
        assert_eq!(cfg.accel_ram_bytes, 64 * 1024 * 1024);

        // The stock A1200 EC020 has a 24-bit bus.
        let err = parse_config(
            r#"
            [machine]
            profile = "A1200"
            [memory]
            accelerator = "64M"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("24-bit"), "{err:#}");

        // Sub-megabyte and beyond-the-slot totals are refused.
        for size in ["512K", "129M"] {
            let err = parse_config(&format!(
                "[cpu]\nmodel = \"68030\"\n[memory]\naccelerator = \"{size}\""
            ))
            .unwrap_err();
            assert!(err.to_string().contains("CPU-slot space"), "{err:#}");
        }
        Ok(())
    }

    #[test]
    fn cpu_cache_flags_gate_on_model() -> Result<()> {
        let cfg = parse_config(
            r#"
            [cpu]
            model = "68030"
            icache = true
            dcache = true
            "#,
        )?;
        assert!(cfg.cpu_icache);
        assert!(cfg.cpu_dcache);

        // Caches default on for the silicon that has them: a 68020/68EC020
        // gets the instruction cache (no data cache); a 68030 gets both.
        let cfg = parse_config("[cpu]\nmodel = \"68020\"")?;
        assert!(cfg.cpu_icache && !cfg.cpu_dcache);
        let cfg = parse_config("[cpu]\nmodel = \"68030\"")?;
        assert!(cfg.cpu_icache && cfg.cpu_dcache);
        // A 68040 gets both its (4 KB) caches by default.
        let cfg = parse_config("[cpu]\nmodel = \"68040\"")?;
        assert!(cfg.cpu_icache && cfg.cpu_dcache);

        // A plain 68000 has neither.
        let cfg = parse_config("[cpu]\nmodel = \"68000\"")?;
        assert!(!cfg.cpu_icache && !cfg.cpu_dcache);

        // The default is overridable: a 020 can opt out of its instruction cache.
        let cfg = parse_config("[cpu]\nmodel = \"68020\"\nicache = false")?;
        assert!(!cfg.cpu_icache);

        let err = parse_config("[cpu]\nicache = true").unwrap_err();
        assert!(err.to_string().contains("icache"), "{err:#}");

        let err = parse_config(
            r#"
            [cpu]
            model = "68020"
            dcache = true
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("68030"), "{err:#}");
        Ok(())
    }

    #[test]
    fn z3_ram_needs_a_32_bit_cpu() {
        let err = parse_config(
            r#"
            [memory]
            z3 = "16M"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("32-bit address bus"), "{err:#}");

        let err = parse_config(
            r#"
            [cpu]
            model = "68EC020"
            [memory]
            z3 = "16M"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("32-bit address bus"), "{err:#}");
    }

    #[test]
    fn z3_ram_parses_with_32_bit_cpu_and_validates_size() -> Result<()> {
        let cfg = parse_config(
            r#"
            [cpu]
            model = "68030"
            [memory]
            z3 = "16M"
            "#,
        )?;
        assert_eq!(cfg.z3_ram_bytes, 16 * 1024 * 1024);

        let err = parse_config(
            r#"
            [cpu]
            model = "68030"
            [memory]
            z3 = "24M"
            "#,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("not an autoconfigurable"),
            "{err:#}"
        );
        Ok(())
    }

    #[test]
    fn scsi_section_parses_units_and_requires_the_boot_rom() -> Result<()> {
        let cfg = parse_config(
            r#"
            [scsi]
            rom = "a2091.rom"
            unit0 = "workbench.hdf"
            unit3 = "data.hdf"
            "#,
        )?;
        assert!(cfg.scsi.enabled());
        assert_eq!(cfg.scsi.rom.as_deref(), Some(Path::new("a2091.rom")));
        assert_eq!(
            cfg.scsi.units[0].as_ref().map(|d| d.path.as_path()),
            Some(Path::new("workbench.hdf"))
        );
        assert!(cfg.scsi.units[1].is_none());
        assert_eq!(
            cfg.scsi.units[3].as_ref().map(|d| d.path.as_path()),
            Some(Path::new("data.hdf"))
        );

        // Drives without the boot ROM cannot work: the ROM carries the
        // scsi.device driver.
        let err = parse_config(
            r#"
            [scsi]
            unit0 = "workbench.hdf"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("boot ROM"), "{err:#}");

        // SCSI works on any machine model (no Gayle requirement). This also
        // exercises the deprecated `model` alias for `[machine] profile`.
        let cfg = parse_config(
            r#"
            [machine]
            model = "A500"
            [scsi]
            rom = "a2091.rom"
            unit0 = "dh0.hdf"
            "#,
        )?;
        assert!(cfg.scsi.enabled());
        Ok(())
    }

    /// CD images (cue sheets and bare ISOs) are recognised by extension:
    /// they attach as SCSI CD-ROM drives, and the ATA-only IDE port
    /// rejects them.
    #[test]
    fn cd_images_fit_scsi_units_but_not_the_ide_port() -> Result<()> {
        assert!(is_cd_image_path(Path::new("games/Disc.CUE")));
        assert!(is_cd_image_path(Path::new("cd32.iso")));
        assert!(!is_cd_image_path(Path::new("workbench.hdf")));
        assert!(!is_cd_image_path(Path::new("directory/")));

        let cfg = parse_config(
            r#"
            [scsi]
            rom = "a2091.rom"
            unit0 = "workbench.hdf"
            unit2 = "game.cue"
            "#,
        )?;
        assert_eq!(
            cfg.scsi.units[2].as_ref().map(|d| d.path.as_path()),
            Some(Path::new("game.cue"))
        );

        let err = parse_config(
            r#"
            [machine]
            profile = "A1200"
            [ide]
            master = "game.iso"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("ATAPI"), "{err:#}");
        Ok(())
    }

    /// The A3000's SCSI is motherboard silicon: its drives need no boot ROM,
    /// they are the default on that machine, and they fit nowhere else.
    #[test]
    fn the_a3000_scsi_bus_takes_drives_without_a_boot_rom() -> Result<()> {
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A3000"
            [scsi]
            unit0 = "workbench.hdf"
            "#,
        )?;
        assert!(cfg.sdmac);
        assert_eq!(cfg.scsi.controller, ScsiController::A3000);
        assert_eq!(
            cfg.scsi.units[0].as_ref().map(|d| d.path.as_path()),
            Some(Path::new("workbench.hdf"))
        );

        // A Zorro board still fits an A3000, and there it does need its ROM.
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A3000"
            [scsi]
            controller = "a2091"
            rom = "a2091.rom"
            unit0 = "workbench.hdf"
            "#,
        )?;
        assert_eq!(cfg.scsi.controller, ScsiController::A2091);

        // No Super DMAC, no motherboard SCSI.
        let err = parse_config(
            r#"
            [machine]
            profile = "A1200"
            [scsi]
            controller = "a3000"
            unit0 = "workbench.hdf"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("motherboard SCSI"), "{err:#}");

        // And there is no ROM to give it.
        let err = parse_config(
            r#"
            [machine]
            profile = "A3000"
            [scsi]
            rom = "a2091.rom"
            unit0 = "workbench.hdf"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no boot ROM"), "{err:#}");
        Ok(())
    }

    /// An A4091 without a named ROM falls back to the bundled one: validation
    /// leaves the sentinel in place, and resolution swaps in the real path.
    #[test]
    fn a4091_without_rom_defaults_to_the_bundled_rom() -> Result<()> {
        let cfg = parse_config(
            r#"
            [scsi]
            controller = "a4091"
            unit0 = "workbench.hdf"
            "#,
        )?;
        assert_eq!(cfg.scsi.controller, ScsiController::A4091);
        assert_eq!(cfg.scsi.rom.as_deref(), Some(Path::new(BUNDLED_A4091_ROM)));

        // A bare `controller = "a4091"` (no drives, no ROM) still fits the
        // board: the bundled ROM makes it enabled, as naming a ROM always did.
        let mut cfg = parse_config(
            r#"
            [scsi]
            controller = "a4091"
            "#,
        )?;
        assert!(cfg.scsi.enabled());
        assert_eq!(cfg.scsi.rom.as_deref(), Some(Path::new(BUNDLED_A4091_ROM)));

        // Resolution finds the ROM bundled in the source tree (assets/a4091).
        resolve_bundled_rom(&mut cfg)?;
        let rom = cfg.scsi.rom.as_deref().expect("resolved A4091 rom");
        assert!(rom.ends_with(crate::romsearch::A4091_ROM_FILE), "{rom:?}");
        assert_ne!(rom, Path::new(BUNDLED_A4091_ROM));

        // An explicit rom still wins.
        let cfg = parse_config(
            r#"
            [scsi]
            controller = "a4091"
            rom = "custom-a4091.rom"
            unit0 = "workbench.hdf"
            "#,
        )?;
        assert_eq!(cfg.scsi.rom.as_deref(), Some(Path::new("custom-a4091.rom")));

        // The A2091 has no bundled ROM, so it still errors without one.
        let err = parse_config(
            r#"
            [scsi]
            controller = "a2091"
            unit0 = "workbench.hdf"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("boot ROM"), "{err:#}");
        Ok(())
    }

    #[test]
    fn drive_entries_accept_a_volume_name_override() -> Result<()> {
        // IDE and SCSI drives take either a bare path or a table carrying an
        // explicit volume name; the bare form leaves the name unset.
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A1200"
            [ide]
            master = { path = "games/", name = "Games" }
            slave = "data.hdf"
            "#,
        )?;
        let master = cfg.ide.master.as_ref().expect("master configured");
        assert_eq!(master.path, Path::new("games/"));
        assert_eq!(master.volume_name.as_deref(), Some("Games"));
        let slave = cfg.ide.slave.as_ref().expect("slave configured");
        assert_eq!(slave.path, Path::new("data.hdf"));
        assert_eq!(slave.volume_name, None);

        let cfg = parse_config(
            r#"
            [scsi]
            rom = "a2091.rom"
            unit0 = { path = "work/", name = "Work Disk" }
            "#,
        )?;
        let unit0 = cfg.scsi.units[0].as_ref().expect("unit0 configured");
        assert_eq!(unit0.volume_name.as_deref(), Some("Work Disk"));
        Ok(())
    }

    #[test]
    fn the_memory_controller_can_be_selected() -> anyhow::Result<()> {
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A1200"
            mem_controller = "ramsey-07"
            "#,
        )?;
        assert_eq!(cfg.mem_controller, MemController::Ramsey7);
        assert_eq!(
            cfg.mem_controller.ramsey_revision(),
            Some(crate::ramsey::RamseyRevision::Rev7)
        );

        let err = parse_config(
            r#"
            [machine]
            mem_controller = "ramsey-08"
            "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ramsey-04"), "{err}");
        Ok(())
    }

    #[test]
    fn log_unmapped_takes_all_or_a_hex_range() -> anyhow::Result<()> {
        let cfg = parse_config(
            r#"
            [debug]
            log_unmapped = "DD0000-DEFFFF"
            "#,
        )?;
        assert_eq!(cfg.log_unmapped, Some(0x00DD_0000..=0x00DE_FFFF));

        let cfg = parse_config(
            r#"
            [debug]
            log_unmapped = "all"
            "#,
        )?;
        // "all" must include the very top of the address space.
        assert_eq!(cfg.log_unmapped, Some(0..=u32::MAX));
        assert!(cfg.log_unmapped.unwrap().contains(&0xFFFF_FFFF));

        assert_eq!(parse_config("")?.log_unmapped, None);

        // An end below the start would silently log nothing.
        let err = parse_config(
            r#"
            [debug]
            log_unmapped = "DE0000-DD0000"
            "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("start must not be above end"), "{err}");
        Ok(())
    }

    #[test]
    fn drive_name_override_is_validated() {
        // A ':' or '/' is illegal in an AmigaDOS volume name.
        let err = parse_config(
            r#"
            [scsi]
            rom = "a2091.rom"
            unit0 = { path = "work/", name = "Bad:Name" }
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid character"), "{err:#}");

        // Over the 30-character FFS volume-label limit.
        let err = parse_config(&format!(
            r#"
            [scsi]
            rom = "a2091.rom"
            unit0 = {{ path = "work/", name = "{}" }}
            "#,
            "X".repeat(31)
        ))
        .unwrap_err();
        assert!(err.to_string().contains("too long"), "{err:#}");

        // A blank name is treated as no override (not an error).
        let cfg = parse_config(
            r#"
            [scsi]
            rom = "a2091.rom"
            unit0 = { path = "work/", name = "  " }
            "#,
        )
        .unwrap();
        assert_eq!(cfg.scsi.units[0].as_ref().unwrap().volume_name, None);

        // An unknown key in the table form is rejected.
        let err = parse_config(
            r#"
            [scsi]
            rom = "a2091.rom"
            unit0 = { path = "work/", label = "Work" }
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("label"), "{err:#}");
    }

    #[test]
    fn mixed_named_and_bare_drives_round_trip_through_saved_toml() {
        // A named drive serializes as a sub-table; a bare sibling must not be
        // swallowed by it (TOML requires scalar keys before sub-tables). Save
        // the whole config the way the panel does and parse it back.
        let raw = RawConfig {
            scsi: RawScsi {
                rom: Some("a2091.rom".to_string()),
                unit0: Some(RawDrive {
                    path: "work/".to_string(),
                    name: Some("Work".to_string()),
                    bootpri: None,
                }),
                unit1: Some(RawDrive::from_path("data.hdf")),
                ..RawScsi::default()
            },
            ..RawConfig::default()
        };
        let text = raw.to_toml_string().unwrap();
        let back: RawConfig = toml::from_str(&text).unwrap();
        assert_eq!(raw, back, "round-trip mismatch; TOML was:\n{text}");
    }

    #[test]
    fn drive_entry_round_trips_through_toml() {
        // No name: serializes back to the bare string form.
        let bare = RawIde {
            master: Some(RawDrive::from_path("disk.hdf")),
            slave: None,
        };
        let text = toml::to_string(&bare).unwrap();
        assert!(text.contains(r#"master = "disk.hdf""#), "{text}");

        // With a name: serializes to the inline table and parses back.
        let named = RawIde {
            master: Some(RawDrive {
                path: "games/".to_string(),
                name: Some("Games".to_string()),
                bootpri: None,
            }),
            slave: None,
        };
        let text = toml::to_string(&named).unwrap();
        let parsed: RawIde = toml::from_str(&text).unwrap();
        assert_eq!(parsed, named);

        // With only a boot priority: still the inline-table form, and the
        // name key stays absent.
        let prioritised = RawIde {
            master: Some(RawDrive {
                path: "wb.hdf".to_string(),
                name: None,
                bootpri: Some(6),
            }),
            slave: None,
        };
        let text = toml::to_string(&prioritised).unwrap();
        assert!(!text.contains("name"), "{text}");
        let parsed: RawIde = toml::from_str(&text).unwrap();
        assert_eq!(parsed, prioritised);
    }

    // A build without the feature has no bridges to configure: the keys are
    // read and ignored, so there is nothing here to assert.
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn floppy_bridge_parses_and_defaults() -> Result<()> {
        let cfg = parse_config(
            r#"
            [floppy.df0]
            bridge = "greaseweazle"
            [floppy.df1]
            bridge = "GreaseWeazle"
            bridge_port = "/dev/tty.usbmodem1111301"
            bridge_mode = "stalling"
            bridge_density = "hd"
            bridge_cable = "b"
            replay_speed = "normal"
        "#,
        )?;
        let df0 = cfg.floppy.bridges[0].as_ref().expect("df0 bridged");
        assert_eq!(df0.driver, BridgeDriver::Greaseweazle);
        // Unset options take the defaults: auto-detect the interface, read
        // without waiting for the index, and sense the density.
        assert_eq!(df0.port, None);
        assert_eq!(df0.mode, BridgeReadMode::Normal);
        assert_eq!(df0.density, BridgeDensity::Auto);
        assert_eq!(df0.speed, DEFAULT_BRIDGE_SPEED_PERCENT);

        // Spelling and case are the user's business, not the parser's.
        let df1 = cfg.floppy.bridges[1].as_ref().expect("df1 bridged");
        assert_eq!(df1.driver, BridgeDriver::Greaseweazle);
        assert_eq!(df1.port.as_deref(), Some("/dev/tty.usbmodem1111301"));
        assert_eq!(df1.mode, BridgeReadMode::Stalling);
        assert_eq!(df1.density, BridgeDensity::Hd);
        assert_eq!(df1.cable, BridgeCable::DriveB);
        assert_eq!(df1.speed, 100);

        // Bridging a bay wires the drive in, and leaves it with no image.
        assert!(cfg.floppy.drives[0].is_none());
        assert!(cfg.floppy_connected[0] && cfg.floppy_connected[1]);
        Ok(())
    }

    /// A driver name this build does not carry is refused where it is read,
    /// naming what is available -- not accepted here only to fail later when
    /// the drive is opened, by which time the machine is half built.
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn a_driver_this_build_lacks_is_refused_with_what_it_has() {
        let available = supported_bridge_drivers();
        assert!(available.contains(&"greaseweazle"), "{available:?}");
        for absent in ["drawbridge", "supercardpro"] {
            if available.contains(&absent) {
                continue;
            }
            let err = parse_config(&format!(
                r#"
                [floppy.df0]
                bridge = "{absent}"
            "#
            ))
            .expect_err("a driver this build lacks cannot be configured")
            .to_string();
            assert!(err.contains("this build has no"), "unexpected: {err}");
            assert!(err.contains("greaseweazle"), "must say what it has: {err}");
        }
    }

    /// Only the listed serving speeds are accepted, by name in the error so
    /// a typo explains itself. A value between two of them is still refused.
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn floppy_bridge_speed_rejects_unsupported_values() {
        for bad in [120, 160, 250] {
            let err = parse_config(&format!(
                r#"
                [floppy.df0]
                bridge = "greaseweazle"
                bridge_speed = {bad}
            "#
            ))
            .expect_err("not a supported serving speed");
            let msg = format!("{err:#}");
            assert!(msg.contains("replay_speed"), "unexpected error: {msg}");
            assert!(msg.contains("normal"), "names the accepted values: {msg}");
        }
    }

    /// Every listed speed parses back as itself, the fastest included.
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn floppy_bridge_speed_accepts_every_listed_value() -> Result<()> {
        for want in SUPPORTED_BRIDGE_SPEED_PERCENTS {
            let cfg = parse_config(&format!(
                r#"
                [floppy.df0]
                bridge = "greaseweazle"
                bridge_speed = {want}
            "#
            ))?;
            let df0 = cfg.floppy.bridges[0].as_ref().expect("df0 bridged");
            assert_eq!(df0.speed, want);
        }
        Ok(())
    }

    // A build without the feature has no bridges to configure: the keys are
    // read and ignored, so there is nothing here to assert.
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn floppy_bridge_rejects_conflicts_and_typos() {
        // A real drive brings its own disk, so an image alongside it is a
        // contradiction rather than a fallback.
        let err = parse_config(
            r#"
            [floppy.df0]
            bridge = "greaseweazle"
            path = "game.adf"
        "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("bridge and a disk image"), "{err}");

        let err = parse_config(
            r#"
            [floppy.df0]
            bridge = "greasewheel"
        "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not a known interface"), "{err}");

        // `off` is how a bay keeps its bridge settings but goes back to images:
        // the image is then parsed normally, so the only complaint left is the
        // missing file rather than anything about bridges.
        let err = parse_config(
            r#"
            [floppy.df0]
            bridge = "off"
            path = "game.adf"
        "#,
        )
        .unwrap_err()
        .to_string();
        assert!(
            !err.contains("bridge"),
            "bridge = off is not a bridge: {err}"
        );

        // "turbo" is not a read mode at all -- it intercepts AmigaDOS calls --
        // so it is named in the refusal rather than silently swapped for one
        // that works, and a config brought over from another emulator
        // explains itself.
        assert_eq!(
            parse_config(
                r#"
                [floppy.df0]
                bridge = "greaseweazle"
                bridge_mode = "normal"
            "#
            )
            .unwrap()
            .floppy
            .bridges[0]
                .as_ref()
                .unwrap()
                .mode,
            BridgeReadMode::Normal
        );
        let err = parse_config(
            r#"
                [floppy.df0]
                bridge = "greaseweazle"
                bridge_mode = "turbo"
            "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("instead of"), "{err}");
    }

    #[test]
    fn drive_bootpri_defaults_to_zero_and_parses() -> Result<()> {
        let cfg = parse_config(
            r#"
            [machine]
            profile = "A1200"
            [ide]
            master = "wb.hdf"
            slave = { path = "extra.hdf", bootpri = -128 }
            "#,
        )?;
        assert_eq!(
            cfg.ide.master.as_ref().unwrap().boot_pri,
            HARDFILE_DEFAULT_BOOT_PRI
        );
        assert_eq!(cfg.ide.slave.as_ref().unwrap().boot_pri, BOOT_PRI_NEVER);

        // Out-of-range values are rejected by the i8 field type.
        assert!(parse_config(
            r#"
            [machine]
            profile = "A1200"
            [ide]
            master = { path = "wb.hdf", bootpri = 500 }
            "#,
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn zorro_metadata_boards_parse_and_gate_on_cpu() -> Result<()> {
        let meta = temp_path("board.toml");
        fs::write(
            &meta,
            r#"
            name = "MegaRAM"
            zorro = 3
            type = "ram"
            size = "32M"
            manufacturer = 2011
            product = 32
            "#,
        )?;

        let cfg = parse_config(&format!(
            r#"
            [cpu]
            model = "68030"
            [[zorro]]
            metadata = "{}"
            "#,
            toml_path(&meta)
        ))?;
        assert_eq!(cfg.zorro_boards.len(), 1);
        assert_eq!(cfg.zorro_boards[0].name, "MegaRAM");
        assert_eq!(cfg.zorro_boards[0].size_bytes, 32 * 1024 * 1024);

        let err = parse_config(&format!(
            r#"
            [[zorro]]
            metadata = "{}"
            "#,
            toml_path(&meta)
        ))
        .unwrap_err();
        assert!(err.to_string().contains("needs a 32-bit CPU"), "{err:#}");

        let _ = fs::remove_file(&meta);
        Ok(())
    }

    #[test]
    fn zorro_metadata_boards_reject_the_bundled_hostsocket_sentinels() -> Result<()> {
        // A metadata board naming a bundled-artifact sentinel must fail fast,
        // not silently instantiate the embedded HostSocket module/ROM under
        // its own autoconfig identity.
        let meta = temp_path("sentinel-board.toml");
        fs::write(
            &meta,
            format!(
                r#"
                name = "Impostor"
                zorro = 2
                type = "wasm"
                size = "64K"
                manufacturer = 2011
                product = 33
                wasm = "{}"
                "#,
                crate::hostsocket::BUNDLED_HOSTSOCKET_WASM
            ),
        )?;
        let err =
            parse_config(&format!("[[zorro]]\nmetadata = \"{}\"\n", toml_path(&meta))).unwrap_err();
        assert!(err.to_string().contains("reserved"), "{err:#}");

        fs::write(
            &meta,
            r#"
            name = "Impostor"
            zorro = 2
            type = "wasm"
            size = "64K"
            manufacturer = 2011
            product = 33
            wasm = "board.wasm"

            [[option]]
            key = "rom"
            label = "ROM"
            type = "file"
            "#,
        )?;
        let err = parse_config(&format!(
            "[[zorro]]\nmetadata = \"{}\"\nconfig = {{ rom = \"{}\" }}\n",
            toml_path(&meta),
            crate::hostsocket::BUNDLED_HOSTSOCKET_ROM
        ))
        .unwrap_err();
        assert!(err.to_string().contains("reserved"), "{err:#}");

        let _ = fs::remove_file(&meta);
        Ok(())
    }

    #[test]
    fn identify_board_present_by_default() -> Result<()> {
        // A bare config (no fast/Z3/metadata boards) still puts the
        // Copperline identification board on the chain.
        let cfg = parse_config("")?;
        assert!(cfg.identify_board);
        let chain = cfg.build_zorro_chain()?;
        let base = crate::zorro::AUTOCONFIG_BASE;
        // er_Type: Zorro II, no MEMLIST, 64K (size code 1) = 0xC1, exposed
        // high nibble then low nibble (er_Type is not inverted).
        assert_eq!(chain.config_read(base, 1), 0xC0);
        assert_eq!(chain.config_read(base + 2, 1), 0x10);
        // er_Product = 2, inverted to 0xFD on the physical nibbles.
        assert_eq!(chain.config_read(base + 4, 1), 0xF0);
        assert_eq!(chain.config_read(base + 6, 1), 0xD0);
        Ok(())
    }

    #[test]
    fn identify_false_drops_the_board() -> Result<()> {
        let cfg = parse_config("identify = false")?;
        assert!(!cfg.identify_board);
        // No boards configured at all: the autoconfig window floats.
        let chain = cfg.build_zorro_chain()?;
        assert_eq!(chain.config_read(crate::zorro::AUTOCONFIG_BASE, 1), 0xFF);
        Ok(())
    }

    #[test]
    fn slow_ram_parses_for_a500_trapdoor_memory() -> Result<()> {
        let cfg = parse_config(
            r#"
            [memory]
            slow = "512K"
            "#,
        )?;
        assert_eq!(cfg.slow_ram_bytes, 512 * 1024);
        Ok(())
    }

    #[test]
    fn slow_ram_is_limited_to_trapdoor_size() {
        let err = parse_config(
            r#"
            [memory]
            slow = "1M"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("slow RAM"), "{err:#}");
    }

    #[test]
    fn floppy_path_implies_enabled_and_write_protect_defaults() -> Result<()> {
        let adf = temp_adf()?;
        let cfg = parse_config(&format!(
            r#"
            [floppy.df0]
            path = "{}"
            "#,
            toml_path(&adf)
        ))?;
        let df0 = cfg.floppy.drives[0].as_ref().unwrap();
        assert_eq!(df0.path, adf);
        assert!(df0.write_protected);
        assert_eq!(cfg.floppy_connected, [true, false, false, false]);
        Ok(())
    }

    #[test]
    fn floppy_drive_count_connects_empty_external_mechanisms() -> Result<()> {
        let cfg = parse_config(
            r#"
            [floppy]
            drives = 3
            "#,
        )?;
        assert_eq!(cfg.floppy_connected, [true, true, true, false]);
        assert!(cfg.floppy.drives.iter().all(Option::is_none));
        Ok(())
    }

    #[test]
    fn floppy_speed_defaults_and_parses_supported_values() -> Result<()> {
        assert_eq!(parse_config("")?.floppy.speed, 100);
        for speed in [100u16, 200, 400, 800, 0] {
            let cfg = parse_config(&format!("[floppy]\nspeed = {speed}\n"))?;
            assert_eq!(cfg.floppy.speed, speed);
        }
        Ok(())
    }

    #[test]
    fn floppy_speed_rejects_unsupported_values() {
        for speed in [50, 150, 300, 1600] {
            let err = parse_config(&format!("[floppy]\nspeed = {speed}\n")).unwrap_err();
            assert!(
                err.to_string().contains("[floppy] speed"),
                "unexpected error for speed {speed}: {err}"
            );
        }
    }

    #[test]
    fn floppy_speed_cli_override_reaches_config() -> Result<()> {
        let cfg = load_overrides(&ConfigOverrides {
            floppy_speed: Some(0),
            ..Default::default()
        })?;
        assert_eq!(cfg.floppy.speed, 0);
        Ok(())
    }

    #[test]
    fn cpu_clock_defaults_per_model_and_converts_to_cck_multiple() {
        assert_eq!(CpuModel::M68000.default_clock_mhz(), 7.09);
        assert_eq!(CpuModel::M68020.default_clock_mhz(), 14.0);
        assert_eq!(CpuModel::M68040.default_clock_mhz(), 25.0);
        // Whole multiples of the colour clock ("multiples of the bus").
        assert_eq!(clocks_per_cck_for_mhz(7.09), 2);
        assert_eq!(clocks_per_cck_for_mhz(14.0), 4);
        assert_eq!(clocks_per_cck_for_mhz(25.0), 7);
        // Never zero.
        assert_eq!(clocks_per_cck_for_mhz(0.5), 1);
    }

    #[test]
    fn cpu_68060_parses_with_full_defaults() -> Result<()> {
        let cfg = parse_config(
            r#"
            [cpu]
            model = "68060"
            "#,
        )?;
        assert_eq!(cfg.cpu, CpuModel::M68060);
        assert_eq!(cfg.cpu_clock_mhz, 50.0, "060 defaults to 50 MHz");
        assert!(cfg.fpu, "the full 68060 has its FPU on-die");
        assert!(cfg.cpu_icache && cfg.cpu_dcache, "8 KB caches default on");
        assert_eq!(cfg.cpu_unimplemented, UnimplementedPolicy::Trap);
        Ok(())
    }

    #[test]
    fn cpu_jit_parses_and_defaults_off() -> Result<()> {
        let cfg = parse_config("")?;
        assert!(!cfg.cpu_jit, "JIT must be opt-in");
        let cfg = parse_config(
            r#"
            [cpu]
            jit = true
            "#,
        )?;
        assert!(cfg.cpu_jit);
        Ok(())
    }

    #[test]
    fn cpu_unimplemented_policy_parses_and_is_68060_only() -> Result<()> {
        let cfg = parse_config(
            r#"
            [cpu]
            model = "68060"
            unimplemented = "native"
            "#,
        )?;
        assert_eq!(cfg.cpu_unimplemented, UnimplementedPolicy::Native);

        let err = parse_config(
            r#"
            [cpu]
            model = "68040"
            unimplemented = "native"
            "#,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("applies only to the 68060"),
            "{err}"
        );

        let err = parse_config(
            r#"
            [cpu]
            model = "68060"
            unimplemented = "sometimes"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("trap"), "{err}");
        Ok(())
    }

    #[test]
    fn cpu_clock_override_is_honoured_and_validated() -> Result<()> {
        let cfg = parse_config(
            r#"
            [cpu]
            model = "68020"
            clock_mhz = 28.0
            "#,
        )?;
        assert_eq!(cfg.cpu, CpuModel::M68020);
        assert_eq!(cfg.cpu_clock_mhz, 28.0);
        // Default applies when unset.
        let cfg = parse_config(
            r#"[cpu]
            model = "68040""#,
        )?;
        assert_eq!(cfg.cpu_clock_mhz, 25.0);
        // Non-positive is rejected.
        assert!(parse_config("[cpu]\nclock_mhz = 0.0").is_err());
        Ok(())
    }

    #[test]
    fn floppy_paths_playlist_is_parsed_in_order() -> Result<()> {
        let disk1 = temp_adf()?;
        let disk2 = temp_adf()?;
        let cfg = parse_config(&format!(
            r#"
            [floppy.df0]
            paths = ["{}", "{}"]
            write_protected = false
            "#,
            toml_path(&disk1),
            toml_path(&disk2),
        ))?;
        // The boot disk is the first playlist entry.
        let df0 = cfg.floppy.drives[0].as_ref().unwrap();
        assert_eq!(df0.path, disk1);
        assert!(!df0.write_protected);
        // The full playlist is exposed in order for the swap key.
        assert_eq!(cfg.floppy_playlists[0], vec![disk1, disk2]);
        assert!(cfg.floppy_playlists[1].is_empty());
        Ok(())
    }

    #[test]
    fn floppy_single_path_yields_one_entry_playlist() -> Result<()> {
        let adf = temp_adf()?;
        let cfg = parse_config(&format!(
            r#"
            [floppy.df0]
            path = "{}"
            "#,
            toml_path(&adf)
        ))?;
        assert_eq!(cfg.floppy_playlists[0], vec![adf]);
        Ok(())
    }

    #[test]
    fn dms_floppy_path_is_accepted() -> Result<()> {
        let dms = temp_path("test.dms");
        fs::write(&dms, b"DMS!test placeholder")?;
        let cfg = parse_config(&format!(
            r#"
            [floppy.df0]
            path = "{}"
            "#,
            toml_path(&dms)
        ))?;
        let df0 = cfg.floppy.drives[0].as_ref().unwrap();
        assert_eq!(df0.path, dms);
        assert!(df0.write_protected);
        let _ = fs::remove_file(df0.path.clone());
        Ok(())
    }

    #[test]
    fn adz_floppy_path_is_accepted() -> Result<()> {
        let adz = temp_path("test.adz");
        fs::write(&adz, [0x1F, 0x8B, 8, 0, 0, 0, 0, 0])?;
        let cfg = parse_config(&format!(
            r#"
            [floppy.df0]
            path = "{}"
            "#,
            toml_path(&adz)
        ))?;
        let df0 = cfg.floppy.drives[0].as_ref().unwrap();
        assert_eq!(df0.path, adz);
        assert!(df0.write_protected);
        let _ = fs::remove_file(df0.path.clone());
        Ok(())
    }

    #[test]
    fn uae_extended_adf_floppy_path_is_accepted() -> Result<()> {
        let adf = temp_path("test.ext.adf");
        let mut image = Vec::new();
        image.extend_from_slice(b"UAE-1ADF");
        image.extend_from_slice(&0u16.to_be_bytes());
        image.extend_from_slice(&0u16.to_be_bytes());
        fs::write(&adf, image)?;
        let cfg = parse_config(&format!(
            r#"
            [floppy.df0]
            path = "{}"
            "#,
            toml_path(&adf)
        ))?;
        let df0 = cfg.floppy.drives[0].as_ref().unwrap();
        assert_eq!(df0.path, adf);
        let _ = fs::remove_file(df0.path.clone());
        Ok(())
    }

    #[test]
    fn ipf_floppy_path_is_accepted() -> Result<()> {
        let ipf = temp_path("test.ipf");
        // The bare CAPS signature chunk an IPF opens with.
        fs::write(&ipf, b"CAPS\x00\x00\x00\x0c")?;
        let cfg = parse_config(&format!(
            r#"
            [floppy.df0]
            path = "{}"
            "#,
            toml_path(&ipf)
        ))?;
        let df0 = cfg.floppy.drives[0].as_ref().unwrap();
        assert_eq!(df0.path, ipf);
        assert!(df0.write_protected);
        let _ = fs::remove_file(df0.path.clone());
        Ok(())
    }

    #[test]
    fn scp_floppy_path_is_accepted() -> Result<()> {
        let scp = temp_path("test.scp");
        fs::write(&scp, b"SCP\x25\x04\x01\x00\x00")?;
        let cfg = parse_config(&format!(
            r#"
            [floppy.df0]
            path = "{}"
            "#,
            toml_path(&scp)
        ))?;
        let df0 = cfg.floppy.drives[0].as_ref().unwrap();
        assert_eq!(df0.path, scp);
        assert!(df0.write_protected);
        let _ = fs::remove_file(df0.path.clone());
        Ok(())
    }

    #[test]
    fn disabled_floppy_ignores_missing_path() -> Result<()> {
        let cfg = parse_config(
            r#"
            [floppy.df1]
            enabled = false
            "#,
        )?;
        assert!(cfg.floppy.drives[1].is_none());
        Ok(())
    }

    #[test]
    fn floppy_image_connects_external_drive_without_count() -> Result<()> {
        let adf = temp_adf()?;
        let cfg = parse_config(&format!(
            r#"
            [floppy.df1]
            path = "{}"
            "#,
            toml_path(&adf)
        ))?;
        assert_eq!(cfg.floppy_connected, [true, true, false, false]);
        Ok(())
    }

    #[test]
    fn floppy_drive_count_rejects_media_beyond_connected_slots() -> Result<()> {
        let adf = temp_adf()?;
        let err = parse_config(&format!(
            r#"
            [floppy]
            drives = 1
            [floppy.df1]
            path = "{}"
            "#,
            toml_path(&adf)
        ))
        .unwrap_err();
        assert!(
            err.to_string().contains("leaves floppy.df1 disconnected"),
            "{err:#}"
        );
        let err = parse_config("[floppy]\ndrives = 0").unwrap_err();
        assert!(err.to_string().contains("between 1 and 4"), "{err:#}");
        Ok(())
    }

    #[test]
    fn rtg_card_selects_the_board() -> Result<()> {
        // The board is Zorro III, so these need a 32-bit-bus CPU.
        let with_cpu = |rtg: &str| format!("[cpu]\nmodel = \"68030\"\n{rtg}");
        assert_eq!(
            parse_config(&with_cpu("[rtg]\ncard = \"z3660\"\n"))?.rtg,
            RtgCard::Z3660
        );
        // Spelling and spacing are forgiving, as for [scsi] controller.
        assert_eq!(
            parse_config(&with_cpu("[rtg]\ncard = \" Z3660 \"\n"))?.rtg,
            RtgCard::Z3660
        );
        assert_eq!(
            parse_config(&with_cpu("[rtg]\ncard = \"none\"\n"))?.rtg,
            RtgCard::None
        );
        // A bare config is a 68000 machine, which cannot host a Zorro III
        // board, so nothing is fitted.
        assert_eq!(parse_config("")?.rtg, RtgCard::None);

        // Picasso II is a Zorro II card and remains valid on the default
        // 68000 machine. Its fitted memory is part of the resolved config.
        let picasso = parse_config("[rtg]\ncard = \" Picasso2 \"\nvram = \"1M\"\n")?;
        assert_eq!(picasso.rtg, RtgCard::Picasso2);
        assert_eq!(picasso.rtg_vram_bytes, 1024 * 1024);
        let picasso = parse_config("[rtg]\ncard = \"picasso2\"\n")?;
        assert_eq!(picasso.rtg_vram_bytes, 2 * 1024 * 1024);
        let plus = parse_config("[rtg]\ncard = \" Picasso2Plus \"\nvram = \"1M\"\n")?;
        assert_eq!(plus.rtg, RtgCard::Picasso2Plus);
        assert_eq!(plus.rtg_vram_bytes, 1024 * 1024);
        let plus_alias = parse_config("[rtg]\ncard = \"picasso2+\"\n")?;
        assert_eq!(plus_alias.rtg, RtgCard::Picasso2Plus);
        Ok(())
    }

    #[test]
    fn picasso2_rejects_non_hardware_vram_sizes() {
        let err = parse_config("[rtg]\ncard = \"picasso2\"\nvram = \"3M\"\n").unwrap_err();
        assert!(
            err.to_string().contains("must be \"1M\" or \"2M\""),
            "{err:#}"
        );
        let err = parse_config("[rtg]\ncard = \"picasso2plus\"\nvram = \"3M\"\n").unwrap_err();
        assert!(
            err.to_string().contains("must be \"1M\" or \"2M\""),
            "{err:#}"
        );
    }

    #[test]
    fn rtg_vram_is_ignored_by_non_picasso_cards() {
        let cfg = parse_config("[rtg]\ncard = \"none\"\nvram = \"oops\"\n").unwrap();
        assert_eq!(cfg.rtg, RtgCard::None);
        assert_eq!(cfg.rtg_vram_bytes, 2 * 1024 * 1024);
        let cfg =
            parse_config("[cpu]\nmodel = \"68030\"\n\n[rtg]\ncard = \"z3660\"\nvram = \"3M\"\n")
                .unwrap();
        assert_eq!(cfg.rtg, RtgCard::Z3660);
        assert_eq!(cfg.rtg_vram_bytes, 2 * 1024 * 1024);
    }

    /// A machine that can host a Zorro III board gets one fitted by default,
    /// so RTG needs no config beyond the guest driver. The gate is the CPU's
    /// address bus, the same one Zorro III RAM uses, not a model list.
    #[test]
    fn rtg_card_defaults_to_the_machine_capability() -> Result<()> {
        assert_eq!(
            parse_config("[machine]\nprofile = \"A4000\"\n")?.rtg,
            RtgCard::Z3660
        );
        assert_eq!(
            parse_config("[machine]\nprofile = \"A3000\"\n")?.rtg,
            RtgCard::Z3660
        );
        // 68EC020: 24-bit bus, so no Zorro III and no card.
        assert_eq!(
            parse_config("[machine]\nprofile = \"A1200\"\n")?.rtg,
            RtgCard::None
        );
        assert_eq!(
            parse_config("[machine]\nprofile = \"A500\"\n")?.rtg,
            RtgCard::None
        );
        // Asking anyway is an error rather than a board the CPU cannot reach.
        let err =
            parse_config("[machine]\nprofile = \"A500\"\n[rtg]\ncard = \"z3660\"\n").unwrap_err();
        assert!(err.to_string().contains("32-bit address bus"), "{err:#}");
        Ok(())
    }

    #[test]
    fn unknown_rtg_card_fails_cleanly() {
        let err = parse_config("[rtg]\ncard = \"picasso4\"\n").unwrap_err();
        assert!(err.to_string().contains("is not known"), "{err:#}");
    }

    #[test]
    fn enabled_floppy_requires_path() {
        let err = parse_config(
            r#"
            [floppy.df0]
            enabled = true
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("has no path"), "{err:#}");
    }

    #[test]
    fn bad_floppy_size_fails_cleanly() -> Result<()> {
        let path = temp_path("bad.adf");
        fs::write(&path, [0u8; 512])?;
        let err = parse_config(&format!(
            r#"
            [floppy.df0]
            path = "{}"
            "#,
            toml_path(&path)
        ))
        .unwrap_err();
        let _ = fs::remove_file(&path);
        assert!(err.to_string().contains("expected 901120 bytes"), "{err:#}");
        Ok(())
    }

    #[test]
    fn cli_overrides_select_a_machine_with_no_config_file() -> Result<()> {
        let overrides = ConfigOverrides {
            model: Some("A1200".to_string()),
            ..Default::default()
        };
        let cfg = load_overrides(&overrides)?;
        assert_eq!(cfg.machine, Some(MachineModel::A1200));
        assert_eq!(cfg.cpu, CpuModel::M68EC020);
        assert_eq!(cfg.chipset, Chipset::Aga);
        assert_eq!(cfg.chip_ram_bytes, 2 * 1024 * 1024);
        Ok(())
    }

    #[test]
    fn cli_overrides_layer_on_top_of_a_profile() -> Result<()> {
        // A model plus explicit CPU/fast-RAM overrides: the profile supplies
        // the chipset and chip RAM, the overrides win where they are set, and
        // everything still goes through the normal validation/derivation.
        let overrides = ConfigOverrides {
            model: Some("A500".to_string()),
            cpu: Some("68020".to_string()),
            fpu: Some(true),
            cpu_clock_mhz: Some(28.0),
            fast: Some("4M".to_string()),
            ..Default::default()
        };
        let cfg = load_overrides(&overrides)?;
        assert_eq!(cfg.machine, Some(MachineModel::A500));
        assert_eq!(cfg.cpu, CpuModel::M68020);
        assert!(cfg.fpu);
        assert_eq!(cfg.cpu_clock_mhz, 28.0);
        assert_eq!(cfg.fast_ram_bytes, 4 * 1024 * 1024);
        assert_eq!(cfg.slow_ram_bytes, 512 * 1024);
        Ok(())
    }

    #[test]
    fn cli_floppy_drive_override_uses_config_validation() -> Result<()> {
        let overrides = ConfigOverrides {
            floppy_drives: Some(4),
            ..Default::default()
        };
        let cfg = load_overrides(&overrides)?;
        assert_eq!(cfg.floppy_connected, [true, true, true, true]);

        let overrides = ConfigOverrides {
            floppy_drives: Some(5),
            ..Default::default()
        };
        let err = load_overrides(&overrides).unwrap_err();
        assert!(err.to_string().contains("between 1 and 4"), "{err:#}");
        Ok(())
    }

    #[test]
    fn serial_defaults_to_stdout() -> Result<()> {
        // An unconfigured machine keeps the historical terminal output.
        let cfg = parse_config("")?;
        assert_eq!(cfg.serial.mode, SerialMode::Stdout);
        assert_eq!(Config::default().serial.mode, SerialMode::Stdout);
        Ok(())
    }

    #[test]
    fn serial_section_selects_mode_and_midi_endpoints() -> Result<()> {
        let cfg = parse_config(
            "[serial]\nmode = \"midi\"\nmidi_out = \"USB MIDI\"\nmidi_in = \"USB MIDI\"\n",
        )?;
        assert_eq!(cfg.serial.mode, SerialMode::Midi);
        assert_eq!(cfg.serial.midi_out.as_deref(), Some("USB MIDI"));
        assert_eq!(cfg.serial.midi_in.as_deref(), Some("USB MIDI"));

        let err = parse_config("[serial]\nmode = \"rs232\"\n").unwrap_err();
        assert!(err.to_string().contains("unknown [serial] mode"), "{err:#}");
        Ok(())
    }

    #[test]
    fn serial_section_selects_tcp_connect_and_address() -> Result<()> {
        let cfg =
            parse_config("[serial]\nmode = \"tcp-connect\"\nconnect = \"bbs.example.com:1337\"\n")?;
        assert_eq!(cfg.serial.mode, SerialMode::TcpConnect);
        assert_eq!(cfg.serial.connect.as_deref(), Some("bbs.example.com:1337"));
        Ok(())
    }

    #[test]
    fn cli_serial_connect_implies_tcp_connect_mode() -> Result<()> {
        // Like --midi-out implying midi mode: naming a dial-out address is
        // enough, unless --serial explicitly chose another mode.
        let overrides = ConfigOverrides {
            serial_connect: Some("bbs.example.com:1337".to_string()),
            ..Default::default()
        };
        let cfg = load_overrides(&overrides)?;
        assert_eq!(cfg.serial.mode, SerialMode::TcpConnect);
        assert_eq!(cfg.serial.connect.as_deref(), Some("bbs.example.com:1337"));

        let overrides = ConfigOverrides {
            serial: Some("off".to_string()),
            serial_connect: Some("bbs.example.com:1337".to_string()),
            ..Default::default()
        };
        let cfg = load_overrides(&overrides)?;
        assert_eq!(cfg.serial.mode, SerialMode::Off);
        assert_eq!(cfg.serial.connect.as_deref(), Some("bbs.example.com:1337"));
        Ok(())
    }

    #[test]
    fn parallel_section_selects_raw_capture_path() -> Result<()> {
        // A bare output path implies the printer (back-compat).
        let cfg = parse_config("[parallel]\noutput = \"printer.raw\"\n")?;
        assert_eq!(cfg.parallel.device, ParallelDevice::Printer);
        assert_eq!(
            cfg.parallel.printer_output.as_deref(),
            Some(std::path::Path::new("printer.raw"))
        );
        // An empty port is the default.
        assert_eq!(parse_config("")?.parallel.device, ParallelDevice::None);
        assert_eq!(parse_config("")?.parallel.printer_output, None);

        let err = parse_config("[parallel]\nmode = \"printer\"\n").unwrap_err();
        assert!(err.to_string().contains("unknown field `mode`"), "{err:#}");
        Ok(())
    }

    #[test]
    fn parallel_device_selects_printer_or_sampler() -> Result<()> {
        // An explicit sampler with its options (gain in dB).
        let cfg = parse_config(
            "[parallel]\ndevice = \"sampler\"\nsampler_input = \"BlackHole\"\nsampler_gain = 6.0\n",
        )?;
        assert_eq!(cfg.parallel.device, ParallelDevice::Sampler);
        assert_eq!(cfg.parallel.sampler_input.as_deref(), Some("BlackHole"));
        assert_eq!(cfg.parallel.sampler_gain_db, 6.0);
        // 0 dB (unity) is valid.
        assert_eq!(
            parse_config("[parallel]\ndevice = \"sampler\"\nsampler_gain = 0\n")?
                .parallel
                .sampler_gain_db,
            0.0
        );

        // An explicit printer needs an output path.
        let err = parse_config("[parallel]\ndevice = \"printer\"\n").unwrap_err();
        assert!(err.to_string().contains("needs an output path"), "{err:#}");

        // Out-of-range gain (dB) is rejected.
        let err =
            parse_config("[parallel]\ndevice = \"sampler\"\nsampler_gain = 100\n").unwrap_err();
        assert!(err.to_string().contains("sampler_gain"), "{err:#}");

        // An unknown device name is rejected.
        let err = parse_config("[parallel]\ndevice = \"plotter\"\n").unwrap_err();
        assert!(err.to_string().contains("must be"), "{err:#}");

        // `none` explicitly empties the port even with a stale output path.
        let cfg = parse_config("[parallel]\ndevice = \"none\"\n")?;
        assert_eq!(cfg.parallel.device, ParallelDevice::None);
        Ok(())
    }

    #[test]
    fn audio_section_selects_output_device() -> Result<()> {
        let cfg = parse_config("[audio]\noutput_device = \"External Speakers\"\n")?;
        assert_eq!(
            cfg.audio.output_device.as_deref(),
            Some("External Speakers")
        );

        // A blank name means "use the system default".
        let cfg = parse_config("[audio]\noutput_device = \"  \"\n")?;
        assert_eq!(cfg.audio.output_device, None);

        // Omitting it entirely is the default.
        assert_eq!(parse_config("")?.audio.output_device, None);
        // A pre-existing [audio] block that never mentions output_device still
        // parses and leaves it None (system default) -- older configs are safe.
        let cfg = parse_config("[audio]\nfloppy_sounds = true\nfloppy_sounds_volume = 80\n")?;
        assert_eq!(cfg.audio.output_device, None);
        Ok(())
    }

    #[test]
    fn audio_output_enabled_defaults_true_and_parses() -> Result<()> {
        // Default and older configs (no key) stay enabled.
        assert!(parse_config("")?.audio.output_enabled);
        assert!(
            parse_config("[audio]\noutput_device = \"Speakers\"\n")?
                .audio
                .output_enabled
        );
        // The GUI "Disabled" option persists as output_enabled = false.
        assert!(
            !parse_config("[audio]\noutput_enabled = false\n")?
                .audio
                .output_enabled
        );
        assert!(
            parse_config("[audio]\noutput_enabled = true\n")?
                .audio
                .output_enabled
        );
        Ok(())
    }

    #[test]
    fn cli_audio_device_overrides_config() -> Result<()> {
        let overrides = ConfigOverrides {
            audio_device: Some("BlackHole".to_string()),
            ..Default::default()
        };
        let cfg = load_overrides(&overrides)?;
        assert_eq!(cfg.audio.output_device.as_deref(), Some("BlackHole"));
        Ok(())
    }

    #[test]
    fn audio_channel_mode_defaults_to_stereo_and_parses() -> Result<()> {
        assert_eq!(parse_config("")?.audio.channel_mode, ChannelMode::Stereo);
        assert_eq!(
            parse_config("[audio]\nchannel_mode = \"mono\"\n")?
                .audio
                .channel_mode,
            ChannelMode::Mono
        );
        assert_eq!(
            parse_config("[audio]\nchannel_mode = \"STEREO\"\n")?
                .audio
                .channel_mode,
            ChannelMode::Stereo
        );
        assert!(parse_config("[audio]\nchannel_mode = \"quad\"\n").is_err());

        // CLI override.
        let overrides = ConfigOverrides {
            audio_channel_mode: Some("mono".to_string()),
            ..Default::default()
        };
        assert_eq!(
            load_overrides(&overrides)?.audio.channel_mode,
            ChannelMode::Mono
        );
        Ok(())
    }

    #[test]
    fn audio_filter_defaults_to_auto_and_parses() -> Result<()> {
        assert_eq!(parse_config("")?.audio.filter, AudioFilterMode::Auto);
        assert_eq!(
            parse_config("[audio]\naudio_filter = \"on\"\n")?
                .audio
                .filter,
            AudioFilterMode::On
        );
        assert_eq!(
            parse_config("[audio]\naudio_filter = \"OFF\"\n")?
                .audio
                .filter,
            AudioFilterMode::Off
        );
        assert_eq!(
            parse_config("[audio]\naudio_filter = \"disabled\"\n")?
                .audio
                .filter,
            AudioFilterMode::Off
        );
        assert!(parse_config("[audio]\naudio_filter = \"sometimes\"\n").is_err());
        // `filter` is accepted as an alias for `audio_filter`.
        assert_eq!(
            parse_config("[audio]\nfilter = \"off\"\n")?.audio.filter,
            AudioFilterMode::Off
        );

        // CLI override.
        let overrides = ConfigOverrides {
            audio_filter: Some("on".to_string()),
            ..Default::default()
        };
        assert_eq!(
            load_overrides(&overrides)?.audio.filter,
            AudioFilterMode::On
        );
        Ok(())
    }

    #[test]
    fn audio_stereo_separation_defaults_to_100_and_validates() -> Result<()> {
        assert_eq!(parse_config("")?.audio.stereo_separation, 100);
        assert_eq!(
            parse_config("[audio]\nstereo_separation = 0\n")?
                .audio
                .stereo_separation,
            0
        );
        assert_eq!(
            parse_config("[audio]\nstereo_separation = 60\n")?
                .audio
                .stereo_separation,
            60
        );
        assert!(parse_config("[audio]\nstereo_separation = 150\n").is_err());

        let overrides = ConfigOverrides {
            audio_stereo_separation: Some(20),
            ..Default::default()
        };
        assert_eq!(load_overrides(&overrides)?.audio.stereo_separation, 20);
        Ok(())
    }

    #[test]
    fn cli_midi_endpoint_implies_midi_mode() -> Result<()> {
        // Naming an endpoint is enough to switch the serial port to MIDI.
        let overrides = ConfigOverrides {
            midi_out: Some("Deluge".to_string()),
            ..Default::default()
        };
        let cfg = load_overrides(&overrides)?;
        assert_eq!(cfg.serial.mode, SerialMode::Midi);
        assert_eq!(cfg.serial.midi_out.as_deref(), Some("Deluge"));

        // An explicit --serial still wins over the implication.
        let overrides = ConfigOverrides {
            serial: Some("stdout".to_string()),
            midi_in: Some("Deluge".to_string()),
            ..Default::default()
        };
        let cfg = load_overrides(&overrides)?;
        assert_eq!(cfg.serial.mode, SerialMode::Stdout);
        Ok(())
    }

    #[test]
    fn a2065_bridge_requires_and_preserves_interface() -> Result<()> {
        let cfg = parse_config(
            r#"
            [a2065]
            net = "bridge"
            interface = "en-test"
            "#,
        )?;
        assert_eq!(
            cfg.a2065_net,
            Some(crate::net::NetConfig::Bridge {
                interface: "en-test".to_string()
            })
        );

        let missing = parse_config("[a2065]\nnet = \"bridge\"\n").unwrap_err();
        assert!(
            missing.to_string().contains("needs an interface"),
            "{missing:#}"
        );
        let stray = parse_config("[a2065]\ninterface = \"en-test\"\n").unwrap_err();
        assert!(stray.to_string().contains("needs net"), "{stray:#}");
        let conflict =
            parse_config("[a2065]\nnet = \"nat\"\ninterface = \"en-test\"\n").unwrap_err();
        assert!(
            conflict.to_string().contains("applies only"),
            "{conflict:#}"
        );

        let overrides = ConfigOverrides {
            a2065_interface: Some("eth-test".to_string()),
            ..Default::default()
        };
        assert_eq!(
            load_overrides(&overrides)?.a2065_net,
            Some(crate::net::NetConfig::Bridge {
                interface: "eth-test".to_string()
            })
        );

        // Replacing a file's bridge backend from the CLI also clears the
        // now-inapplicable carried interface.
        let mut raw: RawConfig =
            toml::from_str("[a2065]\nnet = \"bridge\"\ninterface = \"en-test\"\n")?;
        ConfigOverrides {
            a2065_net: Some("nat".to_string()),
            ..Default::default()
        }
        .apply_to(&mut raw);
        assert!(raw.a2065.interface.is_none());
        assert_eq!(
            Config::try_from(raw)?.a2065_net,
            Some(crate::net::NetConfig::Nat)
        );
        Ok(())
    }

    #[test]
    fn hostsocket_expands_to_the_bundled_wasm_board() -> Result<()> {
        // No [hostsocket] section, no board.
        let cfg = parse_config("")?;
        assert!(cfg.wasm_boards.is_empty());

        let cfg = parse_config(
            r#"
            [hostsocket]
            net = "loopback"
            hostname = "workbench"
            "#,
        )?;
        assert_eq!(cfg.wasm_boards.len(), 1);
        let board = &cfg.wasm_boards[0];
        assert_eq!(
            board.wasm_path,
            Path::new(crate::hostsocket::BUNDLED_HOSTSOCKET_WASM)
        );
        assert_eq!(
            board.spec.manufacturer,
            crate::zorro::COPPERLINE_MANUFACTURER_ID
        );
        assert_eq!(board.spec.diag_vec, Some(crate::hostsocket::DIAG_OFFSET));
        assert!(board.manifest.caps.dma && board.manifest.caps.net);
        assert_eq!(board.manifest.net, crate::net::NetConfig::Loopback);
        assert_eq!(
            board.manifest.config.get("rom").map(String::as_str),
            Some(crate::hostsocket::BUNDLED_HOSTSOCKET_ROM)
        );
        assert_eq!(
            board.manifest.config.get("dns_server").map(String::as_str),
            Some(crate::hostsocket::DEFAULT_DNS_SERVER)
        );
        assert_eq!(
            board.manifest.config.get("hostname").map(String::as_str),
            Some("workbench")
        );
        assert_eq!(board.manifest.file_keys, vec!["rom".to_string()]);
        // address/gateway are bridge-only and left unset here, so they
        // must not appear in the manifest at all -- the plugin's own
        // nat/loopback-shaped defaults (INTERFACE_ADDR/NAT_GATEWAY_ADDR)
        // must apply, not an empty-string override.
        assert!(!board.manifest.config.contains_key("address"));
        assert!(!board.manifest.config.contains_key("gateway"));

        // The bundled board composes with [[zorro]] metadata boards; it is
        // appended after them, so their windows are assigned first.
        Ok(())
    }

    #[test]
    fn hostsocket_bridge_address_and_gateway_reach_the_manifest() -> Result<()> {
        let cfg = parse_config(
            r#"
            [hostsocket]
            net = "bridge"
            interface = "en0"
            address = "192.168.1.50/24"
            gateway = "192.168.1.1"
            "#,
        )?;
        let board = &cfg.wasm_boards[0];
        assert_eq!(
            board.manifest.config.get("address").map(String::as_str),
            Some("192.168.1.50/24")
        );
        assert_eq!(
            board.manifest.config.get("gateway").map(String::as_str),
            Some("192.168.1.1")
        );
        Ok(())
    }

    #[test]
    fn hostsocket_resolver_host_reaches_the_manifest_under_nat_or_bridge() -> Result<()> {
        let cfg = parse_config(
            r#"
            [hostsocket]
            net = "nat"
            resolver = "host"
            "#,
        )?;
        assert_eq!(
            cfg.wasm_boards[0]
                .manifest
                .config
                .get("resolver")
                .map(String::as_str),
            Some("host")
        );

        let cfg = parse_config(
            r#"
            [hostsocket]
            net = "bridge"
            interface = "en0"
            resolver = "HOST"
            "#,
        )?;
        // Normalized to lowercase on the way into the manifest.
        assert_eq!(
            cfg.wasm_boards[0]
                .manifest
                .config
                .get("resolver")
                .map(String::as_str),
            Some("host")
        );
        Ok(())
    }

    #[test]
    fn hostsocket_resolver_defaults_to_host_under_nat_and_bridge_only() -> Result<()> {
        // No explicit `resolver` key anywhere below: nat/bridge should
        // still get "host" (the thing that works without a hand-matched
        // dns_server), while loopback -- where "host" would be rejected
        // outright -- gets no resolver key at all, not a default value
        // that would have failed validation.
        let cfg = parse_config("[hostsocket]\nnet = \"nat\"\n")?;
        assert_eq!(
            cfg.wasm_boards[0]
                .manifest
                .config
                .get("resolver")
                .map(String::as_str),
            Some("host")
        );

        let cfg = parse_config("[hostsocket]\nnet = \"bridge\"\ninterface = \"en0\"\n")?;
        assert_eq!(
            cfg.wasm_boards[0]
                .manifest
                .config
                .get("resolver")
                .map(String::as_str),
            Some("host")
        );

        let cfg = parse_config("[hostsocket]\nnet = \"loopback\"\n")?;
        assert!(!cfg.wasm_boards[0].manifest.config.contains_key("resolver"));

        // An explicit "dns" still opts back out under a backend that would
        // otherwise default to "host" -- e.g. to use a specific dns_server.
        let cfg = parse_config(
            r#"
            [hostsocket]
            net = "nat"
            resolver = "dns"
            dns_server = "1.2.3.4"
            "#,
        )?;
        assert_eq!(
            cfg.wasm_boards[0]
                .manifest
                .config
                .get("resolver")
                .map(String::as_str),
            Some("dns")
        );
        assert_eq!(
            cfg.wasm_boards[0]
                .manifest
                .config
                .get("dns_server")
                .map(String::as_str),
            Some("1.2.3.4")
        );
        Ok(())
    }

    #[test]
    fn hostsocket_resolver_host_rejected_under_loopback_or_bad_value() {
        let err = parse_config(
            r#"
            [hostsocket]
            net = "loopback"
            resolver = "host"
            "#,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("needs net = \"nat\", \"bridge\", or \"host\""),
            "{err:#}"
        );

        let err = parse_config(
            r#"
            [hostsocket]
            net = "nat"
            resolver = "carrier-pigeon"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("is not one of"), "{err:#}");
    }

    #[test]
    fn hostsocket_bridge_requires_and_preserves_interface() -> Result<()> {
        let cfg = parse_config(
            r#"
            [hostsocket]
            net = "bridge"
            interface = "en-test"
            "#,
        )?;
        assert_eq!(
            cfg.wasm_boards[0].manifest.net,
            crate::net::NetConfig::Bridge {
                interface: "en-test".to_string()
            }
        );

        let missing = parse_config("[hostsocket]\nnet = \"bridge\"\n").unwrap_err();
        assert!(
            missing.to_string().contains("needs an interface"),
            "{missing:#}"
        );
        let stray = parse_config("[hostsocket]\ninterface = \"en-test\"\n").unwrap_err();
        assert!(stray.to_string().contains("needs net"), "{stray:#}");
        let conflict =
            parse_config("[hostsocket]\nnet = \"nat\"\ninterface = \"en-test\"\n").unwrap_err();
        assert!(
            conflict.to_string().contains("applies only"),
            "{conflict:#}"
        );

        let overrides = ConfigOverrides {
            hostsocket_interface: Some("eth-test".to_string()),
            ..Default::default()
        };
        assert_eq!(
            load_overrides(&overrides)?.wasm_boards[0].manifest.net,
            crate::net::NetConfig::Bridge {
                interface: "eth-test".to_string()
            }
        );

        // Replacing a file's bridge backend from the CLI also clears the
        // now-inapplicable carried interface.
        let mut raw: RawConfig =
            toml::from_str("[hostsocket]\nnet = \"bridge\"\ninterface = \"en-test\"\n")?;
        ConfigOverrides {
            hostsocket_net: Some("nat".to_string()),
            ..Default::default()
        }
        .apply_to(&mut raw);
        assert!(raw.hostsocket.interface.is_none());
        assert_eq!(
            Config::try_from(raw)?.wasm_boards[0].manifest.net,
            crate::net::NetConfig::Nat
        );
        Ok(())
    }

    #[test]
    fn hostsocket_net_host_selects_the_host_socket_backend() -> Result<()> {
        let cfg = parse_config("[hostsocket]\nnet = \"host\"\n")?;
        let board = &cfg.wasm_boards[0];
        // The underlying smoltcp interface is hardcoded to loopback under
        // "host" -- ICMP/DNS-over-net are the only things still on it,
        // and this mode's whole premise is zero-config, so it isn't
        // user-selectable here the way "bridge" picks a real backend.
        assert_eq!(board.manifest.net, crate::net::NetConfig::Loopback);
        assert!(board.manifest.caps.host_sockets);
        assert_eq!(
            board.manifest.config.get("transport").map(String::as_str),
            Some("host")
        );
        // resolver defaults to "host" here too (loopback's own smoltcp
        // interface couldn't reach a real dns_server anyway).
        assert_eq!(
            board.manifest.config.get("resolver").map(String::as_str),
            Some("host")
        );
        Ok(())
    }

    #[test]
    fn hostsocket_net_host_rejects_interface_address_and_gateway() {
        let err = parse_config("[hostsocket]\nnet = \"host\"\ninterface = \"en0\"\n").unwrap_err();
        assert!(err.to_string().contains("applies only to net"), "{err:#}");

        let err = parse_config(
            r#"
            [hostsocket]
            net = "host"
            address = "192.168.1.50/24"
            "#,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("address/gateway don't apply"),
            "{err:#}"
        );

        let err = parse_config(
            r#"
            [hostsocket]
            net = "host"
            gateway = "192.168.1.1"
            "#,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("address/gateway don't apply"),
            "{err:#}"
        );
    }

    #[test]
    fn hostsocket_net_host_accepts_an_explicit_resolver_override() -> Result<()> {
        // "host" is the default under net = "host" too, but an explicit
        // choice (either value) is still honoured, same as nat/bridge.
        let cfg = parse_config("[hostsocket]\nnet = \"host\"\nresolver = \"dns\"\n")?;
        assert_eq!(
            cfg.wasm_boards[0]
                .manifest
                .config
                .get("resolver")
                .map(String::as_str),
            Some("dns")
        );

        let cfg = parse_config("[hostsocket]\nnet = \"host\"\nresolver = \"host\"\n")?;
        assert_eq!(
            cfg.wasm_boards[0]
                .manifest
                .config
                .get("resolver")
                .map(String::as_str),
            Some("host")
        );
        Ok(())
    }

    #[test]
    fn hostsocket_net_host_reachable_from_the_cli_override() -> Result<()> {
        let overrides = ConfigOverrides {
            hostsocket_net: Some("host".to_string()),
            ..Default::default()
        };
        let cfg = load_overrides(&overrides)?;
        assert_eq!(
            cfg.wasm_boards[0].manifest.net,
            crate::net::NetConfig::Loopback
        );
        assert_eq!(
            cfg.wasm_boards[0]
                .manifest
                .config
                .get("transport")
                .map(String::as_str),
            Some("host")
        );
        Ok(())
    }

    #[test]
    fn cli_overrides_are_validated_like_config_fields() {
        // A 68000 cannot carry an FPU; the override hits the same check as
        // `[cpu] fpu = true` would.
        let overrides = ConfigOverrides {
            cpu: Some("68000".to_string()),
            fpu: Some(true),
            ..Default::default()
        };
        let err = load_overrides(&overrides).unwrap_err();
        assert!(err.to_string().contains("coprocessor interface"), "{err:#}");

        // An unknown chipset name is rejected by the shared parser.
        let overrides = ConfigOverrides {
            chipset: Some("OCS3".to_string()),
            ..Default::default()
        };
        let err = load_overrides(&overrides).unwrap_err();
        assert!(err.to_string().contains("unknown chipset"), "{err:#}");
    }

    fn temp_adf() -> Result<PathBuf> {
        let path = temp_path("test.adf");
        fs::write(&path, vec![0u8; 80 * 2 * 11 * 512])?;
        Ok(path)
    }

    fn temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("copperline-config-test-{nanos}-{name}"))
    }

    fn toml_path(path: &Path) -> String {
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    }
}
