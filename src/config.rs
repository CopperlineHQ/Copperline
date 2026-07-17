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
    /// ([`CpuModel::default_clock_mhz`]); overridable via `[cpu] clock_mhz`.
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
    pub emulation: Emulation,
    pub chip_ram_bytes: usize,
    pub fast_ram_bytes: usize,
    pub slow_ram_bytes: usize,
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
    /// Whether the MSM6242/OKI RTC at $DC0000 is fitted. Defaults to false:
    /// the base A500/A500OCS, A600, A1200, A1000, and CD32 shipped without a
    /// battery-backed clock. Only the A500+ (soldered on the Rev 8A board) and
    /// the CDTV carry one by default; the A600HD and a clock-equipped A1200 set
    /// `[machine] rtc = true`.
    pub rtc_present: bool,
    /// Power-on RTC value in Unix seconds (`[machine] rtc_time` /
    /// `--rtc-time`). When set, the clock starts here and ticks with
    /// *emulated* time instead of following the host wall clock, so the
    /// guest-visible time is deterministic and reproducible. Setting a time
    /// implies fitting the clock (`rtc = true`).
    pub rtc_seed_unix: Option<u64>,
    /// Stop the seeded RTC (`[machine] rtc_frozen`): every read returns
    /// exactly `rtc_seed_unix`, for pinning a guest to one time window.
    pub rtc_frozen: bool,
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
    /// CRT phosphor persistence: the fraction of the previous presented
    /// frame each new frame keeps (0.0 = off). Approximates the phosphor
    /// decay that fuses field-rate dither and interlace flicker on a
    /// real CRT.
    pub phosphor: f32,
    /// Initial host input source for the emulated joystick/CD32-pad port
    /// (`[input] joystick` / `--joystick`). Defaults to
    /// [`JoystickInputMode::Gamepad`]; the runtime status-bar toggle, `Cmd+J` /
    /// `Alt+J`, and the menu's Joystick Input item flip it live without
    /// affecting this start-up value.
    pub joystick_input_mode: JoystickInputMode,
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
    /// TCP listen address for [`SerialMode::Tcp`]; `None` means the
    /// default `127.0.0.1:1234` (the port UAE's `TCP:` serial uses).
    pub listen: Option<String>,
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
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdeConfig {
    pub master: Option<DriveImage>,
    pub slave: Option<DriveImage>,
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
    /// Whether a `[scsi]` section asked for a board at all (a bare
    /// `controller` with no ROM or drives fits nothing).
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
}

/// Real-mode pacing budget model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacingBudget {
    /// Debit the budget by each instruction's actual returned m68k cycle
    /// count plus the chip-bus waits it incurred, clocking the CPU at its
    /// true cycles-per-instruction. The vendored core's 68000 cycle counts
    /// are now accurate (see `crates/m68k/CYCLE_TIMING_GAP.md`), so this is
    /// the correct hardware-rate model and the default. (A separate
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioConfig {
    /// Synthesized floppy-drive sound effects: motor hum, head-step
    /// clicks for seeks (and the empty-drive change-line poll), and a
    /// faint hiss while disk DMA moves data.
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloppyConfig {
    pub drives: [Option<FloppyDriveConfig>; 4],
}

impl Default for FloppyConfig {
    fn default() -> Self {
        Self {
            drives: std::array::from_fn(|_| None),
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
    /// controller and the battery-backed MSM6242 clock. No Gayle -- the
    /// big-box machines carry Gary -- and no slow RAM. Motherboard fast RAM is
    /// not emulated; the OS is happy taking its fast RAM from the Zorro III
    /// board instead.
    A3000,
    /// A4000: the same board a generation later -- AGA, a 25 MHz 68040, and
    /// Ramsey-07. Same story on Gayle, slow RAM and motherboard fast RAM as
    /// the A3000.
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
            "{profile} / {:?} / {:?} / {:?} / chip {}K fast {}K slow {}K / ROM {}{ext}",
            self.cpu,
            self.chipset,
            self.video_standard,
            self.chip_ram_bytes / 1024,
            self.fast_ram_bytes / 1024,
            self.slow_ram_bytes / 1024,
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
            emulation: Emulation {
                power_on: true,
                pacing_budget: PacingBudget::Cycles,
                realtime_priority: false,
                warp_speed: WarpSpeed::default(),
            },
            chip_ram_bytes: 512 * 1024,
            fast_ram_bytes: 0,
            slow_ram_bytes: A500_TRAPDOOR_RAM_BYTES,
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
            rtc_seed_unix: None,
            rtc_frozen: false,
            video_standard: VideoStandard::Pal,
            audio: AudioConfig::default(),
            ide: IdeConfig::default(),
            scsi: ScsiConfig::default(),
            a2065_net: None,
            floppy: FloppyConfig::default(),
            floppy_connected: [true, false, false, false],
            floppy_playlists: std::array::from_fn(|_| Vec::new()),
            overscan: Overscan::Tv,
            pixel_aspect: PixelAspect::Tv,
            phosphor: 0.0,
            joystick_input_mode: JoystickInputMode::Gamepad,
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
    pub chip: Option<String>,
    pub fast: Option<String>,
    pub slow: Option<String>,
    pub floppy_drives: Option<u8>,
    /// Initial joystick input mode (`--joystick`): "gamepad" or "keyboard"
    /// ("auto" still accepted as a compatibility alias). Validated by the same
    /// parser as `[input] joystick`.
    pub joystick: Option<String>,
    /// Device in game port 1 (`--port1`): "mouse", "joystick", "cd32",
    /// "analogue", or "none". Same parser as `[input] port1`.
    pub port1: Option<String>,
    /// Device in game port 2 (`--port2`). Same parser as `[input] port2`.
    pub port2: Option<String>,
    /// Serial port wiring (`--serial`): "off", "stdout", "midi", "tcp",
    /// or "pty" ("none" and "terminal" parse as compatibility aliases of
    /// the first two). Same parser as `[serial] mode`.
    pub serial: Option<String>,
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
    /// Stereo separation percent (`--audio-stereo-separation`), 0-100.
    pub audio_stereo_separation: Option<u16>,
    /// Power-on RTC value (`--rtc-time`): Unix seconds or
    /// "YYYY-MM-DD HH:MM[:SS]". Same parser as `[machine] rtc_time`.
    pub rtc_time: Option<String>,
    /// Freeze the seeded RTC (`--rtc-frozen`). Same as
    /// `[machine] rtc_frozen`.
    pub rtc_frozen: Option<bool>,
}

impl ConfigOverrides {
    /// Whether any override was set.
    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.chipset.is_none()
            && self.cpu.is_none()
            && self.fpu.is_none()
            && self.cpu_clock_mhz.is_none()
            && self.chip.is_none()
            && self.fast.is_none()
            && self.slow.is_none()
            && self.floppy_drives.is_none()
            && self.joystick.is_none()
            && self.port1.is_none()
            && self.port2.is_none()
            && self.serial.is_none()
            && self.midi_out.is_none()
            && self.midi_in.is_none()
            && self.parallel.is_none()
            && self.sampler_input.is_none()
            && self.sampler_gain.is_none()
            && self.audio_device.is_none()
            && self.audio_channel_mode.is_none()
            && self.audio_stereo_separation.is_none()
            && self.rtc_time.is_none()
            && self.rtc_frozen.is_none()
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
        if let Some(chip) = &self.chip {
            raw.memory.chip = Some(chip.clone());
        }
        if let Some(fast) = &self.fast {
            raw.memory.fast = Some(fast.clone());
        }
        if let Some(slow) = &self.slow {
            raw.memory.slow = Some(slow.clone());
        }
        if let Some(drives) = self.floppy_drives {
            raw.floppy.drives = Some(drives);
        }
        if let Some(joystick) = &self.joystick {
            raw.input.joystick = Some(joystick.clone());
        }
        if let Some(port1) = &self.port1 {
            raw.input.port1 = Some(port1.clone());
        }
        if let Some(port2) = &self.port2 {
            raw.input.port2 = Some(port2.clone());
        }
        if let Some(mode) = &self.serial {
            raw.serial.mode = Some(mode.clone());
        }
        if let Some(out) = &self.midi_out {
            raw.serial.midi_out = Some(out.clone());
        }
        if let Some(input) = &self.midi_in {
            raw.serial.midi_in = Some(input.clone());
        }
        // Naming a MIDI endpoint on the command line selects MIDI mode unless
        // `--serial` said otherwise.
        if self.serial.is_none() && (self.midi_out.is_some() || self.midi_in.is_some()) {
            raw.serial.mode = Some(SerialMode::Midi.label().to_string());
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
    /// CRT phosphor persistence fraction, 0.0 (off, default) to 0.95.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) phosphor: Option<f32>,
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
}

/// `[serial]` host wiring for Paula's serial (a.k.a. MIDI) port.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawSerial {
    /// "stdout" (default), "off", or "midi".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mode: Option<String>,
    /// Host MIDI output endpoint name (substring match); MIDI mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) midi_out: Option<String>,
    /// Host MIDI input endpoint name (substring match); MIDI mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) midi_in: Option<String>,
    /// TCP listen address; tcp mode only. Defaults to 127.0.0.1:1234.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) listen: Option<String>,
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
/// (`master = { path = "games/", name = "Games" }`). It serializes back to the
/// bare string when no name is set, so existing minimal configs round-trip
/// unchanged.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawDrive {
    pub(crate) path: String,
    pub(crate) name: Option<String>,
}

impl RawDrive {
    pub(crate) fn from_path(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            name: None,
        }
    }
}

impl Serialize for RawDrive {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.name {
            // No override: a plain string keeps saved configs minimal.
            None => serializer.serialize_str(&self.path),
            Some(name) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("path", &self.path)?;
                map.serialize_entry("name", name)?;
                map.end()
            }
        }
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
                f.write_str("a drive image path, or a table with `path` and optional `name`")
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
                        other => {
                            return Err(serde::de::Error::unknown_field(other, &["path", "name"]));
                        }
                    }
                }
                let path = path.ok_or_else(|| serde::de::Error::missing_field("path"))?;
                Ok(RawDrive { path, name })
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
    /// Host network backend: "loopback" (self-contained), or "none" for an
    /// isolated NIC. Absent means no A2065 board is fitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) net: Option<String>,
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
        let joystick_input_mode = match raw.input.joystick.as_deref() {
            None => defaults.joystick_input_mode,
            Some(s) => parse_joystick_input_mode(s)?,
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
            listen: raw.serial.listen.clone(),
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
        let scsi = ScsiConfig {
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
        if scsi.enabled() && scsi.rom.is_none() && scsi.controller.is_zorro_board() {
            let hint = match scsi.controller {
                ScsiController::A4091 => "a raw A4091 EPROM image, e.g. the open-source a4091.rom",
                _ => "an A590/A2091 6.x ROM image; its scsi.device drives the disks",
            };
            errors.push(anyhow!(
                "[scsi] drives need the boot ROM: set [scsi] rom = \"...\" ({hint})"
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

        let a2065_net = match &raw.a2065.net {
            None => None,
            Some(s) => Some(crate::net::parse_net_config(s).ok_or_else(|| {
                anyhow::anyhow!(
                    "[a2065] net = {:?} is not a known backend (expected \"none\" or \"loopback\")",
                    s
                )
            })?),
        };

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

        errors.extend(validate_chip_ram(chip_ram_bytes, chipset, agnus_revision).err());
        errors.extend(validate_fast_ram(fast_ram_bytes, chip_ram_bytes).err());
        errors.extend(validate_slow_ram(slow_ram_bytes).err());
        errors.extend(validate_z3_ram(z3_ram_bytes, cpu).err());
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

        Ok(Config {
            rom_path: raw.rom.map(PathBuf::from).unwrap_or(defaults.rom_path),
            cpu,
            fpu,
            cpu_clock_mhz,
            cpu_icache,
            cpu_dcache,
            cpu_unimplemented,
            emulation,
            chip_ram_bytes,
            fast_ram_bytes,
            slow_ram_bytes,
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
            rtc_present: match raw.machine.rtc {
                // A configured time on an explicitly unfitted clock would
                // silently do nothing; make the contradiction loud.
                Some(false) if rtc_seed_unix.is_some() => anyhow::bail!(
                    "[machine] rtc_time is set but rtc = false leaves the \
                     clock unfitted; drop one of them"
                ),
                Some(fitted) => fitted,
                None => defaults.rtc_present || rtc_seed_unix.is_some(),
            },
            rtc_seed_unix,
            rtc_frozen,
            log_unmapped: raw
                .debug
                .log_unmapped
                .as_deref()
                .map(parse_log_unmapped)
                .transpose()?,
            mem_controller: match raw.machine.mem_controller.as_deref() {
                None => defaults.mem_controller,
                Some("none") => MemController::None,
                Some("ramsey-04") => MemController::Ramsey4,
                Some("ramsey-07") => MemController::Ramsey7,
                Some(other) => anyhow::bail!(
                    "[machine] mem_controller {other:?} is not one of \
                     none, ramsey-04, ramsey-07"
                ),
            },
            video_standard,
            audio,
            ide,
            scsi,
            a2065_net,
            floppy,
            floppy_connected,
            floppy_playlists,
            overscan,
            pixel_aspect,
            phosphor,
            joystick_input_mode,
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

pub(crate) fn parse_port_device(s: &str, key: &str) -> Result<PortDevice> {
    PortDevice::parse(s).ok_or_else(|| {
        anyhow!(
            "[input] {key} must be \"mouse\", \"joystick\", \"cd32\", \
             \"analogue\", or \"none\", got {s:?}"
        )
    })
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

pub(crate) fn parse_serial_mode(s: &str) -> Result<SerialMode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => Ok(SerialMode::Off),
        "stdout" | "terminal" => Ok(SerialMode::Stdout),
        "midi" => Ok(SerialMode::Midi),
        "tcp" => Ok(SerialMode::Tcp),
        "pty" => Ok(SerialMode::Pty),
        _ => Err(anyhow!(
            "unknown [serial] mode {:?}: expected \"off\", \"stdout\", \"midi\", \"tcp\", or \"pty\"",
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

pub(crate) fn parse_machine_model(s: &str) -> Result<MachineModel> {
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
/// `[cpu]`/`[chipset]`/`[memory]` sections override them.
pub(crate) fn machine_profile_defaults(model: MachineModel) -> Config {
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
            d.cpu = CpuModel::M68030;
            d.cpu_clock_mhz = 25.0;
            d.mem_controller = MemController::Ramsey4;
            d.gate_array = GateArray::FatGary;
            d.rtc_present = true;
            d.sdmac = true;
        }
        // The A4000: the same board a generation later -- AGA, a 25 MHz 68040,
        // and Ramsey-07. Its IDE at $DD2020 is Gayle's ATA cable without the
        // gate array; `[ide]` fits drives to it.
        MachineModel::A4000 => {
            d.chipset = Chipset::Aga;
            d.chip_ram_bytes = 2 * 1024 * 1024;
            d.slow_ram_bytes = 0;
            d.cpu = CpuModel::M68040;
            d.cpu_clock_mhz = 25.0;
            d.mem_controller = MemController::Ramsey7;
            d.gate_array = GateArray::FatGary;
            d.rtc_present = true;
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

fn parse_video_standard(s: &str) -> Result<VideoStandard> {
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

fn cpu_has_32bit_bus(cpu: CpuModel) -> bool {
    matches!(
        cpu,
        CpuModel::M68020 | CpuModel::M68030 | CpuModel::M68040 | CpuModel::M68060
    )
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
    let raws = [raw.df0, raw.df1, raw.df2, raw.df3];
    let mut drives: [Option<FloppyDriveConfig>; 4] = std::array::from_fn(|_| None);
    let mut connected = match connected_count {
        Some(count) => std::array::from_fn(|idx| idx < count),
        None => [true, false, false, false],
    };
    let mut playlists: [Vec<PathBuf>; 4] = std::array::from_fn(|_| Vec::new());
    for (idx, raw_drive) in raws.into_iter().enumerate() {
        let Some(raw_drive) = raw_drive else {
            continue;
        };
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
    Ok((FloppyConfig { drives }, connected, playlists))
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
        || &sig == b"UAE-1ADF"
        || &sig == b"UAE--ADF"
    {
        return Ok(());
    }

    bail!(
        "floppy.df{} image {} is {} bytes, expected {} bytes (standard DD ADF),
        gzip-compressed supported image, UAE extended ADF, SCP, DMS or single file ZIP",
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

/// Substitute the bundled AROS ROM when the user named no ROM. The default
/// `rom_path` is a sentinel ([`BUNDLED_AROS_ROM`]); any real path from
/// `rom = "..."` or the CLI argument replaces it before this runs and is left
/// untouched. When the sentinel survives, locate the bundled AROS main +
/// extended ROM pair and rewrite the config to point at them, so every
/// downstream consumer (start-up banner, window title, save states) sees the
/// real paths. An explicit `extended_rom` still wins over the AROS one.
pub fn resolve_bundled_rom(cfg: &mut Config) -> Result<()> {
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
                rtc_time: Some(RawRtcTime::Text("2005-03-18 01:58:29".to_string())),
                rtc_frozen: Some(true),
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
            },
            memory: RawMemory {
                chip: Some("2M".to_string()),
                fast: Some("8M".to_string()),
                slow: None,
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
        let cfg = parse_config(&format!(
            r#"
            [machine]
            profile = "A4000"
            [ide]
            master = "{}"
            "#,
            img.display()
        ))?;
        assert!(!cfg.rom_scsi_device_disable);

        // Same rule on a Gayle machine: an A1200 with no drives skips the
        // driver, one with drives keeps it.
        let cfg = parse_config("[machine]\nprofile = \"A1200\"")?;
        assert!(cfg.rom_scsi_device_disable);
        let cfg = parse_config(&format!(
            "[machine]\nprofile = \"A1200\"\n[ide]\nmaster = \"{}\"",
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
            "[machine]\nprofile = \"A3000\"\n[scsi]\nunit0 = \"{}\"",
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
            }),
            slave: None,
        };
        let text = toml::to_string(&named).unwrap();
        let parsed: RawIde = toml::from_str(&text).unwrap();
        assert_eq!(parsed, named);
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
